//! The terminal of an interactive msh: raw input while the line editor
//! reads a line, the settings it had for the programs that msh runs,
//! its size, and reading its bytes without a buffer, so that a wait
//! for more of an escape sequence sees all that arrived.

use std::ffi::{c_int, c_short, c_ulong, c_void};

/// musl's `struct termios`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    iflag: u32,
    oflag: u32,
    cflag: u32,
    lflag: u32,
    line: u8,
    cc: [u8; 32],
    ispeed: u32,
    ospeed: u32,
}

#[repr(C)]
struct Winsize {
    rows: u16,
    columns: u16,
    width: u16,
    height: u16,
}

#[repr(C)]
struct Pollfd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

const ICRNL: u32 = 0o400;
const IXON: u32 = 0o2000;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const IEXTEN: u32 = 0o100000;
const VTIME: usize = 5;
const VMIN: usize = 6;
const TCSANOW: c_int = 0;
const TIOCGWINSZ: c_int = 0x5413;
const POLLIN: c_short = 1;

unsafe extern "C" {
    fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, when: c_int, termios: *const Termios) -> c_int;
    fn ioctl(fd: c_int, request: c_int, ...) -> c_int;
    fn poll(fds: *mut Pollfd, n: c_ulong, timeout: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
}

/// Standard input's terminal, with the settings it had at the start.
pub struct Terminal {
    cooked: Termios,
}

impl Terminal {
    /// Standard input's terminal, when it is one.
    pub fn new() -> Option<Terminal> {
        let mut cooked = std::mem::MaybeUninit::<Termios>::uninit();
        // SAFETY: the pointer is valid for a `Termios`.
        let got = unsafe { tcgetattr(0, cooked.as_mut_ptr()) };
        // SAFETY: `tcgetattr` filled it in when it succeeded.
        (got == 0).then(|| Terminal {
            cooked: unsafe { cooked.assume_init() },
        })
    }

    /// Hands every key to msh as it is typed, without echo or the
    /// signals of the terminal's keys.
    pub fn raw(&self) {
        let mut raw = self.cooked;
        raw.lflag &= !(ICANON | ECHO | ISIG | IEXTEN);
        raw.iflag &= !(ICRNL | IXON);
        raw.cc[VMIN] = 1;
        raw.cc[VTIME] = 0;
        // SAFETY: the pointer is valid for a `Termios`.
        unsafe { tcsetattr(0, TCSANOW, &raw) };
    }

    /// Gives the terminal back the settings it had at the start, for the
    /// programs that msh runs.
    pub fn restore(&self) {
        // SAFETY: the pointer is valid for a `Termios`.
        unsafe { tcsetattr(0, TCSANOW, &self.cooked) };
    }
}

/// The terminal's columns and rows, 80 and 24 when it does not say.
pub fn size() -> (usize, usize) {
    let mut size = Winsize {
        rows: 0,
        columns: 0,
        width: 0,
        height: 0,
    };
    // SAFETY: the pointer is valid for a `Winsize`.
    let got = unsafe { ioctl(0, TIOCGWINSZ, &mut size) };
    match (got, size.columns, size.rows) {
        (0, columns, rows) if columns > 0 && rows > 0 => (columns.into(), rows.into()),
        _ => (80, 24),
    }
}

/// Whether standard input has bytes within `ms` milliseconds.
pub fn input_within(ms: i32) -> bool {
    let mut fd = Pollfd {
        fd: 0,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: the pointer is valid for one `Pollfd`.
    unsafe { poll(&mut fd, 1, ms) > 0 }
}

/// Reads what standard input has, waiting for at least one byte; none
/// at its end.
pub fn read_input(buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        // SAFETY: the buffer is valid for its length.
        let n = unsafe { read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}
