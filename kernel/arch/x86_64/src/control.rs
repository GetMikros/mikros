//! The control registers, EFER and XCR0.

pub const CR0_PE: u64 = 1 << 0;
pub const CR0_MP: u64 = 1 << 1;
pub const CR0_EM: u64 = 1 << 2;
pub const CR0_TS: u64 = 1 << 3;
pub const CR0_NE: u64 = 1 << 5;
pub const CR0_WP: u64 = 1 << 16;
pub const CR0_NW: u64 = 1 << 29;
pub const CR0_CD: u64 = 1 << 30;
pub const CR0_PG: u64 = 1 << 31;

pub const CR4_VME: u64 = 1 << 0;
pub const CR4_PVI: u64 = 1 << 1;
pub const CR4_TSD: u64 = 1 << 2;
pub const CR4_PAE: u64 = 1 << 5;
pub const CR4_PGE: u64 = 1 << 7;
pub const CR4_OSFXSR: u64 = 1 << 9;
pub const CR4_OSXMMEXCPT: u64 = 1 << 10;
pub const CR4_UMIP: u64 = 1 << 11;
pub const CR4_LA57: u64 = 1 << 12;
pub const CR4_FSGSBASE: u64 = 1 << 16;
pub const CR4_PCIDE: u64 = 1 << 17;
pub const CR4_OSXSAVE: u64 = 1 << 18;
pub const CR4_SMEP: u64 = 1 << 20;
pub const CR4_SMAP: u64 = 1 << 21;
pub const CR4_PKE: u64 = 1 << 22;

pub const EFER_SCE: u64 = 1 << 0;
pub const EFER_LME: u64 = 1 << 8;
pub const EFER_NXE: u64 = 1 << 11;

pub const XCR0_X87: u64 = 1 << 0;
pub const XCR0_SSE: u64 = 1 << 1;
pub const XCR0_AVX: u64 = 1 << 2;
pub const XCR0_OPMASK: u64 = 1 << 5;
pub const XCR0_ZMM_HI256: u64 = 1 << 6;
pub const XCR0_HI16_ZMM: u64 = 1 << 7;

/// The optional features that have a CR4 bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Optional {
    pub smep: bool,
    pub smap: bool,
    pub umip: bool,
    pub pcid: bool,
    pub fsgsbase: bool,
    pub avx512f: bool,
}

impl Optional {
    /// Reads the features with `cpuid`, which gives EAX, EBX, ECX and EDX
    /// for a leaf and subleaf.
    pub fn read(cpuid: impl Fn(u32, u32) -> [u32; 4]) -> Optional {
        let basic_max = cpuid(0, 0)[0];
        let [_, _, ecx1, _] = cpuid(1, 0);
        let [_, ebx7, ecx7, _] = if basic_max >= 7 { cpuid(7, 0) } else { [0; 4] };
        let bit = |r: u32, n: u32| r & (1 << n) != 0;
        Optional {
            smep: bit(ebx7, 7),
            smap: bit(ebx7, 20),
            umip: bit(ecx7, 2),
            pcid: bit(ecx1, 17),
            fsgsbase: bit(ebx7, 0),
            avx512f: bit(ebx7, 16),
        }
    }
}

/// CR0 from its current value.
pub fn cr0(current: u64) -> u64 {
    (current | CR0_PE | CR0_MP | CR0_NE | CR0_WP | CR0_PG) & !(CR0_EM | CR0_TS | CR0_CD | CR0_NW)
}

/// CR4 from its current value and the optional features.
pub fn cr4(current: u64, f: Optional) -> u64 {
    let mut cr4 = current | CR4_PAE | CR4_PGE | CR4_OSFXSR | CR4_OSXMMEXCPT | CR4_OSXSAVE;
    for (reported, bit) in [
        (f.smep, CR4_SMEP),
        (f.smap, CR4_SMAP),
        (f.umip, CR4_UMIP),
        (f.pcid, CR4_PCIDE),
        (f.fsgsbase, CR4_FSGSBASE),
    ] {
        if reported {
            cr4 |= bit;
        }
    }
    cr4 & !(CR4_TSD | CR4_VME | CR4_PVI | CR4_PKE)
}

/// EFER from its current value.
pub fn efer(current: u64) -> u64 {
    current | EFER_SCE | EFER_LME | EFER_NXE
}

/// The state components to enable in XCR0.
pub fn xcr0(f: Optional) -> u64 {
    let base = XCR0_X87 | XCR0_SSE | XCR0_AVX;
    if f.avx512f {
        base | XCR0_OPMASK | XCR0_ZMM_HI256 | XCR0_HI16_ZMM
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_and_clears_cr0_bits() {
        let v = cr0(CR0_PE | CR0_EM | CR0_TS | CR0_CD | CR0_NW | CR0_PG | 1 << 18);
        assert_eq!(v, CR0_PE | CR0_MP | CR0_NE | CR0_WP | CR0_PG | 1 << 18);
    }

    #[test]
    fn sets_cr4_bits_for_reported_features() {
        let required = CR4_PAE | CR4_PGE | CR4_OSFXSR | CR4_OSXMMEXCPT | CR4_OSXSAVE;
        let none = Optional::default();
        assert_eq!(cr4(CR4_TSD | CR4_VME | CR4_PVI | CR4_PKE, none), required);
        let all = Optional {
            smep: true,
            smap: true,
            umip: true,
            pcid: true,
            fsgsbase: true,
            avx512f: true,
        };
        let optional = CR4_SMEP | CR4_SMAP | CR4_UMIP | CR4_PCIDE | CR4_FSGSBASE;
        assert_eq!(cr4(0, all), required | optional);
    }

    #[test]
    fn enables_state_components() {
        assert_eq!(xcr0(Optional::default()), 0b111);
        let avx512 = Optional {
            avx512f: true,
            ..Optional::default()
        };
        assert_eq!(xcr0(avx512), 0b1110_0111);
        assert_eq!(efer(0), EFER_SCE | EFER_LME | EFER_NXE);
    }

    #[test]
    fn reads_the_features() {
        let f = Optional::read(|leaf, _| match leaf {
            0 => [7, 0, 0, 0],
            1 => [0, 0, 1 << 17, 0],
            7 => [0, 1 << 7 | 1 << 20 | 1 | 1 << 16, 1 << 2, 0],
            _ => [0; 4],
        });
        assert!(f.smep && f.smap && f.umip && f.pcid && f.fsgsbase && f.avx512f);
        let f = Optional::read(|leaf, _| {
            if leaf == 0 {
                [1, 0, 0, 0]
            } else {
                [u32::MAX; 4]
            }
        });
        assert_eq!(
            f,
            Optional {
                pcid: true,
                ..Optional::default()
            }
        );
    }
}
