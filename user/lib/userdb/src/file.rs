//! The file that holds the users: a magic number and the next ID, then
//! each record in increasing order of ID. Numbers are little-endian, and
//! each string is its length in bytes followed by its UTF-8 bytes.

use crate::{FIRST_ID, FULL_NAME_MAX, Kind, LAST_ID, Record, Users, valid_name};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

const MAGIC: &[u8; 8] = b"MIKUSER1";
/// The longest password hash the file holds.
const HASH_MAX: usize = 1024;

/// The file is not one that `encode` wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corrupt;

/// The bytes of the file that holds `users`.
pub fn encode(users: &Users) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&users.next.to_le_bytes());
    out.extend_from_slice(&(users.users.len() as u32).to_le_bytes());
    for r in users.users.values() {
        out.extend_from_slice(&r.id.to_le_bytes());
        out.push(match r.kind {
            Kind::Standard => 1,
            Kind::Admin => 2,
        });
        for s in [&r.name, &r.full_name, &r.hash] {
            out.extend_from_slice(&(s.len() as u16).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
    }
    out
}

/// The users that `bytes`, written by `encode`, holds.
pub fn decode(bytes: &[u8]) -> Result<Users, Corrupt> {
    let mut r = Reader(bytes);
    if r.take(MAGIC.len())? != MAGIC {
        return Err(Corrupt);
    }
    let next = r.u32()?;
    if !(FIRST_ID..=LAST_ID + 1).contains(&next) {
        return Err(Corrupt);
    }
    let count = r.u32()?;
    let mut users = BTreeMap::new();
    let mut last = None;
    for _ in 0..count {
        let id = r.u32()?;
        if id < FIRST_ID || id >= next || last.is_some_and(|l| id <= l) {
            return Err(Corrupt);
        }
        last = Some(id);
        let kind = match r.take(1)? {
            [1] => Kind::Standard,
            [2] => Kind::Admin,
            _ => return Err(Corrupt),
        };
        let name = r.string()?;
        let full_name = r.string()?;
        let hash = r.string()?;
        let taken = users.values().any(|u: &Record| u.name == name);
        if !valid_name(&name) || taken || full_name.len() > FULL_NAME_MAX || hash.len() > HASH_MAX {
            return Err(Corrupt);
        }
        let record = Record {
            id,
            name,
            full_name,
            kind,
            hash,
        };
        users.insert(id, record);
    }
    if !r.0.is_empty() {
        return Err(Corrupt);
    }
    Ok(Users { next, users })
}

/// The bytes of a file not read yet.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Corrupt> {
        if self.0.len() < n {
            return Err(Corrupt);
        }
        let (taken, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, Corrupt> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn string(&mut self) -> Result<String, Corrupt> {
        let len = self.take(2)?;
        let len = u16::from_le_bytes([len[0], len[1]]) as usize;
        let bytes = self.take(len)?;
        let s = core::str::from_utf8(bytes).map_err(|_| Corrupt)?;
        Ok(String::from(s))
    }
}
