//! Choosing the display mode.

use alloc::vec::Vec;
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

/// The protocol on the `console` handle when `open` finds it there, and
/// otherwise the one on the handle `first` gives, if any.
pub fn find<H, P>(
    console: Option<H>,
    mut open: impl FnMut(H) -> Option<P>,
    first: impl FnOnce() -> Option<H>,
) -> Option<P> {
    console
        .and_then(&mut open)
        .or_else(|| first().and_then(open))
}

/// A graphics output protocol, as `framebuffer` uses it.
pub trait Output {
    type Error;

    /// The current mode. Its index and size are not used.
    fn current_mode(&mut self) -> ModeInfo;

    /// All modes, each with its position as index and no size.
    fn modes(&mut self) -> Vec<ModeInfo>;

    /// Makes the mode with this index current.
    fn set_mode(&mut self, index: u32) -> Result<(), Self::Error>;

    /// The size in bytes of the current mode's framebuffer.
    fn framebuffer_size(&mut self) -> u64;

    /// The current mode's framebuffer, or `None` when its pixels are not
    /// linear and 32 bits wide.
    fn framebuffer(&mut self) -> Option<Mode>;
}

/// Picks the display mode of `out`, with the size `requested` on the
/// command line if any, and describes its framebuffer, or gives `None`
/// when there is no usable mode.
pub fn framebuffer<O: Output>(
    out: &mut O,
    requested: Option<(usize, usize)>,
) -> Result<Option<Framebuffer>, O::Error> {
    // The firmware has a framebuffer only for linear modes.
    let size = |out: &mut O, mode: &ModeInfo| mode.linear_32bit.then(|| out.framebuffer_size());
    let mut current = ModeInfo {
        index: u32::MAX,
        size: None,
        ..out.current_mode()
    };
    current.size = size(out, &current);
    let mut modes = out.modes();
    // The firmware reports a mode's framebuffer size only once it is
    // current, so a mode whose framebuffer turns out too small is left
    // out and the choice is made again.
    loop {
        match choose(current, &modes, requested) {
            Choice::Keep => break,
            Choice::Switch(index) if index == current.index => break,
            Choice::Switch(index) => {
                out.set_mode(index)?;
                let i = index as usize;
                modes[i].size = size(out, &modes[i]);
                current = modes[i];
            }
            Choice::Unusable => return Ok(None),
        }
    }
    Ok(out.framebuffer().map(describe))
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

/// The location of a PCI function: its segment, bus, and device times 8
/// plus function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciLocation {
    pub segment: u16,
    pub bus: u8,
    pub devfn: u8,
}

impl PciLocation {
    /// The location from the four numbers `GetLocation` gives, or `None`
    /// when one is out of range.
    pub fn new(segment: usize, bus: usize, device: usize, function: usize) -> Option<Self> {
        if device >= 32 || function >= 8 {
            return None;
        }
        Some(PciLocation {
            segment: u16::try_from(segment).ok()?,
            bus: u8::try_from(bus).ok()?,
            devfn: (device * 8 + function) as u8,
        })
    }
}

/// The one location among `found`, which may repeat it, or `None` when
/// there is none or more than one.
pub fn only_location(found: impl IntoIterator<Item = PciLocation>) -> Option<PciLocation> {
    let mut found = found.into_iter();
    let first = found.next()?;
    found.all(|other| other == first).then_some(first)
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

    #[test]
    fn finds_the_console_protocol_first() {
        // Handles 1 and 2 have the protocol, handle 3 has not.
        let open = |h: u32| (h != 3).then_some(h * 10);
        let first = || Some(2);
        assert_eq!(find(Some(1), open, || panic!("not asked")), Some(10));
        assert_eq!(find(Some(3), open, first), Some(20));
        assert_eq!(find(None, open, first), Some(20));
        assert_eq!(find(Some(3), open, || None), None);
        assert_eq!(find(None, open, || Some(3)), None);
    }

    /// An output whose modes carry their real framebuffer size.
    struct Fake {
        modes: Vec<ModeInfo>,
        current: usize,
        set: Vec<u32>,
        refuse: bool,
    }

    impl Fake {
        fn new(modes: &[ModeInfo], current: usize) -> Fake {
            Fake {
                modes: modes.to_vec(),
                current,
                set: Vec::new(),
                refuse: false,
            }
        }
    }

    impl Output for Fake {
        type Error = &'static str;

        fn current_mode(&mut self) -> ModeInfo {
            self.modes[self.current]
        }

        fn modes(&mut self) -> Vec<ModeInfo> {
            self.modes
                .iter()
                .map(|m| ModeInfo { size: None, ..*m })
                .collect()
        }

        fn set_mode(&mut self, index: u32) -> Result<(), &'static str> {
            if self.refuse {
                return Err("refused");
            }
            self.set.push(index);
            self.current = index as usize;
            Ok(())
        }

        fn framebuffer_size(&mut self) -> u64 {
            let m = self.modes[self.current];
            assert!(m.linear_32bit, "no framebuffer");
            m.size.unwrap()
        }

        fn framebuffer(&mut self) -> Option<Mode> {
            let m = self.modes[self.current];
            m.linear_32bit.then(|| Mode {
                phys_base: 0x8000_0000,
                size: m.size.unwrap(),
                width: m.width as u32,
                height: m.height as u32,
                stride: m.stride as u32,
                pixels: Pixels::Bgr,
            })
        }
    }

    /// `MODES` with framebuffers that fit.
    fn sized() -> Vec<ModeInfo> {
        MODES
            .iter()
            .map(|m| ModeInfo {
                size: Some((m.stride * m.height * 4) as u64),
                ..*m
            })
            .collect()
    }

    fn width(fb: Option<Framebuffer>) -> Option<u32> {
        fb.map(|fb| fb.width)
    }

    #[test]
    fn keeps_a_usable_current_mode_without_switching() {
        let mut out = Fake::new(&sized(), 0);
        assert_eq!(width(framebuffer(&mut out, None).unwrap()), Some(800));
        assert!(out.set.is_empty());
        let fb = framebuffer(&mut out, None).unwrap().unwrap();
        assert_eq!((fb.phys_base, fb.size), (0x8000_0000, 800 * 600 * 4));
    }

    #[test]
    fn switches_to_the_requested_mode() {
        let mut out = Fake::new(&sized(), 0);
        let fb = framebuffer(&mut out, Some((1280, 1024))).unwrap();
        assert_eq!((width(fb), out.set.as_slice()), (Some(1280), &[3][..]));
        // A requested mode that is not usable keeps the current one.
        let mut out = Fake::new(&sized(), 0);
        let fb = framebuffer(&mut out, Some((2560, 1440))).unwrap();
        assert_eq!((width(fb), out.set.len()), (Some(800), 0));
    }

    #[test]
    fn switches_from_an_unusable_mode_to_the_largest() {
        let mut out = Fake::new(&sized(), 2);
        let fb = framebuffer(&mut out, None).unwrap();
        assert_eq!((width(fb), out.set.as_slice()), (Some(1920), &[1][..]));
    }

    #[test]
    fn leaves_out_a_mode_whose_framebuffer_is_too_small() {
        let mut modes = sized();
        modes[1].size = Some(4);
        let mut out = Fake::new(&modes, 2);
        let fb = framebuffer(&mut out, None).unwrap();
        assert_eq!((width(fb), out.set.as_slice()), (Some(1280), &[1, 3][..]));
    }

    #[test]
    fn has_no_framebuffer_without_a_usable_mode() {
        let mut modes = sized();
        for m in &mut modes[1..] {
            m.size = Some(0);
        }
        modes[0].linear_32bit = false;
        let mut out = Fake::new(&modes, 0);
        assert_eq!(framebuffer(&mut out, None), Ok(None));
        assert_eq!(out.set, [1, 3]);
        let mut out = Fake::new(&modes[2..3], 0);
        assert_eq!(framebuffer(&mut out, Some((2560, 1440))), Ok(None));
        assert!(out.set.is_empty());
    }

    #[test]
    fn gives_the_error_of_a_failed_switch() {
        let mut out = Fake::new(&sized(), 2);
        out.refuse = true;
        assert_eq!(framebuffer(&mut out, None), Err("refused"));
    }

    #[test]
    fn takes_a_pci_location_in_range() {
        let at = PciLocation::new(1, 3, 2, 1);
        assert_eq!(
            at,
            Some(PciLocation {
                segment: 1,
                bus: 3,
                devfn: 17
            })
        );
        assert_eq!(PciLocation::new(0x1_0000, 0, 0, 0), None);
        assert_eq!(PciLocation::new(0, 256, 0, 0), None);
        assert_eq!(PciLocation::new(0, 0, 32, 0), None);
        assert_eq!(PciLocation::new(0, 0, 0, 8), None);
    }

    #[test]
    fn takes_only_a_single_location() {
        let a = PciLocation::new(0, 3, 0, 0).unwrap();
        let b = PciLocation::new(0, 4, 0, 0).unwrap();
        assert_eq!(only_location([]), None);
        assert_eq!(only_location([a]), Some(a));
        assert_eq!(only_location([a, a]), Some(a));
        assert_eq!(only_location([a, b]), None);
    }
}
