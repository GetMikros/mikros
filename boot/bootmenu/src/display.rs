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
    /// No mode can be used.
    Unusable,
}

/// Keeps the current mode when it is usable, and otherwise picks the usable
/// mode with the most pixels. Of modes with equally many pixels, the first
/// is taken.
pub fn choose(current: ModeInfo, modes: impl IntoIterator<Item = ModeInfo>) -> Choice {
    if current.linear_32bit {
        return Choice::Keep;
    }
    let mut best: Option<ModeInfo> = None;
    for mode in modes.into_iter().filter(|m| m.linear_32bit) {
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

    #[test]
    fn keeps_a_usable_current_mode() {
        let current = mode(2, 800, 600, true);
        let modes = [mode(0, 1920, 1080, true), current];
        assert_eq!(choose(current, modes), Choice::Keep);
    }

    #[test]
    fn switches_to_the_largest_usable_mode() {
        let current = mode(0, 640, 480, false);
        let modes = [
            current,
            mode(1, 1024, 768, true),
            mode(2, 2560, 1440, false),
            mode(3, 1920, 1080, true),
            mode(4, 1080, 1920, true),
        ];
        assert_eq!(choose(current, modes), Choice::Switch(3));
    }

    #[test]
    fn reports_when_no_mode_is_usable() {
        let current = mode(0, 640, 480, false);
        assert_eq!(choose(current, [current]), Choice::Unusable);
    }

    #[test]
    fn recognizes_32bit_bitmasks() {
        assert!(bitmask_is_32bit(0xff0000, 0xff00, 0xff, 0xff00_0000));
        assert!(bitmask_is_32bit(0xffc0_0000, 0x003f_f000, 0xffc, 0));
        assert!(!bitmask_is_32bit(0xf800, 0x07e0, 0x001f, 0));
        assert!(!bitmask_is_32bit(0xff0000, 0xff00, 0xff, 0));
    }
}
