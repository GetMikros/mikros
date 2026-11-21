//! The system's keyboard settings: the keys `model`, `layout`, `variant`
//! and `options` of `/system/config/keyboard.yaml`, each text, which name
//! the xkb keymap that the compositor compiles (MIKSPEC-0066.R6), and
//! `repeat_rate` and `repeat_delay`, the key repeat that clients get
//! (MIKSPEC-0066.R29), which a user's accessibility settings may replace
//! (MIKSPEC-0102.R14). A name that the file leaves out is empty, which
//! xkbcommon fills in with its defaults. Other keys are ignored, and an
//! invalid file sets nothing (MIKSPEC-0082.R5).

pub const PATH: &str = "/system/config/keyboard.yaml";

/// The names of a keymap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// The key repeat: keys per second, 0 for none, after a delay in
/// milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repeat {
    pub rate: u32,
    pub delay: u32,
}

impl Default for Repeat {
    fn default() -> Repeat {
        Repeat {
            rate: 25,
            delay: 600,
        }
    }
}

/// What a keyboard file sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub layout: Layout,
    pub repeat: Repeat,
}

fn at(line: usize, column: usize, what: &str) -> String {
    format!("line {line}, column {column}: {what}")
}

/// The `repeat_rate` and `repeat_delay` of `map`, those that it has, or
/// why one is not valid.
pub fn repeat_values(map: &mikros_yaml::Map) -> Result<(Option<u32>, Option<u32>), String> {
    let value = |key: &str, min: i64, max: i64| match map.get(key) {
        None => Ok(None),
        Some(node) => match node.integer(min, max) {
            Ok(value) => Ok(Some(value as u32)),
            Err(_) => Err(at(
                node.line,
                node.column,
                &format!("{key} is not {min} to {max}"),
            )),
        },
    };
    Ok((
        value("repeat_rate", 0, 100)?,
        value("repeat_delay", 100, 2000)?,
    ))
}

/// The settings that a keyboard file sets, with a note for each key that
/// it ignores, or why the file is invalid.
pub fn parse(text: &[u8]) -> Result<(Settings, Vec<String>), String> {
    let mut settings = Settings::default();
    // A file of only blank lines and comments sets nothing.
    let lines = text.split(|&b| b == b'\n');
    if lines
        .map(|l| l.trim_ascii())
        .all(|l| l.is_empty() || l.starts_with(b"#"))
    {
        return Ok((settings, Vec::new()));
    }
    let root =
        mikros_yaml::parse(text).map_err(|e| at(e.line, e.column, &format!("{:?}", e.kind)))?;
    let map = root.map().map_err(|e| at(e.line, e.column, "not a map"))?;
    let known = [
        "model",
        "layout",
        "variant",
        "options",
        "repeat_rate",
        "repeat_delay",
    ];
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
    let layout = &mut settings.layout;
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
    let (rate, delay) = repeat_values(&map)?;
    let repeat = &mut settings.repeat;
    repeat.rate = rate.unwrap_or(repeat.rate);
    repeat.delay = delay.unwrap_or(repeat.delay);
    Ok((settings, ignored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_layout() {
        let (settings, ignored) = parse(b"layout: de\nvariant: nodeadkeys\n").unwrap();
        assert_eq!(settings.layout.layout, "de");
        assert_eq!(settings.layout.variant, "nodeadkeys");
        assert_eq!(settings.layout.model, "");
        assert_eq!(settings.repeat, Repeat::default());
        assert!(ignored.is_empty());
        assert_eq!(parse(b"# us\n\n").unwrap().0, Settings::default());
    }

    #[test]
    fn reads_the_key_repeat() {
        let (settings, _) = parse(b"repeat_rate: 0\nrepeat_delay: 250\n").unwrap();
        assert_eq!(
            settings.repeat,
            Repeat {
                rate: 0,
                delay: 250
            }
        );
        let (settings, _) = parse(b"repeat_delay: 2000\n").unwrap();
        assert_eq!(
            settings.repeat,
            Repeat {
                rate: 25,
                delay: 2000
            }
        );
        for text in [
            &b"repeat_rate: 101\n"[..],
            b"repeat_rate: -1\n",
            b"repeat_delay: 99\n",
            b"repeat_delay: slow\n",
        ] {
            assert!(parse(text).is_err(), "{}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn ignores_unknown_keys_and_refuses_invalid_files() {
        let (settings, ignored) = parse(b"layout: fr\nrepeat: 30\n").unwrap();
        assert_eq!(settings.layout.layout, "fr");
        assert_eq!(
            ignored,
            ["line 2, column 1: the unknown key repeat is ignored"]
        );
        assert!(parse(b"layout: [us, de]\n").is_err());
        assert!(parse(b"- us\n").is_err());
    }
}
