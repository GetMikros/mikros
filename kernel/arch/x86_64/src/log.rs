//! The kernel's log and its output.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::log::{Log, Message, Readers, Record, Severity, Source, line};
use kernel_core::object::budget::Budget;
use kernel_core::object::{Header, Object, ObjectType, signals};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// 256 records of 288 bytes: 73728 bytes.
static LOG: Log<256> = Log::new();

/// Appends a kernel record and writes out every record not yet written.
pub fn write(severity: Severity, args: core::fmt::Arguments) {
    let mut message = Message::default();
    let _ = core::fmt::Write::write_fmt(&mut message, args);
    append(0, severity, Source::Kernel, message.as_bytes());
}

/// Appends a record written by the process with ID `koid` and writes out
/// every record not yet written.
pub fn write_user(koid: u64, severity: Severity, message: &[u8]) {
    append(koid, severity, Source::User, message);
}

fn append(koid: u64, severity: Severity, source: Source, message: &[u8]) {
    LOG.write(
        kernel_core::arch::monotonic_now(),
        koid,
        severity,
        source,
        message,
    );
    LOG.drain(|record| {
        kernel_core::arch::debug_write(line(record, b"\r\n").as_bytes());
        kernel_core::arch::console_write(line(record, b"\n").as_bytes());
    });
    // Notifying a reader takes the locks of the signal hook, which the
    // code that logs, or the code an interrupt that logs interrupted, may
    // hold. With any lock held, the readers are notified later.
    READERS.notify(kernel_core::sync::none_held(), notify);
}

/// Notifies the log readers of the records written since a notification
/// was left pending. No lock may be held.
pub fn notify_pending() {
    READERS.run_pending(notify);
}

/// A `LogReader`: a read position in the log.
pub struct LogReader {
    header: Header,
    budget: Arc<Budget>,
    /// The sequence of the last record read.
    position: AtomicU64,
}

impl LogReader {
    /// The reader and its entry in `READERS` with that entry's share of
    /// storage.
    const COST: u64 = (size_of::<LogReader>() + 2 * size_of::<Weak<LogReader>>()) as u64;

    /// A reader at `after`, charged to `budget`. No lock may be held.
    pub fn new(budget: &Arc<Budget>, after: u64) -> Result<Arc<LogReader>, Error> {
        let reader = READERS.with(
            |readers| {
                readers.retain(|r| r.strong_count() > 0);
                readers.try_reserve(1).map_err(|_| Error::NoMemory)?;
                budget.charge_kernel(Self::COST)?;
                let reader = kernel_core::fallible::try_arc(LogReader {
                    header: Header::new(ObjectType::LogReader),
                    budget: budget.clone(),
                    position: AtomicU64::new(after),
                })?;
                readers.push(Arc::downgrade(&reader));
                Ok(reader)
            },
            notify,
        )?;
        reader.update();
        Ok(reader)
    }

    /// The record after the reader's position, or `ShouldWait`.
    pub fn next(&self) -> Result<Record, Error> {
        LOG.next_after(self.position.load(Ordering::Acquire))
            .ok_or(Error::ShouldWait)
    }

    /// Makes `sequence`, that of a record returned by `next` and copied
    /// out, the reader's position.
    pub fn advance(&self, sequence: u64) {
        self.position.store(sequence, Ordering::Release);
        self.update();
    }

    /// Sets READABLE exactly while a record after the position is in the
    /// log.
    fn update(&self) {
        let after = self.position.load(Ordering::Acquire);
        if LOG.next_after(after).is_some() {
            self.header.update_signals(0, signals::READABLE);
        } else {
            self.header.update_signals(signals::READABLE, 0);
        }
    }
}

impl Drop for LogReader {
    fn drop(&mut self) {
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for LogReader {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// Every log reader.
static READERS: Readers<Weak<LogReader>> = Readers::new();

/// Sets READABLE on `reader` exactly while a record after its position is
/// written, as a record still being written cannot be read yet.
fn notify(reader: &Weak<LogReader>) {
    // A reader that upgrades is held by a handle that nothing here
    // closes, so dropping this reference never destroys it.
    if let Some(reader) = reader.upgrade() {
        reader.update();
    }
}

/// The record with the smallest sequence above `after` still in the log.
pub fn next_after(after: u64) -> Option<Record> {
    LOG.next_after(after)
}

/// The `Log` object, through which user space reaches the log.
pub struct LogObject {
    header: Header,
}

impl Object for LogObject {
    fn header(&self) -> &Header {
        &self.header
    }
}

static OBJECT: SpinLock<Option<Arc<LogObject>>> = SpinLock::new(None);

/// The `Log` object, created on the first call. No other is created.
pub fn object() -> Arc<LogObject> {
    OBJECT
        .lock()
        .get_or_insert_with(|| {
            Arc::new(LogObject {
                header: Header::new(ObjectType::Log),
            })
        })
        .clone()
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
