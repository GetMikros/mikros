//! ACPICA for the ACPI server: the library that `cargo xtask acpica`
//! builds from the pinned release and the glue in `glue/`, and its
//! operating system layer, the `AcpiOs` functions, on mikros-rt's
//! threads, futexes and clock.
//!
//! The server gives the layer what only it has through a [`Host`]: the
//! address of the RSDP, and the resources of the device manager. ACPICA
//! may be called from the thread that called [`install`], and runs its
//! own work on threads that the layer starts.

#![no_std]

extern crate alloc;

pub mod devices;
mod interrupts;
mod osl;
mod pci;
mod physical;
mod ports;
pub mod power;
pub mod route;
mod sync;
mod threads;

pub use sync::{Guard, Locked};

use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

/// An ACPICA status, `AE_OK` or an exception.
pub type Status = u32;
pub const AE_OK: Status = 0x0000;
pub const AE_ERROR: Status = 0x0001;
pub const AE_NO_MEMORY: Status = 0x0004;
pub const AE_NOT_FOUND: Status = 0x0005;
pub const AE_NOT_EXIST: Status = 0x0006;
pub const AE_ALREADY_EXISTS: Status = 0x0007;
pub const AE_SUPPORT: Status = 0x000F;
pub const AE_LIMIT: Status = 0x0010;
pub const AE_TIME: Status = 0x0011;
pub const AE_BAD_PARAMETER: Status = 0x1001;

/// What the operating system layer needs of the ACPI server.
pub trait Host: Sync {
    /// The physical address of the RSDP.
    fn root_pointer(&self) -> u64;

    /// A resource of `kind` for `size` bytes or ports from `base`, shared
    /// with other drivers or not, from the device manager's
    /// `RequestResource`: None when it was refused.
    fn request(&self, kind: u32, base: u64, size: u64, shared: bool) -> Option<OwnedHandle>;
}

/// The server's host, which `install` sets once.
static HOST: AtomicPtr<&'static dyn Host> = AtomicPtr::new(core::ptr::null_mut());
/// The server's process, which the layer maps memory into and starts
/// threads in.
static PROCESS: AtomicU32 = AtomicU32::new(0);

/// Readies the operating system layer, for `host` and the process
/// `process`, and starts its threads: false when they did not start.
/// Must be called once, before anything else of the crate.
pub fn install(host: &'static dyn Host, process: sys::Handle) -> bool {
    let host = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(host));
    if HOST
        .compare_exchange(
            core::ptr::null_mut(),
            host,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        // SAFETY: the box was not stored.
        drop(unsafe { alloc::boxed::Box::from_raw(host) });
        return false;
    }
    PROCESS.store(process, Ordering::Release);
    threads::start(process)
}

fn host() -> &'static dyn Host {
    let host = HOST.load(Ordering::Acquire);
    assert!(
        !host.is_null(),
        "ACPICA called before mikros_acpica::install"
    );
    // SAFETY: `install` stored a box that is never freed.
    unsafe { *host }
}

fn process() -> sys::Handle {
    PROCESS.load(Ordering::Acquire)
}

unsafe extern "C" {
    fn mikros_acpi_tables() -> Status;
    fn mikros_acpi_enable() -> Status;
}

fn check(status: Status) -> Result<(), Status> {
    match status {
        AE_OK => Ok(()),
        status => Err(status),
    }
}

/// Starts ACPICA on the firmware's tables, enables ACPI mode and the
/// system control interrupt, initializes the namespace's objects, and
/// tells the firmware that interrupts are routed through I/O APICs: the
/// status of the step that failed when one did. The ECAM ranges and the
/// SCI are read from the tables before anything of ACPICA can need them,
/// and the resources of the power registers are obtained and kept.
pub fn start() -> Result<(), Status> {
    // SAFETY: `install` readied the layer; the glue takes no arguments.
    check(unsafe { mikros_acpi_tables() })?;
    power::hold_resources();
    pci::ranges();
    interrupts::read_sci()?;
    // SAFETY: as above.
    check(unsafe { mikros_acpi_enable() })?;
    route::use_apic()
}
