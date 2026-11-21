//! The standard output and error that the process manager gives the
//! processes it starts (MIKSPEC-0063.R21). A process that its starter
//! gave no `STDOUT` or `STDERR` gets a stream for each, `STDOUT` first,
//! while its startup handles stay within their limit; the log service
//! gets none. The process manager keeps the other ends, and hands them
//! to the log service's `mikros.log.ProcessSources` once it can connect
//! to it. Until then it reads them itself and writes each line to its
//! kernel log handle, after the app ID and the process ID, in messages
//! that the kernel takes. It tries to connect once a second, through a
//! new directory of its own each time, and again after `Add` failed.

use crate::processes::Shared;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::{Pin, pin};
use core::task::Poll;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::log::{ProcessSources, ProcessSourcesAsyncClient};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, ProcessDirectoryAsyncClient};
use mikros_rt::log::{MESSAGE_MAX, Severity};
use mikros_rt::sys;

/// The descriptor kind of a stream, and the `id`s of standard output and
/// error, descriptors 1 and 2 of that kind.
const KIND_STREAM: u32 = 2;
const STDOUT_ID: u32 = 1 | KIND_STREAM << 28;
const STDERR_ID: u32 = 2 | KIND_STREAM << 28;
/// The most startup handles a process gets (MIKSPEC-0034.R46).
const HANDLES_MAX: usize = 32;
/// The log service, which gets no streams, and its protocol.
const LOG_SERVICE: &str = "system.logsvc";
const PROCESS_SOURCES: &str = "mikros.log.ProcessSources";
const SECOND: i64 = 1_000_000_000;
const SHOULD_WAIT: sys::Status = -22;
const READABLE: u32 = 1 << 0;
const PEER_CLOSED: u32 = 1 << 2;
/// The most bytes of a line kept before they are written without a line
/// feed.
const LINE_MAX: usize = 4096;

/// The ends that the process manager keeps of the streams it gave a
/// process.
#[derive(Default)]
pub struct Kept {
    output: Option<OwnedHandle>,
    error: Option<OwnedHandle>,
}

/// The log service's `ProcessSources`, once the process manager is
/// connected to it.
#[derive(Default)]
pub struct Sink {
    sources: RefCell<Option<Rc<ProcessSourcesAsyncClient>>>,
}

/// A new stream, as the end to give and the end to keep.
fn stream() -> Option<(OwnedHandle, OwnedHandle)> {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    let status = unsafe { sys::stream_create(0, &mut a, &mut b) };
    // SAFETY: the kernel just created the handles for us.
    (status == mikros_rt::OK)
        .then(|| unsafe { (OwnedHandle::from_raw(a), OwnedHandle::from_raw(b)) })
}

/// Gives the process of `app_id` a stream in `handles` for each of
/// `STDOUT` and `STDERR` that they lack, `STDOUT` first, while they stay
/// within their limit, and returns the ends kept.
pub fn give(handles: &mut Vec<StartupHandle>, app_id: &str) -> Kept {
    let mut kept = Kept::default();
    if app_id == LOG_SERVICE {
        return kept;
    }
    let roles = [
        (HandleRole::Stdout, STDOUT_ID, &mut kept.output),
        (HandleRole::Stderr, STDERR_ID, &mut kept.error),
    ];
    for (role, id, end) in roles {
        if handles.len() >= HANDLES_MAX || handles.iter().any(|h| h.role == role) {
            continue;
        }
        if let Some((given, ours)) = stream() {
            handles.push(StartupHandle {
                role,
                id,
                handle: given,
            });
            *end = Some(ours);
        }
    }
    kept
}

/// The bytes read from a stream that do not end in a line feed yet.
#[derive(Default)]
struct Lines(Vec<u8>);

impl Lines {
    /// Takes `bytes`, and writes each line that they end, and a line that
    /// grew too long, with `severity` and `prefix`.
    fn push(&mut self, bytes: &[u8], severity: Severity, prefix: &str) {
        for &b in bytes {
            if b == b'\n' {
                write(severity, prefix, &self.0);
                self.0.clear();
                continue;
            }
            self.0.push(b);
            if self.0.len() >= LINE_MAX {
                // A character that the bytes end inside of goes with the
                // next part.
                let cut = match core::str::from_utf8(&self.0) {
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    _ => self.0.len(),
                };
                let rest = self.0.split_off(cut);
                write(severity, prefix, &self.0);
                self.0 = rest;
            }
        }
    }

    /// Writes what is left, as the last line.
    fn finish(&mut self, severity: Severity, prefix: &str) {
        if !self.0.is_empty() {
            write(severity, prefix, &self.0);
            self.0.clear();
        }
    }
}

/// Writes `line` to the kernel log with `severity`, after `prefix`, its
/// bytes that are not UTF-8 replaced, in messages that the kernel takes,
/// cut between characters.
fn write(severity: Severity, prefix: &str, line: &[u8]) {
    let text = String::from_utf8_lossy(line);
    let room = MESSAGE_MAX.saturating_sub(prefix.len()).max(4);
    let mut rest: &str = &text;
    loop {
        let mut end = rest.len().min(room);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (part, after) = rest.split_at(end);
        mikros_rt::log::write(severity, format_args!("{prefix}{part}"));
        if after.is_empty() {
            return;
        }
        rest = after;
    }
}

/// Reads what `stream` holds now into `lines`, and drops the stream,
/// after writing its last line, when its other end closed.
fn drain(stream: &mut Option<OwnedHandle>, lines: &mut Lines, severity: Severity, prefix: &str) {
    let Some(handle) = stream else {
        return;
    };
    let mut buf = [0u8; 1024];
    loop {
        let mut read = 0;
        // SAFETY: the pointers describe `buf` and `read`.
        let status = unsafe {
            sys::stream_read(handle.raw(), buf.as_mut_ptr(), buf.len() as u64, &mut read)
        };
        match status {
            mikros_rt::OK => lines.push(&buf[..read as usize], severity, prefix),
            SHOULD_WAIT => return,
            _ => {
                lines.finish(severity, prefix);
                *stream = None;
                return;
            }
        }
    }
}

/// Waits until a stream of `kept` can be read or closed, or `deadline`.
async fn wait(reactor: &Reactor, kept: &Kept, deadline: i64) {
    let mut signals: Vec<_> = [&kept.output, &kept.error]
        .into_iter()
        .flatten()
        .map(|h| reactor.signals(h.raw(), READABLE | PEER_CLOSED))
        .collect();
    let mut sleep = pin!(reactor.sleep_until(deadline));
    poll_fn(|cx| {
        let woken = signals.iter_mut().any(|s| Pin::new(s).poll(cx).is_ready());
        match woken || sleep.as_mut().poll(cx).is_ready() {
            true => Poll::Ready(()),
            false => Poll::Pending,
        }
    })
    .await
}

/// Forwards the ends `kept` of process `pid` of `app_id`: to the log
/// service once the process manager is connected to it, and until then
/// to the kernel log.
pub async fn forward(shared: Rc<Shared>, pid: u32, app_id: String, mut kept: Kept) {
    let prefix = format!("{app_id} {pid}: ");
    let (mut output, mut error) = (Lines::default(), Lines::default());
    while kept.output.is_some() || kept.error.is_some() {
        let sources = shared.output.sources.borrow().clone();
        if let Some(sources) = sources {
            // The start of a line that was read goes to the kernel log.
            output.finish(Severity::Info, &prefix);
            error.finish(Severity::Error, &prefix);
            let (out, err) = (kept.output.take(), kept.error.take());
            let added = sources.add(pid, app_id.clone(), out, err).await;
            if !matches!(added, Ok(Ok(_))) {
                shared.output.sources.borrow_mut().take();
            }
            return;
        }
        drain(&mut kept.output, &mut output, Severity::Info, &prefix);
        drain(&mut kept.error, &mut error, Severity::Error, &prefix);
        wait(&shared.reactor, &kept, mikros_rt::objects::now() + SECOND).await;
    }
}

/// Connects the process manager to `ProcessSources` through a directory
/// of its own bound over `control`, giving up an attempt after a second,
/// and tries again a second later whenever it is not connected.
pub async fn connect(shared: Rc<Shared>, control: Rc<ProcessDirectoryAsyncClient>) {
    loop {
        if shared.output.sources.borrow().is_none() {
            let mut attempt = pin!(attempt(&shared, &control));
            let mut sleep = pin!(
                shared
                    .reactor
                    .sleep_until(mikros_rt::objects::now() + SECOND)
            );
            let connected = poll_fn(|cx| match attempt.as_mut().poll(cx) {
                Poll::Ready(connected) => Poll::Ready(connected),
                Poll::Pending if sleep.as_mut().poll(cx).is_ready() => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            })
            .await;
            if let Some(sources) = connected {
                *shared.output.sources.borrow_mut() = Some(Rc::new(sources));
            }
        }
        let next = mikros_rt::objects::now() + SECOND;
        if shared.reactor.sleep_until(next).await.is_err() {
            return;
        }
    }
}

async fn attempt(
    shared: &Shared,
    control: &ProcessDirectoryAsyncClient,
) -> Option<ProcessSourcesAsyncClient> {
    let directory = crate::init::bind_own_directory(shared, control)
        .await
        .ok()?;
    let directory = DirectoryAsyncClient::new(directory, shared.reactor.clone());
    let (client, server) = endpoints::<ProcessSources>().ok()?;
    let connected = directory
        .connect(PROCESS_SOURCES.into(), None, server.into_handle())
        .await;
    matches!(connected, Ok(Ok(_)))
        .then(|| ProcessSourcesAsyncClient::new(client, shared.reactor.clone()))
}
