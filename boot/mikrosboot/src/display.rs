//! Choosing the display mode.

/// A graphics mode as reported by the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    pub index: u32,
    pub width: usize,
    pub height: usize,
    /// Whether the mode has a linear framebuffer with 32-bit pixels.
    pub linear_32bit: bool,
}

/// What to do about the display mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Keep the current mode.
    Keep,
    /// Switch to the mode with this index.
    Switch(u32),
    /// No mode can be used, so there is no framebuffer.
    Unusable,
}

/// Picks the usable mode of the `requested` size when there is one. Otherwise
/// keeps the current mode when it is usable, and otherwise picks the usable
/// mode with the most pixels, the first of equally large ones.
pub fn choose(current: ModeInfo, modes: &[ModeInfo], requested: Option<(usize, usize)>) -> Choice {
    let usable = || modes.iter().filter(|m| m.linear_32bit);
    if let Some((width, height)) = requested
        && let Some(mode) = usable().find(|m| (m.width, m.height) == (width, height))
    {
        return Choice::Switch(mode.index);
    }
    if current.linear_32bit {
        return Choice::Keep;
    }
    let mut best: Option<&ModeInfo> = None;
    for mode in usable() {
        if best.is_none_or(|b| mode.width * mode.height > b.width * b.height) {
            best = Some(mode);
        }
    }
    best.map_or(Choice::Unusable, |m| Choice::Switch(m.index))
}

/// Whether pixels with these channel masks are 32 bits wide, which is the
/// case when the highest bit of any mask is bit 31.
pub fn bitmask_is_32bit(red: u32, green: u32, blue: u32, reserved: u32) -> bool {
    (red | green | blue | reserved) & 0x8000_0000 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(index: u32, width: usize, height: usize, linear_32bit: bool) -> ModeInfo {
        ModeInfo {
            index,
            width,
            height,
            linear_32bit,
        }
    }

    const MODES: [ModeInfo; 4] = [
        ModeInfo {
            index: 0,
            width: 800,
            height: 600,
            linear_32bit: true,
        },
        ModeInfo {
            index: 1,
            width: 1920,
            height: 1080,
            linear_32bit: true,
        },
        ModeInfo {
            index: 2,
            width: 2560,
            height: 1440,
            linear_32bit: false,
        },
        ModeInfo {
            index: 3,
            width: 1280,
            height: 1024,
            linear_32bit: true,
        },
    ];

    #[test]
    fn switches_to_a_requested_usable_mode() {
        let current = mode(u32::MAX, 800, 600, true);
        assert_eq!(
            choose(current, &MODES, Some((1280, 1024))),
            Choice::Switch(3)
        );
        assert_eq!(
            choose(current, &MODES, Some((2560, 1440))),
            Choice::Keep,
            "not usable"
        );
        assert_eq!(
            choose(current, &MODES, Some((640, 480))),
            Choice::Keep,
            "missing"
        );
    }

    #[test]
    fn keeps_a_usable_mode_and_otherwise_takes_the_largest() {
        assert_eq!(
            choose(mode(u32::MAX, 1, 1, true), &MODES, None),
            Choice::Keep
        );
        assert_eq!(
            choose(mode(u32::MAX, 1, 1, false), &MODES, None),
            Choice::Switch(1)
        );
        let none = [mode(0, 800, 600, false)];
        assert_eq!(
            choose(mode(u32::MAX, 1, 1, false), &none, None),
            Choice::Unusable
        );
    }

    #[test]
    fn recognizes_32bit_masks() {
        assert!(bitmask_is_32bit(0xff, 0xff00, 0xff_0000, 0xff00_0000));
        assert!(!bitmask_is_32bit(0xf800, 0x07e0, 0x001f, 0));
    }
}
