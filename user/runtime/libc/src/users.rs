//! Users and groups. A process has the user ID of its startup transfer,
//! which is also its group ID, the user's own group; an administrator
//! is also in the group 1, `admin`. The records of users and groups come
//! from the accounts service's `ListUsers`, for musl's `getpw*` and
//! `getgr*` functions, which reach them through `__mikros_user` and
//! `__mikros_group`: each user with its name, ID and full name, the
//! user's own group with the user's name, and `admin` with the
//! administrators as members. User 0 is `system`. No file is read.

use crate::errno::{EINVAL, EIO, ENOENT, ERANGE};
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char};
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::session::{Accounts, AccountsClient, UserKind};

const ACCOUNTS: &str = "mikros.session.Accounts";
const FOREVER: i64 = i64::MAX;
/// The group of the administrators, and the names of it and of user 0.
const ADMIN_GROUP: u32 = 1;
const ADMIN: &str = "admin";
const SYSTEM: &str = "system";
/// The sizes of the buffers that musl gives for a name and a full name,
/// with their NUL.
const NAME_SIZE: usize = 33;
const FULL_NAME_SIZE: usize = 129;

/// The process's user ID.
static USER: AtomicU32 = AtomicU32::new(0);

/// Takes `user`, from the startup transfer, as the process's user.
pub fn install(user: u32) {
    USER.store(user, Ordering::Relaxed);
}

/// `getuid`, `geteuid`, `getgid` and `getegid`: the process's user ID,
/// which is also its group ID.
pub fn user() -> i64 {
    i64::from(USER.load(Ordering::Relaxed))
}

/// A user as the accounts service lists it, with user 0 as `system`.
struct Record {
    id: u32,
    name: String,
    full_name: String,
    admin: bool,
}

/// Every user, from the accounts service, after `system`.
fn records() -> Result<Vec<Record>, i32> {
    let raw = crate::startup::directory();
    // SAFETY: the C library keeps owning the directory handle; the client
    // is never dropped, so it never closes it.
    let directory = ManuallyDrop::new(DirectoryClient::new(ClientEnd::new(unsafe {
        OwnedHandle::from_raw(raw)
    })));
    let (client, server) = endpoints::<Accounts>().map_err(|_| EIO as i32)?;
    match directory.connect(ACCOUNTS.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        _ => return Err(EIO as i32),
    }
    let accounts = AccountsClient::new(client);
    let mut records = alloc::vec![Record {
        id: 0,
        name: SYSTEM.into(),
        full_name: String::new(),
        admin: false,
    }];
    let mut after = 0;
    loop {
        let listed = accounts
            .list_users(after, FOREVER)
            .map_err(|_| EIO as i32)?
            .users;
        let Some(last) = listed.last() else {
            return Ok(records);
        };
        after = last.id;
        records.extend(listed.into_iter().map(|u| Record {
            id: u.id,
            admin: u.kind == UserKind::Admin,
            name: u.name,
            full_name: u.full_name,
        }));
    }
}

/// `getgroups`: the process's groups, its user's own and, for an
/// administrator, `admin`, stored at `list` when `size` is not 0.
pub fn getgroups(size: i64, list: *mut u32) -> i64 {
    let user = USER.load(Ordering::Relaxed);
    let admin = match records() {
        Ok(records) => records.iter().any(|r| r.id == user && r.admin),
        Err(e) => return -i64::from(e),
    };
    let groups: &[u32] = if admin { &[user, ADMIN_GROUP] } else { &[user] };
    if size == 0 {
        return groups.len() as i64;
    }
    if size < groups.len() as i64 {
        return -EINVAL;
    }
    // SAFETY: musl passes room for `size` group IDs.
    unsafe { core::ptr::copy_nonoverlapping(groups.as_ptr(), list, groups.len()) };
    groups.len() as i64
}

/// Copies `text` and a NUL into `out`, which holds `size` bytes.
///
/// # Safety
///
/// `out` must be valid for `size` bytes.
unsafe fn put(text: &str, out: *mut u8, size: usize) -> Result<(), i32> {
    if text.len() >= size {
        return Err(ERANGE as i32);
    }
    // SAFETY: the caller's buffer holds the text and its NUL.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len());
        out.add(text.len()).write(0);
    }
    Ok(())
}

/// The name that `name` points to, if it is not null.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe fn name_of<'a>(name: *const c_char) -> Option<&'a [u8]> {
    // SAFETY: the caller passes null or a NUL-terminated string.
    (!name.is_null()).then(|| unsafe { CStr::from_ptr(name) }.to_bytes())
}

/// `__mikros_user`: the user named `name`, or with ID `uid` when `name`
/// is null: its ID at `id`, its name in `user` and its full name in
/// `full`, of 33 and 129 bytes. Returns 0, `ENOENT` when there is no
/// such user, or another error number.
///
/// # Safety
///
/// The pointers must be as described.
#[unsafe(no_mangle)]
unsafe extern "C" fn __mikros_user(
    name: *const c_char,
    uid: u32,
    id: *mut u32,
    user: *mut u8,
    full: *mut u8,
) -> i32 {
    // SAFETY: musl passes null or a name.
    let name = unsafe { name_of(name) };
    let records = match records() {
        Ok(records) => records,
        Err(e) => return e,
    };
    let found = records.iter().find(|r| match name {
        Some(name) => r.name.as_bytes() == name,
        None => r.id == uid,
    });
    let Some(r) = found else {
        return ENOENT as i32;
    };
    // SAFETY: musl passes buffers of these sizes and room for the ID.
    let put = unsafe { put(&r.name, user, NAME_SIZE).and(put(&r.full_name, full, FULL_NAME_SIZE)) };
    match put {
        Ok(()) => {
            // SAFETY: as above.
            unsafe { id.write(r.id) };
            0
        }
        Err(e) => e,
    }
}

/// `__mikros_group`: the group named `name`, or with ID `gid` when
/// `name` is null: its ID at `id`, its name in `group`, of 33 bytes, and
/// its members in `members`, of `size` bytes, each ended by a NUL, and
/// then another NUL. Returns 0, `ENOENT` when there is no such group,
/// `ERANGE` when the members do not fit, or another error number.
///
/// # Safety
///
/// The pointers must be as described.
#[unsafe(no_mangle)]
unsafe extern "C" fn __mikros_group(
    name: *const c_char,
    gid: u32,
    id: *mut u32,
    group: *mut u8,
    members: *mut u8,
    size: usize,
) -> i32 {
    // SAFETY: musl passes null or a name.
    let name = unsafe { name_of(name) };
    let records = match records() {
        Ok(records) => records,
        Err(e) => return e,
    };
    let admins: Vec<&str> = records
        .iter()
        .filter(|r| r.admin)
        .map(|r| r.name.as_str())
        .collect();
    let is_admin = match name {
        Some(name) => name == ADMIN.as_bytes(),
        None => gid == ADMIN_GROUP,
    };
    let (found, listed): (Option<(u32, &str)>, &[&str]) = if is_admin {
        (Some((ADMIN_GROUP, ADMIN)), &admins)
    } else {
        let own = records.iter().find(|r| match name {
            Some(name) => r.name.as_bytes() == name,
            None => r.id == gid,
        });
        (own.map(|r| (r.id, r.name.as_str())), &[])
    };
    let Some((gid, group_name)) = found else {
        return ENOENT as i32;
    };
    let mut list = Vec::new();
    for member in listed {
        list.extend_from_slice(member.as_bytes());
        list.push(0);
    }
    list.push(0);
    if list.len() > size {
        return ERANGE as i32;
    }
    // SAFETY: musl passes buffers of these sizes and room for the ID.
    match unsafe { put(group_name, group, NAME_SIZE) } {
        Ok(()) => {
            // SAFETY: as above; the list fits in `size` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(list.as_ptr(), members, list.len());
                id.write(gid);
            }
            0
        }
        Err(e) => e,
    }
}
