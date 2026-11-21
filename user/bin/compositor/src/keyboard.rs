//! The system's keyboard layout (MIKSPEC-0066.R6): the keys `model`,
//! `layout`, `variant` and `options` of `/system/config/keyboard.yaml`,
//! each text, which name the xkb keymap that the compositor compiles. A
//! key that the file leaves out is empty, which xkbcommon fills in with
//! its defaults. Other keys are ignored, and an invalid file sets nothing
//! (MIKSPEC-0082.R5).

pub const PATH: &str = "/system/config/keyboard.yaml";

/// The names of a keymap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// The layout that a keyboard file sets, with a note for each key that
/// it ignores, or why the file is invalid.
pub fn parse(text: &[u8]) -> Result<(Layout, Vec<String>), String> {
    let mut layout = Layout::default();
    // A file of only blank lines and comments sets nothing.
    let lines = text.split(|&b| b == b'\n');
    if lines
        .map(|l| l.trim_ascii())
        .all(|l| l.is_empty() || l.starts_with(b"#"))
    {
        return Ok((layout, Vec::new()));
    }
    let at =
        |line: usize, column: usize, what: &str| format!("line {line}, column {column}: {what}");
    let root =
        mikros_yaml::parse(text).map_err(|e| at(e.line, e.column, &format!("{:?}", e.kind)))?;
    let map = root.map().map_err(|e| at(e.line, e.column, "not a map"))?;
    let known = ["model", "layout", "variant", "options"];
    let ignored = map
        .unknown(&known)
        .map(|e| {
            at(
                e.line,
                e.column,
                &format!("the unknown key {} is ignored", e.key),
            )
        })
        .collect();
    let fields = [
        ("model", &mut layout.model),
        ("layout", &mut layout.layout),
        ("variant", &mut layout.variant),
        ("options", &mut layout.options),
    ];
    for (key, field) in fields {
        if let Some(node) = map.get(key) {
            let text = node
                .string()
                .map_err(|_| at(node.line, node.column, &format!("{key} is not text")))?;
            *field = text.into();
        }
    }
    Ok((layout, ignored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_layout() {
        let (layout, ignored) = parse(b"layout: de\nvariant: nodeadkeys\n").unwrap();
        assert_eq!(layout.layout, "de");
        assert_eq!(layout.variant, "nodeadkeys");
        assert_eq!(layout.model, "");
        assert!(ignored.is_empty());
        assert_eq!(parse(b"# us\n\n").unwrap().0, Layout::default());
    }

    #[test]
    fn ignores_unknown_keys_and_refuses_invalid_files() {
        let (layout, ignored) = parse(b"layout: fr\nrepeat: 30\n").unwrap();
        assert_eq!(layout.layout, "fr");
        assert_eq!(
            ignored,
            ["line 2, column 1: the unknown key repeat is ignored"]
        );
        assert!(parse(b"layout: [us, de]\n").is_err());
        assert!(parse(b"- us\n").is_err());
    }
}
