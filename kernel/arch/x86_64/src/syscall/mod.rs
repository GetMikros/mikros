//! The system call entry and return path, and the glue between the
//! dispatch generated from `kernel/syscalls.yaml` and the handlers.

use crate::cpu::{RFLAGS_AC, write_msr};
use crate::gdt::{KERNEL_CODE, USER_CODE, USER_DATA};
use crate::page_tables::{USER_END, USER_START};
use crate::percpu::{KERNEL_STACK_OFFSET, USER_RSP_OFFSET};
use crate::thread_arch::ThreadX86;
use alloc::sync::Arc;
use core::arch::global_asm;
use kernel_core::object::handles::Handle;
use kernel_core::object::{Object, ObjectType};
use kernel_core::status::Error;

mod handlers;

mod generated {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/syscalls.rs"));
}

const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

const RFLAGS_TF: u64 = 1 << 8;
const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_DF: u64 = 1 << 10;
const RFLAGS_NT: u64 = 1 << 14;

/// The registers of a thread that entered with `SYSCALL`, saved on its
/// kernel stack in the layout of an interrupt frame without the vector
/// and error code.
#[repr(C)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// The entry point. SYSCALL leaves the user RIP in RCX and RFLAGS in R11
// and has cleared IF, TF, DF, AC and NT. The entry moves to the thread's
// kernel stack before it stores anything, builds a frame from which both
// SYSRET and IRETQ can return, and saves every general-purpose register.
// The handler returns nonzero when the return must use IRETQ.
global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "swapgs",
    "mov gs:[{user_rsp}], rsp",
    "mov rsp, gs:[{kernel_stack}]",
    "push {user_data}",
    "push qword ptr gs:[{user_rsp}]",
    "push r11",
    "push {user_code}",
    "push rcx",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi",
    "push rbp", "push r8", "push r9", "push r10", "push r11", "push r12",
    "push r13", "push r14", "push r15",
    "cmp byte ptr [rip + {ibrs}], 0",
    "je 5f",
    "mov ecx, {spec_ctrl}",
    "mov eax, 1",
    "xor edx, edx",
    "wrmsr",
    "5:",
    "mov rdi, rsp",
    "call {handler}",
    // R12 keeps the result while IBRS is cleared, and POP leaves the
    // flags alone, so the test decides the path below.
    "mov r12d, eax",
    "cmp byte ptr [rip + {ibrs}], 0",
    "je 6f",
    "mov ecx, {spec_ctrl}",
    "xor eax, eax",
    "xor edx, edx",
    "wrmsr",
    "6:",
    "test r12d, r12d",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10",
    "pop r9", "pop r8", "pop rbp", "pop rdi", "pop rsi", "pop rdx",
    "pop rcx", "pop rbx", "pop rax",
    "jnz 2f",
    "mov rcx, [rsp]",
    "mov r11, [rsp + 16]",
    "mov rsp, [rsp + 24]",
    "cmp byte ptr [rip + {verw}], 0",
    "je 3f",
    "verw word ptr [rip + {verw_selector}]",
    "3:",
    "swapgs",
    "sysretq",
    "2:",
    "cmp byte ptr [rip + {verw}], 0",
    "je 4f",
    "verw word ptr [rip + {verw_selector}]",
    "4:",
    "swapgs",
    "iretq",
    user_rsp = const USER_RSP_OFFSET,
    kernel_stack = const KERNEL_STACK_OFFSET,
    user_data = const USER_DATA as u64 | 3,
    user_code = const USER_CODE as u64 | 3,
    handler = sym syscall_handler,
    verw = sym crate::mitigations::VERW,
    verw_selector = sym crate::mitigations::VERW_SELECTOR,
    ibrs = sym crate::mitigations::IBRS_ON_ENTRY,
    spec_ctrl = const 0x48,
);

unsafe extern "sysv64" {
    fn syscall_entry();
}

/// Points the `SYSCALL` instruction at the entry point.
pub fn init() {
    // SYSRET takes the user selectors from 0x1B: SS at 0x23 and the
    // 64-bit CS at 0x2B. The base carries RPL 3 because AMD processors
    // load SS from it without setting the RPL themselves.
    const _: () = assert!(USER_DATA == 0x18 + 8 && USER_CODE == 0x18 + 16);
    let star = (0x18u64 | 3) << 48 | u64::from(KERNEL_CODE) << 32;
    let mask = RFLAGS_IF | RFLAGS_TF | RFLAGS_DF | RFLAGS_AC | RFLAGS_NT;
    // SAFETY: EFER.SCE is set, the selectors match the GDT, and the entry
    // point switches stacks before using one.
    unsafe {
        write_msr(IA32_STAR, star);
        write_msr(IA32_LSTAR, syscall_entry as *const () as u64);
        write_msr(IA32_FMASK, mask);
    }
}

/// The length of the `SYSCALL` instruction.
const SYSCALL_LEN: u64 = 2;

/// The number of `channel_call`, which never runs again by itself once
/// interrupted, as its request has been written.
pub const CHANNEL_CALL: u64 = generated::CHANNEL_CALL;

/// The waits that a temporary mask of `signal_set_wait_mask` applies to.
const WAITS: [u64; 4] = [
    generated::PORT_WAIT,
    generated::FUTEX_WAIT,
    generated::OBJECT_WAIT_ONE,
    generated::THREAD_SLEEP,
];

extern "sysv64" fn syscall_handler(frame: &mut Frame) -> u64 {
    // The entry code leaves RFLAGS as SYSCALL set it through IA32_FMASK.
    debug_assert!(
        crate::cpu::rflags() & (RFLAGS_IF | RFLAGS_TF | RFLAGS_DF | RFLAGS_AC | RFLAGS_NT) == 0,
        "a system call entered with flags that IA32_FMASK should clear"
    );
    let at = frame.rip.wrapping_sub(SYSCALL_LEN);
    let Some(thread) = crate::sched::current() else {
        unreachable!("a system call without a running thread");
    };
    if thread.process().is_some_and(|p| !p.syscall_allowed(at)) {
        drop(thread);
        refuse(frame, at);
        crate::sched::preempt_user();
        before_user_return(frame);
        crate::sched::exit_if_dead();
        // The instruction pointer changed.
        return 1;
    }
    let args = [
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9, frame.r12, frame.r13,
    ];
    let number = frame.rax;
    // A temporary mask applies to a wait that is the next call, from
    // before it begins, and is dropped for any other call. Signals are
    // delivered only on the way back to user mode, so none slips in
    // before the wait.
    let temporary = {
        let mut own = thread.signals().lock();
        match own.wait_mask.take() {
            Some(mask) if WAITS.contains(&number) => {
                own.saved_mask = Some(own.mask);
                own.mask = mask;
                Some(mask)
            }
            _ => None,
        }
    };
    thread.x86().set_syscall_frame(frame);
    let result = generated::dispatch(number, &args);
    thread.x86().set_syscall_frame(core::ptr::null_mut());
    let changed = thread.x86().take_syscall_changed();
    frame.rax = result as u64;
    if let Some(mask) = temporary
        && result == Error::Interrupted as i64
    {
        // The wait ends with ERR_INTERRUPTED, even for a handler that
        // asks to restart, unless a suspension ended it: then it runs
        // again with the same temporary mask, and the thread's own mask
        // comes back meanwhile.
        if thread.take_restart() {
            frame.rip = at;
            frame.rax = number;
            let mut own = thread.signals().lock();
            own.wait_mask = Some(mask);
            if let Some(saved) = own.saved_mask.take() {
                own.mask = saved;
            }
        }
    } else if result == Error::Interrupted as i64 {
        // A wait that ended for a suspension leaves the thread at its
        // SYSCALL instruction with the number restored, so that the call
        // runs again with the same arguments.
        if thread.take_restart() {
            frame.rip = at;
            frame.rax = number;
        } else {
            thread.set_interrupted(number);
        }
    } else {
        thread.take_restart();
    }
    crate::sched::preempt_user();
    let reported = before_user_return(frame);
    // Without a handler, whose frame keeps it, the thread's own mask
    // comes back as the call returns.
    {
        let mut own = thread.signals().lock();
        if let Some(saved) = own.saved_mask.take() {
            own.mask = saved;
        }
    }
    drop(thread);
    crate::sched::exit_if_dead();
    // SYSRET to an address outside the user range would fault in ring 0,
    // and after a report or a suspension the handler may have changed RCX
    // and R11, which SYSRET overwrites.
    u64::from(reported || changed || !(USER_START..USER_END).contains(&frame.rip))
}

/// Keeps the running `thread`, which waits in a system call, in state
/// `SUSPENDED` while it is to be suspended, with the registers it entered
/// the call with available to `thread_read_state` and
/// `thread_write_state`. Returns whether it was suspended.
pub fn stay_suspended(thread: &Arc<crate::sched::Thread>) -> bool {
    let frame = thread.x86().syscall_frame();
    if frame.is_null() || !thread.suspend_pending() {
        return false;
    }
    // SAFETY: the frame lies on the running thread's kernel stack below
    // the handler, which uses it again only after the call returns.
    let frame = unsafe { &mut *frame };
    let suspended = with_interrupt_frame(frame, |regs| {
        let mut suspended = false;
        while crate::exception::suspend(thread, regs) {
            suspended = true;
        }
        suspended
    });
    if suspended {
        thread.x86().mark_syscall_changed();
    }
    suspended
}

/// Puts the calling thread, whose `SYSCALL` instruction at `at` lies
/// outside its process's gate, into the `EXCEPTION` state with the type
/// `PROTECTION`, as if the instruction had raised a general-protection
/// fault with error code 0: the report and the saved instruction pointer
/// name the instruction.
fn refuse(frame: &mut Frame, at: u64) {
    with_interrupt_frame(frame, |regs| {
        regs.rip = at;
        regs.vector = u64::from(crate::idt::GENERAL_PROTECTION);
        crate::exception::raise(regs, kernel_core::exception::kind::PROTECTION, at);
        true
    });
}

/// Makes the reports that wait for the calling thread to return to user
/// mode, with its registers in the layout of an interrupt frame while the
/// handler may read and change them. Returns whether it reported any.
fn before_user_return(frame: &mut Frame) -> bool {
    with_interrupt_frame(frame, crate::exception::before_user_return)
}

/// Calls `f` with the registers of `frame` in the layout of an interrupt
/// frame of a debug exception, and takes them back when it returns true.
fn with_interrupt_frame(
    frame: &mut Frame,
    change: impl FnOnce(&mut crate::idt::Frame) -> bool,
) -> bool {
    let f = frame;
    let mut regs = crate::idt::Frame {
        r15: f.r15,
        r14: f.r14,
        r13: f.r13,
        r12: f.r12,
        r11: f.r11,
        r10: f.r10,
        r9: f.r9,
        r8: f.r8,
        rbp: f.rbp,
        rdi: f.rdi,
        rsi: f.rsi,
        rdx: f.rdx,
        rcx: f.rcx,
        rbx: f.rbx,
        rax: f.rax,
        vector: u64::from(crate::idt::DEBUG),
        error: 0,
        rip: f.rip,
        cs: f.cs,
        rflags: f.rflags,
        rsp: f.rsp,
        ss: f.ss,
    };
    if !change(&mut regs) {
        return false;
    }
    let r = regs;
    [f.r15, f.r14, f.r13, f.r12, f.r11, f.r10, f.r9, f.r8] =
        [r.r15, r.r14, r.r13, r.r12, r.r11, r.r10, r.r9, r.r8];
    [f.rbp, f.rdi, f.rsi, f.rdx, f.rcx, f.rbx, f.rax] =
        [r.rbp, r.rdi, r.rsi, r.rdx, r.rcx, r.rbx, r.rax];
    [f.rip, f.rflags, f.rsp] = [r.rip, r.rflags, r.rsp];
    true
}

/// The process of the running thread.
fn current_process() -> Result<Arc<crate::process::Process>, Error> {
    crate::sched::current()
        .and_then(|t| t.process().cloned())
        .ok_or(Error::BadHandle)
}

/// A handle argument that passed its checks.
pub struct Checked {
    pub value: Handle,
    pub object: Arc<dyn Object>,
    pub rights: u32,
}

/// Checks `raw` in the calling process's handle table: that it is valid,
/// that its object has one of `types`, any when `None`, and that it holds
/// `rights`.
fn handle(raw: Handle, types: Option<&[ObjectType]>, rights: u32) -> Result<Checked, Error> {
    let process = current_process()?;
    let table = process.handles().lock();
    let entry = table.check(raw, types.unwrap_or(&[]), rights)?;
    Ok(Checked {
        value: raw,
        object: entry.object.clone(),
        rights: entry.rights,
    })
}

pub use crate::user::{Plain, UserIn, UserOut, bytes_of};
