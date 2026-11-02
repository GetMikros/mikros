//! The kernel's log and its output.

use crate::serial;
use kernel_core::log::{Log, Message, Severity, Source, line};

/// 256 records of 288 bytes: 73728 bytes.
static LOG: Log<256> = Log::new();

/// Appends a kernel record and writes out every record not yet written.
pub fn write(severity: Severity, args: core::fmt::Arguments) {
    let mut message = Message::default();
    let _ = core::fmt::Write::write_fmt(&mut message, args);
    LOG.write(
        crate::time::now(),
        0,
        severity,
        Source::Kernel,
        message.as_bytes(),
    );
    LOG.drain(|record| serial::write(line(record, b"\r\n").as_bytes()));
}

/// Logs a formatted kernel message with the given severity.
macro_rules! log {
    ($severity:ident, $($arg:tt)*) => {
        $crate::log::write(
            kernel_core::log::Severity::$severity,
            format_args!($($arg)*),
        )
    };
}

pub(crate) use log;
