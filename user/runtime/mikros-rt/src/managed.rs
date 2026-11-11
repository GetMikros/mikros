//! Receives the startup state that the process manager sends on the
//! bootstrap channel of each process it starts.
//!
//! The messages are `Begin`, then `Handles` batches, then `Namespace`
//! batches, then `Complete`. Everything received is owned by the
//! partial state, so a refused transfer drops it, which closes every
//! handle received so far.

use crate::interfaces::__midl::{self, ClientEnd, OwnedHandle, ServerEnd};
use crate::interfaces::mikros::fs::Node;
use crate::interfaces::mikros::process::{
    Bootstrap, BootstrapAsyncServer, BootstrapRequest, BootstrapServer, NamespaceEntry,
    StartupHandle, StartupHeader,
};
use crate::{heap, sys};
use alloc::vec::Vec;

/// The only version of the transfer.
const VERSION: u32 = 3;
/// The most handles, and the most namespace entries, a transfer holds.
const COUNT_MAX: u32 = 32;

/// The startup state of a process that the process manager started.
pub struct Managed {
    pub header: StartupHeader,
    pub handles: Vec<StartupHandle>,
    pub namespace: Vec<NamespaceEntry>,
    /// The bootstrap channel's server, kept open for as long as the
    /// state lives.
    _bootstrap: alloc::boxed::Box<dyn core::any::Any>,
}

/// Why a transfer was refused.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// The channel failed, closed, or carried an invalid message.
    Channel(__midl::Error),
    /// A message came out of order.
    Order,
    /// `Begin` named another version.
    Version,
    /// A count is over the limit, a batch goes past its count, or
    /// `Complete` came before both counts were filled.
    Count,
    /// A batch was empty or did not start at the next unfilled offset.
    Offset,
}

impl From<__midl::Error> for Refused {
    fn from(e: __midl::Error) -> Refused {
        Refused::Channel(e)
    }
}

/// Receives and checks the whole transfer on `bootstrap`.
pub fn receive(bootstrap: sys::Handle) -> Result<Managed, Refused> {
    let server = BootstrapServer::new(bootstrap_end(bootstrap));
    let mut transfer = Transfer::new(server.next(i64::MAX)?)?;
    loop {
        if let Some(header) = transfer.accept(server.next(i64::MAX)?)? {
            return Ok(transfer.finish(header, server));
        }
    }
}

/// Receives and checks the whole transfer on `bootstrap` like `receive`,
/// waiting on the executor of `reactor`, so that other tasks run
/// meanwhile. Once every startup handle has arrived, before any
/// namespace entry, `handles_received` sees the handles and may take
/// some of them, such as an endpoint that a task serves during the
/// rest of the transfer.
pub async fn receive_async(
    bootstrap: sys::Handle,
    reactor: crate::exec::Reactor,
    mut handles_received: impl FnMut(&mut Vec<StartupHandle>),
) -> Result<Managed, Refused> {
    let server = BootstrapAsyncServer::new(bootstrap_end(bootstrap), reactor);
    let mut transfer = Transfer::new(server.next().await?)?;
    let mut told = false;
    loop {
        if !told && transfer.handles_filled() {
            told = true;
            handles_received(&mut transfer.handles);
        }
        if let Some(header) = transfer.accept(server.next().await?)? {
            return Ok(transfer.finish(header, server));
        }
    }
}

fn bootstrap_end(bootstrap: sys::Handle) -> ServerEnd<Bootstrap> {
    // SAFETY: the kernel gave the program this handle to the bootstrap
    // channel, and nothing else owns it.
    ServerEnd::new(unsafe { OwnedHandle::from_raw(bootstrap) })
}

/// A transfer after `Begin`.
struct Transfer {
    header: Option<StartupHeader>,
    handle_count: u32,
    namespace_count: u32,
    handles: Vec<StartupHandle>,
    /// How many startup handles arrived, including any a callback took.
    handles_received: usize,
    namespace: Vec<NamespaceEntry>,
}

impl Transfer {
    /// Checks the first message, which must be `Begin`.
    fn new(first: BootstrapRequest) -> Result<Transfer, Refused> {
        let BootstrapRequest::Begin(begin) = first else {
            return Err(Refused::Order);
        };
        let header = begin.startup;
        // Batches can be larger than the static arena, so the allocator
        // needs the process handle from here on. On a refusal the
        // program ends before allocating again.
        heap::set_process(header.process.raw());
        if header.version != VERSION {
            return Err(Refused::Version);
        }
        if header.handle_count > COUNT_MAX || header.namespace_count > COUNT_MAX {
            return Err(Refused::Count);
        }
        Ok(Transfer {
            handle_count: header.handle_count,
            namespace_count: header.namespace_count,
            header: Some(header),
            handles: Vec::new(),
            handles_received: 0,
            namespace: Vec::new(),
        })
    }

    /// Whether every startup handle has arrived, whether or not a
    /// callback took some since.
    fn handles_filled(&self) -> bool {
        self.handles_received == self.handle_count as usize
    }

    /// Checks the next message, and returns the header once `Complete`
    /// arrived.
    fn accept(&mut self, request: BootstrapRequest) -> Result<Option<StartupHeader>, Refused> {
        match request {
            BootstrapRequest::Begin(_) => Err(Refused::Order),
            BootstrapRequest::Handles(batch) => {
                if !self.namespace.is_empty() {
                    return Err(Refused::Order);
                }
                let count = batch.handles.len();
                if count == 0 || batch.offset as usize != self.handles_received {
                    return Err(Refused::Offset);
                }
                if self.handles_received + count > self.handle_count as usize {
                    return Err(Refused::Count);
                }
                self.handles.extend(batch.handles);
                self.handles_received += count;
                Ok(None)
            }
            BootstrapRequest::Namespace(batch) => {
                if !self.handles_filled() {
                    return Err(Refused::Order);
                }
                let count = self.namespace_count;
                append(&mut self.namespace, batch.offset, batch.entries, count)?;
                Ok(None)
            }
            BootstrapRequest::Complete(_) => {
                if !self.handles_filled() || self.namespace.len() != self.namespace_count as usize {
                    return Err(Refused::Count);
                }
                Ok(self.header.take())
            }
        }
    }

    fn finish(self, header: StartupHeader, bootstrap: impl core::any::Any) -> Managed {
        Managed {
            header,
            handles: self.handles,
            namespace: self.namespace,
            _bootstrap: alloc::boxed::Box::new(bootstrap),
        }
    }
}

/// Takes the entry of `namespace` whose path is the longest prefix of
/// the absolute `path`, whole components at a time. Returns its
/// directory and the rest of `path` below it, which is `.` when the
/// entry's path is `path` itself.
pub fn take_entry<'a>(
    namespace: &mut Vec<NamespaceEntry>,
    path: &'a str,
) -> Option<(ClientEnd<Node>, &'a str)> {
    let (at, rest) = namespace
        .iter()
        .enumerate()
        .filter_map(|(at, e)| below(&e.path, path).map(|rest| (at, rest)))
        .max_by_key(|&(at, _)| namespace[at].path.trim_end_matches('/').len())?;
    Some((namespace.swap_remove(at).directory, rest))
}

/// The rest of `path` below `prefix`, if `prefix` is `path` or one of
/// its ancestors: `.` for `path` itself.
fn below<'a>(prefix: &str, path: &'a str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix.trim_end_matches('/'))?;
    if rest.is_empty() {
        return Some(".");
    }
    match rest.strip_prefix('/')? {
        "" => Some("."),
        rest => Some(rest),
    }
}

/// Adds `batch`, which claims to start at `offset`, to `to`, which may
/// hold `count` entries.
fn append<T>(to: &mut Vec<T>, offset: u32, batch: Vec<T>, count: u32) -> Result<(), Refused> {
    if batch.is_empty() || offset as usize != to.len() {
        return Err(Refused::Offset);
    }
    if to.len() + batch.len() > count as usize {
        return Err(Refused::Count);
    }
    to.extend(batch);
    Ok(())
}
