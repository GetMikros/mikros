//! The system tests, which a test image runs: the image boots with
//! `selftest` on the kernel command line, the service manager starts
//! selftest once the services that start at boot run, and selftest runs
//! every program of `/usr/bin/tests/` and then powers the machine off,
//! after which QEMU exits by itself. The result comes from selftest's
//! lines in the serial output: a `PASS` or `FAIL` line for each test, and
//! the counts.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PANIC_LINE: &str = "KERNEL PANIC: ";
/// Where selftest's lines start in a line of the serial output, which
/// the log adds its own prefix to.
const MARK: &str = "selftest: ";
/// How long the machine may take to power off once selftest wrote its
/// counts, after which the run stops it.
const POWER_OFF_WAIT: Duration = Duration::from_secs(20);

/// Builds a test image of the build kind `kind`, boots it in `dir` with
/// `selftest`, and fails unless every system test passed within
/// `timeout`. Returns the number of tests that passed.
pub fn run(machine: &MachineArgs, kind: Kind, dir: &Path, timeout: Duration) -> Result<usize> {
    let built = build::build_tests(kind)?;
    fs::create_dir_all(dir)?;
    image::create_with(kind, &built, &dir.join("disk.img"), "selftest", &[])?;
    let log = dir.join("serial.log");
    let mut qemu = scenario::start_with(machine, dir, false, &|_| Ok(()), &[], false)?;
    let mut deadline = Instant::now() + timeout;
    let mut counted = false;
    let ended = loop {
        if qemu.try_wait()?.is_some() {
            break Ok(());
        }
        if !counted && counts(&fs::read_to_string(&log).unwrap_or_default()).is_some() {
            counted = true;
            deadline = deadline.min(Instant::now() + POWER_OFF_WAIT);
        }
        if Instant::now() > deadline {
            break match counted {
                true => {
                    println!("warning  the machine did not power off after selftest");
                    Ok(())
                }
                false => Err(anyhow::anyhow!("timed out waiting for selftest's counts")),
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = qemu.kill();
    let _ = qemu.wait();
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    let result = ended.and_then(|()| verdict(&serial).map_err(|why| anyhow::anyhow!(why)));
    result.with_context(|| {
        format!(
            "the system tests failed; serial output in {}",
            log.display()
        )
    })
}

/// The number of tests that passed by selftest's lines in `serial`, or
/// why the run failed: a kernel panic, a test that failed, or no counts.
fn verdict(serial: &str) -> Result<usize, String> {
    if let Some(line) = serial.lines().find(|l| l.contains(PANIC_LINE)) {
        return Err(format!("the kernel panicked: {line}"));
    }
    let lines: Vec<&str> = serial
        .lines()
        .filter_map(|l| l.find(MARK).map(|at| &l[at + MARK.len()..]))
        .collect();
    let failed: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("FAIL "))
        .collect();
    if !failed.is_empty() {
        return Err(format!("failed: {}", failed.join("; ")));
    }
    match counts(serial) {
        Some((passed, 0)) => Ok(passed),
        Some((_, failed)) => Err(format!("{failed} tests failed")),
        None => Err("selftest wrote no counts".to_owned()),
    }
}

/// The counts of passed and failed tests that selftest wrote last in
/// `serial`, if it did.
fn counts(serial: &str) -> Option<(usize, usize)> {
    serial
        .lines()
        .filter_map(|l| l.find(MARK).map(|at| &l[at + MARK.len()..]))
        .rev()
        .find_map(|l| {
            let (passed, failed) = l.split_once(" passed, ")?;
            let failed = failed.strip_suffix(" failed")?;
            Some((passed.parse().ok()?, failed.parse().ok()?))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_result_from_selftests_lines() {
        let passed = "[3.1] I p40: selftest: PASS clock\n[3.2] I p40: selftest: PASS files\n[3.3] I p40: selftest: 2 passed, 0 failed\n";
        assert_eq!(verdict(passed), Ok(2));
        let failed =
            "x selftest: PASS clock\nx selftest: FAIL files 3\nx selftest: 1 passed, 1 failed\n";
        assert!(verdict(failed).unwrap_err().contains("files 3"));
        assert!(verdict("x selftest: PASS clock\n").is_err());
        let panicked = format!("{passed}KERNEL PANIC: page fault\n");
        assert!(verdict(&panicked).unwrap_err().contains("panicked"));
        assert_eq!(verdict("x selftest: 0 passed, 0 failed\n"), Ok(0));
    }
}
