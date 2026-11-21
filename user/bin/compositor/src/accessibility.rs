//! The accessibility settings that the compositor applies
//! (MIKSPEC-0102.R6): so far `pointer_size`, the pointer's size in
//! logical pixels, from the system's `accessibility.yaml`, or the
//! configuration's, which replaces it as a whole, and then the user's,
//! which may also set the key repeat (MIKSPEC-0102.R14). A file that
//! cannot be parsed sets nothing.

pub const SYSTEM: &str = "/system/share/desktop/accessibility.yaml";
pub const CONFIG: &str = "/system/config/desktop/accessibility.yaml";
/// The user's file, below the home folder.
pub const USER: &str = "apps/system.compositor/data/accessibility.yaml";

/// The pointer's size when no file sets it.
pub const POINTER_SIZE: u32 = 24;

/// What a settings file sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Values {
    pub pointer_size: Option<u32>,
    /// Only a user's file sets these.
    pub repeat_rate: Option<u32>,
    pub repeat_delay: Option<u32>,
}

/// What a settings file sets, a user's when `user`, or why the file
/// cannot be parsed.
pub fn parse(text: &[u8], user: bool) -> Result<Values, String> {
    let mut values = Values::default();
    let lines = text.split(|&b| b == b'\n');
    if lines
        .map(|l| l.trim_ascii())
        .all(|l| l.is_empty() || l.starts_with(b"#"))
    {
        return Ok(values);
    }
    let at =
        |line: usize, column: usize, what: &str| format!("line {line}, column {column}: {what}");
    let root =
        mikros_yaml::parse(text).map_err(|e| at(e.line, e.column, &format!("{:?}", e.kind)))?;
    let map = root.map().map_err(|e| at(e.line, e.column, "not a map"))?;
    if let Some(node) = map.get("pointer_size") {
        let size = node
            .integer(24, 128)
            .map_err(|_| at(node.line, node.column, "pointer_size is not 24 to 128"))?;
        values.pointer_size = Some(size as u32);
    }
    if user {
        (values.repeat_rate, values.repeat_delay) = crate::keyboard::repeat_values(&map)?;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(text: &[u8]) -> Result<Option<u32>, String> {
        parse(text, false).map(|v| v.pointer_size)
    }

    #[test]
    fn reads_the_pointer_size() {
        assert_eq!(
            size(b"pointer_size: 48\nreduce_motion: true\n"),
            Ok(Some(48))
        );
        assert_eq!(size(b"contrast: high\n"), Ok(None));
        assert_eq!(size(b"# nothing\n"), Ok(None));
        assert!(size(b"pointer_size: 12\n").is_err());
        assert!(size(b"pointer_size: big\n").is_err());
    }

    #[test]
    fn reads_the_key_repeat_of_users_only() {
        let text = b"repeat_rate: 40\nrepeat_delay: 300\n";
        let user = parse(text, true).unwrap();
        assert_eq!((user.repeat_rate, user.repeat_delay), (Some(40), Some(300)));
        assert_eq!(parse(text, false).unwrap(), Values::default());
        assert!(parse(b"repeat_rate: 200\n", true).is_err());
        assert!(parse(b"repeat_rate: 200\n", false).is_ok());
    }
}
