//! What a screen cell holds: a character, its colours and attributes,
//! and the width of the character.

/// A colour: the terminal's default, one of the 256 indexed colours, or
/// a direct one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// The attributes of a cell, as bits.
pub const BOLD: u16 = 1 << 0;
pub const DIM: u16 = 1 << 1;
pub const ITALIC: u16 = 1 << 2;
pub const UNDERLINE: u16 = 1 << 3;
pub const BLINK: u16 = 1 << 4;
pub const REVERSE: u16 = 1 << 5;
pub const INVISIBLE: u16 = 1 << 6;
pub const STRIKE: u16 = 1 << 7;

/// Where a cell is in a character that takes two columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Width {
    #[default]
    Single,
    /// The first column of a wide character, which the cell's
    /// character is.
    Wide,
    /// The second column, which shows nothing of its own.
    Continuation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: u16,
    pub width: Width,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: 0,
            width: Width::Single,
        }
    }
}

impl Cell {
    /// A blank cell with the background of `pen`, as erasing leaves.
    pub fn blank(pen: &Cell) -> Cell {
        Cell {
            bg: pen.bg,
            ..Cell::default()
        }
    }
}

/// The columns `c` takes: 0 for combining marks and other zero-width
/// characters, 2 for East Asian wide and fullwidth characters and
/// emoji, and 1 for the rest.
pub fn columns(c: char) -> usize {
    let c = c as u32;
    const ZERO: &[(u32, u32)] = &[
        (0x0300, 0x036F),
        (0x0483, 0x0489),
        (0x0591, 0x05BD),
        (0x0610, 0x061A),
        (0x064B, 0x065F),
        (0x0E31, 0x0E31),
        (0x0E34, 0x0E3A),
        (0x1AB0, 0x1AFF),
        (0x1DC0, 0x1DFF),
        (0x200B, 0x200F),
        (0x20D0, 0x20FF),
        (0xFE00, 0xFE0F),
        (0xFE20, 0xFE2F),
    ];
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115F),
        (0x2E80, 0x303E),
        (0x3041, 0x33FF),
        (0x3400, 0x4DBF),
        (0x4E00, 0x9FFF),
        (0xA000, 0xA4CF),
        (0xAC00, 0xD7A3),
        (0xF900, 0xFAFF),
        (0xFE30, 0xFE4F),
        (0xFF00, 0xFF60),
        (0xFFE0, 0xFFE6),
        (0x1F300, 0x1F64F),
        (0x1F900, 0x1F9FF),
        (0x20000, 0x3FFFD),
    ];
    let within = |ranges: &[(u32, u32)]| ranges.iter().any(|&(a, b)| (a..=b).contains(&c));
    if within(ZERO) {
        0
    } else if within(WIDE) {
        2
    } else {
        1
    }
}

/// What the DEC special graphics set shows for the byte `b`, if it is
/// one of the bytes that the set redefines.
pub fn line_drawing(b: char) -> Option<char> {
    const SET: [char; 32] = [
        ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻',
        '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    let at = (b as u32).checked_sub(0x5F)?;
    SET.get(at as usize).copied()
}
