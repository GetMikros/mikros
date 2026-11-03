//! Processor state the kernel is entered with.

/// CR4.LA57, set while 5-level paging is enabled.
pub const CR4_LA57: u64 = 1 << 12;
/// CR0.WP, which makes read-only pages read-only for the kernel too.
pub const CR0_WP: u64 = 1 << 16;
/// The IA32_EFER model-specific register.
pub const IA32_EFER: u32 = 0xC000_0080;
/// IA32_EFER.NXE, which enables the no-execute page bit.
pub const EFER_NXE: u32 = 1 << 11;

/// Whether `cr4` has 5-level paging enabled. Paging cannot change levels
/// in long mode, so the kernel is not entered then.
pub fn five_level_paging(cr4: u64) -> bool {
    cr4 & CR4_LA57 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_5_level_paging_by_cr4_la57_only() {
        // PAE, PGE, OSFXSR and OSXMMEXCPT, as firmware leaves them.
        let cr4 = 0x6a0;
        assert!(!five_level_paging(cr4));
        assert!(five_level_paging(cr4 | 0x1000));
        assert!(!five_level_paging(!0x1000));
    }

    #[test]
    fn names_the_architectural_bits() {
        assert_eq!(CR0_WP, 0x1_0000);
        assert_eq!(EFER_NXE, 0x800);
    }
}
