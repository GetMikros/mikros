//! Exception types and the reports written to exception channels.

/// Exception types.
pub mod kind {
    pub const GENERAL: u32 = 1;
    pub const PAGE_FAULT: u32 = 2;
    pub const UNDEFINED_INSTRUCTION: u32 = 3;
    pub const ARITHMETIC: u32 = 4;
    pub const FLOATING_POINT: u32 = 5;
    pub const BREAKPOINT: u32 = 6;
    pub const DEBUG: u32 = 7;
    pub const PROTECTION: u32 = 8;
    pub const ALIGNMENT: u32 = 9;
    pub const OUT_OF_MEMORY: u32 = 10;
    pub const THREAD_STARTING: u32 = 11;
    pub const THREAD_EXITING: u32 = 12;
}

/// The size of an exception report.
pub const REPORT_SIZE: usize = 0x30;

/// The `exception_channel_create` option that enables thread start and
/// exit reports.
pub const DEBUG_EVENTS: u32 = 1 << 0;

/// The actions of `thread_resume_from_exception`.
pub const RESUME: u32 = 0;
pub const TERMINATE: u32 = 1;
pub const DELIVER_SIGNAL: u32 = 2;

/// The exit code of a process terminated because of an exception.
pub const EXIT_CODE: i64 = -2;

/// An exception as reported on an exception channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub kind: u32,
    pub process_koid: u64,
    pub thread_koid: u64,
    pub address: u64,
    pub arch_code: u64,
    pub report_id: u64,
}

impl Report {
    pub fn bytes(&self) -> [u8; REPORT_SIZE] {
        let mut r = [0u8; REPORT_SIZE];
        r[0x00..0x04].copy_from_slice(&self.kind.to_le_bytes());
        r[0x08..0x10].copy_from_slice(&self.process_koid.to_le_bytes());
        r[0x10..0x18].copy_from_slice(&self.thread_koid.to_le_bytes());
        r[0x18..0x20].copy_from_slice(&self.address.to_le_bytes());
        r[0x20..0x28].copy_from_slice(&self.arch_code.to_le_bytes());
        r[0x28..0x30].copy_from_slice(&self.report_id.to_le_bytes());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_the_report() {
        let r = Report {
            kind: kind::PAGE_FAULT,
            process_koid: 0x11,
            thread_koid: 0x22,
            address: 0x3333,
            arch_code: 0x4_0000_000E,
            report_id: 7,
        }
        .bytes();
        assert_eq!(&r[0..8], &[2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(u64::from_le_bytes(r[8..16].try_into().unwrap()), 0x11);
        assert_eq!(u64::from_le_bytes(r[16..24].try_into().unwrap()), 0x22);
        assert_eq!(u64::from_le_bytes(r[24..32].try_into().unwrap()), 0x3333);
        assert_eq!(
            u64::from_le_bytes(r[32..40].try_into().unwrap()),
            0x4_0000_000E
        );
        assert_eq!(u64::from_le_bytes(r[40..48].try_into().unwrap()), 7);
    }
}
