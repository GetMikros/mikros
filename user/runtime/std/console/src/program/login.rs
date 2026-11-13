//! Setup and logins through `TextLogin`, on each text console with its
//! number: while no user account exists, the setup program runs on a
//! terminal of its own on console 1, and the other consoles show only the
//! log; otherwise a console asks for a name and a password, without
//! echoing the password, and shows the terminal of the session that
//! `Login` starts until the session ends. Each terminal is closed after its use, and the login
//! prompt shows again. The prompt and setup's terminal take the rows
//! below the log, and a session's terminal the whole screen.

use crate::Console;
use crate::term::Term;
use crate::vt::{View, Vt};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::{TextLoginAsyncClient, TextLoginEvent};
use mikros_vt::screen::Screen;
use std::rc::Rc;

/// The longest name and password that `Login` takes.
const NAME_MAX: usize = 32;
const PASSWORD_MAX: usize = 1024;
const GREETING: &str = "\x1b[2J\x1b[HMikros";
/// The console that setup runs on, and how often the others look
/// whether it has finished.
const SETUP_CONSOLE: u8 = 1;
const SETUP_POLL: i64 = 1_000_000_000;

/// Shows the login prompt's screen of `vt` with `text` written to it.
fn prompt(console: &Console, vt: &Vt, text: &str) {
    let (cols, rows) = console.prompt_size();
    let mut screen = Screen::new(cols, rows);
    screen.write(text.as_bytes());
    console.show(vt, View::Login(Box::new(screen)), false);
}

/// Writes `text` to the login prompt's screen of `vt`.
fn write(console: &Console, vt: &Vt, text: &[u8]) {
    if let View::Login(screen) = &mut *vt.view.borrow_mut() {
        screen.write(text);
    }
    console.changed.notify();
}

/// Reads a line typed at the login prompt of `vt`, of at most `max`
/// bytes, echoing it when `echo` is set.
async fn read_line(console: &Console, vt: &Vt, echo: bool, max: usize) -> String {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let b = loop {
            if let Some(b) = vt.typed.borrow_mut().pop_front() {
                break b;
            }
            vt.typed_in.changed().await;
        };
        match b {
            b'\r' | b'\n' => {
                write(console, vt, b"\r\n");
                return String::from_utf8_lossy(&line).into_owned();
            }
            0x7F | 0x08 => {
                // Whole UTF-8 sequences are erased.
                while let Some(last) = line.pop() {
                    if last & 0xC0 != 0x80 {
                        break;
                    }
                }
                if echo {
                    write(console, vt, b"\x08 \x08");
                }
            }
            0x15 => {
                if echo {
                    for _ in String::from_utf8_lossy(&line).chars() {
                        write(console, vt, b"\x08 \x08");
                    }
                }
                line.clear();
            }
            b if b < 0x20 => {}
            b if line.len() < max => {
                line.push(b);
                if echo {
                    write(console, vt, &[b]);
                }
            }
            _ => {}
        }
    }
}

/// Opens setup's terminal, below the log, and shows it on `vt`, its
/// output read into its screen.
async fn show_terminal(console: &Rc<Console>, vt: &Vt) -> Option<Rc<Term>> {
    let (cols, rows) = console.prompt_size();
    let term = Term::open(
        &console.directory,
        &console.reactor,
        cols,
        rows,
        console.changed.clone(),
    )
    .await?;
    console
        .spawner
        .spawn(term.clone().pump(console.reactor.clone()));
    console.show(vt, View::Terminal(term.clone()), false);
    Some(term)
}

/// Reads the session manager's events, and wakes the console that
/// `OnSessionEnded` names, until it closes `TextLogin`; then wakes every
/// console.
pub async fn events(console: Rc<Console>, text_login: Rc<TextLoginAsyncClient>) {
    while let Ok(TextLoginEvent::OnSessionEnded(e)) = text_login.next_event().await {
        let index = usize::from(e.console).wrapping_sub(1);
        if let Some(vt) = console.vts.get(index) {
            vt.ended.notify();
        }
    }
    console.closed.set(true);
    for vt in &console.vts {
        vt.ended.notify();
    }
}

/// Waits until the session on `vt` ends: false when the session manager
/// closed `TextLogin` instead.
async fn session_ended(console: &Console, vt: &Vt) -> bool {
    vt.ended.changed().await;
    !console.closed.get()
}

/// Runs setup and logins on `vt` for as long as the session manager
/// serves `TextLogin`.
pub async fn run(console: Rc<Console>, vt: Rc<Vt>, text_login: Rc<TextLoginAsyncClient>) {
    let vt = &*vt;
    loop {
        let Ok(setup) = text_login.needs_setup().await else {
            eprintln!("console: the session manager does not answer");
            return;
        };
        if setup.needed && vt.number != SETUP_CONSOLE {
            // Only the log shows until setup on console 1 has finished.
            vt.log_only.set(true);
            console.changed.notify();
            let _ = console
                .reactor
                .sleep_until(crate::now().saturating_add(SETUP_POLL))
                .await;
            continue;
        }
        if vt.log_only.replace(false) {
            vt.with_screen(|s| s.touch());
            console.changed.notify();
        }
        if setup.needed {
            let Some(term) = show_terminal(&console, vt).await else {
                eprintln!("console: no terminal for setup");
                return;
            };
            let Some(token) = term.token() else { return };
            if let Ok(Err(status)) = text_login.run_setup(vt.number, token).await {
                eprintln!("console: RunSetup failed: {status:?}");
            }
            term.close();
            continue;
        }
        let greeting = format!("{GREETING} (console {})\r\n\r\nlogin: ", vt.number);
        prompt(&console, vt, &greeting);
        let name = read_line(&console, vt, true, NAME_MAX).await;
        write(&console, vt, b"Password: ");
        let password = read_line(&console, vt, false, PASSWORD_MAX).await;
        if name.is_empty() {
            continue;
        }
        let (cols, rows) = console.grid.get();
        let Some(term) = Term::open(
            &console.directory,
            &console.reactor,
            cols,
            rows,
            console.changed.clone(),
        )
        .await
        else {
            write(&console, vt, b"No terminal for the session.\r\n");
            continue;
        };
        let Some(token) = term.token() else { continue };
        match text_login.login(vt.number, name, password, token).await {
            Ok(Ok(_)) => {
                console
                    .spawner
                    .spawn(term.clone().pump(console.reactor.clone()));
                console.show(vt, View::Terminal(term.clone()), true);
                if !session_ended(&console, vt).await {
                    return;
                }
                term.close();
            }
            Ok(Err(Status::AccessDenied)) => {
                write(&console, vt, b"\r\nLogin incorrect\r\n");
                console
                    .reactor
                    .sleep_until(crate::now() + 1_000_000_000)
                    .await
                    .ok();
            }
            Ok(Err(status)) => {
                write(
                    &console,
                    vt,
                    format!("\r\nLogin failed: {status:?}\r\n").as_bytes(),
                );
                console
                    .reactor
                    .sleep_until(crate::now() + 1_000_000_000)
                    .await
                    .ok();
            }
            Err(_) => return,
        }
    }
}
