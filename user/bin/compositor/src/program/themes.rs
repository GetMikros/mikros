//! Reading the theme's files (MIKSPEC-0066.R27) and the pointer size of
//! the accessibility settings (MIKSPEC-0102.R6) when the compositor starts
//! and once a second after, so that a change applies within a second. Of
//! each, the configuration's file is used when it exists and can be
//! parsed, the system's otherwise, and then the user's file, below the
//! home folder that the session manager gives.

use super::Compositor;
use crate::accessibility;
use crate::theme::{self, Theme};
use std::rc::Rc;

/// How often the files are read, in nanoseconds.
const PERIOD: i64 = 1_000_000_000;

/// The pointer size of the accessibility settings (MIKSPEC-0102.R6), read
/// as the theme is, and what was wrong with their files.
fn pointer_size(problems: &mut Vec<String>) -> u32 {
    let mut size = |path: &str| {
        let text = std::fs::read(path).ok()?;
        accessibility::pointer_size(&text)
            .map_err(|e| problems.push(format!("{path}: {e}")))
            .ok()
    };
    let base = size(accessibility::CONFIG).or_else(|| size(accessibility::SYSTEM));
    let home = std::env::var("HOME").ok();
    let user = home.and_then(|home| size(&format!("{home}/{}", accessibility::USER)));
    user.flatten()
        .or(base.flatten())
        .unwrap_or(accessibility::POINTER_SIZE)
}

/// The theme of the files, and what was wrong with them.
fn read() -> (Theme, Vec<String>) {
    let mut problems = Vec::new();
    let mut values = |path: &str| {
        let text = std::fs::read(path).ok()?;
        theme::parse(&text)
            .map_err(|e| problems.push(format!("{path}: {e}")))
            .ok()
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
            let size = pointer_size(&mut problems);
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
