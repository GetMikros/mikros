//! The monotonic clock: its source and the TSC frequency.

/// The ACPI PM timer frequency.
pub const PM_TIMER_HZ: u64 = 3_579_545;
/// The PIT input frequency.
pub const PIT_HZ: u64 = 1_193_182;

/// The CPUID values that report TSC properties.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TscLeaves {
    pub basic_max: u32,
    pub ext_max: u32,
    /// Leaf 0x15: EAX, EBX, ECX.
    pub leaf15: [u32; 3],
    /// Leaf 0x16, EAX: the base frequency in MHz.
    pub leaf16_eax: u32,
    /// Leaf 0x8000_0007, EDX.
    pub ext7_edx: u32,
}

impl TscLeaves {
    pub fn read(cpuid: impl Fn(u32) -> [u32; 4]) -> TscLeaves {
        let basic_max = cpuid(0)[0];
        let ext_max = cpuid(0x8000_0000)[0];
        let leaf15 = if basic_max >= 0x15 {
            let [a, b, c, _] = cpuid(0x15);
            [a, b, c]
        } else {
            [0; 3]
        };
        let leaf16_eax = if basic_max >= 0x16 { cpuid(0x16)[0] } else { 0 };
        let ext7_edx = if ext_max >= 0x8000_0007 {
            cpuid(0x8000_0007)[3]
        } else {
            0
        };
        TscLeaves {
            basic_max,
            ext_max,
            leaf15,
            leaf16_eax,
            ext7_edx,
        }
    }

    /// Whether the TSC runs at a constant rate in every power state.
    pub fn invariant(&self) -> bool {
        self.ext7_edx & (1 << 8) != 0
    }

    /// The TSC frequency CPUID reports: from leaf 0x15 when it gives the
    /// crystal frequency and the ratio, and otherwise the base frequency
    /// of leaf 0x16.
    pub fn reported_hz(&self) -> Option<u64> {
        let [denominator, numerator, crystal] = self.leaf15.map(u64::from);
        if denominator != 0 && numerator != 0 && crystal != 0 {
            return Some(crystal * numerator / denominator);
        }
        (self.leaf16_eax != 0).then(|| u64::from(self.leaf16_eax) * 1_000_000)
    }
}

/// The frequency of a counter that advanced `ticks` while a reference
/// running at `reference_hz` advanced `reference_ticks`.
pub fn measured_hz(ticks: u64, reference_ticks: u64, reference_hz: u64) -> u64 {
    (u128::from(ticks) * u128::from(reference_hz) / u128::from(reference_ticks.max(1))) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(leaf15: [u32; 3], leaf16_eax: u32, invariant: bool) -> TscLeaves {
        TscLeaves {
            basic_max: 0x16,
            ext_max: 0x8000_0008,
            leaf15,
            leaf16_eax,
            ext7_edx: if invariant { 1 << 8 } else { 0 },
        }
    }

    #[test]
    fn takes_the_frequency_from_cpuid() {
        let l = leaves([2, 188, 24_000_000], 2200, true);
        assert!(l.invariant());
        assert_eq!(l.reported_hz(), Some(2_256_000_000));
        let l = leaves([2, 188, 0], 2200, false);
        assert!(!l.invariant());
        assert_eq!(l.reported_hz(), Some(2_200_000_000));
        assert_eq!(leaves([0; 3], 0, true).reported_hz(), None);
    }

    #[test]
    fn reads_only_reported_leaves() {
        let l = TscLeaves::read(|leaf| match leaf {
            0 => [0xd, 0, 0, 0],
            0x8000_0000 => [0x8000_0001, 0, 0, 0],
            _ => [u32::MAX; 4],
        });
        assert_eq!((l.leaf15, l.leaf16_eax, l.ext7_edx), ([0; 3], 0, 0));
    }

    #[test]
    fn measures_against_a_reference() {
        assert_eq!(measured_hz(30_000_000, 35_795, PM_TIMER_HZ), 3_000_037_714);
        assert_eq!(measured_hz(2_000_000, 11_931, PIT_HZ), 200_013_745);
    }
}
