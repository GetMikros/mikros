//! Package IDs, which are also the app IDs of their programs
//! (MIKSPEC-0101.R4).

/// The longest ID, in bytes.
pub const ID_MAX: usize = 128;
/// What the IDs of the system version start with.
pub const SYSTEM_PREFIX: &str = "system.";

/// Whether `id` is a package ID: two or more parts separated by `.`,
/// each a lowercase ASCII letter followed by lowercase ASCII letters,
/// digits and `_`, at most `ID_MAX` bytes in all.
pub fn is_valid(id: &str) -> bool {
    id.len() <= ID_MAX && id.split('.').count() >= 2 && id.split('.').all(part)
}

fn part(part: &str) -> bool {
    let mut chars = part.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `id` belongs to the system version, which packages may not
/// use.
pub fn is_system(id: &str) -> bool {
    id.starts_with(SYSTEM_PREFIX)
}

/// Whether `component` may be a component of the package `package`
/// (MIKSPEC-0101.R13): its ID is the package's followed by `.` and more.
pub fn is_component_of(component: &str, package: &str) -> bool {
    component
        .strip_prefix(package)
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_ids() {
        assert!(is_valid("org.example.editor"));
        assert!(is_valid("io.my_app2.x"));
        for bad in [
            "editor",
            "Org.example",
            "org..example",
            "org.example.",
            ".org.example",
            "org.2example",
            "org.ex-ample",
            "org._example",
            "",
        ] {
            assert!(!is_valid(bad), "{bad}");
        }
        let long = alloc::format!("a.{}", "b".repeat(ID_MAX - 2));
        assert!(is_valid(&long));
        assert!(!is_valid(&alloc::format!("{long}c")));
    }

    #[test]
    fn tells_system_ids_and_components() {
        assert!(is_system("system.cli"));
        assert!(!is_system("org.system"));
        assert!(is_component_of(
            "org.example.editor.plugin",
            "org.example.editor"
        ));
        assert!(!is_component_of("org.example.editor", "org.example.editor"));
        assert!(!is_component_of(
            "org.example.editors",
            "org.example.editor"
        ));
        assert!(!is_component_of(
            "org.example.editor.",
            "org.example.editor"
        ));
    }
}
