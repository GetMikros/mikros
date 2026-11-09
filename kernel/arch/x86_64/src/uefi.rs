//! Calling the firmware's runtime services, which the bootloader moved
//! into the runtime window. The kernel calls `GetNextVariableName` once
//! while it starts when the command line holds `uefi_selftest`, and
//! makes no other call.

use crate::log::log;
use crate::{memory, space};
use boot_protocol::x86_64::{RUNTIME_WINDOW_BASE, RUNTIME_WINDOW_SIZE};
use boot_protocol::{BootInfo, Kind, PAGE_SIZE, flags};
use kernel_core::sync::SpinLock;

/// The command line word that asks for the call.
const SELFTEST: &[u8] = b"uefi_selftest";
/// The offset of `RuntimeServices` in the EFI system table.
const RUNTIME_SERVICES: u64 = 0x58;
/// The offset of `GetNextVariableName` in the runtime services table.
const GET_NEXT_VARIABLE_NAME: u64 = 0x50;
/// The size of the buffer for a variable name, in UCS-2 characters.
const NAME_CHARS: usize = 256;

type GetNextVariableName =
    unsafe extern "efiapi" fn(size: *mut usize, name: *mut u16, guid: *mut [u8; 16]) -> usize;

/// Held for each call, so that no two run at the same time on any
/// processor.
static CALLS: SpinLock<()> = SpinLock::new(kernel_core::lock_order::UEFI_CALLS, ());

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

/// `GetNextVariableName` in the runtime window, read from the system
/// table, when the tables and the function lie in ranges the window
/// maps.
fn get_next_variable_name(info: &BootInfo) -> Option<GetNextVariableName> {
    let data = [Kind::FirmwareRuntime, Kind::FirmwareRuntimeCode];
    let table = in_window(
        info,
        &data,
        info.uefi_system_table_phys,
        RUNTIME_SERVICES + 8,
    )?;
    // SAFETY: the window maps the system table's range readable.
    let services = unsafe { ((table + RUNTIME_SERVICES) as *const u64).read_unaligned() };
    let services = in_window(
        info,
        &data,
        window_phys(services)?,
        GET_NEXT_VARIABLE_NAME + 8,
    )?;
    // SAFETY: the window maps the runtime services table's range readable.
    let function = unsafe { ((services + GET_NEXT_VARIABLE_NAME) as *const u64).read_unaligned() };
    in_window(
        info,
        &[Kind::FirmwareRuntimeCode],
        window_phys(function)?,
        1,
    )?;
    // SAFETY: the address is in runtime code that the window maps
    // executable, where the firmware moved GetNextVariableName.
    Some(unsafe { core::mem::transmute::<u64, GetNextVariableName>(function) })
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
    let Some(function) = get_next_variable_name(info) else {
        log!(
            Warning,
            "uefi: GetNextVariableName is not in the runtime window"
        );
        return;
    };
    let mut name = [0u16; NAME_CHARS];
    let mut size = size_of_val(&name);
    let mut guid = [0u8; 16];
    let status = {
        let _call = CALLS.lock();
        // SAFETY: the firmware's runtime services run at their window
        // addresses, which the kernel's tables map, with interrupts
        // disabled and no other call running; the pointers are to this
        // stack frame, with `size` bytes for the name, which starts as
        // the empty name to ask for the first variable.
        unsafe { function(&mut size, name.as_mut_ptr(), &mut guid) }
    };
    log!(Info, "uefi: GetNextVariableName returned {status:#x}");
}
