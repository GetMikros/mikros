//! The Mikros boot manager, a UEFI application.

#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(target_os = "uefi")]
mod uefi_main {
    use uefi::prelude::*;

    /// Marks this image as the Mikros boot manager, so that installers can
    /// tell it apart from other images at the removable-media fallback path.
    #[used]
    #[unsafe(link_section = ".mikros")]
    static MIKROS_SECTION: [u8; 16] = *b"mikros-bootmenu\0";

    #[entry]
    fn main() -> Status {
        if uefi::helpers::init().is_err() {
            return Status::ABORTED;
        }
        Status::SUCCESS
    }
}

/// The boot manager only runs under UEFI. This lets the crate build on the
/// host so that `cargo test` covers its library.
#[cfg(not(target_os = "uefi"))]
fn main() {}
