//! Access control lists of nodes (MIKSPEC-0049.R25, MIKSPEC-0049.R26):
//! the list that a mode gives, the check of a list, the mode that a list
//! sets, which access an extended list allows, and the format of
//! `system.posix_acl_access` and `system.posix_acl_default` that Linux
//! uses, with `USER` entries as named user entries and `APP` entries as
//! named group entries (MIKSPEC-0053.R16).

#![no_std]

extern crate alloc;

#[cfg(feature = "midl")]
pub mod midl;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

/// What an entry is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tag {
    Owner,
    User,
    Group,
    App,
    Mask,
    Other,
}

/// The rights bits, as `mikros.fs.Rights` has them.
pub const READ: u32 = 0x1;
pub const WRITE: u32 = 0x2;
pub const EXECUTE: u32 = 0x4;
const ALL: u32 = READ | WRITE | EXECUTE;

/// An entry: its tag, the user ID or app number of a `USER` or `APP`
/// entry, 0 otherwise, and its rights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub tag: Tag,
    pub id: u32,
    pub rights: u32,
}

impl Entry {
    pub const fn new(tag: Tag, id: u32, rights: u32) -> Entry {
        Entry { tag, id, rights }
    }
}

/// The rights of the permission bits `bits`, `rwx` from the highest.
fn rights_of(bits: u32) -> u32 {
    (if bits & 4 != 0 { READ } else { 0 })
        | (if bits & 2 != 0 { WRITE } else { 0 })
        | (if bits & 1 != 0 { EXECUTE } else { 0 })
}

/// The permission bits of `rights`.
fn bits_of(rights: u32) -> u32 {
    (if rights & READ != 0 { 4 } else { 0 })
        | (if rights & WRITE != 0 { 2 } else { 0 })
        | (if rights & EXECUTE != 0 { 1 } else { 0 })
}

/// The `OWNER`, `GROUP` and `OTHER` entries that `mode` gives.
pub fn from_mode(mode: u32) -> Vec<Entry> {
    alloc::vec![
        Entry::new(Tag::Owner, 0, rights_of(mode >> 6)),
        Entry::new(Tag::Group, 0, rights_of(mode >> 3)),
        Entry::new(Tag::Other, 0, rights_of(mode)),
    ]
}

/// Whether `entries` has entries other than `OWNER`, `GROUP` and
/// `OTHER`.
pub fn is_extended(entries: &[Entry]) -> bool {
    entries
        .iter()
        .any(|e| !matches!(e.tag, Tag::Owner | Tag::Group | Tag::Other))
}

/// `entries` in order, by tag and then ID, when they form a list: one
/// `OWNER`, one `GROUP` and one `OTHER` entry with ID 0, `USER` and `APP`
/// entries with distinct IDs, and one `MASK` entry with ID 0 exactly when
/// there are `USER` or `APP` entries, each with only rights bits; `None`
/// otherwise.
pub fn checked(entries: &[Entry]) -> Option<Vec<Entry>> {
    let mut sorted: Vec<Entry> = entries.to_vec();
    sorted.sort_by_key(|e| (e.tag, e.id));
    let count = |tag| sorted.iter().filter(|e| e.tag == tag).count();
    let named = count(Tag::User) + count(Tag::App);
    let valid = count(Tag::Owner) == 1
        && count(Tag::Group) == 1
        && count(Tag::Other) == 1
        && count(Tag::Mask) == usize::from(named > 0)
        && sorted.iter().all(|e| e.rights & !ALL == 0)
        && sorted
            .iter()
            .all(|e| matches!(e.tag, Tag::User | Tag::App) || e.id == 0)
        && sorted
            .windows(2)
            .all(|w| (w[0].tag, w[0].id) != (w[1].tag, w[1].id));
    valid.then_some(sorted)
}

fn rights(entries: &[Entry], tag: Tag) -> u32 {
    entries
        .iter()
        .find(|e| e.tag == tag)
        .map_or(0, |e| e.rights)
}

/// The mode that the list `entries` sets, keeping the bits of `mode`
/// above the permission bits: the owner's from `OWNER`, the group's from
/// `MASK` or, without one, `GROUP`, and the others' from `OTHER`.
pub fn mode_of(entries: &[Entry], mode: u32) -> u32 {
    let group = match entries.iter().any(|e| e.tag == Tag::Mask) {
        true => rights(entries, Tag::Mask),
        false => rights(entries, Tag::Group),
    };
    (mode & !0o777)
        | bits_of(rights(entries, Tag::Owner)) << 6
        | bits_of(group) << 3
        | bits_of(rights(entries, Tag::Other))
}

/// Whether the extended list `entries` of a node owned by `owner` allows
/// the subject `user` and `app` the rights `wanted`: whether one of the
/// subject's entries grants each of them, `USER` and `APP` entries only
/// as far as the `MASK` entry also grants, and never the `GROUP` entry.
pub fn allows(entries: &[Entry], owner: u32, user: u32, app: u32, wanted: u32) -> bool {
    let mask = rights(entries, Tag::Mask);
    let mut granted = rights(entries, Tag::Other);
    for entry in entries {
        granted |= match entry.tag {
            Tag::Owner if user == owner => entry.rights,
            Tag::User if entry.id == user => entry.rights & mask,
            Tag::App if entry.id == app => entry.rights & mask,
            _ => 0,
        };
    }
    granted & wanted == wanted
}

/// Whom a node connection is for: the user ID, app number, process ID,
/// kernel object ID and `admin` of a process (MIKSPEC-0049.R30).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subject {
    pub user: u32,
    pub app: u32,
    pub pid: u32,
    pub koid: u64,
    pub admin: u32,
}

/// The owner of a node that a connection with `subject` makes: the
/// subject's user, or 0 without one.
pub fn new_owner(subject: Option<&Subject>) -> u32 {
    subject.map_or(0, |s| s.user)
}

/// Whether a connection with `subject` may do what needs `wanted` to a
/// node owned by `owner` whose access list is `entries`, if it has an
/// extended one: always without a subject, for user 0 and without an
/// extended list, and otherwise as `allows` says.
pub fn permits(
    subject: Option<&Subject>,
    entries: Option<&[Entry]>,
    owner: u32,
    wanted: u32,
) -> bool {
    match (subject, entries) {
        (Some(s), Some(entries)) if s.user != 0 && is_extended(entries) => {
            allows(entries, owner, s.user, s.app, wanted)
        }
        _ => true,
    }
}

/// Whether a connection with `subject` needs `admin.acls.manage` to set
/// the lists of a node owned by `owner`: unless it has no subject, or
/// one of user 0 or the owner.
pub fn needs_manage(subject: Option<&Subject>, owner: u32) -> bool {
    subject.is_some_and(|s| s.user != 0 && s.user != owner)
}

/// What a new node gets in a directory whose default list is `default`:
/// its access list, if extended, and its mode from `mode`, and, for a
/// directory, its default list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inherited {
    pub access: Option<Vec<Entry>>,
    pub mode: u32,
    pub default: Option<Vec<Entry>>,
}

pub fn inherit(default: Option<&[Entry]>, mode: u32, directory: bool) -> Inherited {
    match default {
        None => Inherited {
            access: None,
            mode,
            default: None,
        },
        Some(list) => Inherited {
            access: is_extended(list).then(|| list.to_vec()),
            mode: mode_of(list, mode),
            default: directory.then(|| list.to_vec()),
        },
    }
}

/// The version of the format of the extended attributes.
const VERSION: u32 = 2;
/// Linux's tags, and the ID of an entry without one.
const USER_OBJ: u16 = 0x01;
const USER: u16 = 0x02;
const GROUP_OBJ: u16 = 0x04;
const GROUP: u16 = 0x08;
const MASK: u16 = 0x10;
const OTHER: u16 = 0x20;
const UNDEFINED_ID: u32 = u32::MAX;

/// `entries`, a checked list, as the value of `system.posix_acl_access`
/// or `system.posix_acl_default`.
pub fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + 8 * entries.len());
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    for entry in entries {
        let (tag, id) = match entry.tag {
            Tag::Owner => (USER_OBJ, UNDEFINED_ID),
            Tag::User => (USER, entry.id),
            Tag::Group => (GROUP_OBJ, UNDEFINED_ID),
            Tag::App => (GROUP, entry.id),
            Tag::Mask => (MASK, UNDEFINED_ID),
            Tag::Other => (OTHER, UNDEFINED_ID),
        };
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&(bits_of(entry.rights) as u16).to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    bytes
}

/// The list in the value `bytes` of `system.posix_acl_access` or
/// `system.posix_acl_default`, checked, or `None` when it holds no list.
pub fn decode(bytes: &[u8]) -> Option<Vec<Entry>> {
    let (version, rest) = bytes.split_first_chunk::<4>()?;
    let (raws, tail) = rest.as_chunks::<8>();
    if u32::from_le_bytes(*version) != VERSION || !tail.is_empty() {
        return None;
    }
    let mut entries = Vec::with_capacity(raws.len());
    for raw in raws {
        let tag = u16::from_le_bytes([raw[0], raw[1]]);
        let bits = u16::from_le_bytes([raw[2], raw[3]]);
        let id = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let (tag, id) = match tag {
            USER_OBJ => (Tag::Owner, 0),
            USER => (Tag::User, id),
            GROUP_OBJ => (Tag::Group, 0),
            GROUP => (Tag::App, id),
            MASK => (Tag::Mask, 0),
            OTHER => (Tag::Other, 0),
            _ => return None,
        };
        entries.push(Entry::new(tag, id, rights_of(u32::from(bits))));
    }
    checked(&entries)
}
