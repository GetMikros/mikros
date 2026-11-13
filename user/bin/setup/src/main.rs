//! The setup program, which the session manager runs on the text
//! console's terminal on the first boot, while no user account exists.
//! It asks for the computer's name, suggesting `mikros`, and writes it
//! to /etc/hostname. Then it asks for the name, full name and password
//! of the first administrator and creates that account with the
//! accounts service, which accepts this one call from the app ID
//! `system.setup`. It exits with 0 once the account exists, and with 1
//! when the name cannot be written, the account cannot be made or input
//! ends.

#![feature(mikros_handles)]

use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::session::{Accounts, AccountsClient, UserKind};
use mikros_rt::sys;
use std::io::{BufRead, Write};

const ACCOUNTS: &str = "mikros.session.Accounts";
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
/// The file of the computer's name, the name suggested for it, and the
/// longest one.
const HOSTNAME: &str = "/etc/hostname";
const HOST_SUGGESTED: &str = "mikros";
const HOST_MAX: usize = 63;
/// The longest name and full name the accounts service takes.
const NAME_MAX: usize = 32;
const FULL_NAME_MAX: usize = 128;
const PASSWORD_MAX: usize = 1024;

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
const TCSANOW: i32 = 0;

unsafe extern "C" {
    fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, when: i32, termios: *const Termios) -> i32;
}

/// Whether `name` is a valid user name: a lowercase letter, then at most
/// 31 lowercase letters, digits and hyphens.
fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && name.len() <= NAME_MAX
        && bytes.all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// Whether `name` is a valid computer name: 1 to 63 ASCII letters,
/// digits and hyphens, not starting or ending with a hyphen.
fn valid_host(name: &str) -> bool {
    (1..=HOST_MAX).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Asks for the computer's name until it is valid, taking an empty
/// answer as the suggested name, or `None` at the end of input.
fn host_name() -> Option<String> {
    loop {
        let name = ask(&format!("Computer name [{HOST_SUGGESTED}]: "))?;
        let name = match name.trim() {
            "" => HOST_SUGGESTED.to_owned(),
            name => name.to_owned(),
        };
        if valid_host(&name) {
            return Some(name);
        }
        println!(
            "A computer name has 1 to {HOST_MAX} letters, digits and hyphens, and does not start or end with a hyphen."
        );
    }
}

/// Writes `prompt` and reads a line without its end, or `None` at the
/// end of input.
fn ask(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_owned()),
    }
}

/// As `ask`, with the terminal's echo off while the line is typed.
fn ask_secret(prompt: &str) -> Option<String> {
    let mut saved = std::mem::MaybeUninit::<Termios>::uninit();
    // SAFETY: the pointer is valid for a `Termios`.
    let echoing = unsafe { tcgetattr(0, saved.as_mut_ptr()) } == 0;
    if echoing {
        // SAFETY: `tcgetattr` filled it in.
        let mut quiet = unsafe { saved.assume_init() };
        quiet.lflag &= !ECHO;
        // SAFETY: the pointer is valid for a `Termios`.
        unsafe { tcsetattr(0, TCSANOW, &quiet) };
    }
    let line = ask(prompt);
    if echoing {
        // SAFETY: as above; `saved` was filled in.
        unsafe { tcsetattr(0, TCSANOW, saved.as_ptr()) };
        println!();
    }
    line
}

/// A connection to the accounts service through the process's
/// directory.
fn accounts() -> Option<AccountsClient> {
    let mut raw = 0;
    let directory = std::os::mikros::directory().as_raw_handle();
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::handle_duplicate(directory, SAME_RIGHTS, &mut raw) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just made the handle for us.
    let directory = DirectoryClient::new(ClientEnd::new(unsafe { OwnedHandle::from_raw(raw) }));
    let (client, server) = endpoints::<Accounts>().ok()?;
    match directory.connect(ACCOUNTS.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => Some(AccountsClient::new(client)),
        _ => None,
    }
}

/// Asks for the account until it is complete, or `None` at the end of
/// input.
fn account() -> Option<(String, String, String)> {
    let name = loop {
        let name = ask("User name: ")?;
        if valid_name(&name) {
            break name;
        }
        println!(
            "A user name starts with a lowercase letter, followed by at most 31 lowercase letters, digits and hyphens."
        );
    };
    let full_name = loop {
        let full_name = ask("Full name: ")?;
        if !full_name.trim().is_empty() && full_name.len() <= FULL_NAME_MAX {
            break full_name;
        }
        println!("Enter a full name of at most {FULL_NAME_MAX} bytes.");
    };
    let password = loop {
        let password = ask_secret("Password: ")?;
        if password.is_empty() || password.len() > PASSWORD_MAX {
            println!("Enter a password of 1 to {PASSWORD_MAX} bytes.");
            continue;
        }
        if ask_secret("Repeat the password: ")? == password {
            break password;
        }
        println!("The passwords differ.");
    };
    Some((name, full_name, password))
}

fn main() {
    println!("Welcome to Mikros.");
    println!();
    let Some(host) = host_name() else {
        std::process::exit(1);
    };
    if let Err(e) = std::fs::write(HOSTNAME, format!("{host}\n")) {
        println!("Writing the computer's name failed: {e}.");
        std::process::exit(1);
    }
    println!();
    println!("Create the first account, which administers this computer.");
    println!();
    let Some(accounts) = accounts() else {
        println!("The accounts service cannot be reached.");
        std::process::exit(1);
    };
    loop {
        let Some((name, full_name, password)) = account() else {
            std::process::exit(1);
        };
        match accounts.create_user(name.clone(), full_name, UserKind::Admin, password, FOREVER) {
            Ok(Ok(_)) => {
                println!("Created the administrator {name}.");
                return;
            }
            Ok(Err(Status::AlreadyExists)) => {
                println!("{name} exists already. Choose another name.")
            }
            Ok(Err(Status::AccessDenied)) => {
                println!("An account exists already, so setup is not needed.");
                std::process::exit(1);
            }
            Ok(Err(status)) => println!("Creating {name} failed: {status:?}."),
            Err(_) => {
                println!("The accounts service stopped answering.");
                std::process::exit(1);
            }
        }
        println!();
    }
}
