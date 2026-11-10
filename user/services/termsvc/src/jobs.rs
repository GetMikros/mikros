//! Job control on the terminal. Nothing is done yet with what happens
//! on it.

use crate::Service;
use crate::tty::Tty;

/// Acts on what happened on `tty`. Signals are not sent yet.
pub async fn events(_: alloc::rc::Rc<Service>, tty: alloc::rc::Rc<Tty>) {
    loop {
        while tty.next_event().is_some() {}
        tty.event_queued.changed().await;
    }
}
