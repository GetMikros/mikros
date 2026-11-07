//! The user records of the accounts service: their IDs, names, full
//! names, kinds and password hashes, the checks made when they change,
//! and the file that holds them.

#![no_std]

extern crate alloc;

mod file;

use alloc::collections::BTreeMap;
use alloc::string::String;

pub use file::{Corrupt, decode, encode};

/// The ID of the first user.
pub const FIRST_ID: u32 = 1000;
/// The highest user ID.
pub const LAST_ID: u32 = (1 << 31) - 1;
/// The longest name and full name, in bytes.
pub const NAME_MAX: usize = 32;
pub const FULL_NAME_MAX: usize = 128;
/// The most users one listing returns.
pub const LIST_MAX: usize = 64;

/// A user's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Standard,
    Admin,
}

/// A user's record. `hash` is the user's password hash, in the PHC
/// string format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: u32,
    pub name: String,
    pub full_name: String,
    pub kind: Kind,
    pub hash: String,
}

/// Why a change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The name does not have the form of a user's name, or the full
    /// name is too long.
    Invalid,
    /// A user of that name exists.
    Exists,
    /// No user has that ID.
    NotFound,
    /// The change would leave no administrator.
    LastAdmin,
    /// Every user ID has been assigned.
    Exhausted,
}

/// The users, and the ID that the next one gets. IDs are assigned in
/// increasing order and never again once a user is deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Users {
    next: u32,
    users: BTreeMap<u32, Record>,
}

impl Default for Users {
    fn default() -> Users {
        Users::new()
    }
}

impl Users {
    /// No users, with `FIRST_ID` next.
    pub fn new() -> Users {
        Users {
            next: FIRST_ID,
            users: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// The ID that the next user gets, or `LAST_ID + 1` once every ID
    /// has been assigned.
    pub fn next_id(&self) -> u32 {
        self.next
    }

    /// The users with IDs above `after`, in increasing order of ID, at
    /// most `LIST_MAX` of them.
    pub fn list(&self, after: u32) -> impl Iterator<Item = &Record> {
        let start = after.saturating_add(1);
        let range = (after < u32::MAX).then(|| self.users.range(start..));
        range.into_iter().flatten().map(|(_, r)| r).take(LIST_MAX)
    }

    pub fn get(&self, id: u32) -> Option<&Record> {
        self.users.get(&id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Record> {
        self.users.values().find(|r| r.name == name)
    }

    /// Adds a user with the next ID and returns that ID.
    pub fn create(
        &mut self,
        name: &str,
        full_name: &str,
        kind: Kind,
        hash: String,
    ) -> Result<u32, Refusal> {
        if !valid_name(name) || full_name.len() > FULL_NAME_MAX {
            return Err(Refusal::Invalid);
        }
        if self.by_name(name).is_some() {
            return Err(Refusal::Exists);
        }
        if self.next > LAST_ID {
            return Err(Refusal::Exhausted);
        }
        let id = self.next;
        self.next += 1;
        let record = Record {
            id,
            name: String::from(name),
            full_name: String::from(full_name),
            kind,
            hash,
        };
        self.users.insert(id, record);
        Ok(id)
    }

    /// Removes the user `id` and returns its record.
    pub fn delete(&mut self, id: u32) -> Result<Record, Refusal> {
        let record = self.users.get(&id).ok_or(Refusal::NotFound)?;
        if record.kind == Kind::Admin && self.admins() == 1 {
            return Err(Refusal::LastAdmin);
        }
        self.users.remove(&id).ok_or(Refusal::NotFound)
    }

    pub fn set_kind(&mut self, id: u32, kind: Kind) -> Result<(), Refusal> {
        let admins = self.admins();
        let record = self.users.get_mut(&id).ok_or(Refusal::NotFound)?;
        if record.kind == Kind::Admin && kind != Kind::Admin && admins == 1 {
            return Err(Refusal::LastAdmin);
        }
        record.kind = kind;
        Ok(())
    }

    pub fn set_full_name(&mut self, id: u32, full_name: &str) -> Result<(), Refusal> {
        if full_name.len() > FULL_NAME_MAX {
            return Err(Refusal::Invalid);
        }
        let record = self.users.get_mut(&id).ok_or(Refusal::NotFound)?;
        record.full_name = String::from(full_name);
        Ok(())
    }

    pub fn set_hash(&mut self, id: u32, hash: String) -> Result<(), Refusal> {
        let record = self.users.get_mut(&id).ok_or(Refusal::NotFound)?;
        record.hash = hash;
        Ok(())
    }

    /// How many users are administrators.
    fn admins(&self) -> usize {
        self.users
            .values()
            .filter(|r| r.kind == Kind::Admin)
            .count()
    }
}

/// Whether `name` has the form of a user's name: a lowercase ASCII
/// letter, then at most 31 lowercase ASCII letters, digits and `-`.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.split_first() {
        Some((first, rest)) => {
            first.is_ascii_lowercase()
                && rest.len() < NAME_MAX
                && rest
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests;
