//! The colours of cells as XRGB8888 pixels: xterm's 256 indexed colours
//! and the console's default foreground and background.

use crate::emulator::cell::Color;

pub const FOREGROUND: u32 = 0x00D0_D0D0;
pub const BACKGROUND: u32 = 0x0000_0000;

/// The 16 basic colours, as xterm shows them.
const BASIC: [u32; 16] = [
    0x000000, 0xCD0000, 0x00CD00, 0xCDCD00, 0x0000EE, 0xCD00CD, 0x00CDCD, 0xE5E5E5, 0x7F7F7F,
    0xFF0000, 0x00FF00, 0xFFFF00, 0x5C5CFF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
];

/// The pixel of indexed colour `n`: the basic colours, a 6×6×6 cube and
/// 24 greys.
pub fn indexed(n: u8) -> u32 {
    match n {
        0..=15 => BASIC[usize::from(n)],
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * u32::from(v) };
            level(n / 36) << 16 | level(n / 6 % 6) << 8 | level(n % 6)
        }
        _ => {
            let grey = 8 + 10 * u32::from(n - 232);
            grey << 16 | grey << 8 | grey
        }
    }
}

/// The pixel of `color`, with `default` for the default colour.
pub fn pixel(color: Color, default: u32) -> u32 {
    match color {
        Color::Default => default,
        Color::Indexed(n) => indexed(n),
        Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
    }
}

/// `fg` over `bg` with `coverage` out of 255.
pub fn blend(fg: u32, bg: u32, coverage: u8) -> u32 {
    let a = u32::from(coverage);
    let mix = |shift: u32| {
        let f = fg >> shift & 0xFF;
        let b = bg >> shift & 0xFF;
        ((f * a + b * (255 - a)) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}
