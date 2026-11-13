//! `mikros.log.Sources`: the standard output and error of services,
//! whose streams the service manager adds with the pid and app ID of
//! each. Each line becomes a record, of severity INFO from the output
//! and ERROR from the error, which is also copied into the kernel log.

use crate::service::log::Log;
use crate::store::Lines;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::log::{
    Sources, SourcesAddResponse, SourcesAsyncServer, SourcesRequest,
};
use mikros_rt::sys;

const INFO: u8 = 1;
const ERROR: u8 = 3;
/// The longest message of a record; longer lines make several.
const MESSAGE_MAX: usize = 1024;
const SHOULD_WAIT: sys::Status = -22;
const READABLE: u32 = 1 << 0;
const PEER_CLOSED: u32 = 1 << 2;

/// Serves the source adder on `end` until it closes the connection.
pub async fn serve(log: Rc<Log>, end: ServerEnd<Sources>, reactor: Reactor, spawner: Spawner) {
    let server = SourcesAsyncServer::new(end, reactor.clone());
    while let Ok(SourcesRequest::Add(add, responder)) = server.next().await {
        let (pid, app_id) = (add.pid, add.app_id);
        let output = lines(
            log.clone(),
            reactor.clone(),
            add.output,
            pid,
            app_id.clone(),
            INFO,
        );
        spawner.spawn(output);
        spawner.spawn(lines(
            log.clone(),
            reactor.clone(),
            add.error,
            pid,
            app_id,
            ERROR,
        ));
        let _ = responder.reply(server.server(), Ok(SourcesAddResponse {}));
    }
}

/// Adds each line of `stream`, with `severity`, as a record of `pid` and
/// `app_id`, until the stream's other end closes.
async fn lines(
    log: Rc<Log>,
    reactor: Reactor,
    stream: OwnedHandle,
    pid: u32,
    app_id: String,
    severity: u8,
) {
    let add = |line: &[u8]| {
        let message = String::from_utf8_lossy(line).into_owned();
        log.add_copied(pid, app_id.clone(), severity, message);
    };
    let mut lines = Lines::new(MESSAGE_MAX);
    let mut buf = [0u8; 4096];
    loop {
        let mut read = 0;
        // SAFETY: the pointers describe `buf` and `read`.
        let status = unsafe {
            sys::stream_read(stream.raw(), buf.as_mut_ptr(), buf.len() as u64, &mut read)
        };
        if status == SHOULD_WAIT {
            let signals = reactor.signals(stream.raw(), READABLE | PEER_CLOSED);
            if signals.await.is_err() {
                break;
            }
            continue;
        }
        if status != mikros_rt::OK {
            break;
        }
        lines.push(&buf[..read as usize], add);
    }
    // What follows the last line feed is a line too.
    lines.finish(add);
}
