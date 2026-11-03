//! The layouts of the thread state that `thread_read_state` and
//! `thread_write_state` copy, and the checks on written state.

mod layout;

pub use layout::*;

use crate::idt::Frame;

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
}
