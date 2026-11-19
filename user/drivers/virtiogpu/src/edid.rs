//! The modes that an EDID base block lists: its detailed timings, its
//! standard timings and its established timings, in that order, each
//! once, without interlaced ones. A block whose header or checksum is
//! wrong lists none.

use alloc::vec::Vec;

/// A mode as width, height and refresh rate in millihertz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub width: u32,
    pub height: u32,
    pub refresh_millihertz: u32,
}

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const BLOCK: usize = 128;
const ESTABLISHED: usize = 0x23;
const STANDARD: usize = 0x26;
const DESCRIPTORS: usize = 0x36;
const DESCRIPTOR: usize = 18;

/// The established timings, by bit from the most significant one of
/// bytes 0x23 and 0x24, and bit 7 of 0x25. Interlaced 1024x768 is left
/// out as `None`.
const ESTABLISHED_MODES: [Option<(u32, u32, u32)>; 17] = [
    Some((720, 400, 70)),
    Some((720, 400, 88)),
    Some((640, 480, 60)),
    Some((640, 480, 67)),
    Some((640, 480, 72)),
    Some((640, 480, 75)),
    Some((800, 600, 56)),
    Some((800, 600, 60)),
    Some((800, 600, 72)),
    Some((800, 600, 75)),
    Some((832, 624, 75)),
    None,
    Some((1024, 768, 60)),
    Some((1024, 768, 70)),
    Some((1024, 768, 75)),
    Some((1280, 1024, 75)),
    Some((1152, 870, 75)),
];

fn hz(width: u32, height: u32, refresh: u32) -> Timing {
    Timing {
        width,
        height,
        refresh_millihertz: refresh * 1000,
    }
}

/// The detailed timing in the descriptor `d`, if it is one and not
/// interlaced.
fn detailed(d: &[u8]) -> Option<Timing> {
    let clock = u64::from(u16::from_le_bytes([d[0], d[1]])) * 10_000;
    if clock == 0 || d[17] & 0x80 != 0 {
        return None;
    }
    let width = u32::from(d[2]) | (u32::from(d[4] & 0xf0) << 4);
    let hblank = u32::from(d[3]) | (u32::from(d[4] & 0x0f) << 8);
    let height = u32::from(d[5]) | (u32::from(d[7] & 0xf0) << 4);
    let vblank = u32::from(d[6]) | (u32::from(d[7] & 0x0f) << 8);
    let total = u64::from(width + hblank) * u64::from(height + vblank);
    if width == 0 || height == 0 || total == 0 {
        return None;
    }
    let refresh_millihertz = u32::try_from((clock * 1000 + total / 2) / total).ok()?;
    Some(Timing {
        width,
        height,
        refresh_millihertz,
    })
}

/// The standard timing in the two bytes `s`, if it is used.
fn standard(s: &[u8]) -> Option<Timing> {
    if s[0] <= 1 {
        return None;
    }
    let width = (u32::from(s[0]) + 31) * 8;
    let height = match s[1] >> 6 {
        0 => width * 10 / 16,
        1 => width * 3 / 4,
        2 => width * 4 / 5,
        _ => width * 9 / 16,
    };
    Some(hz(width, height, u32::from(s[1] & 0x3f) + 60))
}

/// The modes of the EDID `blob`.
pub fn modes(blob: &[u8]) -> Vec<Timing> {
    let Some(block) = blob.get(..BLOCK) else {
        return Vec::new();
    };
    let sum = block.iter().fold(0u8, |s, b| s.wrapping_add(*b));
    if block[..8] != HEADER || sum != 0 {
        return Vec::new();
    }
    let mut modes: Vec<Timing> = Vec::new();
    let mut add = |t: Timing| {
        if !modes.contains(&t) {
            modes.push(t);
        }
    };
    for d in block[DESCRIPTORS..DESCRIPTORS + 4 * DESCRIPTOR].chunks(DESCRIPTOR) {
        detailed(d).into_iter().for_each(&mut add);
    }
    for s in block[STANDARD..DESCRIPTORS].chunks(2) {
        standard(s).into_iter().for_each(&mut add);
    }
    let bits = u32::from(block[ESTABLISHED]) << 9
        | u32::from(block[ESTABLISHED + 1]) << 1
        | u32::from(block[ESTABLISHED + 2] >> 7);
    for (i, mode) in ESTABLISHED_MODES.iter().enumerate() {
        if bits & (1 << (16 - i)) != 0
            && let Some((w, h, r)) = *mode
        {
            add(hz(w, h, r));
        }
    }
    modes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block with 1920x1080 at 60 Hz as its detailed timing, 1280x1024
    /// at 60 Hz as a standard timing, 640x480 and 800x600 at 60 Hz as
    /// established timings, and an interlaced detailed timing.
    fn block() -> Vec<u8> {
        let mut b = vec![0u8; BLOCK];
        b[..8].copy_from_slice(&HEADER);
        b[ESTABLISHED] = 0x21;
        // 1280 = (0x81 + 31) * 8, 5:4, 60 Hz; the rest unused.
        b[STANDARD..STANDARD + 2].copy_from_slice(&[0x81, 0x80]);
        for s in (STANDARD + 2..DESCRIPTORS).step_by(2) {
            b[s..s + 2].copy_from_slice(&[0x01, 0x01]);
        }
        // 148.5 MHz, 1920 + 280 by 1080 + 45: 60 Hz.
        let dtd = [
            0x02, 0x3a, 0x80, 0x18, 0x71, 0x38, 0x2d, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1e,
        ];
        b[DESCRIPTORS..DESCRIPTORS + DESCRIPTOR].copy_from_slice(&dtd);
        let mut interlaced = dtd;
        interlaced[17] |= 0x80;
        let second = DESCRIPTORS + DESCRIPTOR;
        b[second..second + DESCRIPTOR].copy_from_slice(&interlaced);
        let sum = b.iter().fold(0u8, |s, x| s.wrapping_add(*x));
        b[BLOCK - 1] = 0u8.wrapping_sub(sum);
        b
    }

    #[test]
    fn lists_detailed_standard_and_established_timings() {
        assert_eq!(
            modes(&block()),
            vec![
                hz(1920, 1080, 60),
                hz(1280, 1024, 60),
                hz(640, 480, 60),
                hz(800, 600, 60),
            ]
        );
    }

    #[test]
    fn a_block_with_a_wrong_checksum_or_header_has_no_modes() {
        let mut b = block();
        b[BLOCK - 1] ^= 1;
        assert!(modes(&b).is_empty());
        let mut b = block();
        b[0] = 1;
        assert!(modes(&b).is_empty());
        assert!(modes(&b[..64]).is_empty());
    }
}
