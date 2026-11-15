//! Calling the firmware's runtime services, which the bootloader moved
//! into the runtime window. The kernel calls `GetNextVariableName` once
//! while it starts when the command line holds `uefi_selftest`, and
//! `GetVariable` and `SetVariable` for the firmware variable calls.

use crate::context::ExtendedState;
use crate::log::log;
use crate::{memory, space};
use boot_protocol::x86_64::{RUNTIME_WINDOW_BASE, RUNTIME_WINDOW_SIZE};
use boot_protocol::{BootInfo, Kind, PAGE_SIZE, flags};
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

type GetNextVariableName =
    unsafe extern "efiapi" fn(size: *mut usize, name: *mut u16, guid: *mut [u8; 16]) -> usize;
type GetVariable = unsafe extern "efiapi" fn(
    name: *const u16,
    guid: *const [u8; 16],
    attributes: *mut u32,
    size: *mut usize,
    data: *mut u8,
) -> usize;
type SetVariable = unsafe extern "efiapi" fn(
    name: *const u16,
    guid: *const [u8; 16],
    attributes: u32,
    size: usize,
    data: *const u8,
) -> usize;

/// The variable services in the runtime window, when the bootloader
/// moved them there.
struct Variables {
    get: Option<GetVariable>,
    set: Option<SetVariable>,
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
    // SAFETY: the addresses are in runtime code that the window maps
    // executable, where the firmware moved these services.
    variables.get =
        service(info, GET_VARIABLE).map(|f| unsafe { core::mem::transmute::<u64, GetVariable>(f) });
    // SAFETY: as above.
    variables.set =
        service(info, SET_VARIABLE).map(|f| unsafe { core::mem::transmute::<u64, SetVariable>(f) });
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

/// Calls `GetVariable` for the Mikros variable `name`, a null-terminated
/// UCS-2 string, with `data` as buffer. Gives the status, the size the
/// firmware wrote or needs, and the attributes.
pub fn get_variable(name: &[u16], data: &mut [u8]) -> Result<(usize, usize, u32), Error> {
    let variables = CALLS.lock();
    let get = variables.get.ok_or(Error::NotSupported)?;
    let mut size = data.len();
    let mut attributes = 0;
    in_firmware_state(|| {
        // SAFETY: the firmware's runtime services run at their window
        // addresses, which the kernel's tables map, with interrupts
        // disabled and no other call running. The name is terminated,
        // and `data` has room for `size` bytes; the pointers are to the
        // kernel's own memory.
        let status = unsafe {
            get(
                name.as_ptr(),
                &VENDOR,
                &mut attributes,
                &mut size,
                data.as_mut_ptr(),
            )
        };
        (status, size, attributes)
    })
}

/// Calls `SetVariable` for the Mikros variable `name`, a null-terminated
/// UCS-2 string, with `attributes` and `data`, and gives its status.
pub fn set_variable(name: &[u16], attributes: u32, data: &[u8]) -> Result<usize, Error> {
    let variables = CALLS.lock();
    let set = variables.set.ok_or(Error::NotSupported)?;
    in_firmware_state(|| {
        // SAFETY: as for `get_variable`; `data` holds `data.len()` bytes.
        unsafe {
            set(
                name.as_ptr(),
                &VENDOR,
                attributes,
                data.len(),
                data.as_ptr(),
            )
        }
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
    let function = service(info, GET_NEXT_VARIABLE_NAME).map(|f| {
        // SAFETY: the address is in runtime code that the window maps
        // executable, where the firmware moved GetNextVariableName.
        unsafe { core::mem::transmute::<u64, GetNextVariableName>(f) }
    });
    let Some(function) = function else {
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
