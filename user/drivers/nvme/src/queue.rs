//! Queue pairs: a submission queue and its completion queue, each in
//! contiguous memory, the commands in flight on them by command ID, and
//! the phase tag that tells new completions from old ones.

use crate::regs::Regs;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::sync::atomic::{Ordering, fence};
use core::task::{Poll, Waker};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;
use mikros_storage::{Contiguous, now};

/// The sizes of submission and completion entries.
const SQE: u64 = 64;
const CQE: u64 = 16;

/// A command, without its command ID, which the queue assigns.
#[derive(Clone, Copy, Debug, Default)]
pub struct Command {
    pub opcode: u8,
    pub nsid: u32,
    pub prp1: u64,
    pub prp2: u64,
    /// Command dwords 10 to 15.
    pub cdw: [u32; 6],
}

/// Why a command did not succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The controller completed it with this status field: the status
    /// code type and status code, without the phase tag.
    Status(u16),
    /// The controller was reset before it completed.
    Reset,
}

impl From<Failure> for Status {
    fn from(_: Failure) -> Status {
        Status::Internal
    }
}

/// Where aborting a command stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abort {
    /// No abort was sent for it.
    None,
    /// An abort was sent and has not completed.
    Sent,
    /// An abort for it succeeded at this time.
    Done(i64),
}

/// A command in flight: its opcode, when it was submitted, where
/// aborting it stands, its result once it completes, and the task
/// waiting for it.
struct InFlight {
    opcode: u8,
    since: i64,
    abort: Abort,
    result: Option<Result<u32, Failure>>,
    waker: Option<Waker>,
}

struct State {
    tail: u16,
    head: u16,
    phase: bool,
    free: Vec<u16>,
    in_flight: BTreeMap<u16, InFlight>,
    waiting: Vec<Waker>,
    /// Set while the controller is being reset: submissions wait.
    paused: bool,
}

/// A queue pair of `entries` entries with ID `id`.
pub struct QueuePair {
    pub id: u16,
    pub entries: u16,
    pub submission: Contiguous,
    pub completion: Contiguous,
    regs: Rc<Regs>,
    state: RefCell<State>,
}

impl QueuePair {
    /// A queue pair with ID `id` and `entries` entries, in memory from
    /// the `DMA` resource `dma`, mapped into `process` and pinned through
    /// `bti`. At most `entries - 1` commands are in flight at once, so
    /// that the submission queue never fills.
    pub fn new(
        id: u16,
        entries: u16,
        regs: Rc<Regs>,
        dma: &OwnedHandle,
        bti: &OwnedHandle,
        process: sys::Handle,
    ) -> Result<QueuePair, Status> {
        let n = u64::from(entries);
        let submission = Contiguous::new(dma, bti, process, SQE * n, false)?;
        let completion = Contiguous::new(dma, bti, process, CQE * n, false)?;
        Ok(QueuePair {
            id,
            entries,
            submission,
            completion,
            regs,
            state: RefCell::new(State {
                tail: 0,
                head: 0,
                phase: true,
                free: (0..entries - 1).rev().collect(),
                in_flight: BTreeMap::new(),
                waiting: Vec::new(),
                paused: false,
            }),
        })
    }

    /// Submits `command` and waits until it completes. Returns Dword 0
    /// of its completion.
    pub async fn submit(&self, command: Command) -> Result<u32, Failure> {
        let cid = poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if !state.paused
                && let Some(cid) = state.free.pop()
            {
                return Poll::Ready(cid);
            }
            state.waiting.push(cx.waker().clone());
            Poll::Pending
        })
        .await;
        {
            let mut state = self.state.borrow_mut();
            let at = u64::from(state.tail) * SQE;
            let sq = &self.submission;
            sq.write32(at, u32::from(command.opcode) | u32::from(cid) << 16);
            sq.write32(at + 4, command.nsid);
            sq.write64(at + 8, 0);
            sq.write64(at + 16, 0);
            sq.write64(at + 24, command.prp1);
            sq.write64(at + 32, command.prp2);
            for (i, dword) in command.cdw.iter().enumerate() {
                sq.write32(at + 40 + 4 * i as u64, *dword);
            }
            state.in_flight.insert(
                cid,
                InFlight {
                    opcode: command.opcode,
                    since: now(),
                    abort: Abort::None,
                    result: None,
                    waker: None,
                },
            );
            state.tail = (state.tail + 1) % self.entries;
            fence(Ordering::SeqCst);
            self.regs.ring_submission(self.id, state.tail);
        }
        let result = poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            let command = state
                .in_flight
                .get_mut(&cid)
                .expect("the command is in flight");
            match command.result {
                Some(result) => Poll::Ready(result),
                None => {
                    command.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await;
        self.release(cid);
        result
    }

    /// Makes `cid` free again, and wakes the tasks waiting for one.
    fn release(&self, cid: u16) {
        let waiting = {
            let mut state = self.state.borrow_mut();
            state.in_flight.remove(&cid);
            state.free.push(cid);
            core::mem::take(&mut state.waiting)
        };
        for waker in waiting {
            waker.wake();
        }
    }

    /// Takes the new entries of the completion queue, completes their
    /// commands, and moves the completion queue head past them.
    pub fn process(&self) {
        let mut wakers = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            let mut moved = false;
            loop {
                let at = u64::from(state.head) * CQE;
                let dw3 = self.completion.read32(at + 12);
                if (dw3 & 1 << 16 != 0) != state.phase {
                    break;
                }
                fence(Ordering::SeqCst);
                let dw0 = self.completion.read32(at);
                let cid = dw3 as u16;
                let status = (dw3 >> 17) as u16;
                if let Some(command) = state.in_flight.get_mut(&cid)
                    && command.result.is_none()
                {
                    command.result = Some(match status {
                        0 => Ok(dw0),
                        status => Err(Failure::Status(status)),
                    });
                    wakers.extend(command.waker.take());
                }
                state.head += 1;
                if state.head == self.entries {
                    state.head = 0;
                    state.phase = !state.phase;
                }
                moved = true;
            }
            if moved {
                self.regs.ring_completion(self.id, state.head);
            }
        }
        for waker in wakers {
            waker.wake();
        }
    }

    /// The commands that have been in flight for longer than `limit`
    /// nanoseconds: their IDs, opcodes, and where aborting them stands.
    pub fn overdue(&self, limit: i64) -> Vec<(u16, u8, Abort)> {
        let now = now();
        let state = self.state.borrow();
        state
            .in_flight
            .iter()
            .filter(|(_, c)| c.result.is_none() && now - c.since > limit)
            .map(|(cid, c)| (*cid, c.opcode, c.abort))
            .collect()
    }

    /// Records where aborting `cid` stands.
    pub fn set_abort(&self, cid: u16, abort: Abort) {
        if let Some(command) = self.state.borrow_mut().in_flight.get_mut(&cid) {
            command.abort = abort;
        }
    }

    /// Stops new submissions until `resume`.
    pub fn pause(&self) {
        self.state.borrow_mut().paused = true;
    }

    /// Fails every command in flight with `Failure::Reset`, and starts
    /// the queues from their first entries again, once the controller
    /// has been reset.
    pub fn restart(&self) {
        let mut wakers = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            for command in state.in_flight.values_mut() {
                if command.result.is_none() {
                    command.result = Some(Err(Failure::Reset));
                    wakers.extend(command.waker.take());
                }
            }
            state.tail = 0;
            state.head = 0;
            state.phase = true;
        }
        self.submission.zero();
        self.completion.zero();
        for waker in wakers {
            waker.wake();
        }
    }

    /// Lets submissions go on after `pause`.
    pub fn resume(&self) {
        let waiting = {
            let mut state = self.state.borrow_mut();
            state.paused = false;
            core::mem::take(&mut state.waiting)
        };
        for waker in waiting {
            waker.wake();
        }
    }
}
