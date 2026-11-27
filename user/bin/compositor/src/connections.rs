//! Which forwarded connections the compositor serves
//! (MIKSPEC-0066.R40): in greeter mode only those of the apps that it
//! started there, and in a user session all.

use std::collections::BTreeSet;

/// Whether to serve the connection of the app `app_id`, when the compositor
/// runs in greeter mode or not and has started the apps `started`.
pub fn accepts(greeter_mode: bool, started: &BTreeSet<String>, app_id: &str) -> bool {
    !greeter_mode || started.contains(app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeter_mode_serves_only_the_apps_it_started() {
        let started: BTreeSet<String> = ["system.mikros.greeter".to_owned()].into();
        assert!(accepts(true, &started, "system.mikros.greeter"));
        assert!(!accepts(true, &started, "org.example.app"));
        assert!(!accepts(true, &started, ""));
        assert!(!accepts(true, &BTreeSet::new(), "system.mikros.greeter"));
    }

    #[test]
    fn a_user_session_serves_every_app() {
        assert!(accepts(false, &BTreeSet::new(), "org.example.app"));
    }
}
