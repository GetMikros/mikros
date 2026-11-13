// SPDX-License-Identifier: GPL-2.0
//! Byte-range locks of connections (MIKSPEC-0049.R10), as POSIX `fcntl`
//! record locks.

use crate::driver::fs::{Fs, Open};
use crate::driver::node::Connection;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::Poll;
use core::task::Waker;
use mikros_rt::interfaces::mikros::fs::*;

impl Connection {
    /// Takes a byte-range lock of the file, waiting for it when asked to
    /// and otherwise failing with `WOULD_BLOCK` when it conflicts. A
    /// shared lock needs `READ`, and an exclusive one `WRITE`, as `fcntl`
    /// does.
    pub(crate) async fn lock(&self, q: NodeLockRequest) -> Result<(), Error> {
        self.file()?;
        let exclusive = match q.kind {
            LockKind::Shared => false,
            LockKind::Exclusive => true,
            LockKind::Unknown(_) => return Err(Error::Invalid),
        };
        self.require(if exclusive {
            Rights::WRITE
        } else {
            Rights::READ
        })?;
        let range = range(q.start, q.length)?;
        poll_fn(|cx| {
            let waker = q.wait.then_some(cx.waker());
            match self
                .fs
                .try_lock(&self.open, self.owner, exclusive, range, waker)
            {
                true => Poll::Ready(Ok(())),
                false if q.wait => Poll::Pending,
                false => Poll::Ready(Err(Error::WouldBlock)),
            }
        })
        .await
    }

    pub(crate) fn unlock(&self, start: u64, length: u64) -> Result<(), Error> {
        self.file()?;
        self.fs
            .unlock(&self.open, self.owner, range(start, length)?);
        Ok(())
    }
}

/// The range of a lock from `start` with `length` bytes, 0 meaning to the
/// end of the file, `end` excluded.
fn range(start: u64, length: u64) -> Result<(u64, u64), Error> {
    match length {
        0 => Ok((start, u64::MAX)),
        n => Ok((start, start.checked_add(n).ok_or(Error::Invalid)?)),
    }
}

/// A byte-range lock: its owner, a connection, whether it is exclusive,
/// and its range, `end` excluded.
#[derive(Clone, Copy)]
pub(crate) struct Lock {
    owner: u64,
    exclusive: bool,
    start: u64,
    end: u64,
}

impl Fs {
    /// Takes the lock of `owner` on `start..end` of `open`, replacing its
    /// own locks there, unless another owner's lock conflicts: then false,
    /// and `waker`, when given, is woken once a lock goes.
    pub fn try_lock(
        &self,
        open: &Open,
        owner: u64,
        exclusive: bool,
        (start, end): (u64, u64),
        waker: Option<&Waker>,
    ) -> bool {
        let mut locks = open.locks.borrow_mut();
        let conflicts = locks.iter().any(|l| {
            l.owner != owner && l.start < end && start < l.end && (exclusive || l.exclusive)
        });
        if conflicts {
            if let Some(waker) = waker {
                open.waiting.borrow_mut().push(waker.clone());
            }
            return false;
        }
        cut(&mut locks, owner, start, end);
        locks.push(Lock {
            owner,
            exclusive,
            start,
            end,
        });
        true
    }

    /// Releases the locks of `owner` on `start..end` of `open`.
    pub fn unlock(&self, open: &Open, owner: u64, (start, end): (u64, u64)) {
        cut(&mut open.locks.borrow_mut(), owner, start, end);
        for waker in open.waiting.borrow_mut().drain(..) {
            waker.wake();
        }
    }
}

/// Removes the parts of the locks of `owner` in `locks` that lie in
/// `start..end`, keeping what lies outside.
fn cut(locks: &mut Vec<Lock>, owner: u64, start: u64, end: u64) {
    let mut kept = Vec::with_capacity(locks.len());
    for lock in locks.drain(..) {
        if lock.owner != owner || lock.end <= start || end <= lock.start {
            kept.push(lock);
            continue;
        }
        if lock.start < start {
            kept.push(Lock { end: start, ..lock });
        }
        if end < lock.end {
            kept.push(Lock { start: end, ..lock });
        }
    }
    *locks = kept;
}
