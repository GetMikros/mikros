//! less on Mikros: copying the input when standard output is not a
//! terminal, and otherwise paging it there, on the alternate screen, with
//! the keys read from that terminal while a thread reads the input as far
//! as the screen needs it.

use crate::COMMAND;
use crate::keys;
use crate::pager::{Next, Pager, Shown};
use mikros_cli::{FAILURE, Kind, Run, SUCCESS, complain, start};
use std::ffi::{c_int, c_short, c_ulong, c_void};
use std::io::{ErrorKind, Read, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// The descriptor of standard output, the terminal that less reads its
/// keys from.
const OUTPUT: c_int = 1;
/// How long less waits for a key before it looks for more input.
const POLL_MS: c_int = 50;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const VTIME: usize = 5;
const VMIN: usize = 6;
const TCSANOW: c_int = 0;
const TIOCGWINSZ: c_int = 0x5413;
const POLLIN: c_short = 1;
const SIGHUP: c_int = 1;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const SIGWINCH: c_int = 28;
/// Entering and leaving the alternate screen.
const ALTERNATE: &[u8] = b"\x1b[?1049h";
const NORMAL: &[u8] = b"\x1b[?1049l";

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

unsafe extern "C" {
    fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, when: c_int, termios: *const Termios) -> c_int;
    fn ioctl(fd: c_int, request: c_int, ...) -> c_int;
    fn poll(fds: *mut Pollfd, n: c_ulong, timeout: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
    fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
}

/// Set when the window's size changed, and when a signal asks less to
/// end.
static RESIZED: AtomicBool = AtomicBool::new(false);
static STOPPED: AtomicBool = AtomicBool::new(false);

extern "C" fn resized(_: c_int) {
    RESIZED.store(true, Ordering::Relaxed);
}

extern "C" fn stopped(_: c_int) {
    STOPPED.store(true, Ordering::Relaxed);
}

/// What the reading thread has read and not handed over yet, and how far
/// the screen wants it to read.
struct Input {
    fresh: Vec<String>,
    read: usize,
    wanted: usize,
    ended: bool,
    error: Option<String>,
}

struct Shared {
    input: Mutex<Input>,
    wanted: Condvar,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &nothing);
    let file = run.operands.first().cloned();
    let (input, name): (Box<dyn Read + Send>, String) = match file {
        Some(path) => match std::fs::File::open(&path) {
            Ok(file) => (Box::new(file), path),
            Err(error) => return fail(&run, &format!("cannot open {path}: {error}")),
        },
        None => (Box::new(std::io::stdin()), "(standard input)".into()),
    };
    match Terminal::of_output() {
        Some(terminal) => page(&run, &terminal, input, name),
        None => copy(&run, input, &name),
    }
}

/// Names of system things for completion, of which less takes none.
fn nothing(_: Kind, _: &str) -> Vec<String> {
    Vec::new()
}

/// Writes the error `message` to standard error and gives the status of
/// a failure.
fn fail(run: &Run, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(FAILURE)
}

/// Copies `input` to standard output as it is.
fn copy(run: &Run, mut input: Box<dyn Read + Send>, name: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match std::io::copy(&mut input, &mut out).and_then(|_| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) => fail(run, &format!("cannot copy {name}: {error}")),
    }
}

/// Pages `input` on `terminal` until q or a signal ends it.
fn page(run: &Run, terminal: &Terminal, input: Box<dyn Read + Send>, name: String) -> ExitCode {
    let shared = Arc::new(Shared {
        input: Mutex::new(Input {
            fresh: Vec::new(),
            read: 0,
            wanted: 0,
            ended: false,
            error: None,
        }),
        wanted: Condvar::new(),
    });
    let reader = shared.clone();
    let source = name.clone();
    std::thread::spawn(move || read_lines(input, &source, &reader));
    // SAFETY: the handlers only store to atomics.
    unsafe {
        signal(SIGWINCH, resized);
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            signal(sig, stopped);
        }
    }
    terminal.raw();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(ALTERNATE);
    let (columns, rows) = size();
    let mut pager = Pager::new(&name, columns, rows);
    let mut drawn: Option<Shown> = None;
    let mut keys = [0u8; 64];
    'paging: while !STOPPED.load(Ordering::Relaxed) {
        take_lines(&shared, &mut pager);
        if RESIZED.swap(false, Ordering::Relaxed) {
            let (columns, rows) = size();
            pager.resize(columns, rows);
            drawn = None;
        }
        let shown = pager.shown();
        if drawn.as_ref() != Some(&shown) {
            draw(&mut out, &shown);
            drawn = Some(shown);
        }
        if !readable(OUTPUT, POLL_MS) {
            continue;
        }
        // SAFETY: the buffer is valid for its length.
        let got = unsafe { read(OUTPUT, keys.as_mut_ptr().cast(), keys.len()) };
        if got <= 0 {
            if got < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        for key in keys::decode(&keys[..got as usize]) {
            if pager.handle(key) == Next::Quit {
                break 'paging;
            }
        }
    }
    let _ = out.write_all(NORMAL).and_then(|()| out.flush());
    terminal.restore();
    let error = shared.input.lock().map(|i| i.error.clone()).unwrap_or(None);
    match error {
        Some(error) => fail(run, &error),
        None => ExitCode::from(SUCCESS),
    }
}

/// Hands the lines read since the last call to `pager`, and tells the
/// reading thread how far to read.
fn take_lines(shared: &Shared, pager: &mut Pager) {
    let Ok(mut input) = shared.input.lock() else {
        return;
    };
    for line in input.fresh.drain(..) {
        pager.push(line);
    }
    if input.ended {
        pager.end();
    }
    let wanted = pager.wanted();
    if wanted != input.wanted {
        input.wanted = wanted;
        shared.wanted.notify_one();
    }
}

/// Reads the lines of `input`, named `name`, while the screen wants more
/// of them.
fn read_lines(mut input: Box<dyn Read + Send>, name: &str, shared: &Shared) {
    let mut partial = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let Ok(mut state) = shared.input.lock() else {
            return;
        };
        while state.read >= state.wanted {
            let Ok(next) = shared.wanted.wait(state) else {
                return;
            };
            state = next;
        }
        drop(state);
        let got = input.read(&mut chunk);
        let Ok(mut state) = shared.input.lock() else {
            return;
        };
        match got {
            Ok(0) => {
                if !partial.is_empty() {
                    state
                        .fresh
                        .push(String::from_utf8_lossy(&partial).into_owned());
                    state.read += 1;
                }
                state.ended = true;
                return;
            }
            Ok(n) => {
                for piece in chunk[..n].split_inclusive(|&b| b == b'\n') {
                    match piece.split_last() {
                        Some((b'\n', text)) => {
                            partial.extend_from_slice(text);
                            let line = String::from_utf8_lossy(&partial).into_owned();
                            state.fresh.push(line);
                            state.read += 1;
                            partial.clear();
                        }
                        _ => partial.extend_from_slice(piece),
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                state.error = Some(format!("cannot read {name}: {error}"));
                state.ended = true;
                return;
            }
        }
    }
}

/// Draws `shown`: each row of text in its place, then the last row, with
/// the rest of each row erased.
fn draw(out: &mut impl Write, shown: &Shown) {
    let mut screen = String::new();
    for row in 0..shown.height - 1 {
        let text = shown.rows.get(row).map_or("", String::as_str);
        screen.push_str(&format!("\x1b[{};1H{text}\x1b[K", row + 1));
    }
    screen.push_str(&format!("\x1b[{};1H{}\x1b[K", shown.height, shown.last));
    let _ = out.write_all(screen.as_bytes()).and_then(|()| out.flush());
}

/// Standard output's terminal, with the settings it had at the start.
struct Terminal {
    cooked: Termios,
}

impl Terminal {
    /// Standard output's terminal, when it is one.
    fn of_output() -> Option<Terminal> {
        let mut cooked = std::mem::MaybeUninit::<Termios>::uninit();
        // SAFETY: the pointer is valid for a `Termios`.
        let got = unsafe { tcgetattr(OUTPUT, cooked.as_mut_ptr()) };
        // SAFETY: `tcgetattr` filled it in when it succeeded.
        (got == 0).then(|| Terminal {
            cooked: unsafe { cooked.assume_init() },
        })
    }

    /// Hands each key to less as it is typed, without echo.
    fn raw(&self) {
        let mut raw = self.cooked;
        raw.lflag &= !(ICANON | ECHO);
        raw.cc[VMIN] = 1;
        raw.cc[VTIME] = 0;
        // SAFETY: the pointer is valid for a `Termios`.
        unsafe { tcsetattr(OUTPUT, TCSANOW, &raw) };
    }

    /// Gives the terminal back the settings it had at the start.
    fn restore(&self) {
        // SAFETY: the pointer is valid for a `Termios`.
        unsafe { tcsetattr(OUTPUT, TCSANOW, &self.cooked) };
    }
}

/// The terminal's columns and rows, 80 and 24 when it does not say.
fn size() -> (usize, usize) {
    let mut size = Winsize {
        rows: 0,
        columns: 0,
        width: 0,
        height: 0,
    };
    // SAFETY: the pointer is valid for a `Winsize`.
    let got = unsafe { ioctl(OUTPUT, TIOCGWINSZ, &mut size) };
    match (got, size.columns, size.rows) {
        (0, columns, rows) if columns > 0 && rows > 0 => (columns.into(), rows.into()),
        _ => (80, 24),
    }
}

/// Whether `fd` has bytes to read within `ms` milliseconds.
fn readable(fd: c_int, ms: c_int) -> bool {
    let mut fd = Pollfd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: the pointer is valid for one `Pollfd`.
    unsafe { poll(&mut fd, 1, ms) > 0 }
}
