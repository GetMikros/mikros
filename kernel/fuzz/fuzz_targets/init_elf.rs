//! Checks ELF images as the kernel does for init.

#![no_main]

use kernel_core::elf::{self, EM_X86_64, PF_W, PF_X};
use libfuzzer_sys::fuzz_target;

const USER: core::ops::Range<u64> = 0x1_0000..0x7FFF_FFFF_F000;

fuzz_target!(|data: &[u8]| {
    let Ok(image) = elf::parse(data, EM_X86_64, USER) else {
        return;
    };
    assert!(!image.segments.is_empty());
    for (i, s) in image.segments.iter().enumerate() {
        assert!(s.offset + s.filesz <= data.len() as u64);
        assert!(s.memsz >= s.filesz && s.memsz > 0);
        assert!(s.vaddr >= USER.start && s.vaddr + s.memsz <= USER.end);
        assert_ne!(s.flags & (PF_W | PF_X), PF_W | PF_X);
        for t in &image.segments[i + 1..] {
            let (a, b) = (s.pages(), t.pages());
            assert!(a.end <= b.start || b.end <= a.start);
        }
    }
    assert!(image.segments.iter().any(|s| s.flags & PF_X != 0
        && (s.vaddr..s.vaddr + s.memsz).contains(&image.entry)));
});
