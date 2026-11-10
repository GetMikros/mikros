//! XRGB8888 pixels in the framebuffer's pixel format.

/// The framebuffer formats: bytes red, green, blue, reserved; bytes
/// blue, green, red, reserved; and the layout of the masks.
pub const RGBX: u32 = 1;
pub const BGRX: u32 = 2;
pub const MASKS: u32 = 3;

/// One channel's place in a framebuffer pixel: its lowest bit and its
/// number of bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Channel {
    shift: u32,
    bits: u32,
}

impl Channel {
    fn of(mask: u32) -> Option<Channel> {
        if mask == 0 {
            return None;
        }
        let shift = mask.trailing_zeros();
        let bits = mask.count_ones();
        // The bits of a channel must be one run.
        let run = mask >> shift;
        if run & run.wrapping_add(1) != 0 {
            return None;
        }
        Some(Channel { shift, bits })
    }

    /// `value`, an 8-bit channel value, scaled to the channel's bits and
    /// put in place.
    fn place(self, value: u32) -> u32 {
        let scaled = if self.bits >= 8 {
            value << (self.bits - 8)
        } else {
            value >> (8 - self.bits)
        };
        scaled.checked_shl(self.shift).unwrap_or(0)
    }
}

/// Converts XRGB8888 pixels to one framebuffer pixel format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Converter {
    channels: [Channel; 3],
}

impl Converter {
    /// The converter for `format`, whose masks are `red`, `green` and
    /// `blue`. Formats 1 and 2 have fixed layouts; format 3 takes the
    /// masks, each of which must be one run of bits.
    pub fn new(format: u32, red: u32, green: u32, blue: u32) -> Option<Converter> {
        let (red, green, blue) = match format {
            RGBX => (0xFF, 0xFF00, 0xFF_0000),
            BGRX => (0xFF_0000, 0xFF00, 0xFF),
            MASKS => (red, green, blue),
            _ => return None,
        };
        Some(Converter {
            channels: [Channel::of(red)?, Channel::of(green)?, Channel::of(blue)?],
        })
    }

    /// Whether framebuffer pixels are XRGB8888 pixels, so that rows can
    /// be copied as they are.
    pub fn is_identity(&self) -> bool {
        self.channels
            == [
                Channel { shift: 16, bits: 8 },
                Channel { shift: 8, bits: 8 },
                Channel { shift: 0, bits: 8 },
            ]
    }

    /// `pixel`, an XRGB8888 value, in the framebuffer's format, with
    /// the reserved bits clear.
    pub fn convert(&self, pixel: u32) -> u32 {
        let [r, g, b] = self.channels;
        r.place(pixel >> 16 & 0xFF) | g.place(pixel >> 8 & 0xFF) | b.place(pixel & 0xFF)
    }
}
