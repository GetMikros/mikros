//! `uname`, from which musl's `gethostname` takes the host name, and
//! `sethostname`, which no process may call. The host name is read from
//! `/etc/hostname` at each call: one line of 1 to 63 ASCII letters,
//! digits and `-`, neither first nor last, with or without a line feed
//! after it. A missing file, or one that holds anything else, gives the
//! name `mikros`.

use crate::errno::{EFAULT, EPERM};

/// The length of each field of musl's `struct utsname`, and their count.
const FIELD: usize = 65;
const FIELDS: usize = 6;
const SYSNAME: &[u8] = b"Mikros";
const HOSTNAME: &[u8] = b"/etc/hostname\0";
const DEFAULT_HOST: &[u8] = b"mikros";
/// The longest host name.
const NAME_MAX: usize = 63;

/// The host name that `contents`, the bytes of `/etc/hostname`, give.
fn host_name(contents: &[u8]) -> &[u8] {
    let name = contents.strip_suffix(b"\n").unwrap_or(contents);
    let valid = (1..=NAME_MAX).contains(&name.len())
        && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && !name.starts_with(b"-")
        && !name.ends_with(b"-");
    if valid { name } else { DEFAULT_HOST }
}

/// Reads `/etc/hostname` into `buf`, which holds one byte more than the
/// longest valid contents, and gives how many bytes it read: none when
/// the file cannot be read.
fn read_host_file(buf: &mut [u8; NAME_MAX + 2]) -> usize {
    // Opened for reading, which is the access mode 0.
    let fd = crate::fs::open(crate::fs::AT_FDCWD, HOSTNAME.as_ptr(), 0, 0);
    if fd < 0 {
        return 0;
    }
    let mut len = 0;
    while len < buf.len() {
        let rest = &mut buf[len..];
        match crate::io::read(fd, rest.as_mut_ptr(), rest.len() as u64) {
            n if n > 0 => len += n as usize,
            0 => break,
            _ => {
                len = 0;
                break;
            }
        }
    }
    crate::fd::close(fd);
    len
}

/// `uname`: `Mikros` as the system's name and the host name as the
/// node's, with the other fields empty.
pub fn uname(buf: *mut u8) -> i64 {
    if buf.is_null() {
        return -EFAULT;
    }
    let mut fields = [0u8; FIELD * FIELDS];
    fields[..SYSNAME.len()].copy_from_slice(SYSNAME);
    let mut file = [0u8; NAME_MAX + 2];
    let len = read_host_file(&mut file);
    let name = host_name(&file[..len]);
    fields[FIELD..FIELD + name.len()].copy_from_slice(name);
    // SAFETY: musl passes a `struct utsname`, which is as large.
    unsafe { core::ptr::copy_nonoverlapping(fields.as_ptr(), buf, fields.len()) };
    0
}

/// `sethostname`: the host name changes only with `/etc/hostname`.
pub fn sethostname() -> i64 {
    -EPERM
}
