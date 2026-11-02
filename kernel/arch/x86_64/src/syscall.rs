//! The system call entry and return path, and the glue between the
//! dispatch generated from `kernel/syscalls.yaml` and the handlers.

use crate::aspace::{USER_END, USER_START};
use crate::cpu::write_msr;
use crate::gdt::{KERNEL_CODE, USER_CODE, USER_DATA};
use crate::percpu::{KERNEL_STACK_OFFSET, USER_RSP_OFFSET};
use crate::usercopy::{copy_in, copy_out};
use alloc::sync::Arc;
use core::arch::global_asm;
use core::marker::PhantomData;
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
const RFLAGS_AC: u64 = 1 << 18;

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
    "mov rdi, rsp",
    "call {handler}",
    // POP leaves the flags alone, so the test decides the path below.
    "test eax, eax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10",
    "pop r9", "pop r8", "pop rbp", "pop rdi", "pop rsi", "pop rdx",
    "pop rcx", "pop rbx", "pop rax",
    "jnz 2f",
    "mov rcx, [rsp]",
    "mov r11, [rsp + 16]",
    "mov rsp, [rsp + 24]",
    "swapgs",
    "sysretq",
    "2:",
    "swapgs",
    "iretq",
    user_rsp = const USER_RSP_OFFSET,
    kernel_stack = const KERNEL_STACK_OFFSET,
    user_data = const USER_DATA as u64 | 3,
    user_code = const USER_CODE as u64 | 3,
    handler = sym syscall_handler,
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

extern "sysv64" fn syscall_handler(frame: &mut Frame) -> u64 {
    let args = [
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9, frame.r12, frame.r13,
    ];
    let number = frame.rax;
    frame.rax = generated::dispatch(number, &args) as u64;
    if frame.rax == Error::Interrupted as i64 as u64
        && let Some(thread) = crate::sched::current()
    {
        thread.set_interrupted(number);
    }
    crate::sched::preempt_user();
    let reported = before_user_return(frame);
    crate::sched::exit_if_dead();
    // SYSRET to an address outside the user range would fault in ring 0,
    // and after a report the handler may have changed RCX and R11, which
    // SYSRET overwrites.
    u64::from(reported || !(USER_START..USER_END).contains(&frame.rip))
}

/// Makes the reports that wait for the calling thread to return to user
/// mode, with its registers in the layout of an interrupt frame while the
/// handler may read and change them. Returns whether it reported any.
fn before_user_return(frame: &mut Frame) -> bool {
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
    if !crate::exception::before_user_return(&mut regs) {
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

/// Values that can be copied to and from user memory as their bytes.
///
/// # Safety
///
/// Every bit pattern must be a valid value, and the type must have no
/// padding.
pub unsafe trait Plain: Copy {}

// SAFETY: integers have no padding and no invalid bit patterns.
unsafe impl Plain for u8 {}
unsafe impl Plain for u32 {}
unsafe impl Plain for u64 {}
unsafe impl Plain for i32 {}
unsafe impl Plain for i64 {}

/// A user address the kernel reads a `T` or an array of them from.
#[allow(dead_code, reason = "no call in the table reads user memory yet")]
pub struct UserIn<T>(u64, PhantomData<T>);

#[allow(dead_code, reason = "no call in the table reads user memory yet")]
impl<T: Plain> UserIn<T> {
    pub fn new(addr: u64) -> Self {
        UserIn(addr, PhantomData)
    }

    /// Whether `len` values fit at the address inside the user range.
    pub fn fits(&self, len: u64) -> bool {
        let bytes = len.checked_mul(size_of::<T>() as u64);
        bytes.is_some_and(|b| b == 0 || crate::aspace::in_user_range(self.0, b))
    }

    /// Whether the address is zero.
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// Copies `dst.len()` values in once.
    pub fn read_into(&self, dst: &mut [T]) -> Result<(), Error> {
        // SAFETY: T is Plain, so its bytes may be written freely.
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(dst.as_mut_ptr().cast::<u8>(), size_of_val(dst))
        };
        copy_in(bytes, self.0)
    }

    /// Copies the value in once.
    pub fn read(&self) -> Result<T, Error> {
        let mut value = core::mem::MaybeUninit::<T>::zeroed();
        // SAFETY: T is Plain, so its bytes may be written freely.
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(value.as_mut_ptr().cast::<u8>(), size_of::<T>())
        };
        copy_in(bytes, self.0)?;
        // SAFETY: every bit pattern is a valid T.
        Ok(unsafe { value.assume_init() })
    }
}

#[allow(dead_code, reason = "no call in the table reads user bytes yet")]
impl UserIn<u8> {
    /// Copies `dst.len()` bytes in once.
    pub fn read_bytes(&self, dst: &mut [u8]) -> Result<(), Error> {
        copy_in(dst, self.0)
    }

    /// The address `offset` bytes further on.
    pub fn at(&self, offset: u64) -> Self {
        UserIn::new(self.0.wrapping_add(offset))
    }
}

/// A user address the kernel writes a `T` or an array of them to.
pub struct UserOut<T>(u64, PhantomData<T>);

impl<T: Plain> UserOut<T> {
    pub fn new(addr: u64) -> Self {
        UserOut(addr, PhantomData)
    }

    /// Whether the address is zero.
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// Whether `len` values fit at the address inside the user range.
    pub fn fits(&self, len: u64) -> bool {
        let bytes = len.checked_mul(size_of::<T>() as u64);
        bytes.is_some_and(|b| b == 0 || crate::aspace::in_user_range(self.0, b))
    }

    /// Copies `values` out.
    pub fn write_all(&self, values: &[T]) -> Result<(), Error> {
        // SAFETY: T is Plain, so it has no padding to leak.
        let bytes = unsafe {
            core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values))
        };
        copy_out(self.0, bytes)
    }

    pub fn write(&self, value: T) -> Result<(), Error> {
        // SAFETY: T is Plain, so it has no padding to leak.
        let bytes =
            unsafe { core::slice::from_raw_parts((&raw const value).cast::<u8>(), size_of::<T>()) };
        copy_out(self.0, bytes)
    }
}

impl UserOut<u8> {
    pub fn write_bytes(&self, src: &[u8]) -> Result<(), Error> {
        copy_out(self.0, src)
    }

    /// The address `offset` bytes further on.
    pub fn at(&self, offset: u64) -> Self {
        UserOut::new(self.0.wrapping_add(offset))
    }
}

/// A user address the kernel reads from and writes back to.
#[allow(dead_code, reason = "no call in the table has an inout argument yet")]
pub struct UserInOut<T>(u64, PhantomData<T>);

#[allow(dead_code, reason = "no call in the table has an inout argument yet")]
impl<T: Plain> UserInOut<T> {
    pub fn new(addr: u64) -> Self {
        UserInOut(addr, PhantomData)
    }

    pub fn reader(&self) -> UserIn<T> {
        UserIn::new(self.0)
    }

    pub fn writer(&self) -> UserOut<T> {
        UserOut::new(self.0)
    }
}
