//! `mikros.log.Reader`: the records kept, after a sequence number, and
//! after `Follow` the records that come, as `OnRecords` events.

use crate::service::log::Log;
use alloc::rc::Rc;
use core::cell::Cell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::log::{
    Reader, ReaderAsyncServer, ReaderFollowResponse, ReaderReadResponse, ReaderRequest,
};

/// The most records one reply or event holds.
const BATCH: usize = 32;

/// Serves the reader on `end` until it closes the connection.
pub async fn serve(log: Rc<Log>, end: ServerEnd<Reader>, reactor: Reactor, spawner: Spawner) {
    let server = Rc::new(ReaderAsyncServer::new(end, reactor));
    let following = Rc::new(Cell::new(false));
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            ReaderRequest::Read(read, responder) => {
                let max = usize::try_from(read.max).unwrap_or(BATCH).min(BATCH);
                let records = log.after(read.after, max);
                responder.reply(s, ReaderReadResponse { records })
            }
            ReaderRequest::Follow(_, responder) => {
                if !following.replace(true) {
                    let from = log.latest();
                    spawner.spawn(follow(log.clone(), server.clone(), from));
                }
                responder.reply(s, Ok(ReaderFollowResponse {}))
            }
        };
    }
}

/// Sends the records after sequence number `from`, and then each that
/// comes, as `OnRecords` events, until sending fails.
async fn follow(log: Rc<Log>, server: Rc<ReaderAsyncServer>, mut from: u64) {
    loop {
        log.newer(from).await;
        if log.stopping() && log.latest() <= from {
            return;
        }
        let records = log.after(from, BATCH);
        let Some(last) = records.last() else {
            // Records that came were dropped before they could be sent.
            from = log.latest();
            continue;
        };
        from = last.sequence;
        if server.server().send_on_records(records).is_err() {
            return;
        }
    }
}
