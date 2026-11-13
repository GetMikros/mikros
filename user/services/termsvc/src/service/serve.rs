//! The `Terminal` connections: each request names its terminal by token,
//! and comes from the client identity of the connection.

use crate::service::jobs;
use crate::service::tty::Tty;
use crate::terminal::discipline::Read;
use crate::terminal::termios::{Attributes, VMIN, VTIME};
use crate::{service::Service, service::now};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::terminal::{
    self as proto, Attributes as Wire, Queue, Terminal, TerminalAsyncServer, TerminalDrainResponse,
    TerminalFlushResponse, TerminalGetAttributesResponse, TerminalGetForegroundResponse,
    TerminalGetNumberResponse, TerminalGetReadinessResponse, TerminalGetWindowSizeResponse,
    TerminalMakeControllingResponse, TerminalReadResponse, TerminalRequest,
    TerminalSendBreakResponse, TerminalSetAttributesResponse, TerminalSetForegroundResponse,
    TerminalSetWindowSizeResponse, TerminalWriteResponse, When, WindowSize,
};
use mikros_rt::sys;

/// The most bytes one `Read` or `Write` carries.
const TRANSFER_MAX: usize = 32768;
/// The rights of a readiness event as the client gets it: TRANSFER,
/// WAIT and INSPECT.
const READINESS_RIGHTS: u32 = 1 << 1 | 1 << 6 | 1 << 9;
const NANOS_PER_DECISECOND: i64 = 100_000_000;

pub fn wire(a: &Attributes) -> Wire {
    Wire {
        input_flags: a.input_flags,
        output_flags: a.output_flags,
        control_flags: a.control_flags,
        local_flags: a.local_flags,
        control_chars: a.control_chars,
        input_speed: a.input_speed,
        output_speed: a.output_speed,
    }
}

fn attributes(w: &Wire) -> Attributes {
    Attributes {
        input_flags: w.input_flags,
        output_flags: w.output_flags,
        control_flags: w.control_flags,
        local_flags: w.local_flags,
        control_chars: w.control_chars,
        input_speed: w.input_speed,
        output_speed: w.output_speed,
    }
}

fn window(s: [u16; 4]) -> WindowSize {
    WindowSize {
        rows: s[0],
        columns: s[1],
        width: s[2],
        height: s[3],
    }
}

/// One `Terminal` connection.
struct Connection {
    service: Rc<Service>,
    client: ClientIdentity,
    /// The readiness event of each terminal it asked for one, by the
    /// terminal's token ID.
    readiness: RefCell<BTreeMap<u64, Rc<OwnedHandle>>>,
}

impl Connection {
    fn tty(&self, token: &OwnedHandle) -> Result<Rc<Tty>, Status> {
        self.service.tty(token).ok_or(Status::BadHandle)
    }
}

pub async fn serve(service: Rc<Service>, end: ServerEnd<Terminal>, client: ClientIdentity) {
    let server = TerminalAsyncServer::new(end, service.reactor.clone());
    let c = Connection {
        service,
        client,
        readiness: RefCell::new(BTreeMap::new()),
    };
    while let Ok(request) = server.next().await {
        let s = server.server();
        use TerminalRequest as R;
        let _ = match request {
            R::GetAttributes(q, r) => {
                let got = c.tty(&q.terminal).map(|t| TerminalGetAttributesResponse {
                    attributes: wire(&t.discipline.borrow().attributes),
                });
                r.reply(s, got)
            }
            R::SetAttributes(q, r) => {
                let set = set_attributes(&c, &q.terminal, &q.attributes, q.when).await;
                r.reply(s, set.map(|()| TerminalSetAttributesResponse {}))
            }
            R::GetWindowSize(q, r) => {
                let got = c.tty(&q.terminal).map(|t| TerminalGetWindowSizeResponse {
                    size: window(t.size.get()),
                });
                r.reply(s, got)
            }
            R::SetWindowSize(q, r) => {
                let set = c
                    .tty(&q.terminal)
                    .map(|t| t.set_size(crate::service::size(&q.size)));
                r.reply(s, set.map(|()| TerminalSetWindowSizeResponse {}))
            }
            R::GetForeground(q, r) => {
                let got = c.tty(&q.terminal).map(|t| TerminalGetForegroundResponse {
                    pgid: t.foreground.get().unwrap_or(0),
                });
                r.reply(s, got)
            }
            R::SetForeground(q, r) => {
                let set = match c.tty(&q.terminal) {
                    Ok(t) => jobs::set_foreground(&c.service, &t, &c.client, q.pgid).await,
                    Err(e) => Err(e),
                };
                r.reply(s, set.map(|()| TerminalSetForegroundResponse {}))
            }
            R::MakeControlling(q, r) => {
                let made = match c.tty(&q.terminal) {
                    Ok(t) => jobs::make_controlling(&c.service, &t, &c.client).await,
                    Err(e) => Err(e),
                };
                r.reply(s, made.map(|()| TerminalMakeControllingResponse {}))
            }
            R::Drain(q, r) => {
                let drained = match c.tty(&q.terminal) {
                    Ok(t) => {
                        drain(&t).await;
                        Ok(TerminalDrainResponse {})
                    }
                    Err(e) => Err(e),
                };
                r.reply(s, drained)
            }
            R::Flush(q, r) => {
                let flushed = c.tty(&q.terminal).map(|t| {
                    flush(&t, q.queues);
                    TerminalFlushResponse {}
                });
                r.reply(s, flushed)
            }
            // There is no line to send a break on.
            R::SendBreak(q, r) => {
                let sent = c.tty(&q.terminal).map(|_| TerminalSendBreakResponse {});
                r.reply(s, sent)
            }
            R::Read(q, r) => {
                let read = read(&c, &q.terminal, q.count, q.deadline).await;
                r.reply(s, read.map(|data| TerminalReadResponse { data }))
            }
            R::Write(q, r) => {
                let written = write(&c, &q.terminal, q.data, q.deadline).await;
                r.reply(s, written.map(|written| TerminalWriteResponse { written }))
            }
            R::GetReadiness(q, r) => {
                let got =
                    readiness(&c, &q.terminal).map(|event| TerminalGetReadinessResponse { event });
                r.reply(s, got)
            }
            R::GetNumber(q, r) => {
                let got = c
                    .tty(&q.terminal)
                    .map(|t| TerminalGetNumberResponse { number: t.number });
                r.reply(s, got)
            }
        };
    }
}

fn flush(t: &Tty, queues: Queue) {
    if queues.contains(proto::Queue::INPUT) {
        t.discipline.borrow_mut().flush_input();
    }
    if queues.contains(proto::Queue::OUTPUT) {
        t.flush_output();
    }
    t.update();
}

async fn drain(t: &Tty) {
    while !t.output_empty() && !t.hung_up() {
        t.changed.changed().await;
    }
}

async fn set_attributes(
    c: &Connection,
    token: &OwnedHandle,
    w: &Wire,
    when: When,
) -> Result<(), Status> {
    let t = c.tty(token)?;
    match when {
        When::Now => {}
        When::Drain => drain(&t).await,
        When::Flush => {
            drain(&t).await;
            t.discipline.borrow_mut().flush_input();
        }
        When::Unknown(_) => return Err(Status::InvalidArgs),
    }
    t.discipline.borrow_mut().set_attributes(attributes(w));
    t.update();
    Ok(())
}

fn readiness(c: &Connection, token: &OwnedHandle) -> Result<OwnedHandle, Status> {
    let t = c.tty(token)?;
    let id = crate::service::koid(token).ok_or(Status::BadHandle)?;
    let event = match c.readiness.borrow().get(&id) {
        Some(event) => event.clone(),
        None => {
            let mut raw = 0;
            // SAFETY: the out pointer is valid for the call.
            if unsafe { sys::event_create(0, &mut raw) } != mikros_rt::OK {
                return Err(Status::NoResources);
            }
            // SAFETY: the kernel just created the handle for us.
            Rc::new(unsafe { OwnedHandle::from_raw(raw) })
        }
    };
    c.readiness.borrow_mut().insert(id, event.clone());
    t.add_readiness(&event);
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::handle_duplicate(event.raw(), READINESS_RIGHTS, &mut out) } != mikros_rt::OK {
        return Err(Status::NoResources);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// Waits for a change of `t` until `deadline`: false once it passed.
async fn wait(c: &Connection, t: &Tty, deadline: i64) -> bool {
    if now() >= deadline {
        return false;
    }
    let timeout = c.service.reactor.sleep_until(deadline);
    crate::service::first(t.changed.changed(), timeout)
        .await
        .is_some()
}

async fn read(
    c: &Connection,
    token: &OwnedHandle,
    count: u32,
    deadline: i64,
) -> Result<Vec<u8>, Status> {
    let t = c.tty(token)?;
    let count = count as usize;
    if count > TRANSFER_MAX {
        return Err(Status::OutOfRange);
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    let nonblocking = deadline <= now();
    // Non-canonical input with TIME: the time to wait for a byte, from
    // the call with MIN 0 and from the last byte otherwise.
    let mut timer: Option<i64> = None;
    loop {
        jobs::check_read(&c.service, &t, &c.client).await?;
        let (canonical, min, time, available) = {
            let d = t.discipline.borrow();
            let a = d.attributes;
            let canonical = a.local(crate::terminal::termios::ICANON);
            let cc = a.control_chars;
            (
                canonical,
                usize::from(cc[VMIN]),
                i64::from(cc[VTIME]),
                d.available(),
            )
        };
        let ready = match canonical {
            true => t.discipline.borrow().readable(),
            false => {
                let expired = timer.is_some_and(|at| now() >= at);
                (min == 0 && (time == 0 || available > 0 || expired))
                    || (min > 0 && (available >= min.min(count) || (available > 0 && expired)))
            }
        };
        if ready {
            let got = t.discipline.borrow_mut().read(count);
            t.update();
            return Ok(match got {
                Read::Data(data) => data,
                Read::End | Read::Empty => Vec::new(),
            });
        }
        if t.hung_up() {
            return Ok(Vec::new());
        }
        if !canonical && time > 0 && (min == 0 || available > 0) && timer.is_none() {
            timer = Some(now() + time * NANOS_PER_DECISECOND);
        }
        if now() >= deadline {
            return Err(match nonblocking {
                true => Status::ShouldWait,
                false => Status::TimedOut,
            });
        }
        let until = match timer {
            Some(at) => at.min(deadline),
            None => deadline,
        };
        let before = available;
        wait(c, &t, until).await;
        // A new byte restarts the inter-byte timer.
        if min > 0 && t.discipline.borrow().available() > before {
            timer = None;
        }
    }
}

async fn write(
    c: &Connection,
    token: &OwnedHandle,
    data: Vec<u8>,
    deadline: i64,
) -> Result<u32, Status> {
    let t = c.tty(token)?;
    if data.len() > TRANSFER_MAX {
        return Err(Status::OutOfRange);
    }
    let nonblocking = deadline <= now();
    loop {
        jobs::check_write(&c.service, &t, &c.client).await?;
        if t.hung_up() {
            return Err(Status::PeerClosed);
        }
        if t.writable() {
            let out = t.discipline.borrow_mut().output(&data);
            t.queue_output(&out);
            return Ok(data.len() as u32);
        }
        if now() >= deadline {
            return Err(match nonblocking {
                true => Status::ShouldWait,
                false => Status::TimedOut,
            });
        }
        wait(c, &t, deadline).await;
    }
}
