//! Reading the theme's files (MIKSPEC-0066.R27), the pointer size of the
//! accessibility settings (MIKSPEC-0102.R6) and the key repeat of the
//! keyboard settings (MIKSPEC-0066.R29) when the compositor starts and once
//! a second after, so that a change applies within a second. Of the theme
//! and the accessibility settings, the configuration's file is used when
//! it exists and can be parsed, the system's otherwise, and then the
//! user's file, below the home folder that the session manager gives,
//! whose key repeat replaces that of the keyboard settings
//! (MIKSPEC-0102.R14).

use super::Compositor;
use crate::accessibility;
use crate::keyboard::{self, Repeat};
use crate::theme::{self, Theme};
use std::rc::Rc;

/// How often the files are read, in nanoseconds.
const PERIOD: i64 = 1_000_000_000;

/// The pointer size of the accessibility settings and the key repeat,
/// read as the theme is, and what was wrong with their files.
fn settings(problems: &mut Vec<String>) -> (u32, Repeat) {
    let mut values = |path: &str, user: bool| {
        let text = std::fs::read(path).ok()?;
        accessibility::parse(&text, user)
            .map_err(|e| problems.push(format!("{path}: {e}")))
            .ok()
    };
    let base = values(accessibility::CONFIG, false)
        .or_else(|| values(accessibility::SYSTEM, false))
        .unwrap_or_default();
    let home = std::env::var("HOME").ok();
    let user = home
        .and_then(|home| values(&format!("{home}/{}", accessibility::USER), true))
        .unwrap_or_default();
    let size = user
        .pointer_size
        .or(base.pointer_size)
        .unwrap_or(accessibility::POINTER_SIZE);
    let system = match std::fs::read(keyboard::PATH) {
        Ok(text) => match keyboard::parse(&text) {
            Ok((settings, _)) => settings.repeat,
            Err(e) => {
                problems.push(format!("{}: {e}", keyboard::PATH));
                Repeat::default()
            }
        },
        Err(_) => Repeat::default(),
    };
    let repeat = Repeat {
        rate: user.repeat_rate.unwrap_or(system.rate),
        delay: user.repeat_delay.unwrap_or(system.delay),
    };
    (size, repeat)
}

/// The theme of the files, and what was wrong with them.
fn read() -> (Theme, Vec<String>) {
    let mut problems = Vec::new();
    let mut values = |path: &str| {
        let text = std::fs::read(path).ok()?;
        let (values, ignored) = theme::parse(&text)
            .map_err(|e| problems.push(format!("{path}: {e}")))
            .ok()?;
        problems.extend(ignored.into_iter().map(|note| format!("{path}: {note}")));
        Some(values)
    };
    let base = values(theme::CONFIG).or_else(|| values(theme::SYSTEM));
    let user = std::env::var("HOME")
        .ok()
        .and_then(|home| values(&format!("{home}/{}", theme::USER)));
    let theme = [base, user]
        .iter()
        .flatten()
        .fold(Theme::default(), |theme, values| theme.with(values));
    (theme, problems)
}

impl Compositor {
    /// Gives the server the theme whenever the files change it.
    pub(super) async fn themes(self: Rc<Self>) {
        let mut reported = Vec::new();
        loop {
            let (theme, mut problems) = read();
            let (size, repeat) = settings(&mut problems);
            if problems != reported {
                for problem in &problems {
                    mikros_rt::log!(Error, "compositor: the settings file {problem}");
                }
                reported = problems;
            }
            {
                let mut pointer = self.pointer.borrow_mut();
                let themed = pointer.set_theme(&theme.cursor, &self.renderer);
                if pointer.set_size(size, &self.renderer) || themed {
                    self.damaged_all.set(true);
                    self.changed.notify();
                }
            }
            if self
                .server
                .borrow_mut()
                .set_repeat(repeat.rate, repeat.delay)
            {
                self.flush_all();
            }
            if *self.server.borrow().theme() != theme {
                // The haze is tinted with the scheme.
                let mut store = self.store.borrow_mut();
                if store.scheme != theme.scheme {
                    store.scheme = theme.scheme;
                    store.remake_haze(&self.renderer);
                }
                drop(store);
                self.server.borrow_mut().set_theme(theme);
                self.flush_all();
                self.changed.notify();
            }
            let next = mikros_rt::objects::now() + PERIOD;
            if self.reactor.sleep_until(next).await.is_err() {
                mikros_rt::log!(Error, "compositor: no timer for reading the theme");
                return;
            }
        }
    }
}
