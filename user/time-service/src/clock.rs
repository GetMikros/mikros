//! `mikros.time.Clock`: UTC as the monotonic clock plus the offset of the
//! time page, the page itself, and setting the time, which needs
//! `time.set`, for every client; and `OnTimeChanged` for those that
//! watch.

use crate::page::Page;
use crate::root::{self, Zone};
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::time::{
    Clock, ClockAsyncServer, ClockGetTimePageResponse, ClockGetTimezoneResponse,
    ClockGetUtcResponse, ClockRequest, ClockSetTimezoneResponse, ClockSetUtcResponse,
    ClockWatchResponse,
};
use mikros_rt::sys;

/// The permission that setting the time and the time zone needs.
const TIME_SET: &str = "time.set";
/// The time zone while no other is known.
pub const UTC: &str = "UTC";
pub const NANOS_PER_SECOND: i64 = 1_000_000_000;

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
    /// The boot time of the platform information, in seconds, when it
    /// is valid.
    boot_time: Option<i64>,
    /// The connections that called `Watch`.
    watchers: RefCell<Vec<Weak<ClockAsyncServer>>>,
    /// Whether `SetUtc` changed the time since boot.
    set_since_boot: Cell<bool>,
    /// The root file system, once it is available, the system time zone,
    /// UTC until then, and whether the real-time clock keeps local time.
    root: RefCell<Option<Rc<NodeAsyncClient>>>,
    zone: RefCell<Zone>,
    rtc_local: Cell<bool>,
}

impl Time {
    pub fn new(page: Page, broker: Option<BrokerAsyncClient>, boot_time: Option<i64>) -> Time {
        Time {
            page,
            broker,
            boot_time,
            watchers: RefCell::new(Vec::new()),
            set_since_boot: Cell::new(false),
            root: RefCell::new(None),
            zone: RefCell::new(Zone::utc()),
            rtc_local: Cell::new(false),
        }
    }

    /// Takes the root file system `root`, where the real-time clock was
    /// found to keep local time when `rtc_local`, in `zone`. With local
    /// time, and when `SetUtc` did not set the time since boot, the boot
    /// time counts as local time from now on, and the watchers are told.
    pub fn root_available(&self, root: Rc<NodeAsyncClient>, rtc_local: bool, zone: Zone) {
        let offset = match (rtc_local, self.set_since_boot.get(), self.boot_time) {
            (true, false, Some(boot_time)) => root::local_offset(&zone, boot_time),
            _ => None,
        };
        *self.root.borrow_mut() = Some(root);
        *self.zone.borrow_mut() = zone;
        self.rtc_local.set(rtc_local);
        if let Some(offset) = offset {
            self.page.set(offset);
            self.changed();
        }
    }

    /// Makes `name` the system time zone, once the root file system is
    /// available, and tells the watchers.
    async fn set_timezone(&self, name: &str, reactor: &Reactor) -> Result<(), Status> {
        let root = self.root.borrow().clone().ok_or(Status::BadState)?;
        let zone = root::set_zone(&root, name, reactor).await?;
        *self.zone.borrow_mut() = zone;
        self.changed();
        Ok(())
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
    let server = Rc::new(ClockAsyncServer::new(end, reactor.clone()));
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
                let name = time.zone.borrow().name.clone();
                responder.reply(s, ClockGetTimezoneResponse { name })
            }
            ClockRequest::SetTimezone(set, responder) => {
                let result = match time.may_set(&client).await {
                    true => time.set_timezone(&set.name, &reactor).await,
                    false => Err(Status::AccessDenied),
                };
                responder.reply(s, result.map(|()| ClockSetTimezoneResponse {}))
            }
            ClockRequest::Watch(_, responder) => {
                time.watchers.borrow_mut().push(Rc::downgrade(&server));
                responder.reply(s, Ok(ClockWatchResponse {}))
            }
        };
    }
}
