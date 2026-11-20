//! Reading the theme's files (MIKSPEC-0066.R19) when the compositor
//! starts and once a second after, so that clients get a change within a
//! second. The configuration's file is used when it exists and can be
//! parsed, the system's otherwise, and then the user's file, below the
//! home folder that the session manager gives.

use super::Compositor;
use crate::theme::{self, Theme};
use std::rc::Rc;

/// How often the files are read, in nanoseconds.
const PERIOD: i64 = 1_000_000_000;

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
            let (theme, problems) = read();
            if problems != reported {
                for problem in &problems {
                    mikros_rt::log!(Error, "compositor: the theme file {problem}");
                }
                reported = problems;
            }
            if *self.server.borrow().theme() != theme {
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
