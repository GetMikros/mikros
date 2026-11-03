//! Checks written thread state as `thread_write_state` and signal
//! returns do: the `GENERAL` and `DEBUG` layouts and XSAVE images, each
//! compared with a model of what the kernel must accept and store.

#![no_main]

#[path = "../../arch/x86_64/src/thread_state/layout.rs"]
mod layout;

use layout::{DEBUG_SIZE, GENERAL_SIZE, General};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

const USER: core::ops::Range<u64> = 0x1_0000..0x7FFF_FFFF_F000;

fn user(a: u64) -> bool {
    USER.contains(&a)
}

/// The RFLAGS bits a written state keeps: CF, PF, AF, ZF, SF, TF, DF, OF
/// and AC.
const KEPT: [u32; 9] = [0, 2, 4, 6, 7, 8, 10, 11, 18];

#[derive(Arbitrary, Debug)]
struct Input {
    general: [u8; GENERAL_SIZE],
    debug: [u8; DEBUG_SIZE],
    xcr0: u64,
    mxcsr_mask: u32,
    area: Vec<u8>,
}

fn words<const N: usize>(bytes: &[u8]) -> [u64; N] {
    core::array::from_fn(|i| u64::from_le_bytes(bytes[8 * i..8 * i + 8].try_into().unwrap()))
}

fn check_general(bytes: &[u8; GENERAL_SIZE]) {
    let w: [u64; 20] = words(bytes);
    let (rip, rflags, fs, gs) = (w[16], w[17], w[18], w[19]);
    let base_ok = |b: u64| b == 0 || user(b);
    let parsed = General::parse(bytes, user);
    assert_eq!(parsed.is_some(), user(rip) && base_ok(fs) && base_ok(gs));
    let Some(g) = parsed else { return };
    assert_eq!(g.frame[..17], w[..17]);
    assert_eq!((g.fs_base, g.gs_base), (fs, gs));
    let kept = KEPT.iter().fold(0, |m, b| m | 1 << b);
    assert_eq!(g.frame[17], rflags & kept | 1 << 1 | 1 << 9);
    // What the kernel stores reads back unchanged.
    let out = g.bytes();
    assert_eq!(General::parse(&out, user), Some(g));
    assert_eq!(out[..136], bytes[..136]);
    assert_eq!(out[144..], bytes[144..]);
}

fn check_debug(bytes: &[u8; DEBUG_SIZE]) {
    let regs: [u64; 6] = words(bytes);
    let dr7 = regs[5];
    let fields_ok =
        (0..32).all(|b| dr7 >> b & 1 == 0 || b < 8 && b % 2 == 0 || b >= 16) && dr7 >> 32 == 0;
    let no_io = (0..4).all(|i| dr7 >> (16 + 4 * i) & 0b11 != 0b10);
    let addrs_ok = (0..4).all(|i| dr7 >> (2 * i) & 1 == 0 || user(regs[i]));
    let parsed = layout::parse_debug(bytes, user);
    assert_eq!(parsed.is_some(), fields_ok && no_io && addrs_ok);
    let enabled: Vec<usize> = layout::enabled(dr7).collect();
    assert!(enabled.iter().all(|&i| i < 4 && dr7 >> (2 * i) & 1 != 0));
    assert_eq!(enabled.len(), (0..4).filter(|i| dr7 >> (2 * i) & 1 != 0).count());
    if let Some(r) = parsed {
        assert_eq!(r, regs);
        assert_eq!(layout::debug_bytes(&r), *bytes);
    }
}

fn check_xsave(area: &[u8], xcr0: u64, mask: u32) {
    let loadable = layout::xsave_loadable(area, xcr0, mask);
    if area.len() < 576 {
        assert!(!loadable);
    } else {
        let u64_at = |o: usize| u64::from_le_bytes(area[o..o + 8].try_into().unwrap());
        let mxcsr = u32::from_le_bytes(area[24..28].try_into().unwrap());
        let model = u64_at(512) & !xcr0 == 0
            && u64_at(520) == 0
            && area[528..576].iter().all(|&b| b == 0)
            && mxcsr & !mask == 0;
        assert_eq!(loadable, model);
        // More enabled components or a wider mask never refuse an image.
        if loadable {
            assert!(layout::xsave_loadable(area, u64::MAX, u32::MAX));
        }
    }
    if area.len() >= 32 {
        let recorded = u32::from_le_bytes(area[28..32].try_into().unwrap());
        let expected = if recorded == 0 { 0xFFBF } else { recorded };
        assert_eq!(layout::mxcsr_mask(area), expected);
    }
}

fuzz_target!(|input: Input| {
    check_general(&input.general);
    check_debug(&input.debug);
    check_xsave(&input.area, input.xcr0, input.mxcsr_mask);
});
