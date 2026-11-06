//! Speculative-execution mitigations: which a processor needs, from what
//! CPUID and `IA32_ARCH_CAPABILITIES` report, and the flags that the
//! entry and exit code reads.

#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "none")]
const IA32_ARCH_CAPABILITIES: u32 = 0x10A;
#[cfg(target_os = "none")]
const IA32_SPEC_CTRL: u32 = 0x48;
#[cfg(target_os = "none")]
const IA32_PRED_CMD: u32 = 0x49;
#[cfg(target_os = "none")]
const IA32_EFER: u32 = 0xC000_0080;
/// `IA32_SPEC_CTRL.IBRS`, `IA32_PRED_CMD.IBPB` and `EFER.AIBRSE`.
#[cfg(target_os = "none")]
const SPEC_CTRL_IBRS: u64 = 1;
#[cfg(target_os = "none")]
const PRED_CMD_IBPB: u64 = 1;
#[cfg(target_os = "none")]
const EFER_AIBRSE: u64 = 1 << 21;
/// Bits of CPUID.(EAX=7,ECX=0):EDX.
const MD_CLEAR: u32 = 1 << 10;
const IBRS_IBPB: u32 = 1 << 26;
const ARCH_CAPABILITIES: u32 = 1 << 29;
/// Bits of CPUID.(EAX=80000008h):EBX.
const AMD_IBPB: u32 = 1 << 12;
const AMD_IBRS: u32 = 1 << 14;
/// Bit of CPUID.(EAX=80000021h):EAX.
const AUTOMATIC_IBRS: u32 = 1 << 8;
/// Bits of `IA32_ARCH_CAPABILITIES`.
const RDCL_NO: u64 = 1;
const IBRS_ALL: u64 = 1 << 1;
const MDS_NO: u64 = 1 << 5;

/// The processor vendors that the mitigations tell apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    #[default]
    Other,
}

impl Vendor {
    /// The vendor that CPUID leaf 0 names in EBX, EDX and ECX.
    pub fn of(ebx: u32, edx: u32, ecx: u32) -> Vendor {
        let mut name = [0u8; 12];
        name[..4].copy_from_slice(&ebx.to_le_bytes());
        name[4..8].copy_from_slice(&edx.to_le_bytes());
        name[8..].copy_from_slice(&ecx.to_le_bytes());
        match &name {
            b"GenuineIntel" => Vendor::Intel,
            b"AuthenticAMD" => Vendor::Amd,
            _ => Vendor::Other,
        }
    }
}

/// What a processor reports that the mitigations depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reported {
    pub vendor: Vendor,
    /// CPUID.(EAX=7,ECX=0):EDX, or 0 without leaf 7.
    pub leaf7_edx: u32,
    /// CPUID.(EAX=80000008h):EBX, or 0 without that leaf.
    pub leaf_80000008_ebx: u32,
    /// CPUID.(EAX=80000021h):EAX, or 0 without that leaf.
    pub leaf_80000021_eax: u32,
    /// `IA32_ARCH_CAPABILITIES`, when CPUID reports it.
    pub arch_capabilities: Option<u64>,
}

/// How indirect branch predictions are restricted in the kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ibrs {
    /// Not at all: the processor has no IBRS.
    #[default]
    None,
    /// Enhanced IBRS, set once in `IA32_SPEC_CTRL` at start-up.
    Enhanced,
    /// AMD's Automatic IBRS, enabled once with `EFER.AIBRSE`.
    Automatic,
    /// IBRS, set on every entry to the kernel and cleared before every
    /// return to user mode.
    OnEntry,
}

/// The mitigations a processor needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needed {
    /// Kernel page-table isolation.
    pub isolation: bool,
    pub ibrs: Ibrs,
    /// An indirect branch prediction barrier on every switch to a thread
    /// of another process.
    pub ibpb: bool,
    /// Clearing CPU buffers with `VERW` before every return to user mode.
    pub verw: bool,
}

/// The mitigations that a processor reporting `reported` needs.
pub fn needed(reported: Reported) -> Needed {
    let capabilities = reported.arch_capabilities.unwrap_or(0);
    let ibrs = if capabilities & IBRS_ALL != 0 {
        Ibrs::Enhanced
    } else if reported.vendor == Vendor::Amd && reported.leaf_80000021_eax & AUTOMATIC_IBRS != 0 {
        Ibrs::Automatic
    } else if reported.leaf7_edx & IBRS_IBPB != 0 || reported.leaf_80000008_ebx & AMD_IBRS != 0 {
        Ibrs::OnEntry
    } else {
        Ibrs::None
    };
    Needed {
        isolation: reported.vendor == Vendor::Intel && capabilities & RDCL_NO == 0,
        ibrs,
        ibpb: reported.leaf7_edx & IBRS_IBPB != 0 || reported.leaf_80000008_ebx & AMD_IBPB != 0,
        verw: reported.leaf7_edx & MD_CLEAR != 0 && capabilities & MDS_NO == 0,
    }
}

/// Whether returns to user mode clear CPU buffers with `VERW` first, as
/// some processor needs.
#[cfg(target_os = "none")]
#[unsafe(link_section = ".data.entry")]
pub static VERW: AtomicBool = AtomicBool::new(false);

/// The operand of `VERW`: a writable data segment's selector.
#[cfg(target_os = "none")]
#[unsafe(link_section = ".data.entry")]
pub static VERW_SELECTOR: u16 = crate::gdt::KERNEL_DATA;

/// Whether every entry to the kernel sets `IA32_SPEC_CTRL.IBRS` and every
/// return to user mode clears it.
#[cfg(target_os = "none")]
#[unsafe(link_section = ".data.entry")]
pub static IBRS_ON_ENTRY: AtomicBool = AtomicBool::new(false);

/// Whether the kernel runs user mode on page tables of its own, as some
/// processor needs: kernel page-table isolation.
#[cfg(target_os = "none")]
#[unsafe(link_section = ".data.entry")]
pub static ISOLATION: AtomicBool = AtomicBool::new(false);

/// Whether the kernel uses kernel page-table isolation. `init` decides it
/// on the boot processor before the kernel's page tables are built.
#[cfg(target_os = "none")]
pub fn isolation() -> bool {
    ISOLATION.load(Ordering::Relaxed)
}

/// Whether switches to another process's thread need a barrier.
#[cfg(target_os = "none")]
static IBPB: AtomicBool = AtomicBool::new(false);

/// What this processor reports.
#[cfg(target_os = "none")]
fn reported() -> Reported {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let leaf0 = __cpuid(0);
    let leaf7_edx = match leaf0.eax >= 7 {
        true => __cpuid_count(7, 0).edx,
        false => 0,
    };
    let extended = __cpuid(0x8000_0000).eax;
    let leaf = |n: u32| (extended >= n).then(|| __cpuid(n));
    Reported {
        vendor: Vendor::of(leaf0.ebx, leaf0.edx, leaf0.ecx),
        leaf7_edx,
        leaf_80000008_ebx: leaf(0x8000_0008).map_or(0, |r| r.ebx),
        leaf_80000021_eax: leaf(0x8000_0021).map_or(0, |r| r.eax),
        arch_capabilities: (leaf7_edx & ARCH_CAPABILITIES != 0)
            // SAFETY: CPUID reports the MSR, and reading it changes nothing.
            .then(|| unsafe { crate::cpu::read_msr(IA32_ARCH_CAPABILITIES) }),
    }
}

/// Reads what this processor reports and turns on the mitigations it
/// needs. Runs on every processor as it starts, before it enters user
/// mode.
#[cfg(target_os = "none")]
pub fn init() {
    let needed = needed(reported());
    match needed.ibrs {
        // SAFETY: the processor reports IBRS_ALL, and the bit restricts
        // speculation and changes nothing else.
        Ibrs::Enhanced => unsafe {
            let spec_ctrl = crate::cpu::read_msr(IA32_SPEC_CTRL);
            crate::cpu::write_msr(IA32_SPEC_CTRL, spec_ctrl | SPEC_CTRL_IBRS);
        },
        // SAFETY: CPUID reports Automatic IBRS, and the bit restricts
        // speculation and changes nothing else.
        Ibrs::Automatic => unsafe {
            let efer = crate::cpu::read_msr(IA32_EFER);
            crate::cpu::write_msr(IA32_EFER, efer | EFER_AIBRSE);
        },
        Ibrs::OnEntry => IBRS_ON_ENTRY.store(true, Ordering::Relaxed),
        Ibrs::None => {}
    }
    if needed.isolation {
        ISOLATION.store(true, Ordering::Relaxed);
    }
    if needed.ibpb {
        IBPB.store(true, Ordering::Relaxed);
    }
    if needed.verw {
        VERW.store(true, Ordering::Relaxed);
    }
}

/// Keeps branch predictions made before from steering this processor's
/// indirect branches, when the processor needs it: for a switch to a
/// thread of another process.
#[cfg(target_os = "none")]
pub fn barrier() {
    if IBPB.load(Ordering::Relaxed) {
        // SAFETY: CPUID reports IBPB; the write only drops predictions.
        unsafe { crate::cpu::write_msr(IA32_PRED_CMD, PRED_CMD_IBPB) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intel(leaf7_edx: u32, arch_capabilities: Option<u64>) -> Needed {
        needed(Reported {
            vendor: Vendor::Intel,
            leaf7_edx,
            arch_capabilities,
            ..Reported::default()
        })
    }

    fn amd(leaf_80000008_ebx: u32, leaf_80000021_eax: u32) -> Needed {
        needed(Reported {
            vendor: Vendor::Amd,
            leaf_80000008_ebx,
            leaf_80000021_eax,
            ..Reported::default()
        })
    }

    #[test]
    fn names_the_vendors() {
        let words = |s: &[u8; 12]| {
            let w = |i: usize| u32::from_le_bytes(s[i..i + 4].try_into().unwrap());
            (w(0), w(4), w(8))
        };
        let (b, d, c) = words(b"GenuineIntel");
        assert_eq!(Vendor::of(b, d, c), Vendor::Intel);
        let (b, d, c) = words(b"AuthenticAMD");
        assert_eq!(Vendor::of(b, d, c), Vendor::Amd);
        let (b, d, c) = words(b"HygonGenuine");
        assert_eq!(Vendor::of(b, d, c), Vendor::Other);
    }

    #[test]
    fn clears_buffers_only_where_mds_applies() {
        assert!(intel(MD_CLEAR, None).verw);
        assert!(intel(MD_CLEAR | ARCH_CAPABILITIES, Some(0)).verw);
        assert!(!intel(MD_CLEAR | ARCH_CAPABILITIES, Some(MDS_NO)).verw);
        assert!(!intel(0, None).verw);
        assert!(!intel(ARCH_CAPABILITIES, Some(0)).verw);
    }

    #[test]
    fn prefers_enhanced_or_automatic_ibrs() {
        let both = IBRS_IBPB | ARCH_CAPABILITIES;
        assert_eq!(intel(both, Some(IBRS_ALL)).ibrs, Ibrs::Enhanced);
        assert_eq!(intel(both, Some(0)).ibrs, Ibrs::OnEntry);
        assert_eq!(intel(0, None).ibrs, Ibrs::None);
        assert_eq!(amd(AMD_IBRS, AUTOMATIC_IBRS).ibrs, Ibrs::Automatic);
        assert_eq!(amd(AMD_IBRS, 0).ibrs, Ibrs::OnEntry);
        assert_eq!(amd(0, 0).ibrs, Ibrs::None);
        // Automatic IBRS counts only on AMD processors.
        let other = needed(Reported {
            leaf_80000021_eax: AUTOMATIC_IBRS,
            ..Reported::default()
        });
        assert_eq!(other.ibrs, Ibrs::None);
    }

    #[test]
    fn isolates_only_intel_processors_without_rdcl_no() {
        assert!(intel(0, None).isolation);
        assert!(intel(ARCH_CAPABILITIES, Some(IBRS_ALL)).isolation);
        assert!(!intel(ARCH_CAPABILITIES, Some(RDCL_NO)).isolation);
        assert!(!amd(0, 0).isolation);
        assert!(!needed(Reported::default()).isolation);
    }

    #[test]
    fn takes_ibpb_from_either_leaf() {
        assert!(intel(IBRS_IBPB, None).ibpb);
        assert!(amd(AMD_IBPB, 0).ibpb);
        assert!(!amd(AMD_IBRS, 0).ibpb);
        assert!(!intel(0, None).ibpb);
    }
}
