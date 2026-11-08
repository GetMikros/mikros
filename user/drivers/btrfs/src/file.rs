// SPDX-License-Identifier: GPL-2.0
//! The requests of a connection about a node's contents and attributes:
//! reading and writing files at an offset or the cursor
//! (MIKSPEC-0049.R14), truncating them, making ranges of them share data
//! (MIKSPEC-0049.R9, MIKSPEC-0053.R5), setting attributes, cloning the
//! connection, and reading symbolic links.

use crate::change::koids;
use crate::dir::link_target;
use crate::fs::{self, error};
use crate::node::{Connection, MODE_BITS};
use alloc::vec;
use alloc::vec::Vec;
use mikros_btrfs::node::Changes;
use mikros_rt::interfaces::mikros::fs::*;

/// The most bytes one read returns, the limit of its reply.
const READ_MAX: u32 = 32768;

impl Connection {
    pub(crate) fn set_attributes(
        &self,
        q: NodeSetAttributesRequest,
    ) -> Result<NodeSetAttributesResponse, Error> {
        self.require_write()?;
        let changes = Changes {
            mode: q
                .mask
                .contains(AttributeMask::MODE)
                .then_some(q.mode & MODE_BITS),
            accessed: q
                .mask
                .contains(AttributeMask::ACCESSED)
                .then(|| fs::time(q.accessed)),
            modified: q
                .mask
                .contains(AttributeMask::MODIFIED)
                .then(|| fs::time(q.modified)),
            owner: None,
            group: None,
        };
        self.open.node.set_attributes(&changes).map_err(error)?;
        self.fs.changed(&self.open, WatchKind::Attributes);
        Ok(NodeSetAttributesResponse {})
    }

    pub(crate) fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.fs
            .serve(q.node, self.open.clone(), q.rights, self.user, self.append);
        Ok(NodeCloneResponse {})
    }

    /// The bytes of the file from `offset`, at most `count` of them.
    pub(crate) fn read(&self, offset: u64, count: u32) -> Result<Vec<u8>, Error> {
        self.require(Rights::READ)?;
        self.file()?;
        let mut data = vec![0u8; count.min(READ_MAX) as usize];
        let n = self.open.node.read(offset, &mut data).map_err(error)?;
        data.truncate(n);
        Ok(data)
    }

    pub(crate) fn read_next(&mut self, count: u32) -> Result<NodeReadNextResponse, Error> {
        let data = self.read(self.cursor, count)?;
        self.cursor += data.len() as u64;
        Ok(NodeReadNextResponse { data })
    }

    /// Writes `data` at `offset`, or at the end without one: how many
    /// bytes it wrote, and where.
    pub(crate) fn write(&self, offset: Option<u64>, data: &[u8]) -> Result<(u32, u64), Error> {
        self.require_write()?;
        self.file()?;
        let mut done = 0;
        let mut at = offset;
        let mut start = None;
        // The shim writes a piece at a time; one request writes all.
        while done < data.len() {
            let (n, wrote_at) = match at {
                Some(at) => (self.open.node.write(at, &data[done..]).map_err(error)?, at),
                None => self.open.node.append(&data[done..]).map_err(error)?,
            };
            start.get_or_insert(wrote_at);
            done += n;
            at = Some(wrote_at + n as u64);
            if n == 0 {
                break;
            }
        }
        self.fs.changed(&self.open, WatchKind::Modified);
        Ok((done as u32, start.unwrap_or(offset.unwrap_or(0))))
    }

    pub(crate) fn write_next(&mut self, data: &[u8]) -> Result<NodeWriteNextResponse, Error> {
        let offset = if self.append { None } else { Some(self.cursor) };
        let (written, at) = self.write(offset, data)?;
        self.cursor = at + u64::from(written);
        Ok(NodeWriteNextResponse { written })
    }

    pub(crate) fn truncate(&self, size: u64) -> Result<(), Error> {
        self.require_write()?;
        self.file()?;
        self.open.node.truncate(size).map_err(error)?;
        self.fs.changed(&self.open, WatchKind::Modified);
        Ok(())
    }

    /// Makes the range of the file from `offset` share the data of the
    /// range of the file that the connection `source` is open to, which
    /// needs `READ`, without copying it, as btrfs reflinks do. A source
    /// that is no connection of this server is on another file system.
    pub(crate) fn clone_range(&self, q: NodeCloneRangeRequest) -> Result<(), Error> {
        self.require_write()?;
        self.file()?;
        let (_, server_end) = koids(q.source.handle()).ok_or(Error::Invalid)?;
        let connection = self.fs.connections.borrow().get(&server_end).cloned();
        let (source, rights) = connection.ok_or(Error::CrossDevice)?;
        if !rights.contains(Rights::READ) {
            return Err(Error::AccessDenied);
        }
        self.open
            .node
            .clone_range(q.offset, &source.node, q.source_offset, q.length)
            .map_err(error)?;
        self.fs.changed(&self.open, WatchKind::Modified);
        Ok(())
    }

    pub(crate) fn seek(
        &mut self,
        offset: i64,
        origin: SeekOrigin,
    ) -> Result<NodeSeekResponse, Error> {
        self.file().map_err(|_| Error::Invalid)?;
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => self.cursor,
            SeekOrigin::End => self.open.attributes()?.size,
            SeekOrigin::Unknown(_) => return Err(Error::Invalid),
        };
        let position = i128::from(base) + i128::from(offset);
        self.cursor = u64::try_from(position).map_err(|_| Error::Invalid)?;
        Ok(NodeSeekResponse {
            position: self.cursor,
        })
    }

    pub(crate) fn read_link(&self) -> Result<NodeReadLinkResponse, Error> {
        self.require(Rights::READ)?;
        Ok(NodeReadLinkResponse {
            target: link_target(&self.open)?,
        })
    }
}
