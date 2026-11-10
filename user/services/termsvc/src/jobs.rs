//! Job control on the terminal: which callers may read and write it,
//! its controlling session and foreground group. Not enforced yet: every
//! caller may read and write, and the session calls are not supported.

use crate::Service;
use crate::tty::Tty;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

pub async fn check_read(_: &Service, _: &Tty, _: &ClientIdentity) -> Result<(), Status> {
    Ok(())
}

pub async fn check_write(_: &Service, _: &Tty, _: &ClientIdentity) -> Result<(), Status> {
    Ok(())
}

pub async fn set_foreground(
    _: &Service,
    _: &Tty,
    _: &ClientIdentity,
    _: u32,
) -> Result<(), Status> {
    Err(Status::NotSupported)
}

pub async fn make_controlling(_: &Service, _: &Tty, _: &ClientIdentity) -> Result<(), Status> {
    Err(Status::NotSupported)
}

/// Acts on what happened on `tty`. Signals are not sent yet.
pub async fn events(_: alloc::rc::Rc<Service>, tty: alloc::rc::Rc<Tty>) {
    loop {
        while tty.next_event().is_some() {}
        tty.event_queued.changed().await;
    }
}
