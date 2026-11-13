//! A program that uses the Rust standard library, for the process
//! manager tests. It prints a greeting for its first argument to
//! standard output, and exits with 0 when it was given exactly one
//! argument and every check below passed, and with the number of the
//! first check that failed otherwise. The launcher probe starts it, and
//! gives it a time page only from its second request on, and a `USER`
//! startup handle with `id` 9.

#![feature(mikros_handles)]

use std::collections::HashMap;
use std::os::mikros::startup;
use std::panic;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 1 January 2024 in seconds since the Unix epoch, the time on the
/// probe's time page when the monotonic clock is 0, and a day.
const JANUARY_2024: u64 = 1_704_067_200;
/// The `USER` role of startup handles.
const USER: u32 = 16;
const DAY: u64 = 86_400;

fn main() -> ExitCode {
    match checks() {
        Ok(name) => {
            println!("hello, {name}, from std");
            ExitCode::SUCCESS
        }
        Err(step) => ExitCode::from(step),
    }
}

/// Runs the checks, and returns the argument or the number of the first
/// check that failed.
fn checks() -> Result<String, u8> {
    let mut step = 0;
    let mut check = |ok: bool| {
        step += 1;
        if ok { Ok(()) } else { Err(step) }
    };
    let args: Vec<String> = std::env::args().collect();
    check(args.len() == 2)?;
    // A hash map seeds its hasher from the C library's random source.
    let mut map = HashMap::new();
    map.insert("one", 1);
    check(map.get("one") == Some(&1))?;
    // A thread starts, returns a value and is joined.
    let joined = thread::spawn(|| (1..=10).sum::<u32>()).join();
    check(joined.ok() == Some(55))?;
    // The process manager gives the program's path.
    let exe = std::env::current_exe();
    check(exe.is_ok_and(|p| p == std::path::Path::new("/usr/bin/hello-std")))?;
    // Instant comes from the clock page, and goes on.
    let before = Instant::now();
    thread::sleep(Duration::from_millis(2));
    check(before.elapsed() >= Duration::from_millis(2))?;
    // SystemTime comes from the time page. The probe refuses the first
    // connection to its clock, so the first reading finds no time page
    // and fails, which std does not cover with another clock; the next
    // gets the page, whose time is 1 January 2024 plus the monotonic
    // clock.
    panic::set_hook(Box::new(|_| {}));
    let first = panic::catch_unwind(SystemTime::now);
    let _ = panic::take_hook();
    check(first.is_err())?;
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    check(since.is_ok_and(|t| (JANUARY_2024..JANUARY_2024 + DAY).contains(&t.as_secs())))?;
    // The USER startup handle 9 that the probe gives is the program's to
    // take, once; one it did not give is not there. The directory
    // connection is the C library's, and open.
    check(startup::take_handle(USER, 9).is_some())?;
    check(startup::take_handle(USER, 9).is_none() && startup::take_handle(USER, 10).is_none())?;
    check(std::os::mikros::directory().as_raw_handle() != 0)?;
    // So is the process's own handle.
    check(std::os::mikros::process::current().as_raw_handle() != 0)?;
    // A panic unwinds to `catch_unwind`.
    panic::set_hook(Box::new(|_| {}));
    let caught = panic::catch_unwind(|| panic!("unwinding"));
    let _ = panic::take_hook();
    check(caught.is_err())?;
    Ok(args[1].clone())
}
