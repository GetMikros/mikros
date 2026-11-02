//! The status values system calls return.

/// A failed system call's status. Success is `OK`, which is not an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    Internal = -1,
    NotSupported = -2,
    NoResources = -3,
    NoMemory = -4,
    BadSyscall = -5,
    InvalidArgs = -10,
    BadHandle = -11,
    WrongType = -12,
    AccessDenied = -13,
    OutOfRange = -14,
    BufferTooSmall = -15,
    BadState = -20,
    TimedOut = -21,
    ShouldWait = -22,
    Canceled = -23,
    PeerClosed = -24,
    NotFound = -25,
    AlreadyExists = -26,
    Interrupted = -27,
    Io = -28,
}

/// The status value of success.
pub const OK: i64 = 0;

/// The status value of `result`.
pub fn status<T>(result: Result<T, Error>) -> i64 {
    match result {
        Ok(_) => OK,
        Err(e) => e as i64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_the_status_values() {
        assert_eq!(status(Ok::<(), Error>(())), 0);
        assert_eq!(status::<()>(Err(Error::NoResources)), -3);
        assert_eq!(status::<()>(Err(Error::BadHandle)), -11);
        assert_eq!(status::<()>(Err(Error::Io)), -28);
    }
}
