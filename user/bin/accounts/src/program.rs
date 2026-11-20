//! accounts on Mikros: it pages through the accounts service's
//! `ListUsers`, finds named users there, reads passwords from the
//! terminal with its echo off, and calls the accounts service, which
//! asks for an administrator's authorization where it needs one.

use crate::listing::{self, COMMAND, Request, User};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::{AccountsClient, LoginMode, UserKind};
use std::io::{Read, Write};
use std::process::ExitCode;

const ACCOUNTS: &str = "mikros.session.Accounts";
const FOREVER: i64 = i64::MAX;

/// The terminal attributes of musl's `struct termios`, and the local
/// flag that echoes input.
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
const ECHO: u32 = 0o10;
const ICANON: u32 = 0o2;
const TCSANOW: i32 = 0;
/// The signals that end the program while a password is typed, after
/// which the terminal gets its attributes back.
const ENDING: [i32; 3] = [SIGHUP, SIGINT, SIGTERM];
const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
/// The keys that end an answer and remove a character.
const ENTER: [u8; 2] = *b"\r\n";
const BACKSPACE: [u8; 2] = [0x7f, 0x08];

type Handler = extern "C" fn(i32);

unsafe extern "C" {
    fn getuid() -> u32;
    fn isatty(fd: i32) -> i32;
    fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, when: i32, termios: *const Termios) -> i32;
    fn signal(signal: i32, handler: Handler) -> Handler;
    fn _exit(status: i32) -> !;
}

/// The terminal's attributes while a password is typed, for `restore`.
static mut SAVED: Option<Termios> = None;

/// Gives the terminal its attributes back and ends the program, for a
/// signal that ends it while a password is typed.
extern "C" fn restore(_: i32) {
    // SAFETY: `SAVED` is set before the handler is, and not changed
    // while it is; `tcsetattr` and `_exit` may be called in a handler.
    unsafe {
        if let Some(saved) = &*core::ptr::addr_of!(SAVED) {
            tcsetattr(0, TCSANOW, saved);
        }
        _exit(i32::from(FAILURE));
    }
}

/// A password, which is wiped when it is dropped.
struct Secret(String);

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zero bytes keep the string valid UTF-8.
        for byte in unsafe { self.0.as_bytes_mut() } {
            // SAFETY: the pointer refers to a byte of the string.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
    }
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let fail = |status: u8, message: &str| fail(&run, status, message);
    let request = match listing::read(&run) {
        Ok(request) => request,
        Err(reason) => return fail(INVALID, &reason),
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    let accounts = match directory.connect(ACCOUNTS) {
        Ok(client) => AccountsClient::new(client),
        Err(why) => return fail(FAILURE, &why),
    };
    match change(&accounts, request, Paint::stdout(&run)) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(why) => fail(FAILURE, &why),
    }
}

/// Does `request`, writing the list with `paint`, and says why it
/// failed.
fn change(accounts: &AccountsClient, request: Request, paint: Paint) -> Result<(), String> {
    let called = |name: &str, done: Result<Result<(), Status>, _>, own_password: bool| match done {
        Ok(Ok(())) => Ok(()),
        Ok(Err(status)) => Err(listing::failure(
            name,
            &listing::lowercase(&format!("{status:?}")),
            own_password,
        )),
        Err(error) => Err(format!("the accounts service failed: {error:?}")),
    };
    match request {
        Request::List { json } => {
            let users = list(accounts)?;
            let text = match json {
                true => listing::json(&users),
                false => listing::text(&users, paint),
            };
            std::io::stdout()
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())
        }
        Request::Add {
            name,
            admin,
            full_name,
        } => {
            let password = new_password()?;
            let kind = if admin {
                UserKind::Admin
            } else {
                UserKind::Standard
            };
            let full_name = full_name.unwrap_or_else(|| name.clone());
            let done =
                accounts.create_user(name.clone(), full_name, kind, password.0.clone(), FOREVER);
            called(&name, done.map(|r| r.map(drop)), false)
        }
        Request::Delete { name, delete_home } => {
            let id = find(accounts, &name)?;
            let done = accounts.delete_user(id, delete_home, FOREVER);
            called(&name, done.map(|r| r.map(drop)), false)
        }
        Request::Password { name } => {
            // SAFETY: the call takes no pointers.
            let caller = unsafe { getuid() };
            let (id, name) = match name {
                Some(name) => (find(accounts, &name)?, name),
                None => {
                    let me = list(accounts)?.into_iter().find(|u| u.id == caller);
                    (caller, me.map_or_else(|| caller.to_string(), |u| u.name))
                }
            };
            let own = id == caller;
            terminal()?;
            let current = match own {
                true => ask_secret("current password: ")?,
                false => Secret(String::new()),
            };
            let new = new_password()?;
            let done = accounts.set_password(id, current.0.clone(), new.0.clone(), FOREVER);
            called(&name, done.map(|r| r.map(drop)), own)
        }
        Request::Kind { name, admin } => {
            let id = find(accounts, &name)?;
            let kind = if admin {
                UserKind::Admin
            } else {
                UserKind::Standard
            };
            let done = accounts.set_kind(id, kind, FOREVER);
            called(&name, done.map(|r| r.map(drop)), false)
        }
        Request::FullName { name, text } => {
            let id = find(accounts, &name)?;
            let done = accounts.set_full_name(id, text, FOREVER);
            called(&name, done.map(|r| r.map(drop)), false)
        }
        Request::Login { name, desktop } => {
            let (id, name) = match name {
                Some(name) => (find(accounts, &name)?, name),
                None => {
                    // SAFETY: the call takes no pointers.
                    let caller = unsafe { getuid() };
                    (caller, caller.to_string())
                }
            };
            let mode = match desktop {
                true => LoginMode::Desktop,
                false => LoginMode::Console,
            };
            let done = accounts.set_login_mode(id, mode, FOREVER);
            called(&name, done.map(|r| r.map(drop)), false)
        }
    }
}

/// Every user of `ListUsers`, page by page.
fn list(accounts: &AccountsClient) -> Result<Vec<User>, String> {
    let mut all: Vec<User> = Vec::new();
    loop {
        let after = all.last().map_or(0, |u| u.id);
        let users = accounts
            .list_users(after, FOREVER)
            .map_err(|e| format!("ListUsers failed: {e:?}"))?
            .users;
        if users.is_empty() {
            return Ok(all);
        }
        for u in users {
            let login = accounts
                .get_login_mode(u.id, FOREVER)
                .map_err(|e| format!("GetLoginMode failed: {e:?}"))?;
            all.push(User {
                id: u.id,
                name: u.name,
                full_name: u.full_name,
                kind: match u.kind {
                    UserKind::Admin => "admin",
                    _ => "standard",
                },
                login: match login {
                    Ok(r) if r.mode == LoginMode::Console => "console",
                    Ok(_) => "desktop",
                    // A user deleted since the listing.
                    Err(_) => "-",
                },
            });
        }
    }
}

/// The user ID of the user named `name`.
fn find(accounts: &AccountsClient, name: &str) -> Result<u32, String> {
    list(accounts)?
        .into_iter()
        .find(|u| u.name == name)
        .map(|u| u.id)
        .ok_or_else(|| format!("{name}: no such user"))
}

/// Fails unless standard input is a terminal.
fn terminal() -> Result<(), String> {
    // SAFETY: the call takes no pointers.
    match unsafe { isatty(0) } == 1 {
        true => Ok(()),
        false => Err("standard input is not a terminal".into()),
    }
}

/// A new password, asked twice from the terminal.
fn new_password() -> Result<Secret, String> {
    terminal()?;
    let first = ask_secret("new password: ")?;
    let again = ask_secret("new password again: ")?;
    if first.0 != again.0 {
        return Err("the passwords differ".into());
    }
    if first.0.is_empty() {
        return Err("the password is empty".into());
    }
    Ok(first)
}

/// Writes `prompt` to standard error and reads an answer from standard
/// input with the terminal's echo and canonical input off: a `*` for
/// each character typed, Backspace removes the last one, and Enter ends
/// the answer with a line feed. Other control characters are left out.
/// The terminal gets its attributes back when the answer ends, and when
/// a signal ends the program before.
fn ask_secret(prompt: &str) -> Result<Secret, String> {
    let mut saved = std::mem::MaybeUninit::<Termios>::uninit();
    // SAFETY: the pointer is valid for a `Termios`.
    if unsafe { tcgetattr(0, saved.as_mut_ptr()) } != 0 {
        return Err("standard input is not a terminal".into());
    }
    // SAFETY: `tcgetattr` filled it in.
    let saved = unsafe { saved.assume_init() };
    let mut quiet = saved;
    quiet.lflag &= !(ECHO | ICANON);
    // SAFETY: no handler runs yet, so nothing reads `SAVED` meanwhile.
    unsafe { *core::ptr::addr_of_mut!(SAVED) = Some(saved) };
    // SAFETY: `restore` may run as a handler.
    let before = ENDING.map(|sig| unsafe { signal(sig, restore) });
    let mut err = std::io::stderr();
    let _ = err.write_all(prompt.as_bytes());
    // SAFETY: the pointer is valid for a `Termios`.
    unsafe { tcsetattr(0, TCSANOW, &quiet) };
    let mut answer = Secret(String::new());
    let mut bytes = Secret(String::new());
    let mut input = std::io::stdin().lock();
    let ended = loop {
        let mut byte = [0u8];
        if !matches!(input.read(&mut byte), Ok(1)) {
            break false;
        }
        let byte = byte[0];
        if ENTER.contains(&byte) {
            break true;
        }
        if BACKSPACE.contains(&byte) {
            if answer.0.pop().is_some() {
                let _ = err.write_all(b"\x08 \x08");
            }
            continue;
        }
        if byte < 0x20 {
            continue;
        }
        // The bytes of a character gather until they make one.
        // SAFETY: the bytes are checked before they are taken as text.
        unsafe { bytes.0.as_mut_vec().push(byte) };
        match std::str::from_utf8(bytes.0.as_bytes()) {
            Ok(c) => {
                answer.0.push_str(c);
                let _ = err.write_all(b"*");
                bytes.0.clear();
            }
            Err(e) if e.error_len().is_some() => bytes.0.clear(),
            Err(_) => {}
        }
    };
    // SAFETY: as above.
    unsafe { tcsetattr(0, TCSANOW, &saved) };
    for (sig, handler) in ENDING.into_iter().zip(before) {
        // SAFETY: the handler is the one there was before.
        unsafe { signal(sig, handler) };
    }
    let _ = err.write_all(b"\n");
    match ended {
        true => Ok(answer),
        false => Err("no password was given".into()),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(run: &Run, status: u8, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(status)
}
