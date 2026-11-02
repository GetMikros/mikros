//! The Mikros bootloader, a UEFI application.

#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(target_os = "uefi")]
extern crate alloc;

#[cfg(target_os = "uefi")]
mod efi;

#[cfg(target_os = "uefi")]
mod uefi_main {
    use uefi::prelude::*;

    #[entry]
    fn main() -> Status {
        crate::efi::run()
    }
}

/// The bootloader only runs under UEFI. This lets the crate build on the
/// host so that `cargo test` covers its library.
#[cfg(not(target_os = "uefi"))]
fn main() {}
