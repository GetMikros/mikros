//! Runs of the test image that end in an expected kernel panic: the test
//! init runs the scenario its command line names with `test=`, and the
//! runner may raise an event through the QEMU monitor once the init is
//! waiting for it.

use crate::build::{self, Kind};
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image};
use anyhow::{Context, Result, bail};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A run that ends in a kernel panic.
struct Scenario {
    /// The name of the run and of its directory.
    name: &'static str,
    /// What the kernel command line gets.
    cmdline: &'static str,
    /// The monitor command that raises the panic, once the test init
    /// logs `WAITING`.
    raise: Option<&'static str>,
    /// The title the panic report must have.
    title: &'static str,
    /// Whether the panic report must replace the framebuffer's contents.
    screen: bool,
    /// Whether the console must show text while the test init waits.
    console: bool,
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "init-exit",
        cmdline: "test=init-exit",
        raise: None,
        title: "init terminated with exit code 7",
        screen: true,
        console: false,
    },
    Scenario {
        name: "nmi",
        cmdline: "test=wait",
        raise: Some("nmi"),
        title: "NMI (vector 2)",
        screen: false,
        console: true,
    },
    Scenario {
        name: "machine-check",
        cmdline: "test=wait",
        // An uncorrected error in bank 0, with the valid, uncorrected and
        // enabled bits, and a machine check in progress with the return
        // address valid, which QEMU raises as a machine check.
        raise: Some("mce 0 0 0xb200000000000000 0x5 0 0"),
        title: "machine check (vector 18)",
        screen: false,
        console: false,
    },
];

/// The line the test init logs once it waits for the runner.
pub const WAITING: &str = "SCENARIO waiting";
/// The line the test init logs once it painted the framebuffer.
const PAINTED: &str = "SCENARIO framebuffer painted";
/// The colour it paints the framebuffer, as red, green and blue.
const PAINT: [u8; 3] = [0x12, 0x34, 0x56];
const PANIC_LINE: &str = "KERNEL PANIC: ";
const PANIC_END: &str = "END KERNEL PANIC";
/// How long the screen may take to show a panic report after the serial
/// port did.
const SETTLE: Duration = Duration::from_secs(60);

/// Sends `command` to the QEMU monitor listening on `socket`.
pub fn monitor(socket: &Path, command: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to the monitor at {}", socket.display()))?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    // The monitor reads the command before the connection closes.
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// Reads from `stream` until what it read ends with the monitor's
/// prompt, and returns it.
fn read_to_prompt(stream: &mut UnixStream, deadline: Instant) -> Result<String> {
    let mut read = Vec::new();
    let mut buf = [0u8; 65536];
    while !read.ends_with(b"(qemu) ") {
        if Instant::now() > deadline {
            bail!("the monitor did not answer");
        }
        match stream.read(&mut buf) {
            Ok(0) => bail!("the monitor closed the connection"),
            Ok(n) => read.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(String::from_utf8_lossy(&read).into_owned())
}

/// The lines the monitor on `socket` answers `command` with, without its
/// prompts, its echo of the command and terminal control sequences.
pub fn monitor_output(socket: &Path, command: &str) -> Result<String> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to the monitor at {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    read_to_prompt(&mut stream, deadline)?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    let answer = read_to_prompt(&mut stream, deadline)?;
    Ok(answer
        .lines()
        .map(|line| strip_escapes(line).replace('\r', ""))
        .filter(|line| !line.contains("(qemu)") && !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `line` without ANSI escape sequences.
fn strip_escapes(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI sequences end with a letter.
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// Dumps the screen through the monitor on `socket` to `path` and reads
/// the dump.
pub fn screen_dump(socket: &Path, path: &Path) -> Result<Vec<u8>> {
    monitor(socket, &format!("screendump {}", path.display()))?;
    std::thread::sleep(Duration::from_millis(500));
    fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// Why the screen `ppm` does not show the console's text, if it does not.
fn console_problem(ppm: &[u8]) -> Option<&'static str> {
    match ppm_pixels(ppm) {
        None => Some("the screen dump is not a PPM image"),
        Some(pixels) if pixels.iter().all(|&p| p == pixels[0]) => Some("the console shows nothing"),
        Some(_) => None,
    }
}

/// The pixels of a binary PPM image, or `None` when `ppm` is not one.
pub fn ppm_pixels(ppm: &[u8]) -> Option<Vec<[u8; 3]>> {
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while ppm.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !ppm.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        fields.push(std::str::from_utf8(&ppm[start..at]).ok()?);
    }
    let [magic, width, height, max] = fields[..] else {
        return None;
    };
    let (width, height): (usize, usize) = (width.parse().ok()?, height.parse().ok()?);
    if magic != "P6" || max != "255" {
        return None;
    }
    let data = ppm.get(at + 1..at + 1 + width * height * 3)?;
    Some(data.as_chunks::<3>().0.to_vec())
}

/// Why the screen `ppm` does not show a panic report drawn over a
/// framebuffer painted in `PAINT`, if it does not.
fn screen_problem(ppm: &[u8]) -> Option<&'static str> {
    let Some(pixels) = ppm_pixels(ppm) else {
        return Some("the screen dump is not a PPM image");
    };
    if pixels.contains(&PAINT) {
        return Some("the paint is left on the screen");
    }
    if pixels.iter().all(|&p| p == pixels[0]) {
        return Some("the screen is blank");
    }
    None
}

/// Why `serial` does not show `scenario` ending as it must, if it does
/// not, given whether the screen dump showed a problem.
fn verdict(scenario: &Scenario, serial: &str, screen: Option<&str>) -> Result<(), String> {
    let Some(report) = serial.split_once(PANIC_LINE).map(|(_, r)| r) else {
        return Err("the kernel did not panic".into());
    };
    if !report.starts_with(scenario.title) {
        let line = report.lines().next().unwrap_or("");
        return Err(format!("the kernel panicked with {line:?}"));
    }
    if !report.contains(PANIC_END) {
        return Err("the panic report is cut short".into());
    }
    if let Some(problem) = screen {
        return Err(problem.into());
    }
    Ok(())
}

/// Waits until `log` holds `line`, or fails once `deadline` has passed
/// or `qemu` has exited.
pub fn wait_for_line(
    log: &Path,
    line: &str,
    qemu: &mut Child,
    deadline: Instant,
) -> Result<String> {
    loop {
        let serial = String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).into_owned();
        if serial.contains(line) {
            return Ok(serial);
        }
        if qemu.try_wait()?.is_some() {
            bail!("QEMU exited before {line:?}");
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {line:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Where the monitor of the run in `dir` listens. A Unix socket's path
/// must fit in 108 bytes, which a path under a deep checkout may not, so
/// the socket goes in the temporary directory, named after a hash of
/// `dir`.
pub fn monitor_socket(dir: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    dir.hash(&mut hasher);
    std::env::temp_dir().join(format!("mikros-{:016x}.sock", hasher.finish()))
}

/// Boots `dir`/disk.img with the serial output going to `dir`/serial.log
/// and a monitor listening on `monitor_socket(dir)`, on the standard VGA
/// device, the one the firmware gives a linear framebuffer for. A reset
/// ends QEMU unless `reboot`.
pub fn start(machine: &MachineArgs, dir: &Path, reboot: bool) -> Result<Child> {
    let disk = dir.join("disk.img");
    for name in ["serial.log", "screen.ppm", "console.ppm"] {
        let _ = fs::remove_file(dir.join(name));
    }
    let socket = monitor_socket(dir);
    let _ = fs::remove_file(&socket);
    let firmware = firmware::paths();
    let vars = disk.with_extension("vars.fd");
    fs::copy(&firmware.vars, &vars)?;
    let mut machine = machine.clone();
    machine.gpu = qemu::Gpu::Basic;
    crate::test::scratch_disks(&dir.join("serial.log"))?;
    let mut args = crate::test::test_args(
        qemu::args(
            &machine,
            qemu::Processor::available(),
            &firmware,
            &vars,
            &disk,
        ),
        &dir.join("serial.log"),
    );
    if reboot {
        args.retain(|a| a != "-no-reboot");
    }
    args.extend([
        "-monitor".into(),
        format!("unix:{},server=on,wait=off", socket.display()),
    ]);
    Command::new(qemu::QEMU)
        .args(args)
        .spawn()
        .with_context(|| format!("failed to start {}", qemu::QEMU))
}

/// Boots the test image with the command line of `scenario`, raises its
/// event, and returns the serial output and what the screen dump showed.
fn boot(
    machine: &MachineArgs,
    dir: &Path,
    scenario: &Scenario,
    timeout: Duration,
) -> Result<(String, Option<&'static str>)> {
    let log = dir.join("serial.log");
    let socket = monitor_socket(dir);
    let screen = dir.join("screen.ppm");
    let mut qemu = start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let mut problem = None;
        if let Some(command) = scenario.raise {
            wait_for_line(&log, WAITING, &mut qemu, deadline)?;
            if scenario.console {
                problem = console_problem(&screen_dump(&socket, &dir.join("console.ppm"))?);
            }
            monitor(&socket, command)?;
        }
        let serial = wait_for_line(&log, PANIC_END, &mut qemu, deadline)?;
        if scenario.screen && serial.contains(PAINTED) {
            // The report goes to the serial port before the screen, which
            // an emulated machine may draw seconds later, so the screen is
            // looked at until it shows no problem or `SETTLE` has passed.
            let settled = (Instant::now() + SETTLE).min(deadline);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                problem = screen_problem(&screen_dump(&socket, &screen)?);
                if problem.is_none() || Instant::now() >= settled {
                    break;
                }
            }
        } else if scenario.screen {
            println!(
                "scenario {}: no framebuffer, screen not checked",
                scenario.name
            );
        }
        Ok((serial, problem))
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    result
}

/// Runs every scenario on the components `built` of the build kind
/// `kind`, under `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    for scenario in &SCENARIOS {
        let dir = dir.join(scenario.name);
        fs::create_dir_all(&dir)?;
        image::create_with(kind, built, &dir.join("disk.img"), scenario.cmdline, &[])?;
        let (serial, screen) = boot(machine, &dir, scenario, timeout).with_context(|| {
            format!(
                "scenario {} failed; serial output in {}",
                scenario.name,
                dir.display()
            )
        })?;
        if let Err(why) = verdict(scenario, &serial, screen) {
            bail!(
                "scenario {} failed: {why}; serial output in {}",
                scenario.name,
                dir.join("serial.log").display()
            );
        }
        println!("scenario {} ended in the expected panic", scenario.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ppm(pixels: &[[u8; 3]]) -> Vec<u8> {
        let mut out = format!("P6\n{} 1\n255\n", pixels.len()).into_bytes();
        out.extend(pixels.iter().flatten());
        out
    }

    #[test]
    fn checks_that_the_report_replaced_the_paint() {
        assert_eq!(screen_problem(&ppm(&[[0; 3], [255; 3]])), None);
        assert_eq!(
            screen_problem(&ppm(&[[0; 3], PAINT])),
            Some("the paint is left on the screen")
        );
        assert_eq!(
            screen_problem(&ppm(&[[0; 3], [0; 3]])),
            Some("the screen is blank")
        );
        assert!(screen_problem(b"P3\n1 1\n255\n0 0 0").is_some());
        assert_eq!(console_problem(&ppm(&[[0; 3], PAINT])), None);
        assert_eq!(
            console_problem(&ppm(&[[0; 3], [0; 3]])),
            Some("the console shows nothing")
        );
    }

    #[test]
    fn needs_the_title_and_a_whole_report() {
        let nmi = &SCENARIOS[1];
        let report = "x\nKERNEL PANIC: NMI (vector 2)\nrip 0x1\nEND KERNEL PANIC\n";
        assert_eq!(verdict(nmi, report, None), Ok(()));
        assert!(verdict(nmi, "x\n", None).is_err());
        assert!(verdict(nmi, "KERNEL PANIC: NMI (vector 2)\n", None).is_err());
        assert!(verdict(nmi, "KERNEL PANIC: page fault\nEND KERNEL PANIC\n", None).is_err());
        assert!(verdict(nmi, report, Some("the screen is blank")).is_err());
    }
}
