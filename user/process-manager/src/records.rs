//! The records of the processes the process manager knows.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use mikros_rt::interfaces::__midl::OwnedHandle;

/// The process ID that children of a terminated process get as parent.
pub const INIT_PID: u32 = 1;
/// The highest process ID.
const PID_MAX: u32 = i32::MAX as u32;

/// What the process manager keeps about one process.
pub struct Record {
    pub koid: u64,
    pub parent: u32,
    pub process_group: u32,
    pub session: u32,
    pub app_id: String,
    pub user: u32,
    pub login_session: u32,
    #[expect(dead_code, reason = "kept so that the budget lives")]
    pub budget: OwnedHandle,
    #[expect(dead_code, reason = "read once GetInfo is served")]
    pub path: String,
    pub process: OwnedHandle,
    /// How the process ended, once it did.
    pub exit: Option<Exit>,
}

/// How a process ended: its exit code, or the signal that terminated
/// it.
#[derive(Clone, Copy)]
pub struct Exit {
    pub code: i64,
    pub signal: u32,
}

/// The records by process ID.
#[derive(Default)]
pub struct Records {
    by_pid: BTreeMap<u32, Record>,
    /// IDs given to processes that are being started.
    reserved: BTreeSet<u32>,
}

impl Records {
    /// The lowest process ID above every ID in use or reserved, never 0.
    pub fn next_pid(&self) -> u32 {
        let used = self.by_pid.keys().next_back().copied().unwrap_or(0);
        let reserved = self.reserved.last().copied().unwrap_or(0);
        used.max(reserved) + 1
    }

    /// Reserves the next process ID for a process being started, or
    /// returns `None` when none is left.
    pub fn reserve(&mut self) -> Option<u32> {
        let pid = self.next_pid();
        if pid > PID_MAX {
            return None;
        }
        self.reserved.insert(pid);
        Some(pid)
    }

    /// Gives up the reservation of `pid`.
    pub fn release(&mut self, pid: u32) {
        self.reserved.remove(&pid);
    }

    /// Adds `record` under `pid`, unless that ID is in use, and ends its
    /// reservation.
    pub fn insert(&mut self, pid: u32, record: Record) -> bool {
        self.reserved.remove(&pid);
        if self.by_pid.contains_key(&pid) {
            return false;
        }
        self.by_pid.insert(pid, record);
        true
    }

    /// Puts `record` in place of the record of `pid`, which must be of
    /// a running process, keeping that process's children, and returns
    /// the record it replaced.
    pub fn replace(&mut self, pid: u32, record: Record) -> Option<Record> {
        let old = self.by_pid.get_mut(&pid)?;
        if old.exit.is_some() {
            return None;
        }
        Some(core::mem::replace(old, record))
    }

    pub fn get(&self, pid: u32) -> Option<&Record> {
        self.by_pid.get(&pid)
    }

    /// Whether `pid` is a descendant of `ancestor`.
    pub fn descends_from(&self, pid: u32, ancestor: u32) -> bool {
        let mut at = pid;
        // Each step moves to a parent, and the chain ends at 0, so at
        // most every record is visited once.
        for _ in 0..=self.by_pid.len() {
            match self.by_pid.get(&at) {
                Some(record) if record.parent == ancestor => return true,
                Some(record) if record.parent != 0 => at = record.parent,
                _ => return false,
            }
        }
        false
    }

    /// The children of `parent` that `selector` chooses: the child with
    /// that ID when positive, children in the parent's process group
    /// when 0, every child when -1, and children in process group
    /// `-selector` otherwise.
    pub fn children(&self, parent: u32, selector: i32) -> impl Iterator<Item = (u32, &Record)> {
        let group = match selector {
            0 => self.by_pid.get(&parent).map(|r| r.process_group),
            s if s < -1 => Some(s.unsigned_abs()),
            _ => None,
        };
        self.by_pid
            .iter()
            .filter(move |&(&pid, record)| {
                record.parent == parent
                    && match selector {
                        s if s > 0 => pid == s as u32,
                        -1 => true,
                        _ => Some(record.process_group) == group,
                    }
            })
            .map(|(&pid, record)| (pid, record))
    }

    /// Removes the record of `pid` and returns how it ended.
    pub fn collect(&mut self, pid: u32) -> Option<Exit> {
        let exit = self.by_pid.get(&pid)?.exit?;
        self.by_pid.remove(&pid);
        Some(exit)
    }

    /// Keeps `exit` as how process `pid` ended, drops the uncollected
    /// statuses of its children, and gives its other children process
    /// ID 1 as their parent.
    pub fn exited(&mut self, pid: u32, exit: Exit) {
        if let Some(record) = self.by_pid.get_mut(&pid) {
            record.exit = Some(exit);
        }
        self.by_pid
            .retain(|_, record| record.parent != pid || record.exit.is_none());
        for record in self.by_pid.values_mut() {
            if record.parent == pid {
                record.parent = INIT_PID;
            }
        }
    }
}
