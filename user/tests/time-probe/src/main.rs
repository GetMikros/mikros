//! A program that the service manager starts as two boot services after
//! the time service, to drive it as a client. As `time-probe`, which
//! holds `time.set` and gets the platform information, it checks that
//! UTC is the boot time plus the monotonic clock, as the time page also
//! says, that the page cannot be written, that setting the time changes
//! it and is sent to watchers, and that without a root file system the
//! zone is UTC and cannot be set, and that the time page of its startup,
//! when the process manager had one to give, says the same as the
//! service. As `time-guest`, which holds no
//! permission, it checks that it can neither set the time nor the zone.
//! It exits with 0 when every check passed, or with the number of the
//! first that failed.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::time::{Clock, ClockClient, ClockEvent};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// How far UTC may be from the boot time plus the monotonic clock: the
/// boot time has whole seconds, and was read before the clock started.
const SLACK: i64 = 5 * NANOS_PER_SECOND;
/// The `id` of the `platform_info` boot handle among the `USER` startup
/// handles, and the flag and offset of its boot time.
const PLATFORM_INFO_ID: u32 = 5;
const BOOT_TIME_VALID: u32 = 1 << 1;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE: u64 = 4096;
const GUEST: &str = "system.time-guest";

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor, |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let directory = DirectoryClient::new(managed.header.directory);
    let Some(clock) = connect(&directory) else {
        return 1;
    };
    let process = managed.header.process.raw();
    let startup_page = managed.header.time_page.as_ref().map(|page| page.raw());
    let result = match managed.header.app_id.as_str() {
        GUEST => guest(&clock),
        _ => probe(&clock, &managed.handles, process, startup_page),
    };
    match result {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

fn connect(directory: &DirectoryClient) -> Option<ClockClient> {
    let (client, server) = endpoints::<Clock>().ok()?;
    match directory.connect(
        "mikros.time.Clock".into(),
        None,
        server.into_handle(),
        FOREVER,
    ) {
        Ok(Ok(_)) => Some(ClockClient::new(client)),
        _ => None,
    }
}

/// The boot time of the platform information among `handles`, in
/// nanoseconds, or 0 when it is not valid.
fn boot_time(handles: &[StartupHandle]) -> Option<i64> {
    let info = handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == PLATFORM_INFO_ID)?;
    let read = |at: u64, buf: &mut [u8]| {
        // SAFETY: the pointer and length describe `buf`.
        let status =
            unsafe { sys::mem_read(info.handle.raw(), at, buf.as_mut_ptr(), buf.len() as u64) };
        (status == mikros_rt::OK).then_some(())
    };
    let (mut flags, mut seconds) = ([0u8; 4], [0u8; 8]);
    read(0x04, &mut flags)?;
    read(0x60, &mut seconds)?;
    if u32::from_le_bytes(flags) & BOOT_TIME_VALID == 0 {
        return Some(0);
    }
    i64::from_le_bytes(seconds).checked_mul(NANOS_PER_SECOND)
}

/// The offset of the time page `page`, mapped into `process`, read as
/// the page's sequence says, and its version.
fn page_offset(process: sys::Handle, page: sys::Handle) -> Option<(u32, i64)> {
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    if unsafe { sys::vm_map(process, PERM_READ, 0, page, 0, PAGE, &mut at) } != mikros_rt::OK {
        return None;
    }
    let read = || loop {
        // SAFETY: the page is mapped readable at `at` for a page.
        let (before, version, offset, after) = unsafe {
            let p = at as *const u8;
            let before = (p as *const u32).read_volatile();
            let version = (p.add(4) as *const u32).read_volatile();
            let offset = (p.add(8) as *const i64).read_volatile();
            let after = (p as *const u32).read_volatile();
            (before, version, offset, after)
        };
        if before % 2 == 0 && before == after {
            return (version, offset);
        }
    };
    let read = read();
    // SAFETY: the call takes no pointers.
    unsafe { sys::vm_unmap(process, at, PAGE) };
    Some(read)
}

fn probe(
    clock: &ClockClient,
    handles: &[StartupHandle],
    process: sys::Handle,
    startup_page: Option<sys::Handle>,
) -> Result<(), i64> {
    // UTC is the boot time plus the monotonic clock.
    let boot = boot_time(handles).ok_or(2)?;
    let utc = clock.get_utc(FOREVER).map_err(|_| 3)?.utc;
    if (utc - (boot + now())).abs() > SLACK {
        return Err(3);
    }
    // The time page holds the same offset, and cannot be mapped writable.
    let page = clock.get_time_page(FOREVER).map_err(|_| 4)?.page;
    let (version, offset) = page_offset(process, page.raw()).ok_or(4)?;
    let mono = now();
    let utc = clock.get_utc(FOREVER).map_err(|_| 5)?.utc;
    if version != 1 || (utc - (mono + offset)).abs() > NANOS_PER_SECOND {
        return Err(5);
    }
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let writable = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            page.raw(),
            0,
            PAGE,
            &mut at,
        )
    };
    if writable == mikros_rt::OK {
        return Err(6);
    }
    // Setting the time changes it, and the watchers hear of it.
    if !matches!(clock.watch(FOREVER), Ok(Ok(_))) {
        return Err(7);
    }
    let set = 2_000_000_000 * NANOS_PER_SECOND;
    if !matches!(clock.set_utc(set, FOREVER), Ok(Ok(_))) {
        return Err(8);
    }
    let Ok(ClockEvent::OnTimeChanged(_)) = clock.next_event(now() + 60 * NANOS_PER_SECOND) else {
        return Err(9);
    };
    let utc = clock.get_utc(FOREVER).map_err(|_| 10)?.utc;
    if !(set..set + SLACK).contains(&utc) {
        return Err(10);
    }
    let (_, offset) = page_offset(process, page.raw()).ok_or(11)?;
    if (now() + offset - set).abs() > SLACK {
        return Err(11);
    }
    // Without a root file system, the zone is UTC, and cannot be set.
    let zone = clock.get_timezone(FOREVER).map_err(|_| 12)?.name;
    if zone != "UTC" {
        return Err(12);
    }
    if !matches!(
        clock.set_timezone("Europe/Berlin".into(), FOREVER),
        Ok(Err(Status::BadState))
    ) {
        return Err(13);
    }
    // The process manager gets the page once the service runs, and none
    // waits for it; when it had the page as the probe started, the
    // probe's startup holds the page that the service writes.
    if let Some(page) = startup_page {
        let (version, offset) = page_offset(process, page).ok_or(14)?;
        let utc = clock.get_utc(FOREVER).map_err(|_| 14)?.utc;
        if version != 1 || (utc - (now() + offset)).abs() > SLACK {
            return Err(14);
        }
    }
    Ok(())
}

fn guest(clock: &ClockClient) -> Result<(), i64> {
    match clock.set_utc(0, FOREVER) {
        Ok(Err(Status::AccessDenied)) => {}
        _ => return Err(20),
    }
    match clock.set_timezone("UTC".into(), FOREVER) {
        Ok(Err(Status::AccessDenied)) => Ok(()),
        _ => Err(21),
    }
}
