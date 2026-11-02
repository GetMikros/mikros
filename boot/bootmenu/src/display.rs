//! Choosing the display mode.

/// A graphics mode as reported by the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    pub index: u32,
    pub width: usize,
    pub height: usize,
    pub kind: Kind,
}

/// How a mode's pixels can be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A linear framebuffer with 32-bit pixels.
    Linear32,
    /// No framebuffer; pixels are only reachable through `Blt()`.
    BltOnly,
    /// Any other pixel format, which is not used.
    Other,
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

/// Keeps the current mode when it is linear 32-bit, and otherwise picks the
/// linear 32-bit mode with the most pixels. Without one, does the same with
/// Blt-only modes. Of modes with equally many pixels, the first is taken.
pub fn choose(current: ModeInfo, modes: impl IntoIterator<Item = ModeInfo> + Clone) -> Choice {
    [Kind::Linear32, Kind::BltOnly]
        .into_iter()
        .find_map(|kind| choose_kind(kind, current, modes.clone()))
        .unwrap_or(Choice::Unusable)
}

/// `choose` restricted to modes of `kind`, or `None` when there is none.
fn choose_kind(
    kind: Kind,
    current: ModeInfo,
    modes: impl IntoIterator<Item = ModeInfo>,
) -> Option<Choice> {
    if current.kind == kind {
        return Some(Choice::Keep);
    }
    let mut best: Option<ModeInfo> = None;
    for mode in modes.into_iter().filter(|m| m.kind == kind) {
        if best.is_none_or(|b| mode.width * mode.height > b.width * b.height) {
            best = Some(mode);
        }
    }
    best.map(|m| Choice::Switch(m.index))
}

/// Whether pixels with these channel masks are 32 bits wide, which is the
/// case when the highest bit of any mask is bit 31.
pub fn bitmask_is_32bit(red: u32, green: u32, blue: u32, reserved: u32) -> bool {
    (red | green | blue | reserved) & 0x8000_0000 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(index: u32, width: usize, height: usize, kind: Kind) -> ModeInfo {
        ModeInfo {
            index,
            width,
            height,
            kind,
        }
    }

    #[test]
    fn keeps_a_usable_current_mode() {
        let current = mode(2, 800, 600, Kind::Linear32);
        let modes = [mode(0, 1920, 1080, Kind::Linear32), current];
        assert_eq!(choose(current, modes), Choice::Keep);
    }

    #[test]
    fn switches_to_the_largest_usable_mode() {
        let current = mode(0, 640, 480, Kind::Other);
        let modes = [
            current,
            mode(1, 1024, 768, Kind::Linear32),
            mode(2, 2560, 1440, Kind::Other),
            mode(3, 1920, 1080, Kind::Linear32),
            mode(4, 1080, 1920, Kind::Linear32),
        ];
        assert_eq!(choose(current, modes), Choice::Switch(3));
    }

    #[test]
    fn reports_when_no_mode_is_usable() {
        let current = mode(0, 640, 480, Kind::Other);
        assert_eq!(choose(current, [current]), Choice::Unusable);
    }

    #[test]
    fn prefers_linear_modes_to_blt_only_ones() {
        let current = mode(0, 2560, 1440, Kind::BltOnly);
        let modes = [current, mode(1, 800, 600, Kind::Linear32)];
        assert_eq!(choose(current, modes), Choice::Switch(1));
    }

    #[test]
    fn keeps_a_blt_only_current_mode_without_linear_modes() {
        let current = mode(1, 800, 600, Kind::BltOnly);
        let modes = [mode(0, 1920, 1080, Kind::BltOnly), current];
        assert_eq!(choose(current, modes), Choice::Keep);
    }

    #[test]
    fn switches_to_the_largest_blt_only_mode_without_linear_modes() {
        let current = mode(0, 640, 480, Kind::Other);
        let modes = [
            current,
            mode(1, 1024, 768, Kind::BltOnly),
            mode(2, 2560, 1440, Kind::Other),
            mode(3, 1280, 800, Kind::BltOnly),
        ];
        assert_eq!(choose(current, modes), Choice::Switch(3));
    }

    #[test]
    fn recognizes_32bit_bitmasks() {
        assert!(bitmask_is_32bit(0xff0000, 0xff00, 0xff, 0xff00_0000));
        assert!(bitmask_is_32bit(0xffc0_0000, 0x003f_f000, 0xffc, 0));
        assert!(!bitmask_is_32bit(0xf800, 0x07e0, 0x001f, 0));
        assert!(!bitmask_is_32bit(0xff0000, 0xff00, 0xff, 0));
    }
}
