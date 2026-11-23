//! App entries (MIKSPEC-0075.R25): the keys of an app's `app.yaml` that the
//! process manager reads, which are the reasons that the app gives for the
//! permissions it needs (MIKSPEC-0094.R1).

use alloc::string::String;

/// The most bytes of a reason.
const REASON_MAX: usize = 256;

/// The reason that the entry in `bytes` gives for `permission`: the
/// string under that name in its `reasons` map. A reason of no bytes or
/// more than 256, with a control character or not a string, a name that
/// is not that of a `user` permission, an entry that is not a map and
/// `reasons` that is not one count as absent, and so is `None`.
pub fn reason(bytes: &[u8], permission: &str) -> Option<String> {
    if !permission.starts_with("user.") {
        return None;
    }
    let root = mikros_yaml::parse(bytes).ok()?;
    let entries = root.map().ok()?;
    let reasons = entries.get("reasons")?.map().ok()?;
    let text = reasons.get(permission)?.string().ok()?;
    let valid = !text.is_empty() && text.len() <= REASON_MAX && !text.chars().any(char::is_control);
    valid.then(|| String::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    const ENTRY: &str = "id: org.example.app\nname: App\nreasons:\n  \
        user.camera.use: To show you in video calls\n  \
        user.documents.access: \"Opens your documents\"\n  \
        admin.time.set: not a user permission\n  \
        user.location.read: \"\"\n";

    #[test]
    fn gives_the_reason_for_a_user_permission() {
        assert_eq!(
            reason(ENTRY.as_bytes(), "user.camera.use").as_deref(),
            Some("To show you in video calls")
        );
        assert_eq!(
            reason(ENTRY.as_bytes(), "user.documents.access").as_deref(),
            Some("Opens your documents")
        );
    }

    #[test]
    fn treats_what_breaks_the_rules_as_absent() {
        // No reason for it, an empty one, one for a permission that is
        // not a user's.
        assert_eq!(reason(ENTRY.as_bytes(), "user.microphone.use"), None);
        assert_eq!(reason(ENTRY.as_bytes(), "user.location.read"), None);
        assert_eq!(reason(ENTRY.as_bytes(), "admin.time.set"), None);
        // Too long, and with a control character.
        let long = format!("reasons:\n  user.camera.use: {}\n", "a".repeat(257));
        assert_eq!(reason(long.as_bytes(), "user.camera.use"), None);
        let fits = format!("reasons:\n  user.camera.use: {}\n", "a".repeat(256));
        assert_eq!(
            reason(fits.as_bytes(), "user.camera.use").map(|r| r.len()),
            Some(256)
        );
        let control = "reasons:\n  user.camera.use: \"a\\tb\"\n";
        assert_eq!(reason(control.as_bytes(), "user.camera.use"), None);
        // Not a map, or not YAML.
        assert_eq!(reason(b"reasons: [a, b]\n", "user.camera.use"), None);
        assert_eq!(reason(b"reasons: x\n", "user.camera.use"), None);
        assert_eq!(reason(b"\xff\xfe", "user.camera.use"), None);
        assert_eq!(reason(b"- a\n- b\n", "user.camera.use"), None);
    }
}
