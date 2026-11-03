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

/// The factor and shift that turn TSC ticks at `hz` into nanoseconds as
/// `ticks * mult >> shift`: the largest shift whose factor still fits in
/// 32 bits.
pub fn tsc_scale(hz: u64) -> (u32, u32) {
    let hz = u128::from(hz.max(1));
    let mut shift = 63;
    loop {
        let mult = ((1_000_000_000u128 << shift) + hz / 2) / hz;
        if mult <= u128::from(u32::MAX) || shift == 0 {
            return (mult.min(u128::from(u32::MAX)) as u32, shift);
        }
        shift -= 1;
    }
}

/// Nanoseconds for `ticks` with the scale of `tsc_scale`.
pub fn scaled_ns(ticks: u64, mult: u32, shift: u32) -> i64 {
    i64::try_from((u128::from(ticks) * u128::from(mult)) >> shift).unwrap_or(i64::MAX)
}

/// The counters the monotonic clock can come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Tsc,
    Hpet,
    PmTimer,
}

/// The source of the monotonic clock: the TSC when it is invariant,
/// otherwise the HPET when there is one, otherwise the ACPI PM timer when
/// the FADT describes one.
pub fn clock_source(invariant_tsc: bool, hpet: bool, pm_timer: bool) -> Option<Source> {
    if invariant_tsc {
        Some(Source::Tsc)
    } else if hpet {
        Some(Source::Hpet)
    } else if pm_timer {
        Some(Source::PmTimer)
    } else {
        None
    }
}

/// The size of the clock page header and the fields of `source` 1.
pub const CLOCK_PAGE_SIZE: usize = 0x28;

/// The start of the clock page. `tsc` gives `mult`, `shift`, `tsc_base`
/// and `ns_base` when the clock comes from the TSC.
pub fn clock_page(tsc: Option<(u32, u32, u64, i64)>) -> [u8; CLOCK_PAGE_SIZE] {
    let mut page = [0u8; CLOCK_PAGE_SIZE];
    page[0x04..0x08].copy_from_slice(&1u32.to_le_bytes());
    if let Some((mult, shift, tsc_base, ns_base)) = tsc {
        page[0x08..0x0C].copy_from_slice(&1u32.to_le_bytes());
        page[0x10..0x14].copy_from_slice(&mult.to_le_bytes());
        page[0x14..0x18].copy_from_slice(&shift.to_le_bytes());
        page[0x18..0x20].copy_from_slice(&tsc_base.to_le_bytes());
        page[0x20..0x28].copy_from_slice(&ns_base.to_le_bytes());
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_invariant_tsc_then_the_hpet_then_the_pm_timer() {
        use Source::*;
        let cases = [
            ((true, true, true), Some(Tsc)),
            ((true, false, false), Some(Tsc)),
            ((false, true, true), Some(Hpet)),
            ((false, true, false), Some(Hpet)),
            ((false, false, true), Some(PmTimer)),
            ((false, false, false), None),
        ];
        for ((tsc, hpet, pm), expected) in cases {
            assert_eq!(clock_source(tsc, hpet, pm), expected, "{tsc} {hpet} {pm}");
        }
    }

    #[test]
    fn scales_tsc_ticks() {
        for hz in [1_000_000, 2_400_000_000, 3_000_000_000, 5_200_000_000] {
            let (mult, shift) = tsc_scale(hz);
            assert!(mult > u32::MAX / 2, "{hz}: {mult}");
            // One hour of ticks lands within a microsecond.
            let hour = scaled_ns(hz * 3600, mult, shift);
            assert!((hour - 3_600_000_000_000).abs() < 1000, "{hz}: {hour}");
        }
        assert_eq!(tsc_scale(1_000_000_000), (1 << 31, 31));
        assert_eq!(scaled_ns(u64::MAX, u32::MAX, 0), i64::MAX);
    }

    #[test]
    fn lays_out_the_clock_page() {
        let page = clock_page(None);
        assert_eq!(
            page[..0x10],
            [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert!(page[0x10..].iter().all(|&b| b == 0));
        let page = clock_page(Some((7, 3, 0x1122, -5)));
        assert_eq!(page[0x08..0x0C], 1u32.to_le_bytes());
        assert_eq!(page[0x10..0x18], [7, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(page[0x18..0x20], 0x1122u64.to_le_bytes());
        assert_eq!(page[0x20..0x28], (-5i64).to_le_bytes());
    }

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
