//! timeout on Mikros: it starts the command through the standard
//! library and waits for it, while a thread sleeps until the time is up
//! and then sends the signal, and KILL after `-k`. Without
//! `--foreground`, timeout first makes a process group of its own, which
//! the command joins, and also signals the group, so that what the
//! command started is signalled too; KILL goes to the command alone, so
//! that timeout outlives it to exit with 124. It exits with 127 when the
//! command is not found and 126 when it cannot be started otherwise, as
//! POSIX gives the commands that run others.

use crate::limits::{self, COMMAND, SIGCONT, SIGKILL, SIGSTOP, plan};
use mikros_cli::{INVALID, complain, start};
use std::io::ErrorKind;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const NOT_FOUND: u8 = 127;
const CANNOT_RUN: u8 = 126;
const SIG_IGN: usize = 1;

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn setpgid(pid: i32, group: i32) -> i32;
    fn signal(signal: i32, handler: usize) -> usize;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let plan = match plan(&run) {
        Ok(plan) => plan,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    // SAFETY: the call takes no pointers. It fails for a process that
    // leads its session, which then signals the command alone.
    let group = !plan.foreground && unsafe { setpgid(0, 0) } == 0;
    let (program, arguments) = plan.command.split_first().expect("read with a command");
    let mut child = match Command::new(program).args(arguments).spawn() {
        Ok(child) => child,
        Err(e) => {
            complain(&COMMAND, &run, &format!("{program}: {e}"));
            return match e.kind() {
                ErrorKind::NotFound => ExitCode::from(NOT_FOUND),
                _ => ExitCode::from(CANNOT_RUN),
            };
        }
    };
    let pid = child.id() as i32;
    let fired = Arc::new(AtomicBool::new(false));
    if let Some(limit) = plan.limit {
        let fired = fired.clone();
        let (signal, kill_after) = (plan.signal, plan.kill_after);
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            fired.store(true, Ordering::SeqCst);
            send(pid, signal, group);
            if let Some(after) = kill_after {
                std::thread::sleep(after);
                send(pid, SIGKILL, false);
            }
        });
    }
    match child.wait() {
        Ok(status) => ExitCode::from(limits::status(
            fired.load(Ordering::SeqCst),
            plan.preserve,
            status.code(),
            status.signal(),
        )),
        Err(e) => {
            complain(&COMMAND, &run, &format!("waiting for {program}: {e}"));
            ExitCode::from(CANNOT_RUN)
        }
    }
}

/// Sends `sig` to the command `pid`, and with `group` to the process
/// group, which timeout itself then ignores it in, unless it is KILL or
/// STOP, which it cannot ignore; and CONT after it, unless it is KILL,
/// STOP or CONT, so that a stopped command sees it.
fn send(pid: i32, sig: i32, group: bool) {
    let group = group && sig != SIGKILL && sig != SIGSTOP;
    let send_to = |sig: i32| {
        // SAFETY: the calls take no pointers.
        unsafe {
            kill(pid, sig);
            if group {
                signal(sig, SIG_IGN);
                kill(0, sig);
            }
        }
    };
    send_to(sig);
    if ![SIGKILL, SIGSTOP, SIGCONT].contains(&sig) {
        send_to(SIGCONT);
    }
}
