//! Setup and logins through `TextLogin`: while no user account exists,
//! the setup program runs on a terminal of its own; otherwise the console
//! asks for a name and a password, without echoing the password, and
//! shows the terminal of the session that `Login` starts until the
//! session ends. Each terminal is closed after its use, and the login
//! prompt shows again.

use crate::term::Term;
use crate::{Console, View};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::{TextLoginAsyncClient, TextLoginEvent};
use mikros_vt::screen::Screen;
use std::rc::Rc;

/// The longest name and password that `Login` takes.
const NAME_MAX: usize = 32;
const PASSWORD_MAX: usize = 1024;
const GREETING: &str = "\x1b[2J\x1b[HMikros\r\n\r\n";

/// Shows the login prompt's screen with `text` written to it.
fn prompt(console: &Console, text: &str) {
    let (cols, rows) = console.grid.get();
    let mut screen = Screen::new(cols, rows);
    screen.write(text.as_bytes());
    console.show(View::Login(screen));
}

/// Writes `text` to the login prompt's screen.
fn write(console: &Console, text: &[u8]) {
    if let View::Login(screen) = &mut *console.view.borrow_mut() {
        screen.write(text);
    }
    console.changed.notify();
}

/// Reads a line typed at the login prompt, of at most `max` bytes,
/// echoing it when `echo` is set.
async fn read_line(console: &Console, echo: bool, max: usize) -> String {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let b = loop {
            if let Some(b) = console.typed.borrow_mut().pop_front() {
                break b;
            }
            console.typed_in.changed().await;
        };
        match b {
            b'\r' | b'\n' => {
                write(console, b"\r\n");
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
                    write(console, b"\x08 \x08");
                }
            }
            0x15 => {
                if echo {
                    for _ in String::from_utf8_lossy(&line).chars() {
                        write(console, b"\x08 \x08");
                    }
                }
                line.clear();
            }
            b if b < 0x20 => {}
            b if line.len() < max => {
                line.push(b);
                if echo {
                    write(console, &[b]);
                }
            }
            _ => {}
        }
    }
}

/// Opens a terminal of the grid's size and shows it, its output read
/// into its screen.
async fn show_terminal(console: &Rc<Console>) -> Option<Rc<Term>> {
    let (cols, rows) = console.grid.get();
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
    console.show(View::Terminal(term.clone()));
    Some(term)
}

/// Waits for `OnSessionEnded`: false when the channel closed instead.
async fn session_ended(text_login: &TextLoginAsyncClient) -> bool {
    matches!(
        text_login.next_event().await,
        Ok(TextLoginEvent::OnSessionEnded(_))
    )
}

/// Runs setup and logins for as long as the session manager serves
/// `TextLogin`.
pub async fn run(console: Rc<Console>, text_login: Rc<TextLoginAsyncClient>) {
    loop {
        let Ok(setup) = text_login.needs_setup().await else {
            eprintln!("console: the session manager does not answer");
            return;
        };
        if setup.needed {
            let Some(term) = show_terminal(&console).await else {
                eprintln!("console: no terminal for setup");
                return;
            };
            let Some(token) = term.token() else { return };
            if let Ok(Err(status)) = text_login.run_setup(token).await {
                eprintln!("console: RunSetup failed: {status:?}");
            }
            term.close();
            continue;
        }
        prompt(&console, &format!("{GREETING}login: "));
        let name = read_line(&console, true, NAME_MAX).await;
        write(&console, b"Password: ");
        let password = read_line(&console, false, PASSWORD_MAX).await;
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
            write(&console, b"No terminal for the session.\r\n");
            continue;
        };
        let Some(token) = term.token() else { continue };
        match text_login.login(name, password, token).await {
            Ok(Ok(_)) => {
                console
                    .spawner
                    .spawn(term.clone().pump(console.reactor.clone()));
                console.show(View::Terminal(term.clone()));
                if !session_ended(&text_login).await {
                    return;
                }
                term.close();
            }
            Ok(Err(Status::AccessDenied)) => {
                write(&console, b"\r\nLogin incorrect\r\n");
                console
                    .reactor
                    .sleep_until(crate::now() + 1_000_000_000)
                    .await
                    .ok();
            }
            Ok(Err(status)) => {
                write(
                    &console,
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
