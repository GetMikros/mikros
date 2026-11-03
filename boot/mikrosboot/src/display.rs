//! Choosing the display mode.

use boot_protocol::{Framebuffer, format};

/// A graphics mode as reported by the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    pub index: u32,
    pub width: usize,
    pub height: usize,
    /// Pixels per line in memory.
    pub stride: usize,
    /// The framebuffer's size in bytes, which the firmware reports only
    /// for the current mode; `None` until the mode has been current.
    pub size: Option<u64>,
    /// Whether the mode has a linear framebuffer with 32-bit pixels.
    pub linear_32bit: bool,
}

impl ModeInfo {
    /// Whether the mode can be used: a linear 32-bit mode with pixels,
    /// lines at least as long as its width, and, once its size is known,
    /// a framebuffer that holds all its lines.
    pub fn usable(&self) -> bool {
        let lines = (self.stride as u128) * (self.height as u128) * 4;
        self.linear_32bit
            && self.width > 0
            && self.height > 0
            && self.stride >= self.width
            && self.size.is_none_or(|size| u128::from(size) >= lines)
    }
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
/// mode with the most pixels, the first of equally large ones. A mode whose
/// size is not known yet counts as usable until it is.
pub fn choose(current: ModeInfo, modes: &[ModeInfo], requested: Option<(usize, usize)>) -> Choice {
    let usable = || modes.iter().filter(|m| m.usable());
    if let Some((width, height)) = requested
        && let Some(mode) = usable().find(|m| (m.width, m.height) == (width, height))
    {
        return Choice::Switch(mode.index);
    }
    if current.usable() {
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

/// The pixel layout of a usable mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pixels {
    Rgb,
    Bgr,
    /// Red, green, blue and reserved masks.
    Bitmask([u32; 4]),
}

/// A usable mode's framebuffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub phys_base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixels: Pixels,
}

/// The kernel's description of the framebuffer of `mode`.
pub fn describe(mode: Mode) -> Framebuffer {
    let (format, [red, green, blue, reserved]) = match mode.pixels {
        Pixels::Rgb => (format::RGB, [0xff, 0xff00, 0xff_0000, 0xff00_0000]),
        Pixels::Bgr => (format::BGR, [0xff_0000, 0xff00, 0xff, 0xff00_0000]),
        Pixels::Bitmask(masks) => (format::BITMASK, masks),
    };
    Framebuffer {
        phys_base: mode.phys_base,
        size: mode.size,
        width: mode.width,
        height: mode.height,
        stride: mode.stride,
        format,
        red_mask: red,
        green_mask: green,
        blue_mask: blue,
        reserved_mask: reserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(index: u32, width: usize, height: usize, linear_32bit: bool) -> ModeInfo {
        ModeInfo {
            index,
            width,
            height,
            stride: width,
            size: None,
            linear_32bit,
        }
    }

    const MODES: [ModeInfo; 4] = [
        ModeInfo {
            index: 0,
            width: 800,
            height: 600,
            stride: 800,
            size: None,
            linear_32bit: true,
        },
        ModeInfo {
            index: 1,
            width: 1920,
            height: 1080,
            stride: 1920,
            size: None,
            linear_32bit: true,
        },
        ModeInfo {
            index: 2,
            width: 2560,
            height: 1440,
            stride: 2560,
            size: None,
            linear_32bit: false,
        },
        ModeInfo {
            index: 3,
            width: 1280,
            height: 1024,
            stride: 1280,
            size: None,
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
    fn refuses_modes_whose_lines_do_not_fit() {
        let good = ModeInfo {
            stride: 832,
            size: Some(832 * 600 * 4),
            ..mode(0, 800, 600, true)
        };
        assert!(good.usable());
        assert!(!ModeInfo { width: 0, ..good }.usable());
        assert!(!ModeInfo { height: 0, ..good }.usable());
        assert!(
            !ModeInfo {
                stride: 799,
                ..good
            }
            .usable()
        );
        let small = ModeInfo {
            size: Some(832 * 600 * 4 - 1),
            ..good
        };
        assert!(!small.usable());
        assert!(
            ModeInfo {
                size: None,
                ..small
            }
            .usable()
        );
        // A too small current mode is not kept; a requested mode whose
        // framebuffer turned out too small is not chosen again.
        assert_eq!(choose(small, &MODES, None), Choice::Switch(1));
        let mut modes = MODES;
        modes[3].size = Some(0);
        assert_eq!(choose(small, &modes, Some((1280, 1024))), Choice::Switch(1));
    }

    #[test]
    fn recognizes_32bit_masks() {
        assert!(bitmask_is_32bit(0xff, 0xff00, 0xff_0000, 0xff00_0000));
        assert!(!bitmask_is_32bit(0xf800, 0x07e0, 0x001f, 0));
    }
    #[test]
    fn describes_the_pixel_layout() {
        let mode = |pixels| Mode {
            phys_base: 0x8000_0000,
            size: 0x40_0000,
            width: 1000,
            height: 1000,
            stride: 1024,
            pixels,
        };
        let rgb = describe(mode(Pixels::Rgb));
        assert_eq!(
            (rgb.format, rgb.red_mask, rgb.blue_mask),
            (1, 0xff, 0xff_0000)
        );
        assert_eq!((rgb.stride, rgb.phys_base), (1024, 0x8000_0000));
        let bgr = describe(mode(Pixels::Bgr));
        assert_eq!(
            (bgr.format, bgr.red_mask, bgr.blue_mask),
            (2, 0xff_0000, 0xff)
        );
        let masks = [0x3ff, 0xffc00, 0x3ff0_0000, 0xc000_0000];
        let bits = describe(mode(Pixels::Bitmask(masks)));
        assert_eq!(
            (
                bits.format,
                [
                    bits.red_mask,
                    bits.green_mask,
                    bits.blue_mask,
                    bits.reserved_mask
                ]
            ),
            (3, masks)
        );
    }
}
