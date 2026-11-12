//! The kernel's log and its output.

use super::{Log, Lookup, Message, Readers, Record, Severity, Source, line};
use crate::object::budget::Budget;
use crate::object::{Header, Object, ObjectType, signals};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

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

/// The kernel thread that draws records on the console, once it is
/// started.
static WRITER: AtomicPtr<Arc<crate::sched::Thread>> = AtomicPtr::new(core::ptr::null_mut());
/// Set when a record was appended while the writer could not be woken,
/// so that `notify_pending` wakes it.
static WRITE_PENDING: AtomicBool = AtomicBool::new(false);
/// Set while the writer draws records.
static WRITING: AtomicBool = AtomicBool::new(false);
/// The sequence of the last record drawn on the console.
static DRAWN: AtomicU64 = AtomicU64::new(0);
/// How long the writer sleeps between rounds when nothing wakes it.
const WRITER_PERIOD: i64 = 50_000_000;

fn writer() -> Option<&'static Arc<crate::sched::Thread>> {
    // SAFETY: `start_writer` stores a pointer to a reference that it
    // leaks, so it stays valid forever.
    unsafe { WRITER.load(Ordering::Acquire).as_ref() }
}

/// Starts the kernel thread that draws records on the console from now
/// on. Drawing them, on a slow framebuffer, takes long, which a thread
/// that logged with interrupts disabled and locks held must not wait for.
/// Until it runs, whoever appends records also draws them. The serial
/// port is still written by whoever appends, so that the records reach it
/// even when the machine is switched off right after, from user space.
pub fn start_writer() -> Result<(), Error> {
    let thread = crate::sched::spawn(write_out, 0)?;
    let kept = crate::fallible::try_box(thread)?;
    WRITER.store(alloc::boxed::Box::into_raw(kept), Ordering::Release);
    Ok(())
}

/// The writer: draws the records not yet drawn, letting other threads
/// run between them, and sleeps until woken or `WRITER_PERIOD` has
/// passed. A record still being written stops it until that record's
/// writer wakes it.
fn write_out(_: usize) {
    let mut next = DRAWN.load(Ordering::Acquire) + 1;
    loop {
        WRITING.store(true, Ordering::Release);
        while next <= LOG.head() {
            match LOG.get(next) {
                Lookup::Found(record) => {
                    crate::arch::console_write(line(&record, b"\n").as_bytes());
                    crate::sched::preempt_point();
                }
                Lookup::Gone => {}
                Lookup::Pending => break,
            }
            next += 1;
        }
        WRITING.store(false, Ordering::Release);
        let _ = crate::sched::sleep_until(crate::arch::monotonic_now() + WRITER_PERIOD);
    }
}

/// Whether the writer is drawing records, so that drawing a record may
/// let other threads run.
pub fn writing() -> bool {
    WRITING.load(Ordering::Acquire)
}

/// Writes out, with `out`, the records not yet written to the serial
/// port, also while a processor was stopped in the middle of writing
/// them: for a panic, once nothing else runs.
/// Not generic, so that `LOG` stays private to this crate, where the GDB
/// helpers look for it.
pub fn flush_for_panic(out: &mut dyn FnMut(&[u8])) {
    LOG.flush(|record| out(line(record, b"\r\n").as_bytes()));
}

fn append(koid: u64, severity: Severity, source: Source, message: &[u8]) {
    LOG.write(
        crate::arch::monotonic_now(),
        koid,
        severity,
        source,
        message,
    );
    let writer = writer();
    LOG.drain(|record| {
        crate::arch::debug_write(line(record, b"\r\n").as_bytes());
        if writer.is_none() {
            crate::arch::console_write(line(record, b"\n").as_bytes());
            DRAWN.store(record.sequence, Ordering::Release);
        }
    });
    match writer {
        // Waking the writer takes a scheduler's lock, so with a lock held
        // it is woken later, as readers are notified.
        Some(writer) if crate::sync::none_held() => crate::sched::wake_sleeper(writer),
        Some(_) => WRITE_PENDING.store(true, Ordering::Release),
        None => {}
    }
    // Notifying a reader takes the locks of the signal hook, which the
    // code that logs, or the code an interrupt that logs interrupted, may
    // hold. With any lock held, the readers are notified later.
    READERS.notify(crate::sync::none_held(), notify);
}

/// Notifies the log readers of the records written since a notification
/// was left pending. No lock may be held.
pub fn notify_pending() {
    READERS.run_pending(notify);
    if WRITE_PENDING.swap(false, Ordering::AcqRel)
        && let Some(writer) = writer()
    {
        crate::sched::wake_sleeper(writer);
    }
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
                let reader = crate::fallible::try_arc(LogReader {
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

static OBJECT: SpinLock<Option<Arc<LogObject>>> =
    SpinLock::new(crate::lock_order::LOG_OBJECT, None);

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
#[doc(hidden)]
#[macro_export]
macro_rules! __kernel_log {
    ($severity:ident, $($arg:tt)*) => {
        $crate::log::write(
            $crate::log::Severity::$severity,
            format_args!($($arg)*),
        )
    };
}

pub use __kernel_log as log;

/// In the diagnostic build, logs a formatted kernel message of severity
/// debug; in the others, nothing. The crate that uses it needs a `diag`
/// feature.
#[doc(hidden)]
#[macro_export]
macro_rules! __kernel_diag {
    ($($arg:tt)*) => {{
        #[cfg(feature = "diag")]
        $crate::log::write($crate::log::Severity::Debug, format_args!($($arg)*));
    }};
}

pub use __kernel_diag as diag;
