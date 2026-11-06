//! Speculative-execution mitigations: which a processor needs, from what
//! CPUID and `IA32_ARCH_CAPABILITIES` report, and the flags that the
//! entry and exit code reads.

#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "none")]
const IA32_ARCH_CAPABILITIES: u32 = 0x10A;
/// Bits of CPUID.(EAX=7,ECX=0):EDX.
const MD_CLEAR: u32 = 1 << 10;
const ARCH_CAPABILITIES: u32 = 1 << 29;
/// Bits of `IA32_ARCH_CAPABILITIES`.
const MDS_NO: u64 = 1 << 5;

/// What a processor reports that the mitigations depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reported {
    /// CPUID.(EAX=7,ECX=0):EDX, or 0 without leaf 7.
    pub leaf7_edx: u32,
    /// `IA32_ARCH_CAPABILITIES`, when CPUID reports it.
    pub arch_capabilities: Option<u64>,
}

/// The mitigations a processor needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needed {
    /// Clearing CPU buffers with `VERW` before every return to user mode.
    pub verw: bool,
}

/// The mitigations that a processor reporting `reported` needs.
pub fn needed(reported: Reported) -> Needed {
    let capabilities = reported.arch_capabilities.unwrap_or(0);
    Needed {
        verw: reported.leaf7_edx & MD_CLEAR != 0 && capabilities & MDS_NO == 0,
    }
}

/// Whether returns to user mode clear CPU buffers with `VERW` first, as
/// some processor needs.
#[cfg(target_os = "none")]
pub static VERW: AtomicBool = AtomicBool::new(false);

/// The operand of `VERW`: a writable data segment's selector.
#[cfg(target_os = "none")]
pub static VERW_SELECTOR: u16 = crate::gdt::KERNEL_DATA;

/// Reads what this processor reports and turns on the mitigations it
/// needs. Runs on every processor as it starts, before it enters user
/// mode.
#[cfg(target_os = "none")]
pub fn init() {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let leaf7_edx = match __cpuid(0).eax >= 7 {
        true => __cpuid_count(7, 0).edx,
        false => 0,
    };
    let arch_capabilities = (leaf7_edx & ARCH_CAPABILITIES != 0)
        // SAFETY: CPUID reports the MSR, and reading it changes nothing.
        .then(|| unsafe { crate::cpu::read_msr(IA32_ARCH_CAPABILITIES) });
    let needed = needed(Reported {
        leaf7_edx,
        arch_capabilities,
    });
    if needed.verw {
        VERW.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_buffers_only_where_mds_applies() {
        let with = |leaf7_edx, arch_capabilities| {
            needed(Reported {
                leaf7_edx,
                arch_capabilities,
            })
        };
        assert!(with(MD_CLEAR, None).verw);
        assert!(with(MD_CLEAR | ARCH_CAPABILITIES, Some(0)).verw);
        assert!(!with(MD_CLEAR | ARCH_CAPABILITIES, Some(MDS_NO)).verw);
        assert!(!with(0, None).verw);
        assert!(!with(ARCH_CAPABILITIES, Some(0)).verw);
    }
}
