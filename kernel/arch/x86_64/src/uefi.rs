//! Calling the firmware's runtime services, which the bootloader moved
//! into the runtime window. The kernel calls `GetNextVariableName` once
//! while it starts when the command line holds `uefi_selftest`, and
//! `GetVariable` and `SetVariable` for the firmware variable calls.
//!
//! A page fault or general protection fault in the window during a call
//! ends the call. The kernel then removes the window's mappings and makes
//! no more calls until the next boot.

use crate::context::ExtendedState;
use crate::idt::{Frame, GENERAL_PROTECTION, PAGE_FAULT};
use crate::log::log;
use crate::{memory, percpu, space};
use boot_protocol::x86_64::{RUNTIME_WINDOW_BASE, RUNTIME_WINDOW_SIZE};
use boot_protocol::{BootInfo, Kind, PAGE_SIZE, flags};
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use kernel_core::firmware::VENDOR;
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The command line word that asks for the call.
const SELFTEST: &[u8] = b"uefi_selftest";
/// The offset of `RuntimeServices` in the EFI system table.
const RUNTIME_SERVICES: u64 = 0x58;
/// The offsets of `GetVariable`, `GetNextVariableName` and `SetVariable`
/// in the runtime services table.
const GET_VARIABLE: u64 = 0x48;
const GET_NEXT_VARIABLE_NAME: u64 = 0x50;
const SET_VARIABLE: u64 = 0x58;
/// The size of the buffer for a variable name, in UCS-2 characters.
const NAME_CHARS: usize = 256;
/// The flags with which `recover` resumes a call that faulted: interrupts
/// disabled, as in all kernel code, until `firmware_resume` restores the
/// caller's.
const RESUME_FLAGS: u64 = 2;

/// The variable services in the runtime window, as their addresses, when
/// the bootloader moved them there and no call has faulted.
struct Variables {
    get: Option<u64>,
    set: Option<u64>,
}

/// Held for each call, so that no two run at the same time on any
/// processor.
static CALLS: SpinLock<Variables> = SpinLock::new(
    kernel_core::lock_order::UEFI_CALLS,
    Variables {
        get: None,
        set: None,
    },
);

/// The stack pointer at which `firmware_resume` ends the call that runs,
/// which `firmware_call` stores; zero while no call runs, and once
/// `recover` ended the call.
static RESUME: AtomicU64 = AtomicU64::new(0);
/// The number of the processor that makes the call.
static CALLER: AtomicUsize = AtomicUsize::new(usize::MAX);
/// The fault that ended a call: its vector, error code and instruction
/// address, and the address it touched for a page fault.
static FAULT: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

// Calls the efiapi function at RDI with the five arguments at RSI and
// returns its status. Before the call it stores in `RESUME` the stack
// pointer at which `firmware_resume` restores the flags and the registers
// that the caller keeps, and returns to the caller; `recover` resumes
// there when the call faults. The stack below holds the home of the four
// register arguments and the fifth argument, and stays 16-byte aligned
// for the call.
global_asm!(
    ".global firmware_call",
    ".global firmware_resume",
    "firmware_call:",
    "push rbp",
    "mov rbp, rsp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "pushfq",
    "mov [rip + {resume}], rsp",
    "sub rsp, 48",
    "mov rax, rdi",
    "mov r10, rsi",
    "mov rcx, [r10]",
    "mov rdx, [r10 + 8]",
    "mov r8, [r10 + 16]",
    "mov r9, [r10 + 24]",
    "mov r11, [r10 + 32]",
    "mov [rsp + 32], r11",
    "call rax",
    "add rsp, 48",
    "firmware_resume:",
    "popfq",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    resume = sym RESUME,
);

unsafe extern "sysv64" {
    fn firmware_call(function: u64, args: *const [u64; 5]) -> usize;
    static firmware_resume: u8;
}

/// The address in the runtime window of the `len` bytes at physical
/// address `phys`, when they lie in one range of `kinds` that the window
/// maps.
fn in_window(info: &BootInfo, kinds: &[Kind], phys: u64, len: u64) -> Option<u64> {
    let end = phys.checked_add(len)?;
    if end > RUNTIME_WINDOW_SIZE {
        return None;
    }
    memory::memory_map(info)
        .any(|e| {
            let range_end = e.phys_start + e.page_count * PAGE_SIZE;
            kinds.iter().any(|&k| k as u32 == e.kind) && e.phys_start <= phys && end <= range_end
        })
        .then_some(RUNTIME_WINDOW_BASE + phys)
}

/// The physical address of `virt` in the runtime window.
fn window_phys(virt: u64) -> Option<u64> {
    virt.checked_sub(RUNTIME_WINDOW_BASE)
        .filter(|&phys| phys < RUNTIME_WINDOW_SIZE)
}

/// The address of the runtime service at `offset` of the runtime
/// services table, read from the system table, when the tables and the
/// function lie in ranges the window maps.
fn service(info: &BootInfo, offset: u64) -> Option<u64> {
    let data = [
        Kind::FirmwareRuntime,
        Kind::FirmwareRuntimeCode,
        Kind::FirmwareRuntimeMixed,
    ];
    let table = in_window(
        info,
        &data,
        info.uefi_system_table_phys,
        RUNTIME_SERVICES + 8,
    )?;
    // SAFETY: the window maps the system table's range readable.
    let services = unsafe { ((table + RUNTIME_SERVICES) as *const u64).read_unaligned() };
    let services = in_window(info, &data, window_phys(services)?, offset + 8)?;
    // SAFETY: the window maps the runtime services table's range readable.
    let function = unsafe { ((services + offset) as *const u64).read_unaligned() };
    in_window(
        info,
        &[Kind::FirmwareRuntimeCode, Kind::FirmwareRuntimeMixed],
        window_phys(function)?,
        1,
    )?;
    Some(function)
}

/// Finds the variable services, when the bootloader moved the runtime
/// services into the runtime window. Runs on the boot processor while the
/// kernel starts, before boot memory is reclaimed.
pub fn init(info: &BootInfo) {
    if info.flags & flags::RUNTIME_SERVICES_MAPPED == 0 {
        return;
    }
    let mut variables = CALLS.lock();
    variables.get = service(info, GET_VARIABLE);
    variables.set = service(info, SET_VARIABLE);
    if variables.get.is_none() || variables.set.is_none() {
        log!(
            Warning,
            "uefi: the variable services are not in the runtime window"
        );
    }
}

/// Runs `call` with the extended state that UEFI expects, its initial
/// state, and puts the running thread's back afterwards: the firmware may
/// change the registers that the calling convention does not keep.
fn in_firmware_state<R>(call: impl FnOnce() -> R) -> Result<R, Error> {
    let thread = ExtendedState::new().ok_or(Error::NoMemory)?;
    let initial = ExtendedState::new().ok_or(Error::NoMemory)?;
    thread.save();
    initial.restore();
    let result = call();
    thread.restore();
    Ok(result)
}

/// Calls the runtime service at `function` with `args`, with `CALLS` held
/// as `variables`. Gives the status it returns, or `None` when it faulted;
/// the variable services are then gone from `variables`, and the caller
/// calls `end` once it no longer holds `CALLS`.
///
/// # Safety
///
/// `function` is the address of a runtime service in the window, and
/// `args` are arguments it accepts, pointing to the kernel's memory.
unsafe fn call(
    variables: &mut Variables,
    function: u64,
    args: [u64; 5],
) -> Result<Option<usize>, Error> {
    let status = in_firmware_state(|| {
        CALLER.store(percpu::index() as usize, Ordering::Relaxed);
        // SAFETY: the firmware's runtime services run at their window
        // addresses, which the kernel's tables map, with interrupts
        // disabled and no other call running, which `CALLS` ensures; the
        // caller vouches for the function and its arguments.
        let status = unsafe { firmware_call(function, &args) };
        let returned = RESUME.swap(0, Ordering::Relaxed) != 0;
        CALLER.store(usize::MAX, Ordering::Relaxed);
        returned.then_some(status)
    })?;
    if status.is_none() {
        variables.get = None;
        variables.set = None;
    }
    Ok(status)
}

/// Ends a page fault or general protection fault in kernel mode at an
/// instruction in the runtime window, while this processor is in a call,
/// by resuming the call's caller at `firmware_resume` with the registers
/// in `frame`. Returns whether the fault was one.
pub fn recover(frame: &mut Frame) -> bool {
    let vector = frame.vector as u8;
    let resume = RESUME.load(Ordering::Relaxed);
    if !matches!(vector, PAGE_FAULT | GENERAL_PROTECTION)
        || frame.cs & 3 != 0
        || resume == 0
        || CALLER.load(Ordering::Relaxed) != percpu::index() as usize
        || window_phys(frame.rip).is_none()
    {
        return false;
    }
    let address = match vector {
        PAGE_FAULT => crate::cpu::read_cr2(),
        _ => 0,
    };
    let fault = [frame.vector, frame.error, frame.rip, address];
    for (slot, value) in FAULT.iter().zip(fault) {
        slot.store(value, Ordering::Relaxed);
    }
    RESUME.store(0, Ordering::Relaxed);
    frame.rip = (&raw const firmware_resume) as u64;
    frame.rsp = resume;
    frame.rflags = RESUME_FLAGS;
    frame.rax = 0;
    frame.cs = u64::from(crate::gdt::KERNEL_CODE);
    frame.ss = u64::from(crate::gdt::KERNEL_DATA);
    true
}

/// Ends the runtime services after a call to `service` faulted: removes
/// the runtime window's mappings and logs the fault. `CALLS` must not be
/// held; the variable services are already gone from it, so that no other
/// call reaches the window meanwhile.
fn end(service: &str) {
    space::unmap_runtime_window();
    let [vector, error, rip, address] = FAULT.each_ref().map(|v| v.load(Ordering::Relaxed));
    log!(
        Error,
        "uefi: {service} raised a {} at {rip:#x}, error code {error:#x}, address {address:#x}; the runtime services are off until the next boot",
        crate::idt::exception_name(vector as u8)
    );
}

/// Calls `GetVariable` for the Mikros variable `name`, a null-terminated
/// UCS-2 string, with `data` as buffer. Gives the status, the size the
/// firmware wrote or needs, and the attributes. Fails with `Io` when the
/// call faulted.
pub fn get_variable(name: &[u16], data: &mut [u8]) -> Result<(usize, usize, u32), Error> {
    let mut variables = CALLS.lock();
    let get = variables.get.ok_or(Error::NotSupported)?;
    let vendor = VENDOR;
    let mut size = data.len();
    let mut attributes = 0u32;
    let args = [
        name.as_ptr() as u64,
        (&raw const vendor) as u64,
        (&raw mut attributes) as u64,
        (&raw mut size) as u64,
        data.as_mut_ptr() as u64,
    ];
    // SAFETY: `get` is `GetVariable` in the window. The name is
    // terminated, and `data` has room for `size` bytes; the pointers are
    // to the kernel's own memory.
    let status = unsafe { call(&mut variables, get, args) }?;
    drop(variables);
    match status {
        Some(status) => Ok((status, size, attributes)),
        None => {
            end("GetVariable");
            Err(Error::Io)
        }
    }
}

/// Calls `SetVariable` for the Mikros variable `name`, a null-terminated
/// UCS-2 string, with `attributes` and `data`, and gives its status.
/// Fails with `Io` when the call faulted.
pub fn set_variable(name: &[u16], attributes: u32, data: &[u8]) -> Result<usize, Error> {
    let mut variables = CALLS.lock();
    let set = variables.set.ok_or(Error::NotSupported)?;
    let vendor = VENDOR;
    let args = [
        name.as_ptr() as u64,
        (&raw const vendor) as u64,
        u64::from(attributes),
        data.len() as u64,
        data.as_ptr() as u64,
    ];
    // SAFETY: `set` is `SetVariable` in the window; as for
    // `get_variable`, and `data` holds `data.len()` bytes.
    let status = unsafe { call(&mut variables, set, args) }?;
    drop(variables);
    status.ok_or_else(|| {
        end("SetVariable");
        Error::Io
    })
}

/// Calls `GetNextVariableName` for the first variable and logs the status
/// it returns, when the command line holds `uefi_selftest` and the
/// runtime services were moved into the runtime window. Runs on the boot
/// processor while the kernel starts, before boot memory is reclaimed.
pub fn selftest(info: &BootInfo) {
    let asked = {
        let cmdline = space::CMDLINE.lock();
        kernel_core::cmdline::has_word(&cmdline.0[..cmdline.1], SELFTEST)
    };
    if !asked || info.flags & flags::RUNTIME_SERVICES_MAPPED == 0 {
        return;
    }
    let Some(function) = service(info, GET_NEXT_VARIABLE_NAME) else {
        log!(
            Warning,
            "uefi: GetNextVariableName is not in the runtime window"
        );
        return;
    };
    let mut name = [0u16; NAME_CHARS];
    let mut size = size_of_val(&name);
    let mut guid = [0u8; 16];
    let args = [
        (&raw mut size) as u64,
        name.as_mut_ptr() as u64,
        (&raw mut guid) as u64,
        0,
        0,
    ];
    let mut variables = CALLS.lock();
    // SAFETY: `function` is `GetNextVariableName` in the window; the
    // pointers are to this stack frame, with `size` bytes for the name,
    // which starts as the empty name to ask for the first variable.
    let status = unsafe { call(&mut variables, function, args) };
    drop(variables);
    match status {
        Ok(Some(status)) => log!(Info, "uefi: GetNextVariableName returned {status:#x}"),
        Ok(None) => end("GetNextVariableName"),
        Err(e) => log!(Warning, "uefi: calling GetNextVariableName: {e:?}"),
    }
}
