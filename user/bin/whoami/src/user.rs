//! What whoami takes, and how it writes the caller's user
//! (MIKSPEC-0098.R56): its name, `system` for user ID 0, or an object
//! with `name` and `id` in JSON.

use mikros_cli::{Opt, Spec, json_string};

pub static COMMAND: Spec = Spec {
    options: &[Opt::long(
        "json",
        "write an object with the user's name and ID",
    )],
    ..Spec::new(
        "whoami",
        "Writes the name of your user, system for user ID 0, also under sudo.",
    )
};

/// The name of user ID 0, which the accounts service does not list.
pub const SYSTEM: &str = "system";

/// The name of the user `id` among `users`, a page of the accounts
/// service's IDs and names.
pub fn name_in(id: u32, users: &[(u32, String)]) -> Option<&str> {
    users
        .iter()
        .find(|(user, _)| *user == id)
        .map(|(_, name)| name.as_str())
}

/// What whoami writes for the user `id` named `name`.
pub fn shown(name: &str, id: u32, json: bool) -> String {
    match json {
        true => format!("{{\"name\":{},\"id\":{id}}}\n", json_string(name)),
        false => format!("{name}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_user_in_a_page() {
        let users = vec![(1000, "ada".to_string()), (1001, "bob".to_string())];
        assert_eq!(name_in(1001, &users), Some("bob"));
        assert_eq!(name_in(1002, &users), None);
    }

    #[test]
    fn writes_the_name_or_json() {
        assert_eq!(shown("ada", 1000, false), "ada\n");
        assert_eq!(shown("system", 0, true), "{\"name\":\"system\",\"id\":0}\n");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/whoami.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
