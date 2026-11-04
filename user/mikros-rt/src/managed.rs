//! Receives the startup state that the process manager sends on the
//! bootstrap channel of each process it starts.
//!
//! The messages are `Begin`, then `Handles` batches, then `Namespace`
//! batches, then `Complete`. Everything received is owned by the
//! partial state, so a refused transfer drops it, which closes every
//! handle received so far.

use crate::interfaces::__midl::{self, OwnedHandle, ServerEnd};
use crate::interfaces::mikros::process::{
    BootstrapRequest, BootstrapServer, NamespaceEntry, StartupHandle, StartupHeader,
};
use crate::{heap, sys};
use alloc::vec::Vec;

/// The only version of the transfer.
const VERSION: u32 = 1;
/// The most handles, and the most namespace entries, a transfer holds.
const COUNT_MAX: u32 = 32;

/// The startup state of a process that the process manager started.
pub struct Managed {
    pub header: StartupHeader,
    pub handles: Vec<StartupHandle>,
    pub namespace: Vec<NamespaceEntry>,
    /// Kept open for as long as the state lives.
    _bootstrap: BootstrapServer,
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
    // SAFETY: the kernel gave the program this handle to the bootstrap
    // channel, and nothing else owns it.
    let end = ServerEnd::new(unsafe { OwnedHandle::from_raw(bootstrap) });
    let server = BootstrapServer::new(end);
    let BootstrapRequest::Begin(begin) = server.next(i64::MAX)? else {
        return Err(Refused::Order);
    };
    let header = begin.startup;
    // Batches can be larger than the static arena, so the allocator
    // needs the process handle from here on. On a refusal the program
    // ends before allocating again.
    heap::set_process(header.process.raw());
    if header.version != VERSION {
        return Err(Refused::Version);
    }
    if header.handle_count > COUNT_MAX || header.namespace_count > COUNT_MAX {
        return Err(Refused::Count);
    }
    let (mut handles, mut namespace) = (Vec::new(), Vec::new());
    loop {
        match server.next(i64::MAX)? {
            BootstrapRequest::Begin(_) => return Err(Refused::Order),
            BootstrapRequest::Handles(batch) => {
                if !namespace.is_empty() {
                    return Err(Refused::Order);
                }
                append(
                    &mut handles,
                    batch.offset,
                    batch.handles,
                    header.handle_count,
                )?;
            }
            BootstrapRequest::Namespace(batch) => {
                append(
                    &mut namespace,
                    batch.offset,
                    batch.entries,
                    header.namespace_count,
                )?;
            }
            BootstrapRequest::Complete(_) => {
                if handles.len() != header.handle_count as usize
                    || namespace.len() != header.namespace_count as usize
                {
                    return Err(Refused::Count);
                }
                return Ok(Managed {
                    header,
                    handles,
                    namespace,
                    _bootstrap: server,
                });
            }
        }
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
