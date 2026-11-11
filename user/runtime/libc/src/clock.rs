//! The clocks of `clock_gettime`: the monotonic clock from the clock
//! page, the real-time clock from the time page and the monotonic clock,
//! and the CPU time of the process and of the thread. A process started
//! without the time page asks `mikros.time.Clock` for it through its
//! directory.

use crate::errno::{self, EAGAIN, EFAULT, EINTR, EINVAL, EIO};
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::time::ClockClient;
use mikros_rt::sys::{self, Handle};

pub const CLOCK_REALTIME: i64 = 0;
pub const CLOCK_MONOTONIC: i64 = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: i64 = 2;
pub const CLOCK_THREAD_CPUTIME_ID: i64 = 3;
/// The time since boot, which is the monotonic clock's on Mikros.
pub const CLOCK_BOOTTIME: i64 = 7;

const PERM_READ: u32 = 1 << 0;
const PAGE_SIZE: u64 = 4096;
const INFO_PROCESS: u32 = 2;
const INFO_THREAD: u32 = 3;
/// The offsets of `cpu_time` in the `PROCESS` and `THREAD` records.
const PROCESS_CPU_TIME: usize = 0x18;
const THREAD_CPU_TIME: usize = 0x10;
/// The clock page `source` that reads the time stamp counter.
const SOURCE_TSC: u32 = 1;

/// The addresses of the mapped clock and time pages, or 0.
static CLOCK_PAGE: AtomicU64 = AtomicU64::new(0);
static TIME_PAGE: AtomicU64 = AtomicU64::new(0);
/// The protocol that hands out the time page.
const CLOCK_PROTOCOL: &str = "mikros.time.Clock";
const FOREVER: i64 = i64::MAX;

/// The process's directory connection, which the time page is asked
/// for through, or 0.
static DIRECTORY: AtomicU32 = AtomicU32::new(0);
/// Set while a thread asks for the time page.
static ACQUIRING: AtomicBool = AtomicBool::new(false);
/// The handles that the CPU time clocks read.
static PROCESS: AtomicU32 = AtomicU32::new(0);
static THREAD: AtomicU32 = AtomicU32::new(0);

/// Maps the clock page and, when the transfer holds one, the time page,
/// and records the process and thread whose CPU time the clocks read.
/// A page that cannot be mapped is left unused.
pub fn install(
    process: Handle,
    thread: Handle,
    directory: Handle,
    clock_page: Handle,
    time_page: Option<Handle>,
) {
    PROCESS.store(process, Ordering::Relaxed);
    DIRECTORY.store(directory, Ordering::Relaxed);
    THREAD.store(thread, Ordering::Relaxed);
    if let Some(addr) = map(process, clock_page) {
        CLOCK_PAGE.store(addr, Ordering::Release);
    }
    if let Some(addr) = time_page.and_then(|page| map(process, page)) {
        TIME_PAGE.store(addr, Ordering::Release);
    }
}

fn map(process: Handle, page: Handle) -> Option<u64> {
    let mut addr = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process, PERM_READ, 0, page, 0, PAGE_SIZE, &mut addr) };
    (status == mikros_rt::OK).then_some(addr)
}

#[repr(C)]
pub struct Timespec {
    sec: i64,
    nsec: i64,
}

/// `clock_gettime`: stores the time of `clock` in `ts`.
pub fn gettime(clock: i64, ts: *mut Timespec) -> i64 {
    let ns = match clock {
        CLOCK_MONOTONIC | CLOCK_BOOTTIME => monotonic(),
        CLOCK_REALTIME => realtime(),
        CLOCK_PROCESS_CPUTIME_ID => cpu_time(PROCESS.load(Ordering::Relaxed), INFO_PROCESS),
        CLOCK_THREAD_CPUTIME_ID => cpu_time(THREAD.load(Ordering::Relaxed), INFO_THREAD),
        _ => Err(EINVAL),
    };
    match ns {
        Ok(ns) => {
            // SAFETY: musl passes a timespec to store the time in.
            unsafe {
                ts.write(Timespec {
                    sec: ns.div_euclid(1_000_000_000),
                    nsec: ns.rem_euclid(1_000_000_000),
                })
            };
            0
        }
        Err(e) => -e,
    }
}

/// The `clock_nanosleep` flag for a deadline instead of a duration.
const TIMER_ABSTIME: i64 = 1;
const NANOSECONDS: i64 = 1_000_000_000;
const INTERRUPTED: mikros_rt::sys::Status = -27;

/// The nanoseconds that `ts` holds, or `EINVAL` when it is not a valid
/// time.
pub fn nanos(ts: *const Timespec) -> Result<i64, i64> {
    // SAFETY: musl passes a `struct timespec`.
    let Some(t) = (unsafe { ts.as_ref() }) else {
        return Err(EFAULT);
    };
    if t.sec < 0 || !(0..NANOSECONDS).contains(&t.nsec) {
        return Err(EINVAL);
    }
    Ok(t.sec.saturating_mul(NANOSECONDS).saturating_add(t.nsec))
}

/// `clock_nanosleep`, and `nanosleep` as `CLOCK_REALTIME` without flags:
/// sleeps until the time `req` of `clock` with `TIMER_ABSTIME`, and for
/// `req` otherwise. A signal ends the sleep with `EINTR`, after storing
/// at `rem` what was left of a sleep for a time.
pub fn nanosleep(clock: i64, flags: i64, req: *const Timespec, rem: *mut Timespec) -> i64 {
    let sleep = || -> Result<i64, i64> {
        if clock != CLOCK_MONOTONIC && clock != CLOCK_REALTIME {
            return Err(EINVAL);
        }
        let ns = nanos(req)?;
        let now = monotonic()?;
        let absolute = flags & TIMER_ABSTIME != 0;
        let deadline = match (absolute, clock) {
            (false, _) => now.saturating_add(ns),
            (true, CLOCK_MONOTONIC) => ns,
            (true, _) => now.saturating_add(ns.saturating_sub(realtime()?)),
        };
        // SAFETY: the call takes no pointers.
        match unsafe { sys::thread_sleep(deadline) } {
            mikros_rt::OK => Ok(0),
            INTERRUPTED => {
                if !absolute && !rem.is_null() {
                    let left = deadline.saturating_sub(monotonic()?).max(0);
                    // SAFETY: musl passes room for the time left.
                    unsafe {
                        rem.write(Timespec {
                            sec: left / NANOSECONDS,
                            nsec: left % NANOSECONDS,
                        })
                    };
                }
                Err(EINTR)
            }
            status => Err(errno::from_status(status)),
        }
    };
    sleep().unwrap_or_else(|e| -e)
}

/// Reads the `u32` at `offset` of the page at `page`.
fn u32_at(page: u64, offset: u64) -> u32 {
    // SAFETY: the page is mapped readable for as long as the process
    // runs, and `offset` lies within it.
    unsafe { core::ptr::read_volatile((page + offset) as *const u32) }
}

fn u64_at(page: u64, offset: u64) -> u64 {
    // SAFETY: as for `u32_at`.
    unsafe { core::ptr::read_volatile((page + offset) as *const u64) }
}

/// Reads with `read` the fields of the page at `page` that its
/// `sequence` at offset 0 guards, retrying while it is odd or changes.
fn consistent<T>(page: u64, read: impl Fn() -> T) -> T {
    loop {
        let before = u32_at(page, 0);
        fence(Ordering::Acquire);
        let value = read();
        fence(Ordering::Acquire);
        if before % 2 == 0 && u32_at(page, 0) == before {
            return value;
        }
        core::hint::spin_loop();
    }
}

/// The monotonic clock, from the clock page when its source is the time
/// stamp counter, and from `clock_get_monotonic` otherwise.
pub fn monotonic() -> Result<i64, i64> {
    let page = CLOCK_PAGE.load(Ordering::Acquire);
    if page != 0 {
        let fields = consistent(page, || {
            (
                u32_at(page, 0x08),
                u32_at(page, 0x10),
                u32_at(page, 0x14),
                u64_at(page, 0x18),
                u64_at(page, 0x20) as i64,
            )
        });
        if let (SOURCE_TSC, mult, shift, tsc_base, ns_base) = fields {
            // SAFETY: RDTSC has no preconditions.
            let tsc = unsafe { core::arch::x86_64::_rdtsc() };
            let product = u128::from(tsc.wrapping_sub(tsc_base)) * u128::from(mult);
            return Ok(ns_base.wrapping_add((product >> shift) as i64));
        }
    }
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::clock_get_monotonic(&mut now) };
    if status == mikros_rt::OK {
        Ok(now)
    } else {
        Err(errno::from_status(status))
    }
}

/// UTC: the time page's offset plus the monotonic clock. Without a time
/// page, it is asked for again on every call.
pub(crate) fn realtime() -> Result<i64, i64> {
    let mut page = TIME_PAGE.load(Ordering::Acquire);
    if page == 0 {
        page = acquire()?;
    }
    let offset = consistent(page, || u64_at(page, 0x08) as i64);
    Ok(offset.wrapping_add(monotonic()?))
}

/// Asks the time service for the time page and maps it. While another
/// thread asks, or while the service does not provide its protocol yet,
/// the time is not known yet.
fn acquire() -> Result<u64, i64> {
    if ACQUIRING.swap(true, Ordering::Acquire) {
        return Err(EAGAIN);
    }
    let fetched = fetch_time_page();
    if let Ok(addr) = fetched {
        TIME_PAGE.store(addr, Ordering::Release);
    }
    ACQUIRING.store(false, Ordering::Release);
    fetched
}

fn fetch_time_page() -> Result<u64, i64> {
    let raw = DIRECTORY.load(Ordering::Relaxed);
    if raw == 0 {
        return Err(EAGAIN);
    }
    // SAFETY: the directory handle stays open for as long as the
    // process runs; the client is never dropped, so it never closes it.
    let directory = ManuallyDrop::new(DirectoryClient::new(ClientEnd::new(unsafe {
        OwnedHandle::from_raw(raw)
    })));
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    let status = unsafe { sys::channel_create(0, &mut ours, &mut theirs) };
    if status != mikros_rt::OK {
        return Err(errno::from_status(status));
    }
    // SAFETY: the channel was just created for us.
    let (ours, theirs) = unsafe { (OwnedHandle::from_raw(ours), OwnedHandle::from_raw(theirs)) };
    match directory.connect(CLOCK_PROTOCOL.into(), None, theirs, FOREVER) {
        Ok(Ok(_)) => {}
        Ok(Err(Status::NotFound)) => return Err(EAGAIN),
        Ok(Err(status)) => return Err(errno::from_status(i64::from(status.into_raw()))),
        Err(_) => return Err(EIO),
    }
    let clock = ClockClient::new(ClientEnd::new(ours));
    let page = clock.get_time_page(FOREVER).map_err(|_| EIO)?.page;
    // The mapping keeps the page alive once its handle is closed.
    map(PROCESS.load(Ordering::Relaxed), page.raw()).ok_or(EIO)
}

/// The `cpu_time` of the `topic` record of `handle`.
fn cpu_time(handle: Handle, topic: u32) -> Result<i64, i64> {
    let at = if topic == INFO_PROCESS {
        PROCESS_CPU_TIME
    } else {
        THREAD_CPU_TIME
    };
    let mut record = [0u8; 0x28];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            handle,
            topic,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK {
        return Err(errno::from_status(status));
    }
    if len < (at + 8) as u64 {
        return Err(errno::EIO);
    }
    Ok(i64::from_le_bytes(
        record[at..at + 8].try_into().expect("8 bytes"),
    ))
}
