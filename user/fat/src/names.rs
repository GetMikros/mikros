//! Names for new entries: their long-name entries, which names FAT can
//! hold, and unique short names.

use crate::dir::{ENTRY, LONG_NAME, PER_ENTRY, UNITS};
use alloc::vec::Vec;

/// The long-name entries for `name`, in the order they are stored, for a
/// short entry whose name has `checksum`.
pub(crate) fn long_entries(name: &str, checksum: u8) -> Vec<[u8; ENTRY]> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let count = units.len().div_ceil(PER_ENTRY);
    let mut entries = Vec::with_capacity(count);
    for order in (1..=count).rev() {
        let mut b = [0u8; ENTRY];
        b[0] = order as u8 | if order == count { 0x40 } else { 0 };
        b[11] = LONG_NAME;
        b[13] = checksum;
        for (i, &at) in UNITS.iter().enumerate() {
            let k = (order - 1) * PER_ENTRY + i;
            let unit = match k.cmp(&units.len()) {
                core::cmp::Ordering::Less => units[k],
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 0xFFFF,
            };
            b[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        entries.push(b);
    }
    entries
}

/// Whether `name` can be a long name: not empty, `.` or `..`, without
/// control characters, `"*/:<>?\|`, or a trailing dot or space.
pub fn valid_long(name: &str) -> bool {
    let bad = |c: char| c < ' ' || c == '\u{7F}' || "\"*/:<>?\\|".contains(c);
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name.chars().any(bad)
}

/// How many UTF-16 code units `name` takes.
pub(crate) fn units(name: &str) -> usize {
    name.encode_utf16().count()
}

/// The characters a short name may hold besides letters and digits.
const SHORT_SPECIAL: &[u8] = b"$%'-_@~`!(){}^#&";

/// The short-name basis of `name`: its uppercase 8.3 form, with the
/// characters a short name cannot hold replaced by `_`, and whether the
/// conversion lost nothing but case.
pub(crate) fn short_basis(name: &str) -> ([u8; 11], bool) {
    let mut lossless = true;
    let stripped = name.trim_start_matches('.');
    lossless &= stripped.len() == name.len();
    let (base, ext) = match stripped.rfind('.') {
        Some(dot) => (&stripped[..dot], &stripped[dot + 1..]),
        None => (stripped, ""),
    };
    let convert = |part: &str, room: usize, out: &mut [u8], lossless: &mut bool| {
        let mut n = 0;
        for c in part.chars() {
            if c == ' ' || c == '.' {
                *lossless = false;
                continue;
            }
            if n == room {
                *lossless = false;
                break;
            }
            let upper = c.to_ascii_uppercase();
            out[n] = match upper {
                'A'..='Z' | '0'..='9' => upper as u8,
                c if c.is_ascii() && SHORT_SPECIAL.contains(&(c as u8)) => c as u8,
                _ => {
                    *lossless = false;
                    b'_'
                }
            };
            n += 1;
        }
    };
    let mut short = [b' '; 11];
    convert(base, 8, &mut short[..8], &mut lossless);
    convert(ext, 3, &mut short[8..], &mut lossless);
    if short[0] == b' ' {
        short[0] = b'_';
        lossless = false;
    }
    (short, lossless)
}

/// `basis` with the numeric tail `~n`.
pub(crate) fn with_tail(basis: &[u8; 11], n: u32) -> [u8; 11] {
    let mut tail = [0u8; 8];
    let mut digits = 0;
    let mut v = n;
    while v > 0 {
        tail[7 - digits] = b'0' + (v % 10) as u8;
        v /= 10;
        digits += 1;
    }
    let tail = &tail[8 - digits..];
    let used = basis[..8].iter().position(|&c| c == b' ').unwrap_or(8);
    let keep = used.min(8 - 1 - tail.len());
    let mut short = *basis;
    short[keep] = b'~';
    short[keep + 1..keep + 1 + tail.len()].copy_from_slice(tail);
    for c in &mut short[keep + 1 + tail.len()..8] {
        *c = b' ';
    }
    short
}
