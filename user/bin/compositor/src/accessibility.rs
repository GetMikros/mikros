//! The accessibility settings that the compositor applies
//! (MIKSPEC-0102.R6): so far `pointer_size`, the pointer's size in
//! logical pixels, from the system's `accessibility.yaml`, or the
//! configuration's, which replaces it as a whole, and then the user's. A
//! file that cannot be parsed sets nothing.

pub const SYSTEM: &str = "/system/share/desktop/accessibility.yaml";
pub const CONFIG: &str = "/system/config/desktop/accessibility.yaml";
/// The user's file, below the home folder.
pub const USER: &str = "apps/system.compositor/data/accessibility.yaml";

/// The pointer's size when no file sets it.
pub const POINTER_SIZE: u32 = 24;

/// The `pointer_size` that a settings file sets, if it sets it, or why
/// the file cannot be parsed.
pub fn pointer_size(text: &[u8]) -> Result<Option<u32>, String> {
    let lines = text.split(|&b| b == b'\n');
    if lines
        .map(|l| l.trim_ascii())
        .all(|l| l.is_empty() || l.starts_with(b"#"))
    {
        return Ok(None);
    }
    let at =
        |line: usize, column: usize, what: &str| format!("line {line}, column {column}: {what}");
    let root =
        mikros_yaml::parse(text).map_err(|e| at(e.line, e.column, &format!("{:?}", e.kind)))?;
    let map = root.map().map_err(|e| at(e.line, e.column, "not a map"))?;
    let Some(node) = map.get("pointer_size") else {
        return Ok(None);
    };
    match node.integer(24, 128) {
        Ok(size) => Ok(Some(size as u32)),
        Err(_) => Err(at(node.line, node.column, "pointer_size is not 24 to 128")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_pointer_size() {
        assert_eq!(
            pointer_size(b"pointer_size: 48\nreduce_motion: true\n"),
            Ok(Some(48))
        );
        assert_eq!(pointer_size(b"contrast: high\n"), Ok(None));
        assert_eq!(pointer_size(b"# nothing\n"), Ok(None));
        assert!(pointer_size(b"pointer_size: 12\n").is_err());
        assert!(pointer_size(b"pointer_size: big\n").is_err());
    }
}
