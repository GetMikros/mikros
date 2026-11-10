//! musl's error numbers, and the ones that Mikros status values become.

use mikros_rt::interfaces::mikros::fs::Error;
use mikros_rt::sys::Status;

pub const EPERM: i64 = 1;
pub const ENOENT: i64 = 2;
pub const ESRCH: i64 = 3;
pub const EINTR: i64 = 4;
pub const EIO: i64 = 5;
pub const E2BIG: i64 = 7;
pub const EBADF: i64 = 9;
pub const ECHILD: i64 = 10;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EFAULT: i64 = 14;
pub const EBUSY: i64 = 16;
pub const EEXIST: i64 = 17;
pub const EXDEV: i64 = 18;
pub const ENODEV: i64 = 19;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const EMFILE: i64 = 24;
pub const ENOTTY: i64 = 25;
pub const EFBIG: i64 = 27;
pub const ENOSPC: i64 = 28;
pub const ESPIPE: i64 = 29;
pub const EROFS: i64 = 30;
pub const EPIPE: i64 = 32;
pub const ERANGE: i64 = 34;
pub const ENAMETOOLONG: i64 = 36;
pub const ENOSYS: i64 = 38;
pub const ENOTEMPTY: i64 = 39;
pub const ELOOP: i64 = 40;
pub const ENOTSUP: i64 = 95;
pub const ETIMEDOUT: i64 = 110;
pub const ECONNREFUSED: i64 = 111;

/// The error number of the failed status `status`.
pub fn from_status(status: Status) -> i64 {
    match status {
        -4 => ENOMEM,     // NO_MEMORY
        -3 => EAGAIN,     // NO_RESOURCES
        -10 => EINVAL,    // INVALID_ARGS
        -11 => EBADF,     // BAD_HANDLE
        -13 => EACCES,    // ACCESS_DENIED
        -14 => ERANGE,    // OUT_OF_RANGE
        -21 => ETIMEDOUT, // TIMED_OUT
        -22 => EAGAIN,    // SHOULD_WAIT
        -27 => EINTR,     // INTERRUPTED
        -24 => EPIPE,     // PEER_CLOSED
        -25 => ENOENT,    // NOT_FOUND
        -26 => EEXIST,    // ALREADY_EXISTS
        -2 => ENOTSUP,    // NOT_SUPPORTED
        _ => EIO,
    }
}

/// The result of a system call that musl expects: `value` on success,
/// and the negated error number of `status` otherwise.
pub fn result(status: Status, value: i64) -> i64 {
    if status == mikros_rt::OK {
        value
    } else {
        -from_status(status)
    }
}

/// The error number of the file system error `error`.
pub fn from_fs(error: Error) -> i64 {
    match error {
        Error::NotFound => ENOENT,
        Error::Exists => EEXIST,
        Error::NotDirectory => ENOTDIR,
        Error::IsDirectory => EISDIR,
        Error::NotEmpty => ENOTEMPTY,
        Error::NoSpace => ENOSPC,
        Error::ReadOnly => EROFS,
        Error::AccessDenied => EACCES,
        Error::Invalid => EINVAL,
        Error::NameTooLong => ENAMETOOLONG,
        Error::CrossDevice => EXDEV,
        Error::Loop => ELOOP,
        Error::Io => EIO,
        Error::NotSupported => ENOTSUP,
        Error::Busy => EBUSY,
        Error::TooBig => EFBIG,
        Error::WouldBlock => EAGAIN,
        Error::Refused => ECONNREFUSED,
        Error::Unknown(_) => EIO,
    }
}
