//! The ABI level of the system that this C library is part of.

/// The ABI level of this release.
const ABI_LEVEL: u32 = 0;

#[unsafe(no_mangle)]
extern "C" fn mikros_abi_level() -> u32 {
    ABI_LEVEL
}
