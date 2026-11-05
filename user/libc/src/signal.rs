//! Signals, with the process signal system calls.
//!
//! The kernel runs one trampoline for every signal that has a handler.
//! It builds a `siginfo_t` from the kernel's information record and a
//! `ucontext_t` from the saved context, calls the handler that
//! `sigaction` recorded, copies the registers and the mask the handler
//! may have changed back into the saved context, and returns to the
//! restorer, which calls `signal_return`.

use crate::errno::{self, EINVAL, ENOMEM, EPERM, ESRCH};
use crate::thread;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use mikros_rt::sys;

const SIGNALS: usize = 64;
const SA_SIGINFO: u64 = 4;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
/// The kernel's action flags, by the `SA_` flag of each.
const FLAGS: [(u64, u32); 4] = [
    (SA_RESTART, 1),
    (SA_ONSTACK, 2),
    (SA_NODEFER, 4),
    (SA_RESETHAND, 8),
];
const SS_ONSTACK: i32 = 1;
const SS_DISABLE: i32 = 2;
/// The smallest alternate stack the kernel takes.
const ALTSTACK_MIN: u64 = 16384;
const NANOSECONDS: i64 = 1_000_000_000;

/// musl's `struct k_sigaction`.
#[repr(C)]
pub struct KSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

/// The kernel's signal action.
#[repr(C)]
#[derive(Default)]
struct Action {
    handler: u64,
    mask: u64,
    restorer: u64,
    flags: u32,
    reserved: u32,
}

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

/// The handler and `SA_` flags that `sigaction` set for each signal.
static HANDLERS: [AtomicUsize; SIGNALS] = [const { AtomicUsize::new(0) }; SIGNALS];
static HANDLER_FLAGS: [AtomicU64; SIGNALS] = [const { AtomicU64::new(0) }; SIGNALS];

/// `rt_sigaction`.
pub fn sigaction(signo: i64, act: *const KSigaction, old: *mut KSigaction, size: u64) -> i64 {
    if !(1..=SIGNALS as i64).contains(&signo) || size != 8 {
        return -EINVAL;
    }
    let at = signo as usize - 1;
    let previous = (
        HANDLERS[at].load(Ordering::Acquire),
        HANDLER_FLAGS[at].load(Ordering::Acquire),
    );
    let mut action = Action::default();
    // SAFETY: musl passes a `struct k_sigaction` or null.
    let new = unsafe { act.as_ref() };
    if let Some(new) = new {
        action.mask = new.mask;
        action.flags = FLAGS
            .iter()
            .filter(|(sa, _)| new.flags & sa != 0)
            .fold(0, |f, (_, k)| f | k);
        // 0 and 1 are the default action and ignore.
        if new.handler > 1 {
            HANDLER_FLAGS[at].store(new.flags, Ordering::Release);
            HANDLERS[at].store(new.handler, Ordering::Release);
            action.handler = trampoline as *const () as u64;
            action.restorer = restore as *const () as u64;
        } else {
            action.handler = new.handler as u64;
        }
    }
    let mut kernel_old = Action::default();
    let action_ptr = new.map_or(core::ptr::null(), |_| (&raw const action).cast::<u8>());
    // SAFETY: the pointers describe the two actions.
    let status =
        unsafe { sys::signal_set_action(signo as u32, action_ptr, (&raw mut kernel_old).cast()) };
    if status != mikros_rt::OK {
        HANDLERS[at].store(previous.0, Ordering::Release);
        HANDLER_FLAGS[at].store(previous.1, Ordering::Release);
        return -errno::from_status(status);
    }
    // SAFETY: musl passes a `struct k_sigaction` or null.
    if let Some(old) = unsafe { old.as_mut() } {
        let caught = kernel_old.handler == trampoline as *const () as u64;
        let mut flags = FLAGS
            .iter()
            .filter(|(_, k)| kernel_old.flags & k != 0)
            .fold(SA_RESTORER, |f, (sa, _)| f | sa);
        if caught {
            flags |= previous.1 & SA_SIGINFO;
        }
        *old = KSigaction {
            handler: if caught {
                previous.0
            } else {
                kernel_old.handler as usize
            },
            flags,
            restorer: restore as *const () as usize,
            mask: kernel_old.mask,
        };
    }
    0
}

/// The handler of every caught signal: `signo`, the kernel's record at
/// `info` and the saved context at `context`.
extern "C" fn trampoline(signo: i32, info: *const Info, context: *mut u64) {
    let Some(at) = (signo as usize).checked_sub(1).filter(|&a| a < SIGNALS) else {
        return;
    };
    // SAFETY: the kernel passes its record and the saved context, laid
    // out as MIKSPEC-0016.R22 defines.
    let (info, fs_base) = unsafe { (&*info, *context.add(CONTEXT_FS_BASE)) };
    if fs_base == 0 {
        // SAFETY: as above.
        unsafe { defer(signo as u32, info, context) };
        return;
    }
    let handler = HANDLERS[at].load(Ordering::Acquire);
    if handler <= 1 {
        return;
    }
    let mut siginfo = [0u8; 128];
    fill_siginfo(info, &mut siginfo);
    let mut uc = Ucontext::new();
    // SAFETY: as above.
    unsafe { uc.load(context, info) };
    // SAFETY: `sigaction` recorded a handler of this type.
    let handler: extern "C" fn(i32, *mut u8, *mut Ucontext) =
        unsafe { core::mem::transmute(handler) };
    handler(signo, siginfo.as_mut_ptr(), &mut uc);
    // SAFETY: as above.
    unsafe { uc.store(context) };
}

/// Puts off a signal that interrupted a new thread before its entry set
/// the thread pointer, which handlers need: the thread blocks it once it
/// returns, and it is sent again, to the process or to the thread as it
/// was, to be handled once the thread unblocks it.
///
/// # Safety
///
/// `context` must be the saved context of the signal's frame.
unsafe fn defer(signo: u32, info: &Info, context: *mut u64) {
    // SAFETY: the mask and RSP are in the saved context.
    let sp = unsafe {
        *context.add(CONTEXT_MASK) |= 1 << (signo - 1);
        *context.add(GREGS[15])
    };
    // SAFETY: sending a signal takes no pointers.
    unsafe {
        if info.code == 0 {
            sys::process_signal(crate::syscall::process(), signo, info.value);
        } else if let Some(handle) = thread::starting(sp) {
            sys::thread_signal(handle, signo, info.value);
        }
    }
}

/// The return address of the trampoline, with the saved context 64
/// bytes above the stack pointer.
#[unsafe(naked)]
unsafe extern "C" fn restore() -> ! {
    core::arch::naked_asm!(
        "lea rdi, [rsp + 64]",
        "mov eax, {signal_return}",
        "syscall",
        "ud2",
        signal_return = const 36,
    )
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

/// musl's `ucontext_t` for x86_64.
#[repr(C)]
struct Ucontext {
    flags: u64,
    link: u64,
    stack: [u64; 3],
    gregs: [u64; 23],
    fpregs: u64,
    reserved: [u64; 8],
    sigmask: [u64; 16],
    fpregs_mem: [u64; 64],
}

/// The indices in the kernel's register state of `gregs[0..18]`: R8 to
/// R15, RDI, RSI, RBP, RBX, RDX, RAX, RCX, RSP, RIP and RFLAGS.
const GREGS: [usize; 18] = [8, 9, 10, 11, 12, 13, 14, 15, 5, 4, 6, 1, 3, 0, 2, 7, 16, 17];
const REG_CR2: usize = 22;
/// The index of the FS base in the kernel's register state.
const CONTEXT_FS_BASE: usize = 18;
/// The offsets in the saved context of the mask, the size of the
/// extended state and the extended state.
const CONTEXT_MASK: usize = 0xA0 / 8;
const CONTEXT_XSAVE_SIZE: usize = 0xA8 / 8;
const CONTEXT_XSAVE: usize = 0xC0;

impl Ucontext {
    fn new() -> Self {
        Ucontext {
            flags: 0,
            link: 0,
            stack: [0; 3],
            gregs: [0; 23],
            fpregs: 0,
            reserved: [0; 8],
            sigmask: [0; 16],
            fpregs_mem: [0; 64],
        }
    }

    /// Fills the context from the saved context at `context`.
    ///
    /// # Safety
    ///
    /// `context` must be a saved context of a signal frame.
    unsafe fn load(&mut self, context: *const u64, info: &Info) {
        for (g, &k) in GREGS.iter().enumerate() {
            // SAFETY: the register state has 20 values.
            self.gregs[g] = unsafe { *context.add(k) };
        }
        if info.code == 2 {
            self.gregs[REG_CR2] = info.address;
        }
        // SAFETY: the mask and the size follow the register state.
        let (mask, xsave_size) =
            unsafe { (*context.add(CONTEXT_MASK), *context.add(CONTEXT_XSAVE_SIZE)) };
        self.sigmask[0] = mask;
        // The legacy area of the XSAVE format is the `FXSAVE` layout of
        // `struct _fpstate`.
        if xsave_size >= 512 {
            self.fpregs = context as u64 + CONTEXT_XSAVE as u64;
        }
        let (base, size) = thread::altstack();
        let flags = if size == 0 { SS_DISABLE } else { 0 };
        self.stack = [base, flags as u64, size];
    }

    /// Stores the registers and the mask back into the saved context.
    ///
    /// # Safety
    ///
    /// As for `load`.
    unsafe fn store(&self, context: *mut u64) {
        for (g, &k) in GREGS.iter().enumerate() {
            // SAFETY: the register state has 20 values.
            unsafe { *context.add(k) = self.gregs[g] };
        }
        // SAFETY: the mask follows the register state.
        unsafe { *context.add(CONTEXT_MASK) = self.sigmask[0] };
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
