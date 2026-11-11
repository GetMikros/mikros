//! Slave terminal descriptors: each holds the token of a terminal that
//! the terminal service made, and this process's own connection to the
//! service and readiness event, made when first needed. Reads, writes,
//! terminal control and readiness all go through that connection,
//! naming the terminal by its token; another process given the token
//! makes its own connection.

use crate::errno::{EAGAIN, EBADF, EINTR, EINVAL, EIO, ENOTTY, EPERM};
use crate::fs::O_NONBLOCK;
use crate::lock::Lock;
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::terminal::{
    Attributes, Queue, Terminal as Protocol, TerminalClient, When, WindowSize,
};
use mikros_rt::sys;

const TERMINAL: &str = "mikros.terminal.Terminal";
/// The readiness event's signals, for input and for output.
pub const USER0: u32 = 1 << 24;
pub const USER1: u32 = 1 << 25;
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
/// The most bytes one request carries.
const TRANSFER_MAX: u64 = 32768;
/// The `ioctl` requests of musl's terminal functions.
const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TCSETSW: u64 = 0x5403;
const TCSETSF: u64 = 0x5404;
const TCSBRK: u64 = 0x5409;
const TCFLSH: u64 = 0x540B;
const TIOCSCTTY: u64 = 0x540E;
const TIOCGPGRP: u64 = 0x540F;
const TIOCSPGRP: u64 = 0x5410;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCSWINSZ: u64 = 0x5414;
/// The terminal's number, which `ptsname` and `ttyname` name it by.
const TIOCGPTN: u64 = 0x8004_5430;
/// `TCFLSH`'s queues.
const TCIFLUSH: u64 = 0;
const TCOFLUSH: u64 = 1;
const TCIOFLUSH: u64 = 2;
/// musl's `struct termios`: four flag words, `c_line`, 32 control
/// characters, and the two speeds after padding.
const TERMIOS_CC: usize = 17;
const TERMIOS_ISPEED: usize = 52;
const TERMIOS_OSPEED: usize = 56;
const TERMIOS_SIZE: usize = 60;
const NCCS: usize = 32;

/// A slave terminal descriptor's open description.
pub struct Terminal {
    pub token: OwnedHandle,
    /// The `O_` status flags; `O_NONBLOCK` makes calls not wait.
    pub flags: AtomicU32,
    access: Lock<Option<Access>>,
}

/// This process's connection to the terminal service, and its readiness
/// event for the terminal once asked for.
struct Access {
    client: TerminalClient,
    readiness: Option<OwnedHandle>,
}

/// The error number of a terminal call's status.
fn errno(status: Status) -> i64 {
    match status {
        Status::ShouldWait | Status::TimedOut => EAGAIN,
        Status::Interrupted => EINTR,
        Status::AccessDenied => EPERM,
        Status::BadHandle => EBADF,
        Status::InvalidArgs | Status::OutOfRange => EINVAL,
        Status::NotSupported => ENOTTY,
        _ => EIO,
    }
}

impl Terminal {
    pub fn new(token: OwnedHandle) -> Terminal {
        Terminal {
            token,
            flags: AtomicU32::new(0),
            access: Lock::new(None),
        }
    }

    fn token(&self) -> Result<OwnedHandle, i64> {
        let mut out = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(self.token.raw(), SAME_RIGHTS, &mut out) };
        if status != mikros_rt::OK {
            return Err(EBADF);
        }
        // SAFETY: the kernel just created the handle for us.
        Ok(unsafe { OwnedHandle::from_raw(out) })
    }

    fn deadline(&self) -> i64 {
        match self.flags.load(Ordering::Relaxed) & O_NONBLOCK {
            0 => FOREVER,
            _ => 0,
        }
    }

    /// Runs `call` with this process's connection, making it first.
    fn with<T>(&self, call: impl FnOnce(&mut Access) -> T) -> Result<T, i64> {
        let mut access = self.access.lock();
        if access.is_none() {
            let raw = crate::startup::directory();
            // The directory connection stays the process's.
            let directory = ManuallyDrop::new(DirectoryClient::new(ClientEnd::new(
                // SAFETY: `start` installed the process's own directory.
                unsafe { OwnedHandle::from_raw(raw) },
            )));
            let (client, server) = endpoints::<Protocol>().map_err(|_| EIO)?;
            match directory.connect(TERMINAL.into(), None, server.into_handle(), FOREVER) {
                Ok(Ok(_)) => {}
                _ => return Err(EIO),
            }
            *access = Some(Access {
                client: TerminalClient::new(client),
                readiness: None,
            });
        }
        Ok(call(access.as_mut().expect("made above")))
    }

    /// `read`: at most one request's worth of input.
    pub fn read(&self, buf: *mut u8, len: u64) -> i64 {
        let Ok(token) = self.token() else {
            return -EBADF;
        };
        let deadline = self.deadline();
        let count = len.min(TRANSFER_MAX) as u32;
        let read = self.with(|a| a.client.read(token, count, deadline, FOREVER));
        match read {
            Ok(Ok(Ok(r))) => {
                let n = r.data.len().min(len as usize);
                // SAFETY: musl passes a buffer of `len` bytes.
                unsafe { core::ptr::copy_nonoverlapping(r.data.as_ptr(), buf, n) };
                n as i64
            }
            Ok(Ok(Err(status))) => -errno(status),
            Ok(Err(_)) => -EIO,
            Err(e) => -e,
        }
    }

    /// `write`: in requests of at most `TRANSFER_MAX` bytes, returning
    /// what was written once a later request fails.
    pub fn write(&self, buf: *const u8, len: u64) -> i64 {
        let deadline = self.deadline();
        let mut done = 0u64;
        while done < len {
            let n = (len - done).min(TRANSFER_MAX);
            // SAFETY: musl passes a buffer of `len` bytes.
            let data =
                unsafe { core::slice::from_raw_parts(buf.add(done as usize), n as usize) }.to_vec();
            let Ok(token) = self.token() else {
                return -EBADF;
            };
            let written = self.with(|a| a.client.write(token, data, deadline, FOREVER));
            let error = match written {
                Ok(Ok(Ok(r))) => {
                    done += u64::from(r.written);
                    if u64::from(r.written) < n {
                        break;
                    }
                    continue;
                }
                Ok(Ok(Err(status))) => errno(status),
                Ok(Err(_)) => EIO,
                Err(e) => e,
            };
            return if done > 0 { done as i64 } else { -error };
        }
        done as i64
    }

    /// The raw handle of this process's readiness event for the
    /// terminal, which has `USER0` for input and `USER1` for output.
    pub fn readiness(&self) -> Result<sys::Handle, i64> {
        let token = self.token()?;
        self.with(|a| {
            if a.readiness.is_none() {
                match a.client.get_readiness(token, FOREVER) {
                    Ok(Ok(r)) => a.readiness = Some(r.event),
                    Ok(Err(status)) => return Err(errno(status)),
                    Err(_) => return Err(EIO),
                }
            }
            Ok(a.readiness.as_ref().expect("got above").raw())
        })?
    }

    /// The terminal requests of `ioctl`.
    pub fn ioctl(&self, request: u64, arg: u64) -> i64 {
        let Ok(token) = self.token() else {
            return -EBADF;
        };
        let result = self.with(|a| control(&a.client, token, request, arg));
        match result {
            Ok(Ok(value)) => value,
            Ok(Err(e)) | Err(e) => -e,
        }
    }
}

/// Makes standard input's terminal the controlling terminal of the
/// caller's session when the caller leads it, as a process started in a
/// new session does. Nothing changes when standard input is no terminal
/// or the terminal service refuses, as it does for a session that has a
/// controlling terminal already.
pub fn control_standard_input() {
    let Some(description) = crate::fd::get(0) else {
        return;
    };
    if let crate::fd::Description::Terminal(terminal) = &*description
        && crate::process::leads_session()
    {
        let _ = terminal.ioctl(TIOCSCTTY, 0);
    }
}

/// Answers `ioctl` request `request` with argument `arg`, a pointer for
/// most of them.
fn control(
    client: &TerminalClient,
    token: OwnedHandle,
    request: u64,
    arg: u64,
) -> Result<i64, i64> {
    let done = |r: Result<Result<(), Status>, _>| match r {
        Ok(Ok(())) => Ok(0),
        Ok(Err(status)) => Err(errno(status)),
        Err(_) => Err(EIO),
    };
    let ptr = arg as *mut u8;
    match request {
        TCGETS => {
            let a = match client.get_attributes(token, FOREVER) {
                Ok(Ok(r)) => r.attributes,
                Ok(Err(status)) => return Err(errno(status)),
                Err(_) => return Err(EIO),
            };
            let mut bytes = [0u8; TERMIOS_SIZE];
            for (i, word) in [
                a.input_flags,
                a.output_flags,
                a.control_flags,
                a.local_flags,
            ]
            .iter()
            .enumerate()
            {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
            }
            bytes[TERMIOS_CC..TERMIOS_CC + NCCS].copy_from_slice(&a.control_chars);
            bytes[TERMIOS_ISPEED..TERMIOS_ISPEED + 4].copy_from_slice(&a.input_speed.to_ne_bytes());
            bytes[TERMIOS_OSPEED..TERMIOS_OSPEED + 4]
                .copy_from_slice(&a.output_speed.to_ne_bytes());
            // SAFETY: musl passes a `struct termios`.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, TERMIOS_SIZE) };
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            let mut bytes = [0u8; TERMIOS_SIZE];
            // SAFETY: musl passes a `struct termios`.
            unsafe { core::ptr::copy_nonoverlapping(ptr, bytes.as_mut_ptr(), TERMIOS_SIZE) };
            let word =
                |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
            let mut control_chars = [0u8; NCCS];
            control_chars.copy_from_slice(&bytes[TERMIOS_CC..TERMIOS_CC + NCCS]);
            let attributes = Attributes {
                input_flags: word(0),
                output_flags: word(4),
                control_flags: word(8),
                local_flags: word(12),
                control_chars,
                input_speed: word(TERMIOS_ISPEED),
                output_speed: word(TERMIOS_OSPEED),
            };
            let when = match request {
                TCSETS => When::Now,
                TCSETSW => When::Drain,
                _ => When::Flush,
            };
            done(
                client
                    .set_attributes(token, attributes, when, FOREVER)
                    .map(|r| r.map(|_| ())),
            )
        }
        // musl's tcdrain passes 1; 0 sends a break.
        TCSBRK if arg != 0 => done(client.drain(token, FOREVER).map(|r| r.map(|_| ()))),
        TCSBRK => done(client.send_break(token, FOREVER).map(|r| r.map(|_| ()))),
        TCFLSH => {
            let queues = match arg {
                TCIFLUSH => Queue::INPUT,
                TCOFLUSH => Queue::OUTPUT,
                TCIOFLUSH => Queue(Queue::INPUT.0 | Queue::OUTPUT.0),
                _ => return Err(EINVAL),
            };
            done(client.flush(token, queues, FOREVER).map(|r| r.map(|_| ())))
        }
        TIOCSCTTY => done(
            client
                .make_controlling(token, FOREVER)
                .map(|r| r.map(|_| ())),
        ),
        TIOCGPGRP => match client.get_foreground(token, FOREVER) {
            Ok(Ok(r)) => {
                // SAFETY: musl passes a `pid_t`.
                unsafe { (ptr as *mut i32).write_unaligned(r.pgid as i32) };
                Ok(0)
            }
            Ok(Err(status)) => Err(errno(status)),
            Err(_) => Err(EIO),
        },
        TIOCSPGRP => {
            // SAFETY: musl passes a `pid_t`.
            let pgid = unsafe { (ptr as *const i32).read_unaligned() };
            let pgid = u32::try_from(pgid).map_err(|_| EINVAL)?;
            done(
                client
                    .set_foreground(token, pgid, FOREVER)
                    .map(|r| r.map(|_| ())),
            )
        }
        TIOCGWINSZ => match client.get_window_size(token, FOREVER) {
            Ok(Ok(r)) => {
                let s = r.size;
                let words = [s.rows, s.columns, s.width, s.height];
                // SAFETY: musl passes a `struct winsize` of four shorts.
                unsafe { core::ptr::copy_nonoverlapping(words.as_ptr(), ptr as *mut u16, 4) };
                Ok(0)
            }
            Ok(Err(status)) => Err(errno(status)),
            Err(_) => Err(EIO),
        },
        TIOCGPTN => match client.get_number(token, FOREVER) {
            Ok(Ok(r)) => {
                // SAFETY: musl passes an `unsigned int`.
                unsafe { (ptr as *mut u32).write_unaligned(r.number) };
                Ok(0)
            }
            Ok(Err(status)) => Err(errno(status)),
            Err(_) => Err(EIO),
        },
        TIOCSWINSZ => {
            let mut words = [0u16; 4];
            // SAFETY: musl passes a `struct winsize` of four shorts.
            unsafe { core::ptr::copy_nonoverlapping(ptr as *const u16, words.as_mut_ptr(), 4) };
            let size = WindowSize {
                rows: words[0],
                columns: words[1],
                width: words[2],
                height: words[3],
            };
            done(
                client
                    .set_window_size(token, size, FOREVER)
                    .map(|r| r.map(|_| ())),
            )
        }
        _ => Err(ENOTTY),
    }
}
