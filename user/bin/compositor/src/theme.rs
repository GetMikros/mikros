//! The theme of the desktop (MIKSPEC-0066.R27): the values of the
//! system's `theme.yaml`, or of the configuration's, which replaces it as
//! a whole, with those that the user's file sets over them. A file that
//! cannot be read or parsed sets none, and keys that the theme does not
//! have are ignored (MIKSPEC-0082.R5).

/// The system's file, the configuration's and the user's, below the home
/// folder.
pub const SYSTEM: &str = "/system/share/desktop/theme.yaml";
pub const CONFIG: &str = "/system/config/desktop/theme.yaml";
pub const USER: &str = "apps/system.compositor/data/theme.yaml";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scheme {
    #[default]
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub accent: [u8; 3],
    pub scheme: Scheme,
    pub font: String,
    /// In hundredths of a point.
    pub font_size: u32,
    /// The name of the cursor theme, which only the compositor uses.
    pub cursor: String,
}

/// The theme that the system's file sets, for the values that no file
/// sets.
impl Default for Theme {
    fn default() -> Theme {
        Theme {
            accent: [0x00, 0x66, 0xff],
            scheme: Scheme::Light,
            font: "sans-serif".into(),
            font_size: 1000,
            cursor: "default".into(),
        }
    }
}

/// The values that a file sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    accent: Option<[u8; 3]>,
    scheme: Option<Scheme>,
    font: Option<String>,
    font_size: Option<u32>,
    cursor: Option<String>,
}

impl Theme {
    pub fn with(mut self, values: &Values) -> Theme {
        self.accent = values.accent.unwrap_or(self.accent);
        self.scheme = values.scheme.unwrap_or(self.scheme);
        self.font = values.font.clone().unwrap_or(self.font);
        self.font_size = values.font_size.unwrap_or(self.font_size);
        self.cursor = values.cursor.clone().unwrap_or(self.cursor);
        self
    }
}

/// The values of a theme file, with a note for each key that it ignores,
/// or why it has none.
pub fn parse(text: &[u8]) -> Result<(Values, Vec<String>), String> {
    let at =
        |line: usize, column: usize, what: &str| format!("line {line}, column {column}: {what}");
    let mut values = Values::default();
    // A file of only blank lines and comments sets nothing.
    let lines = text.split(|&b| b == b'\n');
    if lines
        .map(|l| l.trim_ascii())
        .all(|l| l.is_empty() || l.starts_with(b"#"))
    {
        return Ok((values, Vec::new()));
    }
    let root =
        mikros_yaml::parse(text).map_err(|e| at(e.line, e.column, &format!("{:?}", e.kind)))?;
    let map = root.map().map_err(|e| at(e.line, e.column, "not a map"))?;
    let known = ["accent", "scheme", "font", "font_size", "cursor"];
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
    let text = |key: &str| match map.get(key) {
        None => Ok(None),
        Some(node) => match node.string() {
            Ok(text) => Ok(Some(((node.line, node.column), text))),
            Err(_) => Err(at(node.line, node.column, &format!("{key} is not text"))),
        },
    };
    let at = |(line, column): (usize, usize), what: &str| at(line, column, what);
    if let Some((place, text)) = text("accent")? {
        values.accent = Some(colour(text).ok_or_else(|| at(place, "accent is not #rrggbb"))?);
    }
    if let Some((place, text)) = text("scheme")? {
        values.scheme = Some(match text {
            "light" => Scheme::Light,
            "dark" => Scheme::Dark,
            _ => return Err(at(place, "scheme is neither light nor dark")),
        });
    }
    if let Some((place, text)) = text("font")? {
        if text.is_empty() {
            return Err(at(place, "font is empty"));
        }
        values.font = Some(text.into());
    }
    if let Some((place, text)) = text("cursor")? {
        // A file name in /system/share/cursors.
        let name = text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if text.is_empty() || text.starts_with('.') || !name {
            return Err(at(place, "cursor is not the name of a cursor theme"));
        }
        values.cursor = Some(text.into());
    }
    if let Some((place, text)) = text("font_size")? {
        let size = hundredths(text).filter(|&s| (100..=100_000).contains(&s));
        values.font_size =
            Some(size.ok_or_else(|| at(place, "font_size is not 1 to 1000 points"))?);
    }
    Ok((values, ignored))
}

/// The red, green and blue of `#rrggbb`.
fn colour(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// The hundredths in a number of points, with at most two decimals.
fn hundredths(text: &str) -> Option<u32> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || (!fraction.is_empty() && !digits(fraction)) || fraction.len() > 2 {
        return None;
    }
    let fraction = format!("{fraction:0<2}").parse::<u32>().ok()?;
    whole
        .parse::<u32>()
        .ok()?
        .checked_mul(100)?
        .checked_add(fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_key() {
        let text =
            b"accent: \"#2279fb\"\nscheme: dark\nfont: DejaVu Sans\nfont_size: 10.5\ncursor: big\n";
        let theme = Theme::default().with(&parse(text).unwrap().0);
        assert_eq!(
            theme,
            Theme {
                accent: [0x22, 0x79, 0xfb],
                scheme: Scheme::Dark,
                font: "DejaVu Sans".into(),
                font_size: 1050,
                cursor: "big".into(),
            }
        );
    }

    #[test]
    fn sets_only_the_values_a_file_has() {
        let system =
            parse(b"accent: \"#0066ff\"\nscheme: light\nfont: sans-serif\nfont_size: 10\n");
        let user = parse(b"scheme: dark\n").unwrap().0;
        let theme = Theme::default().with(&system.unwrap().0).with(&user);
        assert_eq!(theme.scheme, Scheme::Dark);
        assert_eq!(theme.accent, [0x00, 0x66, 0xff]);
        assert_eq!(theme.font_size, 1000);
        assert_eq!(parse(b"").unwrap().0, Values::default());
        assert_eq!(parse(b"# nothing\n\n").unwrap().0, Values::default());
    }

    #[test]
    fn refuses_values_that_are_not_valid() {
        for text in [
            &b"accent: \"#12345\"\n"[..],
            b"accent: blue\n",
            b"scheme: grey\n",
            b"font: \"\"\n",
            b"font_size: 0\n",
            b"font_size: 10.555\n",
            b"font_size: ten\n",
            b"cursor: ../x\n",
            b"cursor: \"\"\n",
            b"- accent\n",
        ] {
            assert!(parse(text).is_err(), "{}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn ignores_unknown_keys() {
        let (values, ignored) = parse(b"scheme: dark\ncolour: \"#000000\"\n").unwrap();
        assert_eq!(values.scheme, Some(Scheme::Dark));
        assert_eq!(
            ignored,
            ["line 2, column 1: the unknown key colour is ignored"]
        );
    }
}
