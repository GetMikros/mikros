//! Runs of the test image that end in an expected kernel panic: the test
//! init runs the scenario its command line names with `test=`, and the
//! runner may raise an event through the QEMU monitor once the init is
//! waiting for it.

use crate::build::{self, Kind};
use crate::qemu::{self, MachineArgs};
use crate::{firmware, image};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
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
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        name: "init-exit",
        cmdline: "test=init-exit",
        raise: None,
        title: "init terminated with exit code 7",
        screen: true,
    },
    Scenario {
        name: "nmi",
        cmdline: "test=wait",
        raise: Some("nmi"),
        title: "NMI (vector 2)",
        screen: false,
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
    },
];

/// The line the test init logs once it waits for the runner.
const WAITING: &str = "SCENARIO waiting";
/// The line the test init logs once it painted the framebuffer.
const PAINTED: &str = "SCENARIO framebuffer painted";
/// The colour it paints the framebuffer, as red, green and blue.
const PAINT: [u8; 3] = [0x12, 0x34, 0x56];
const PANIC_LINE: &str = "KERNEL PANIC: ";
const PANIC_END: &str = "END KERNEL PANIC";

/// Sends `command` to the QEMU monitor listening on `socket`.
fn monitor(socket: &Path, command: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to the monitor at {}", socket.display()))?;
    stream.write_all(format!("{command}\n").as_bytes())?;
    // The monitor reads the command before the connection closes.
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// The pixels of a binary PPM image, or `None` when `ppm` is not one.
fn ppm_pixels(ppm: &[u8]) -> Option<Vec<[u8; 3]>> {
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
    Some(data.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect())
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
fn wait_for_line(log: &Path, line: &str, qemu: &mut Child, deadline: Instant) -> Result<String> {
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

/// Boots the test image with the command line of `scenario`, raises its
/// event, and returns the serial output and what the screen dump showed.
fn boot(
    machine: &MachineArgs,
    dir: &Path,
    scenario: &Scenario,
    timeout: Duration,
) -> Result<(String, Option<&'static str>)> {
    let disk = dir.join("disk.img");
    let log = dir.join("serial.log");
    let socket = dir.join("monitor.sock");
    let screen = dir.join("screen.ppm");
    for path in [&log, &socket, &screen] {
        let _ = fs::remove_file(path);
    }
    let firmware = firmware::paths();
    let vars = disk.with_extension("vars.fd");
    fs::copy(&firmware.vars, &vars)?;
    // The firmware gives a linear framebuffer only on the standard VGA
    // device.
    let mut machine = machine.clone();
    if scenario.screen {
        machine.gpu = qemu::Gpu::Basic;
    }
    let mut args = crate::test::test_args(
        qemu::args(&machine, qemu::kvm_available(), &firmware, &vars, &disk),
        &log,
    );
    args.extend([
        "-monitor".into(),
        format!("unix:{},server=on,wait=off", socket.display()),
    ]);
    let mut qemu = Command::new(qemu::QEMU)
        .args(args)
        .spawn()
        .with_context(|| format!("failed to start {}", qemu::QEMU))?;
    let deadline = Instant::now() + timeout;
    let result = (|| {
        if let Some(command) = scenario.raise {
            wait_for_line(&log, WAITING, &mut qemu, deadline)?;
            monitor(&socket, command)?;
        }
        let serial = wait_for_line(&log, PANIC_END, &mut qemu, deadline)?;
        let mut problem = None;
        if scenario.screen && serial.contains(PAINTED) {
            // The report goes to the serial port before the screen.
            std::thread::sleep(Duration::from_millis(500));
            monitor(&socket, &format!("screendump {}", screen.display()))?;
            std::thread::sleep(Duration::from_millis(500));
            let dump =
                fs::read(&screen).with_context(|| format!("reading {}", screen.display()))?;
            problem = screen_problem(&dump);
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
        image::create_with(kind, built, &dir.join("disk.img"), scenario.cmdline)?;
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
