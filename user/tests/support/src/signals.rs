//! A signal handler that records what it finds, for the tests of signals
//! in both tiers: `signal_probe` starts it, and `signal_restorer`
//! returns from a signal, for other handlers too.

use crate::{Result, check};
use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::sys;

/// The stack pointer and RFLAGS at the first instruction of the signal
/// handler `signal_probe`, the arguments `signal_handler` got, the mask
/// while it ran and how often it ran.
pub static SIGNAL_ENTRY: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
pub static SIGNAL_ARGS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
pub static SIGNAL_MASK: AtomicU64 = AtomicU64::new(0);
/// The signals pending as the last handler started.
pub static SIGNAL_PENDING: AtomicU64 = AtomicU64::new(0);
pub static HANDLED: AtomicU64 = AtomicU64::new(0);
/// The signal frame `signal_handler` found, from 72 bytes below the
/// context to its extended state: the restorer, the information record,
/// the register state, the mask, the extended state size and 16 bytes.
static mut SIGNAL_FRAME: [u64; 33] = [0; 33];

// `signal_probe` records the stack pointer and the flags the handler
// starts with and goes on in `signal_handler`, which returns to
// `signal_restorer`; that finds the context 64 bytes above the stack
// pointer, where the return left it, and returns from the signal.
core::arch::global_asm!(
    ".global signal_probe",
    "signal_probe:",
    "mov [rip + {entry}], rsp",
    "pushfq",
    "pop qword ptr [rip + {entry} + 8]",
    "jmp {handler}",
    ".global signal_restorer",
    "signal_restorer:",
    "lea rdi, [rsp + 64]",
    "call {back}",
    entry = sym SIGNAL_ENTRY,
    handler = sym signal_handler,
    back = sym signal_back,
);

unsafe extern "sysv64" {
    pub safe fn signal_probe();
    pub safe fn signal_restorer();
}

extern "sysv64" fn signal_handler(signo: u64, info: u64, context: u64) {
    SPINNER_LOOPS_HANDLED.store(SPINNER_LOOPS.load(Ordering::Relaxed), Ordering::Release);
    let mut mask = 0;
    // SAFETY: the pointer is valid; adding no signal only reads the mask.
    unsafe { sys::signal_set_mask(0, 0, &mut mask) };
    SIGNAL_MASK.store(mask, Ordering::Release);
    let mut pending = 0;
    // SAFETY: the pointer is valid for the call.
    unsafe { sys::signal_get_pending(&mut pending) };
    SIGNAL_PENDING.store(pending, Ordering::Release);
    for (slot, value) in SIGNAL_ARGS.iter().zip([signo, info, context]) {
        slot.store(value, Ordering::Release);
    }
    let from = (context - 72) as *const u64;
    for i in 0..33 {
        // SAFETY: the frame lies on this thread's stack from 72 bytes
        // below the context, and only this handler writes the copy.
        unsafe {
            let word = from.add(i).read_volatile();
            (&raw mut SIGNAL_FRAME)
                .cast::<u64>()
                .add(i)
                .write_volatile(word);
        }
    }
    HANDLED.fetch_add(1, Ordering::AcqRel);
}

extern "sysv64" fn signal_back(context: u64) -> ! {
    // SAFETY: `context` is the context of the frame the handler got.
    unsafe { sys::signal_return(context) };
    unreachable!("signal_return returned");
}

pub const USR1: u32 = 10;

/// Makes `signal_probe` the handler of `USR1` with `flags`, blocking
/// `USR2` while it runs, and returns the action it replaced.
pub fn catch_usr1(flags: u32) -> Result<[u8; 32]> {
    let mut action = [0u8; 32];
    action[0x00..0x08].copy_from_slice(&(signal_probe as *const () as u64).to_le_bytes());
    action[0x08..0x10].copy_from_slice(&(1u64 << 11).to_le_bytes());
    action[0x10..0x18].copy_from_slice(&(signal_restorer as *const () as u64).to_le_bytes());
    action[0x18..0x1C].copy_from_slice(&flags.to_le_bytes());
    let mut old = [0u8; 32];
    // SAFETY: the pointers describe `action` and `old`.
    check("setting the action", unsafe {
        sys::signal_set_action(USR1, action.as_ptr(), old.as_mut_ptr())
    })?;
    HANDLED.store(0, Ordering::Release);
    Ok(old)
}

pub fn restore_usr1(old: &[u8; 32]) -> Result {
    // SAFETY: the pointer describes `old`.
    check("restoring the action", unsafe {
        sys::signal_set_action(USR1, old.as_ptr(), core::ptr::null_mut())
    })
}

/// The copy of the last signal frame.
pub fn signal_frame_copy() -> [u64; 33] {
    // SAFETY: the handler that wrote the copy has returned.
    unsafe { core::ptr::read_volatile(&raw const SIGNAL_FRAME) }
}

/// Counts the loops of `spin_until_handled`.
pub static SPINNER_LOOPS: AtomicU64 = AtomicU64::new(0);
/// `SPINNER_LOOPS` as the last signal handler started.
pub static SPINNER_LOOPS_HANDLED: AtomicU64 = AtomicU64::new(0);

/// Spins in user mode, never entering the kernel, until the handler has
/// run `rounds` times, then exits.
pub extern "sysv64" fn spin_until_handled(rounds: u64, _: u64) -> ! {
    while HANDLED.load(Ordering::Acquire) < rounds {
        SPINNER_LOOPS.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}
