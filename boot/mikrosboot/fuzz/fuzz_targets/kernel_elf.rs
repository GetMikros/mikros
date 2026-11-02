//! Checks kernel images as the bootloader does before loading them.

#![no_main]

use boot_protocol::x86_64::KERNEL_RANGE_START;
use libfuzzer_sys::fuzz_target;
use mikrosboot::elf;

fuzz_target!(|data: &[u8]| {
    let Ok(kernel) = elf::check(data, KERNEL_RANGE_START) else {
        return;
    };
    assert!(!kernel.segments.is_empty());
});
