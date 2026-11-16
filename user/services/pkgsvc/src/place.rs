//! Where packages are installed (MIKSPEC-0101.R36), as paths from the
//! root of the root file system, whose top-level subvolumes are `apps`,
//! `libraries`, `drivers` and `users` (MIKSPEC-0100.R4).

use mikros_mika::manifest::Kind;

/// Whom a package is installed for: every user, or the user of this name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Local(String),
}

/// The subvolume of the package `id` of `kind` for `scope`, from the root
/// of the root file system; none for a driver for one user, which is not
/// allowed.
pub fn subvolume(kind: Kind, id: &str, scope: &Scope) -> Option<String> {
    match (kind, scope) {
        (Kind::App, Scope::Global) => Some(format!("apps/{id}")),
        (Kind::App, Scope::Local(user)) => Some(format!("users/{user}/apps/{id}/package")),
        (Kind::Library, Scope::Global) => Some(format!("libraries/{id}")),
        (Kind::Library, Scope::Local(user)) => Some(format!("users/{user}/libraries/{id}")),
        (Kind::Driver, Scope::Global) => Some(format!("drivers/{id}")),
        (Kind::Driver, Scope::Local(_)) => None,
    }
}

/// The folders whose entries are the installed packages of `scope`, each
/// with the kind of the packages it holds, and, for a user's apps, the
/// folder below an app's that holds its package.
pub fn folders(scope: &Scope) -> Vec<(String, Kind, Option<&'static str>)> {
    match scope {
        Scope::Global => vec![
            ("apps".into(), Kind::App, None),
            ("libraries".into(), Kind::Library, None),
            ("drivers".into(), Kind::Driver, None),
        ],
        Scope::Local(user) => vec![
            (format!("users/{user}/apps"), Kind::App, Some("package")),
            (format!("users/{user}/libraries"), Kind::Library, None),
        ],
    }
}

/// Whether a library's or a driver's subvolume is made read-only once it
/// is written; an app's stays writable.
pub fn read_only(kind: Kind) -> bool {
    kind != Kind::App
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_each_kind() {
        let ada = Scope::Local("ada".into());
        let id = "org.example.x";
        assert_eq!(
            subvolume(Kind::App, id, &Scope::Global).unwrap(),
            "apps/org.example.x"
        );
        assert_eq!(
            subvolume(Kind::App, id, &ada).unwrap(),
            "users/ada/apps/org.example.x/package"
        );
        assert_eq!(
            subvolume(Kind::Library, id, &ada).unwrap(),
            "users/ada/libraries/org.example.x"
        );
        assert_eq!(
            subvolume(Kind::Driver, id, &Scope::Global).unwrap(),
            "drivers/org.example.x"
        );
        assert_eq!(subvolume(Kind::Driver, id, &ada), None);
        assert!(read_only(Kind::Library) && read_only(Kind::Driver) && !read_only(Kind::App));
        assert_eq!(folders(&ada)[0].2, Some("package"));
    }
}
