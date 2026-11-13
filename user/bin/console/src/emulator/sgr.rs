//! SGR, select graphic rendition: the attributes and colours that
//! characters are written with.

use crate::emulator::cell::{
    BLINK, BOLD, Cell, Color, DIM, INVISIBLE, ITALIC, REVERSE, STRIKE, UNDERLINE,
};
use alloc::vec::Vec;

/// SGR: sets `pen`'s attributes and colours from `params` and their
/// subparameters `sub`.
pub fn sgr(pen: &mut Cell, params: &[Option<u32>], sub: &[Vec<Option<u32>>]) {
    if params.is_empty() {
        *pen = Cell::default();
        return;
    }
    let mut i = 0;
    while i < params.len() {
        let p = params[i].unwrap_or(0);
        match p {
            0 => *pen = Cell::default(),
            1 => pen.attrs |= BOLD,
            2 => pen.attrs |= DIM,
            3 => pen.attrs |= ITALIC,
            4 => match sub[i].first().copied().flatten() {
                Some(0) => pen.attrs &= !UNDERLINE,
                _ => pen.attrs |= UNDERLINE,
            },
            5 | 6 => pen.attrs |= BLINK,
            7 => pen.attrs |= REVERSE,
            8 => pen.attrs |= INVISIBLE,
            9 => pen.attrs |= STRIKE,
            21 => pen.attrs |= UNDERLINE,
            22 => pen.attrs &= !(BOLD | DIM),
            23 => pen.attrs &= !ITALIC,
            24 => pen.attrs &= !UNDERLINE,
            25 => pen.attrs &= !BLINK,
            27 => pen.attrs &= !REVERSE,
            28 => pen.attrs &= !INVISIBLE,
            29 => pen.attrs &= !STRIKE,
            30..=37 => pen.fg = Color::Indexed((p - 30) as u8),
            39 => pen.fg = Color::Default,
            40..=47 => pen.bg = Color::Indexed((p - 40) as u8),
            49 => pen.bg = Color::Default,
            90..=97 => pen.fg = Color::Indexed((p - 90 + 8) as u8),
            100..=107 => pen.bg = Color::Indexed((p - 100 + 8) as u8),
            38 | 48 => {
                let (color, used) = extended(&params[i + 1..], &sub[i]);
                if let Some(color) = color {
                    if p == 38 {
                        pen.fg = color;
                    } else {
                        pen.bg = color;
                    }
                }
                i += used;
            }
            _ => {}
        }
        i += 1;
    }
}

/// The colour of an SGR 38 or 48, from its subparameters `sub` when it
/// has them, and otherwise from the parameters that follow it, `rest`,
/// with how many of those it took.
fn extended(rest: &[Option<u32>], sub: &[Option<u32>]) -> (Option<Color>, usize) {
    let byte = |v: Option<u32>| u8::try_from(v.unwrap_or(0)).ok();
    if !sub.is_empty() {
        let color = match sub {
            [Some(5), n, ..] => byte(*n).map(Color::Indexed),
            // 38:2:r:g:b, or with a colour space ID first.
            [Some(2), _, r, g, b, ..] | [Some(2), r, g, b] => Some(Color::Rgb(
                byte(*r).unwrap_or(0),
                byte(*g).unwrap_or(0),
                byte(*b).unwrap_or(0),
            )),
            _ => None,
        };
        return (color, 0);
    }
    match rest {
        [Some(5), n, ..] => (byte(*n).map(Color::Indexed), 2),
        [Some(2), r, g, b, ..] => {
            let rgb = Color::Rgb(
                byte(*r).unwrap_or(0),
                byte(*g).unwrap_or(0),
                byte(*b).unwrap_or(0),
            );
            (Some(rgb), 4)
        }
        _ => (None, rest.len()),
    }
}
