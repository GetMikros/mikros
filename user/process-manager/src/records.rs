//! The records of the processes the process manager knows.

use alloc::collections::BTreeMap;
use alloc::string::String;
use mikros_rt::interfaces::__midl::OwnedHandle;

/// What the process manager keeps about one process.
#[expect(dead_code, reason = "read once the process manager serves Launcher")]
pub struct Record {
    pub koid: u64,
    pub parent: u32,
    pub app_id: String,
    pub user: u32,
    pub login_session: u32,
    pub budget: OwnedHandle,
    pub path: String,
    pub process: OwnedHandle,
    /// How the process ended, once it did.
    pub exit: Option<Exit>,
}

/// How a process ended: its exit code, or the signal that terminated
/// it.
#[derive(Clone, Copy)]
#[expect(dead_code, reason = "read once the process manager serves Wait")]
pub struct Exit {
    pub code: i64,
    pub signal: u32,
}

/// The records by process ID.
#[derive(Default)]
pub struct Records {
    by_pid: BTreeMap<u32, Record>,
}

impl Records {
    /// The lowest process ID above every ID in use, never 0.
    pub fn next_pid(&self) -> u32 {
        self.by_pid.keys().next_back().map_or(1, |&pid| pid + 1)
    }

    /// Adds `record` under `pid`, unless that ID is in use.
    pub fn insert(&mut self, pid: u32, record: Record) -> bool {
        if self.by_pid.contains_key(&pid) {
            return false;
        }
        self.by_pid.insert(pid, record);
        true
    }

    /// Keeps `exit` as how process `pid` ended.
    pub fn exited(&mut self, pid: u32, exit: Exit) {
        if let Some(record) = self.by_pid.get_mut(&pid) {
            record.exit = Some(exit);
        }
    }
}
