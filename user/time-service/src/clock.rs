//! `mikros.time.Clock`: UTC as the monotonic clock plus the offset of the
//! time page, the page itself, and setting the time, which needs
//! `time.set`, for every client; and `OnTimeChanged` for those that
//! watch.

use crate::page::Page;
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::time::{
    Clock, ClockAsyncServer, ClockGetTimePageResponse, ClockGetTimezoneResponse,
    ClockGetUtcResponse, ClockRequest, ClockSetUtcResponse, ClockWatchResponse,
};
use mikros_rt::sys;

/// The permission that setting the time and the time zone needs.
const TIME_SET: &str = "time.set";
/// The time zone while no other is known.
pub const UTC: &str = "UTC";

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// What every connection shares.
pub struct Time {
    pub page: Page,
    broker: Option<BrokerAsyncClient>,
    /// The connections that called `Watch`.
    watchers: RefCell<Vec<Weak<ClockAsyncServer>>>,
    /// Whether `SetUtc` changed the time since boot.
    pub set_since_boot: Cell<bool>,
}

impl Time {
    pub fn new(page: Page, broker: Option<BrokerAsyncClient>) -> Time {
        Time {
            page,
            broker,
            watchers: RefCell::new(Vec::new()),
            set_since_boot: Cell::new(false),
        }
    }

    /// UTC now, in nanoseconds since 1970-01-01T00:00:00.
    pub fn utc(&self) -> i64 {
        now().saturating_add(self.page.offset())
    }

    /// Makes UTC `utc` now, and tells the watchers.
    pub fn set_utc(&self, utc: i64) {
        self.page.set(utc.saturating_sub(now()));
        self.changed();
    }

    /// Sends `OnTimeChanged` to every connection that watches, and forgets
    /// the closed ones.
    pub fn changed(&self) {
        self.watchers.borrow_mut().retain(|w| {
            w.upgrade()
                .is_some_and(|server| server.server().send_on_time_changed().is_ok())
        });
    }

    /// Whether `client` holds `time.set`, as the broker says.
    async fn may_set(&self, client: &ClientIdentity) -> bool {
        let Some(broker) = &self.broker else {
            return false;
        };
        let client = ClientIdentity {
            pid: client.pid,
            user: client.user,
            app_id: client.app_id.clone(),
        };
        let checked = broker.check(client, TIME_SET.into()).await;
        matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }
}

/// Serves the clock on `end` for `client` until it closes the
/// connection.
pub async fn serve(
    time: Rc<Time>,
    end: ServerEnd<Clock>,
    reactor: Reactor,
    client: ClientIdentity,
) {
    let server = Rc::new(ClockAsyncServer::new(end, reactor));
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            ClockRequest::GetUtc(_, responder) => {
                responder.reply(s, ClockGetUtcResponse { utc: time.utc() })
            }
            ClockRequest::GetTimePage(_, responder) => match time.page.handle() {
                Some(page) => responder.reply(s, ClockGetTimePageResponse { page }),
                // A reply needs a page; without one the client sees the
                // connection close.
                None => return,
            },
            ClockRequest::SetUtc(set, responder) => {
                let result = match time.may_set(&client).await {
                    true => {
                        time.set_since_boot.set(true);
                        time.set_utc(set.utc);
                        Ok(ClockSetUtcResponse {})
                    }
                    false => Err(Status::AccessDenied),
                };
                responder.reply(s, result)
            }
            ClockRequest::GetTimezone(_, responder) => {
                responder.reply(s, ClockGetTimezoneResponse { name: UTC.into() })
            }
            ClockRequest::SetTimezone(_, responder) => {
                let result = match time.may_set(&client).await {
                    // No time zone can be set before the root file system
                    // is available.
                    true => Err(Status::BadState),
                    false => Err(Status::AccessDenied),
                };
                responder.reply(s, result)
            }
            ClockRequest::Watch(_, responder) => {
                time.watchers.borrow_mut().push(Rc::downgrade(&server));
                responder.reply(s, Ok(ClockWatchResponse {}))
            }
        };
    }
}
