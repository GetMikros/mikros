//! Writing to the kernel log.

use crate::sys;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, Ordering};

/// The longest message the kernel takes, in bytes.
pub const MESSAGE_MAX: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Severity {
    Debug = 0,
    Info = 1,
    Warning = 2,
    Error = 3,
}

/// The `Log` handle, 0 until the program has one.
static LOG: AtomicU32 = AtomicU32::new(0);

/// Makes `log` the handle that messages are written to.
pub fn set_handle(log: sys::Handle) {
    LOG.store(log, Ordering::Relaxed);
}

/// A message being formatted, cut at `MESSAGE_MAX` bytes on a character
/// boundary.
struct Message {
    bytes: [u8; MESSAGE_MAX],
    len: usize,
}

impl Write for Message {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = MESSAGE_MAX - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// Writes a formatted message, if the program has a `Log` handle.
pub fn write(severity: Severity, args: fmt::Arguments) {
    let log = LOG.load(Ordering::Relaxed);
    if log == 0 {
        return;
    }
    let mut message = Message {
        bytes: [0; MESSAGE_MAX],
        len: 0,
    };
    let _ = message.write_fmt(args);
    // SAFETY: the pointer and length describe `message.bytes`. A failure
    // cannot be reported anywhere else.
    unsafe {
        sys::log_write(
            log,
            severity as u32,
            message.bytes.as_ptr(),
            message.len as u64,
        )
    };
}

/// Writes a formatted message with the given severity to the kernel log.
#[macro_export]
macro_rules! log {
    ($severity:ident, $($arg:tt)*) => {
        $crate::log::write($crate::log::Severity::$severity, format_args!($($arg)*))
    };
}
