//! The layouts of the thread state that `thread_read_state` and
//! `thread_write_state` copy, and the checks on written state.

use crate::idt::Frame;

/// The size of the `GENERAL` register state.
pub const GENERAL_SIZE: usize = 160;

/// The RFLAGS bits taken from a written register state: CF, PF, AF, ZF,
/// SF, TF, DF, OF and AC.
const RFLAGS_USER: u64 =
    1 << 0 | 1 << 2 | 1 << 4 | 1 << 6 | 1 << 7 | 1 << 8 | 1 << 10 | 1 << 11 | 1 << 18;
/// The RFLAGS bits always set: bit 1 and IF.
const RFLAGS_FIXED: u64 = 1 << 1 | 1 << 9;

/// The user registers of a thread, in the order of the `GENERAL` layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct General {
    pub frame: [u64; 18],
    pub fs_base: u64,
    pub gs_base: u64,
}

impl General {
    /// The registers saved in `frame`, with the segment bases.
    pub fn from_frame(f: &Frame, fs_base: u64, gs_base: u64) -> General {
        General {
            frame: [
                f.rax, f.rbx, f.rcx, f.rdx, f.rsi, f.rdi, f.rbp, f.rsp, f.r8, f.r9, f.r10, f.r11,
                f.r12, f.r13, f.r14, f.r15, f.rip, f.rflags,
            ],
            fs_base,
            gs_base,
        }
    }

    pub fn bytes(&self) -> [u8; GENERAL_SIZE] {
        let mut out = [0u8; GENERAL_SIZE];
        let values = self.frame.iter().chain([&self.fs_base, &self.gs_base]);
        for (chunk, v) in out.as_chunks_mut::<8>().0.iter_mut().zip(values) {
            *chunk = v.to_le_bytes();
        }
        out
    }

    /// The state in `bytes`, or `None` when its RIP or a segment base is
    /// not allowed, as `user` decides for an address. RFLAGS is reduced
    /// to the bits user space controls.
    pub fn parse(bytes: &[u8; GENERAL_SIZE], user: impl Fn(u64) -> bool) -> Option<General> {
        let mut values = [0u64; 20];
        for (v, chunk) in values.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *v = u64::from_le_bytes(*chunk);
        }
        let mut frame = [0u64; 18];
        frame.copy_from_slice(&values[..18]);
        let [fs_base, gs_base] = [values[18], values[19]];
        if !user(frame[16]) || [fs_base, gs_base].iter().any(|&b| b != 0 && !user(b)) {
            return None;
        }
        frame[17] = frame[17] & RFLAGS_USER | RFLAGS_FIXED;
        Some(General {
            frame,
            fs_base,
            gs_base,
        })
    }

    /// Stores the registers into `frame`, leaving its segments alone.
    pub fn apply(&self, f: &mut Frame) {
        let r = &self.frame;
        [f.rax, f.rbx, f.rcx, f.rdx, f.rsi, f.rdi, f.rbp, f.rsp] =
            [r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]];
        [f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15] =
            [r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]];
        f.rip = r[16];
        f.rflags = r[17];
    }
}

/// The size of the `DEBUG` state: DR0 to DR3, DR6 and DR7.
pub const DEBUG_SIZE: usize = 48;

/// The DR7 bits a written state may set: the local enable bits L0 to L3
/// and the condition and length fields.
const DR7_ALLOWED: u64 = 0b0101_0101 | 0xFFFF_0000;

/// The breakpoints of a `DEBUG` state that are enabled, by index.
pub fn enabled(dr7: u64) -> impl Iterator<Item = usize> {
    (0..4).filter(move |i| dr7 >> (2 * i) & 1 != 0)
}

/// The `DEBUG` state in `bytes`, or `None` when it sets a DR7 bit that is
/// not allowed, selects I/O breakpoints, or enables a breakpoint at an
/// address `user` refuses.
pub fn parse_debug(bytes: &[u8; DEBUG_SIZE], user: impl Fn(u64) -> bool) -> Option<[u64; 6]> {
    let mut regs = [0u64; 6];
    for (r, chunk) in regs.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *r = u64::from_le_bytes(*chunk);
    }
    let dr7 = regs[5];
    let io = (0..4).any(|i| dr7 >> (16 + 4 * i) & 0b11 == 0b10);
    if dr7 & !DR7_ALLOWED != 0 || io || enabled(dr7).any(|i| !user(regs[i])) {
        return None;
    }
    Some(regs)
}

/// The bytes of a `DEBUG` state.
pub fn debug_bytes(regs: &[u64; 6]) -> [u8; DEBUG_SIZE] {
    let mut out = [0u8; DEBUG_SIZE];
    for (chunk, r) in out.as_chunks_mut::<8>().0.iter_mut().zip(regs) {
        *chunk = r.to_le_bytes();
    }
    out
}

const MXCSR: usize = 24;
const MXCSR_MASK: usize = 28;
const XSTATE_BV: usize = 512;
const XCOMP_BV: usize = 520;
const HEADER_END: usize = 576;

/// Whether `area`, an XSAVE image in the standard format, can be loaded
/// with XRSTOR: its components are enabled in `xcr0`, it does not use
/// the compacted format, the rest of its header is zero, and MXCSR sets
/// no bit outside `mxcsr_mask`.
pub fn xsave_loadable(area: &[u8], xcr0: u64, mxcsr_mask: u32) -> bool {
    if area.len() < HEADER_END {
        return false;
    }
    let u64_at = |o: usize| u64::from_le_bytes(area[o..o + 8].try_into().expect("8 bytes"));
    let u32_at = |o: usize| u32::from_le_bytes(area[o..o + 4].try_into().expect("4 bytes"));
    u64_at(XSTATE_BV) & !xcr0 == 0
        && u64_at(XCOMP_BV) == 0
        && area[XCOMP_BV + 8..HEADER_END].iter().all(|&b| b == 0)
        && u32_at(MXCSR) & !mxcsr_mask == 0
}

/// The MXCSR mask an XSAVE image records, or the default mask when it
/// records none.
pub fn mxcsr_mask(area: &[u8]) -> u32 {
    let mask = u32::from_le_bytes(
        area[MXCSR_MASK..MXCSR_MASK + 4]
            .try_into()
            .expect("4 bytes"),
    );
    if mask == 0 { 0xFFBF } else { mask }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(a: u64) -> bool {
        (0x1_0000..0x7FFF_FFFF_F000).contains(&a)
    }

    #[test]
    fn round_trips_the_general_layout() {
        let frame = Frame {
            rax: 1,
            rsp: 8,
            r15: 16,
            rip: 0x40_0000,
            rflags: 0x202,
            ..Frame::default()
        };
        let g = General::from_frame(&frame, 0x5000_0000, 0);
        let bytes = g.bytes();
        assert_eq!(&bytes[0..8], &1u64.to_le_bytes());
        assert_eq!(&bytes[56..64], &8u64.to_le_bytes());
        assert_eq!(&bytes[120..128], &16u64.to_le_bytes());
        assert_eq!(&bytes[128..136], &0x40_0000u64.to_le_bytes());
        assert_eq!(&bytes[144..152], &0x5000_0000u64.to_le_bytes());
        assert_eq!(General::parse(&bytes, user), Some(g));
        let mut out = Frame::default();
        g.apply(&mut out);
        assert_eq!((out.rax, out.rsp, out.r15, out.rip), (1, 8, 16, 0x40_0000));
    }

    #[test]
    fn checks_written_registers() {
        let mut g = General::default();
        g.frame[16] = 0x40_0000;
        g.frame[17] = u64::MAX;
        let parsed = General::parse(&g.bytes(), user).unwrap();
        assert_eq!(parsed.frame[17], RFLAGS_USER | RFLAGS_FIXED);
        g.frame[17] = 0;
        assert_eq!(General::parse(&g.bytes(), user).unwrap().frame[17], 0x202);
        g.frame[16] = 0xFFFF_8000_0000_0000;
        assert!(General::parse(&g.bytes(), user).is_none());
        g.frame[16] = 0x40_0000;
        g.gs_base = 0x10;
        assert!(General::parse(&g.bytes(), user).is_none());
    }

    #[test]
    fn checks_debug_state() {
        let state = |r: [u64; 6]| debug_bytes(&r);
        let ok = |r| parse_debug(&state(r), user);
        assert_eq!(
            ok([0x40_0000, 0, 0, 0, 0, 1 | 0b1101 << 16]),
            Some([0x40_0000, 0, 0, 0, 0, 0xD0001])
        );
        // A disabled breakpoint may hold any address.
        assert!(ok([u64::MAX, 0, 0, 0, 0, 0]).is_some());
        assert!(ok([u64::MAX, 0, 0, 0, 0, 1]).is_none());
        assert!(ok([0, 0, 0, 0x40_0000, 0, 1 << 6]).is_some());
        assert!(ok([0x40_0000, 0, 0, 0, 0, 1 << 6]).is_none());
        assert!(ok([0x40_0000, 0, 0, 0, 0, 1 | 1 << 1]).is_none());
        assert!(ok([0x40_0000, 0, 0, 0, 0, 1 | 0b10 << 16]).is_none());
        assert!(ok([0, 0, 0, 0, 0, 1 << 32]).is_none());
        assert_eq!(enabled(0b0100_0001).collect::<std::vec::Vec<_>>(), [0, 3]);
    }

    #[test]
    fn checks_xsave_headers() {
        let mut area = std::vec![0u8; 832];
        area[MXCSR..MXCSR + 4].copy_from_slice(&0x1F80u32.to_le_bytes());
        area[XSTATE_BV] = 0b111;
        assert!(xsave_loadable(&area, 0b111, 0xFFFF));
        assert!(!xsave_loadable(&area, 0b011, 0xFFFF));
        assert!(!xsave_loadable(&area[..575], 0b111, 0xFFFF));
        area[XCOMP_BV + 7] = 0x80;
        assert!(!xsave_loadable(&area, 0b111, 0xFFFF));
        area[XCOMP_BV + 7] = 0;
        area[570] = 1;
        assert!(!xsave_loadable(&area, 0b111, 0xFFFF));
        area[570] = 0;
        area[MXCSR + 2] = 1;
        assert!(!xsave_loadable(&area, 0b111, 0xFFFF));
        assert_eq!(mxcsr_mask(&area), 0xFFBF);
    }
}
