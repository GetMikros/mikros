//! Job control on the terminal: the session it is the controlling
//! terminal of and that session's foreground process group, which
//! callers of that session may read and write, and the signals the
//! terminal sends. Callers are known by the process ID of their
//! connection's client identity, and the process manager says which
//! group and session they are in now.

use crate::service::Service;
use crate::service::tty::{Event, Tty};
use crate::terminal::discipline::Signal;
use crate::terminal::termios::TOSTOP;
use alloc::rc::Rc;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

const SIGHUP: u32 = 1;
const SIGINT: u32 = 2;
const SIGQUIT: u32 = 3;
const SIGTSTP: u32 = 20;
const SIGTTIN: u32 = 21;
const SIGTTOU: u32 = 22;
const SIGWINCH: u32 = 28;

/// The process ID, process group and session of `client`'s process.
async fn caller(service: &Service, client: &ClientIdentity) -> Result<(u32, u32, u32), Status> {
    match service.launcher.get_info(client.pid).await {
        Ok(Ok(r)) => Ok((r.info.pid, r.info.process_group, r.info.session)),
        Ok(Err(_)) => Err(Status::AccessDenied),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Checks that `client` may take input from `tty`: when it is the
/// caller's controlling terminal, only its foreground group may.
/// Another group gets `TTIN` and the read fails with `INTERRUPTED`.
pub async fn check_read(
    service: &Service,
    tty: &Tty,
    client: &ClientIdentity,
) -> Result<(), Status> {
    background(service, tty, client, SIGTTIN).await
}

/// As `check_read` for output, which only `TOSTOP` keeps from other
/// groups, with `TTOU`.
pub async fn check_write(
    service: &Service,
    tty: &Tty,
    client: &ClientIdentity,
) -> Result<(), Status> {
    if !tty.discipline.borrow().attributes.local(TOSTOP) {
        return Ok(());
    }
    background(service, tty, client, SIGTTOU).await
}

async fn background(
    service: &Service,
    tty: &Tty,
    client: &ClientIdentity,
    signo: u32,
) -> Result<(), Status> {
    let Some(session) = tty.session.get() else {
        return Ok(());
    };
    let (_, group, theirs) = caller(service, client).await?;
    if theirs != session || tty.foreground.get() == Some(group) {
        return Ok(());
    }
    let _ = service.launcher.kill_group(group, signo).await;
    Err(Status::Interrupted)
}

/// Makes `tty` the controlling terminal of `client`'s session, with the
/// caller's group in the foreground: only for a session leader whose
/// session has no controlling terminal, and when `tty` has no other
/// session.
pub async fn make_controlling(
    service: &Service,
    tty: &Tty,
    client: &ClientIdentity,
) -> Result<(), Status> {
    let (pid, group, session) = caller(service, client).await?;
    let leader = pid == session;
    let has_one = service.has_controlling(session);
    let other = tty.session.get().is_some_and(|s| s != session);
    if !leader || has_one || other {
        return Err(Status::AccessDenied);
    }
    tty.session.set(Some(session));
    tty.foreground.set(Some(group));
    tty.update();
    Ok(())
}

/// Puts process group `pgid` in the foreground of `tty`: only a group
/// of the terminal's session, for a caller of that session.
pub async fn set_foreground(
    service: &Service,
    tty: &Tty,
    client: &ClientIdentity,
    pgid: u32,
) -> Result<(), Status> {
    let (_, _, session) = caller(service, client).await?;
    if tty.session.get() != Some(session) {
        return Err(Status::AccessDenied);
    }
    if !group_in(service, pgid, session).await {
        return Err(Status::AccessDenied);
    }
    tty.foreground.set(Some(pgid));
    // Blocked reads and writes check again.
    tty.update();
    Ok(())
}

/// Whether some process of session `session` is in group `pgid`.
async fn group_in(service: &Service, pgid: u32, session: u32) -> bool {
    // The group's leader, when it still runs, says so at once.
    if let Ok(Ok(r)) = service.launcher.get_info(pgid).await
        && r.info.process_group == pgid
    {
        return r.info.session == session;
    }
    let Ok(listed) = service.launcher.list().await else {
        return false;
    };
    for pid in listed.pids {
        if let Ok(Ok(r)) = service.launcher.get_info(pid).await
            && r.info.process_group == pgid
        {
            return r.info.session == session;
        }
    }
    false
}

/// Sends the signals for what happened on `tty`: those of the signal
/// characters and `WINCH` to its foreground group, and `HUP` to its
/// session's leader once the master stream closed, after which it is no
/// longer the session's controlling terminal.
pub async fn events(service: Rc<Service>, tty: Rc<Tty>) {
    loop {
        while let Some(event) = tty.next_event() {
            let to_group = match event {
                Event::Signal(Signal::Interrupt) => Some(SIGINT),
                Event::Signal(Signal::Quit) => Some(SIGQUIT),
                Event::Signal(Signal::Suspend) => Some(SIGTSTP),
                Event::WindowChanged => Some(SIGWINCH),
                Event::HungUp => {
                    if let Some(leader) = tty.session.get() {
                        let _ = service.launcher.kill(leader, SIGHUP).await;
                    }
                    // The session may take another controlling terminal.
                    tty.session.set(None);
                    tty.foreground.set(None);
                    None
                }
            };
            if let (Some(signo), Some(group)) = (to_group, tty.foreground.get()) {
                let _ = service.launcher.kill_group(group, signo).await;
            }
        }
        tty.event_queued.changed().await;
    }
}
