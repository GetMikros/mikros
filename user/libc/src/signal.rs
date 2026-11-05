//! Signals, with the process signal system calls: masks, pending
//! signals, alternate stacks, signals to threads and waiting for
//! signals.

use crate::errno::{self, EINVAL, ENOMEM, EPERM, ESRCH};
use crate::thread;
use mikros_rt::sys;

const SIGNALS: usize = 64;
const SS_ONSTACK: i32 = 1;
const SS_DISABLE: i32 = 2;
/// The smallest alternate stack the kernel takes.
const ALTSTACK_MIN: u64 = 16384;
const NANOSECONDS: i64 = 1_000_000_000;

/// The kernel's signal information record.
#[repr(C)]
#[derive(Default)]
struct Info {
    signo: u32,
    code: u32,
    sender: u64,
    value: u64,
    address: u64,
    exception: u32,
    reserved: u32,
}

/// `stack_t`.
#[repr(C)]
pub struct Stack {
    sp: u64,
    flags: i32,
    size: u64,
}

/// Fills the `siginfo_t` at `out` from the kernel's record.
fn fill_siginfo(info: &Info, out: &mut [u8; 128]) {
    const SI_USER: i32 = 0;
    const SI_QUEUE: i32 = -1;
    const SI_TKILL: i32 = -6;
    const SI_KERNEL: i32 = 0x80;
    let code = match info.code {
        0 if info.value != 0 => SI_QUEUE,
        0 => SI_USER,
        1 => SI_TKILL,
        2 => exception_code(info.exception),
        _ => SI_KERNEL,
    };
    out[0..4].copy_from_slice(&(info.signo as i32).to_ne_bytes());
    out[8..12].copy_from_slice(&code.to_ne_bytes());
    if info.code == 2 {
        out[16..24].copy_from_slice(&info.address.to_ne_bytes());
    } else {
        out[16..20].copy_from_slice(&(info.sender as i32).to_ne_bytes());
        out[24..32].copy_from_slice(&info.value.to_ne_bytes());
    }
}

/// The `si_code` of an exception of the kernel's type `exception`.
fn exception_code(exception: u32) -> i32 {
    match exception {
        // PAGE_FAULT: SEGV_MAPERR; PROTECTION: SEGV_ACCERR.
        2 => 1,
        8 => 2,
        // UNDEFINED_INSTRUCTION: ILL_ILLOPC; ARITHMETIC: FPE_INTDIV.
        3 | 4 => 1,
        // FLOATING_POINT: FPE_FLTINV.
        5 => 7,
        // BREAKPOINT: TRAP_BRKPT; DEBUG: TRAP_TRACE.
        6 => 1,
        7 => 2,
        // ALIGNMENT: BUS_ADRALN; OUT_OF_MEMORY: BUS_ADRERR.
        9 => 1,
        10 => 2,
        // GENERAL and the rest: SI_KERNEL.
        _ => 0x80,
    }
}

/// `rt_sigprocmask`: changes the calling thread's mask with `how` as the
/// kernel does, or only reads it when `set` is null.
pub fn sigprocmask(how: i64, set: *const u64, old: *mut u64, size: u64) -> i64 {
    if size != 8 {
        return -EINVAL;
    }
    // SAFETY: musl passes a signal set or null.
    let (how, mask) = match unsafe { set.as_ref() } {
        Some(&mask) if (0..=2).contains(&how) => (how as u32, mask),
        Some(_) => return -EINVAL,
        None => (0, 0),
    };
    let mut previous = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::signal_set_mask(how, mask, &mut previous) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    if !old.is_null() {
        // SAFETY: musl passes a signal set or null.
        unsafe { old.write(previous) };
    }
    0
}

/// `rt_sigpending`.
pub fn sigpending(set: *mut u64, size: u64) -> i64 {
    if size != 8 {
        return -EINVAL;
    }
    let mut pending = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::signal_get_pending(&mut pending) };
    if status == mikros_rt::OK {
        // SAFETY: musl passes a signal set.
        unsafe { set.write(pending) };
    }
    errno::result(status, 0)
}

fn stack_pointer() -> u64 {
    let sp: u64;
    // SAFETY: reading RSP touches no memory.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack)) };
    sp
}

/// `sigaltstack`. The kernel cannot report a stack without replacing it,
/// so each thread's stack is recorded here too.
pub fn sigaltstack(ss: *const Stack, old: *mut Stack) -> i64 {
    let (base, size) = thread::altstack();
    let sp = stack_pointer();
    let on = size != 0 && (base..base + size).contains(&sp);
    // SAFETY: musl passes a `stack_t` or null.
    if let Some(ss) = unsafe { ss.as_ref() } {
        if on {
            return -EPERM;
        }
        let new = if ss.flags & SS_DISABLE != 0 {
            (0, 0)
        } else if ss.size < ALTSTACK_MIN {
            return -ENOMEM;
        } else {
            (ss.sp, ss.size)
        };
        let mut previous = [0u64; 2];
        // SAFETY: the out pointer holds two values.
        let status = unsafe { sys::signal_set_altstack(new.0, new.1, previous.as_mut_ptr()) };
        if status != mikros_rt::OK {
            return -errno::from_status(status);
        }
        thread::set_altstack(new);
    }
    if !old.is_null() {
        let flags = match (size, on) {
            (0, _) => SS_DISABLE,
            (_, true) => SS_ONSTACK,
            _ => 0,
        };
        // SAFETY: musl passes a `stack_t` or null.
        unsafe {
            old.write(Stack {
                sp: base,
                flags,
                size,
            })
        };
    }
    0
}

/// `tkill` and `tgkill`: sends `signo` to thread `tid`, or only checks
/// that it exists for 0.
pub fn tkill(tid: i64, signo: i64) -> i64 {
    if !(0..=SIGNALS as i64).contains(&signo) {
        return -EINVAL;
    }
    let Some(handle) = u32::try_from(tid).ok().and_then(thread::handle_of) else {
        return -ESRCH;
    };
    if signo == 0 {
        return 0;
    }
    // SAFETY: sending a signal takes no pointers.
    errno::result(unsafe { sys::thread_signal(handle, signo as u32, 0) }, 0)
}

/// `rt_sigtimedwait`: waits for a signal of `set` until the relative
/// `timeout`, and returns its number.
pub fn sigtimedwait(set: *const u64, out: *mut u8, timeout: *const [i64; 2], size: u64) -> i64 {
    if size != 8 || set.is_null() {
        return -EINVAL;
    }
    // SAFETY: musl passes a timeout or null.
    let deadline = match unsafe { timeout.as_ref() } {
        None => i64::MAX,
        Some(&[sec, nsec]) if sec >= 0 && (0..NANOSECONDS).contains(&nsec) => {
            let relative = sec.saturating_mul(NANOSECONDS).saturating_add(nsec);
            match crate::clock::monotonic() {
                Ok(now) => now.saturating_add(relative),
                Err(e) => return -e,
            }
        }
        Some(_) => return -EINVAL,
    };
    let mut info = Info::default();
    // SAFETY: musl passes a signal set, and the record is valid for the
    // call.
    let status = unsafe { sys::signal_wait(*set, deadline, (&raw mut info).cast()) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    if !out.is_null() {
        let mut siginfo = [0u8; 128];
        fill_siginfo(&info, &mut siginfo);
        // SAFETY: musl passes a `siginfo_t` or null.
        unsafe { core::ptr::copy_nonoverlapping(siginfo.as_ptr(), out, 128) };
    }
    i64::from(info.signo)
}
