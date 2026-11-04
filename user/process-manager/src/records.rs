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
}

/// The records by process ID.
#[derive(Default)]
pub struct Records {
    by_pid: BTreeMap<u32, Record>,
}

impl Records {
    /// Adds `record` under `pid`, unless that ID is in use.
    pub fn insert(&mut self, pid: u32, record: Record) -> bool {
        if self.by_pid.contains_key(&pid) {
            return false;
        }
        self.by_pid.insert(pid, record);
        true
    }
}
