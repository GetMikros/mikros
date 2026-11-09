//! The fields of `BootInfo` that depend on what the firmware provided.

use boot_protocol::{Framebuffer, NANOS_PER_SECOND, flags};

/// `BootInfo`'s flags and the fields they describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Optional {
    pub flags: u32,
    pub framebuffer: Framebuffer,
    pub rng_seed: [u8; 32],
    pub boot_time: i64,
    pub boot_time_ns: i64,
}

/// The flags and fields for what the loader found: each flag is set
/// exactly when its value is there, and a field without one is zero. A
/// boot time whose nanoseconds do not fit an `i64` counts as none.
pub fn optional(
    framebuffer: Option<Framebuffer>,
    rng_seed: Option<[u8; 32]>,
    boot_time: Option<u64>,
) -> Optional {
    let boot_time = boot_time
        .and_then(|t| i64::try_from(t).ok())
        .and_then(|t| Some((t, t.checked_mul(NANOS_PER_SECOND)?)));
    let present = [
        (framebuffer.is_some(), flags::FRAMEBUFFER_PRESENT),
        (rng_seed.is_some(), flags::RNG_SEED_VALID),
        (boot_time.is_some(), flags::BOOT_TIME_VALID),
    ];
    Optional {
        flags: present
            .iter()
            .filter(|(there, _)| *there)
            .fold(0, |f, (_, flag)| f | flag),
        framebuffer: framebuffer.unwrap_or_default(),
        rng_seed: rng_seed.unwrap_or_default(),
        boot_time: boot_time.map_or(0, |(seconds, _)| seconds),
        boot_time_ns: boot_time.map_or(0, |(_, nanos)| nanos),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINED: u32 =
        flags::FRAMEBUFFER_PRESENT | flags::RNG_SEED_VALID | flags::BOOT_TIME_VALID;

    #[test]
    fn leaves_everything_zero_without_values() {
        let o = optional(None, None, None);
        assert_eq!(o.flags, 0);
        assert_eq!(o.framebuffer, Framebuffer::default());
        assert_eq!(o.rng_seed, [0; 32]);
        assert_eq!((o.boot_time, o.boot_time_ns), (0, 0));
    }

    #[test]
    fn sets_only_the_defined_flags_of_the_values_there() {
        let fb = Framebuffer {
            width: 1,
            ..Framebuffer::default()
        };
        let o = optional(Some(fb), Some([7; 32]), Some(1_700_000_000));
        assert_eq!(o.flags, DEFINED);
        assert_eq!(
            (o.framebuffer, o.rng_seed, o.boot_time, o.boot_time_ns),
            (fb, [7; 32], 1_700_000_000, 1_700_000_000_000_000_000)
        );
        let o = optional(None, Some([7; 32]), None);
        assert_eq!(o.flags, flags::RNG_SEED_VALID);
    }

    #[test]
    fn drops_a_boot_time_beyond_i64() {
        let o = optional(None, None, Some(u64::MAX));
        assert_eq!((o.flags, o.boot_time, o.boot_time_ns), (0, 0, 0));
    }

    #[test]
    fn drops_a_boot_time_whose_nanoseconds_do_not_fit() {
        let last = (i64::MAX / NANOS_PER_SECOND) as u64;
        let o = optional(None, None, Some(last));
        assert_eq!(o.flags, flags::BOOT_TIME_VALID);
        assert_eq!(o.boot_time_ns, last as i64 * NANOS_PER_SECOND);
        let o = optional(None, None, Some(last + 1));
        assert_eq!((o.flags, o.boot_time, o.boot_time_ns), (0, 0, 0));
    }
}
