//! Writing to the kernel log, or, in a program that has no `Log` handle,
//! to the log service's `mikros.log.Writer`.

use crate::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use crate::interfaces::mikros::log::{self as service, Writer, WriterClient};
use crate::interfaces::mikros::service::{Directory, DirectoryClient};
use crate::sys;
use alloc::string::String;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, Ordering};

/// The longest message the kernel takes, in bytes.
pub const MESSAGE_MAX: usize = 256;
/// The longest message the log service's `Writer` takes, in bytes.
pub const WRITER_MESSAGE_MAX: usize = 1024;
/// The protocol of the log service that writers use.
const WRITER: &str = "mikros.log.Writer";
const FOREVER: i64 = i64::MAX;

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
/// The client end of a `Writer` channel, 0 until the program has one.
static WRITER_CHANNEL: AtomicU32 = AtomicU32::new(0);

/// Makes `log` the handle that messages are written to.
pub fn set_handle(log: sys::Handle) {
    LOG.store(log, Ordering::Relaxed);
}

/// Makes the log service's `Writer` on `writer` take the messages while
/// the program has no `Log` handle. The channel stays open while the
/// program runs.
pub fn set_writer(writer: ClientEnd<Writer>) {
    let old = WRITER_CHANNEL.swap(writer.into_handle().into_raw(), Ordering::AcqRel);
    if old != 0 {
        // SAFETY: the handle was the writer's, which this swap took.
        drop(unsafe { OwnedHandle::from_raw(old) });
    }
}

/// Connects to the log service's `Writer` through the service directory
/// on `directory`, for the messages of a program that has no `Log`
/// handle. It calls on the channel itself, so nothing else may use the
/// channel meanwhile, as at startup. False when the directory refuses.
pub fn connect(directory: &ClientEnd<Directory>) -> bool {
    let Ok((client, server)) = endpoints::<Writer>() else {
        return false;
    };
    // SAFETY: the handle stays the caller's: the client that borrows it
    // is forgotten below, so that it does not close it.
    let raw = unsafe { OwnedHandle::from_raw(directory.handle().raw()) };
    let borrowed = DirectoryClient::new(ClientEnd::new(raw));
    let connected = borrowed.connect(WRITER.into(), None, server.into_handle(), FOREVER);
    core::mem::forget(borrowed);
    match connected {
        Ok(Ok(_)) => {
            set_writer(client);
            true
        }
        _ => false,
    }
}

/// A message being formatted, cut at `N` bytes on a character boundary.
struct Message<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Message<N> {
    fn new(args: fmt::Arguments) -> Message<N> {
        let mut message = Message {
            bytes: [0; N],
            len: 0,
        };
        let _ = message.write_fmt(args);
        message
    }
}

impl<const N: usize> Write for Message<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = N - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// Writes a formatted message, to the `Log` handle if the program has
/// one, or else to the log service's `Writer` if it has one.
pub fn write(severity: Severity, args: fmt::Arguments) {
    let log = LOG.load(Ordering::Relaxed);
    if log != 0 {
        let message = Message::<MESSAGE_MAX>::new(args);
        // SAFETY: the pointer and length describe `message.bytes`. A
        // failure cannot be reported anywhere else.
        unsafe {
            sys::log_write(
                log,
                severity as u32,
                message.bytes.as_ptr(),
                message.len as u64,
            )
        };
        return;
    }
    let writer = WRITER_CHANNEL.load(Ordering::Acquire);
    if writer == 0 {
        return;
    }
    let message = Message::<WRITER_MESSAGE_MAX>::new(args);
    // The bytes were cut on a character boundary of formatted text.
    let text = core::str::from_utf8(&message.bytes[..message.len]).unwrap_or_default();
    let severity = match severity {
        Severity::Debug => service::Severity::Debug,
        Severity::Info => service::Severity::Info,
        Severity::Warning => service::Severity::Warning,
        Severity::Error => service::Severity::Error,
    };
    // SAFETY: the handle is the writer's, which stays open; the client
    // is forgotten below, so that it does not close it.
    let client = WriterClient::new(ClientEnd::new(unsafe { OwnedHandle::from_raw(writer) }));
    // A failure cannot be reported anywhere else.
    let _ = client.write(severity, String::from(text));
    core::mem::forget(client);
}

/// Writes a formatted message with the given severity to the kernel log,
/// or to the log service.
#[macro_export]
macro_rules! log {
    ($severity:ident, $($arg:tt)*) => {
        $crate::log::write($crate::log::Severity::$severity, format_args!($($arg)*))
    };
}

/// In the diagnostic build, which sets the `mikros_diag` cfg, writes a
/// formatted message of severity debug as `log!` does; in the others,
/// nothing.
#[cfg(mikros_diag)]
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::log!(Debug, $($arg)*)
    };
}

/// In the diagnostic build, which sets the `mikros_diag` cfg, writes a
/// formatted message of severity debug as `log!` does; in the others,
/// nothing.
#[cfg(not(mikros_diag))]
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        ()
    };
}
