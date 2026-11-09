//! Time: UTC from the time service's time page, and the system time
//! zone, whose name the time service gives and whose rules lie in
//! `/usr/share/zoneinfo/`.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{Ordering, fence};
use mikros_fat::Local;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::process::NamespaceEntry;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::time::{Clock as ClockProtocol, ClockClient};
use mikros_rt::{managed, sys};

const CLOCK_PROTOCOL: &str = "mikros.time.Clock";
const FOREVER: i64 = i64::MAX;
const PERM_READ: u32 = 1 << 0;
const NS: i64 = 1_000_000_000;
/// Where the rules of the time zones lie.
const ZONEINFO: &str = "/usr/share/zoneinfo";
/// How long a call to the time service or a zone file may take.
const CALL_NS: i64 = NS;
/// The largest zone file read, and how much of it one read asks for.
const ZONE_MAX: usize = 1 << 20;
const READ_MAX: u32 = 32768;

/// UTC from the time page, and the system time zone from the time
/// service and the zone files, once they are there.
pub struct Clock {
    /// The server's name, for its log messages.
    name: &'static str,
    page: Option<u64>,
    clock: Option<ClockClient>,
    zoneinfo: Option<NodeClient>,
    /// The zone last named, and its local time: UTC when its rules could
    /// not be read.
    zone: RefCell<Option<(String, Local)>>,
}

impl Clock {
    /// UTC and the system time zone of the server `name` through the
    /// time service in `directory`, when it runs, with the zone files of
    /// `zoneinfo`.
    pub fn new(
        name: &'static str,
        directory: &DirectoryClient,
        process: sys::Handle,
        zoneinfo: Option<NodeClient>,
    ) -> Clock {
        let clock = connect(directory);
        Clock {
            name,
            page: clock.as_ref().and_then(|c| time_page(c, process)),
            clock,
            zoneinfo,
            zone: RefCell::new(None),
        }
    }

    /// UTC now, in nanoseconds since 1970, or 0 while it is not known.
    pub fn now(&self) -> i64 {
        let Some(page) = self.page else {
            return 0;
        };
        let read = |at: u64| {
            // SAFETY: the page is mapped read-only for as long as the
            // process runs.
            unsafe { core::ptr::read_volatile(at as *const u64) }
        };
        let offset = loop {
            let before = read(page) as u32;
            fence(Ordering::Acquire);
            let offset = read(page + 8) as i64;
            fence(Ordering::Acquire);
            if before % 2 == 0 && read(page) as u32 == before {
                break offset;
            }
            core::hint::spin_loop();
        };
        monotonic().saturating_add(offset)
    }

    /// The system time zone now: the one whose name the time service
    /// gives, with the rules of its file, read once the name changes,
    /// which is logged. While either cannot be had, local time is UTC.
    pub fn local(&self) -> Local {
        let Some(name) = self.zone_name() else {
            return Local::utc();
        };
        if let Some((cached, local)) = self.zone.borrow().as_ref()
            && *cached == name
        {
            return local.clone();
        }
        let local = match self.rules(&name) {
            Some(rules) => {
                mikros_rt::log!(
                    Info,
                    "{}: local times are in the time zone {name}",
                    self.name
                );
                Local::zone(rules)
            }
            None => {
                mikros_rt::log!(
                    Error,
                    "{}: the time zone {name} cannot be read: local times are UTC",
                    self.name
                );
                Local::utc()
            }
        };
        *self.zone.borrow_mut() = Some((name, local.clone()));
        local
    }

    /// The name of the system time zone, from the time service.
    fn zone_name(&self) -> Option<String> {
        let clock = self.clock.as_ref()?;
        let deadline = monotonic().saturating_add(CALL_NS);
        clock.get_timezone(deadline).ok().map(|r| r.name)
    }

    /// The rules of the zone `name`, from its file.
    fn rules(&self, name: &str) -> Option<mikros_tz::Zone> {
        let dir = self.zoneinfo.as_ref()?;
        if !is_zone_name(name) {
            return None;
        }
        let deadline = monotonic().saturating_add(CALL_NS);
        let (node, server) = endpoints::<Node>().ok()?;
        let opened = dir.open(name.into(), OpenFlags(0), 0, Rights::READ, server, deadline);
        if !matches!(opened, Ok(Ok(_))) {
            return None;
        }
        let file = NodeClient::new(node);
        let mut bytes = Vec::new();
        loop {
            let data = file
                .read(bytes.len() as u64, READ_MAX, deadline)
                .ok()?
                .ok()?
                .data;
            if data.is_empty() {
                break;
            }
            bytes.extend_from_slice(&data);
            if bytes.len() > ZONE_MAX {
                return None;
            }
        }
        mikros_tz::Zone::parse(&bytes).ok()
    }
}

/// The monotonic clock now, in nanoseconds.
fn monotonic() -> i64 {
    let mut monotonic = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut monotonic) };
    monotonic
}

/// A connection to the time service in `directory`, when it runs.
fn connect(directory: &DirectoryClient) -> Option<ClockClient> {
    let (client, server) = endpoints::<ClockProtocol>().ok()?;
    match directory.connect(CLOCK_PROTOCOL.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => Some(ClockClient::new(ClientEnd::new(client.into_handle()))),
        _ => None,
    }
}

/// Maps the time page that `clock` gives.
fn time_page(clock: &ClockClient, process: sys::Handle) -> Option<u64> {
    let page = clock.get_time_page(FOREVER).ok()?.page;
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, PERM_READ, 0, page.raw(), 0, 4096, &mut at) };
    (status == mikros_rt::OK).then_some(at)
}

/// The directory of the zone files in `namespace`: its entry for them, or
/// the one that holds them.
pub fn zoneinfo(namespace: &mut Vec<NamespaceEntry>) -> Option<NodeClient> {
    let (dir, rest) = managed::take_entry(namespace, ZONEINFO)?;
    let dir = NodeClient::new(dir);
    if rest == "." {
        return Some(dir);
    }
    let (node, server) = endpoints::<Node>().ok()?;
    let opened = dir.open(
        rest.into(),
        OpenFlags::DIRECTORY,
        0,
        Rights::READ,
        server,
        FOREVER,
    );
    matches!(opened, Ok(Ok(_))).then(|| NodeClient::new(node))
}

/// Whether `name` can name a file below the zone files' directory: parts
/// of letters, digits and `_`, `-`, `+` and `.`, none of them `.` or
/// `..`, as IANA names have.
fn is_zone_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-+.".contains(&b))
        })
}
