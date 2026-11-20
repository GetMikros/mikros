//! The theme of the desktop (MIKSPEC-0066.R19): the values of the
//! system's `theme.yaml`, or of the configuration's, which replaces it as
//! a whole, with those that the user's file sets over them. A file that
//! cannot be read or parsed sets none.

/// The system's file, the configuration's and the user's, below the home
/// folder.
pub const SYSTEM: &str = "/system/share/desktop/theme.yaml";
pub const CONFIG: &str = "/system/config/desktop/theme.yaml";
pub const USER: &str = "apps/system.compositor/data/theme.yaml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
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
}

impl Theme {
    pub fn with(mut self, values: &Values) -> Theme {
        self.accent = values.accent.unwrap_or(self.accent);
        self.scheme = values.scheme.unwrap_or(self.scheme);
        self.font = values.font.clone().unwrap_or(self.font);
        self.font_size = values.font_size.unwrap_or(self.font_size);
        self
    }
}

/// The values of a theme file, or why it has none.
pub fn parse(text: &[u8]) -> Result<Values, String> {
    let at = |line: usize, what: &str| format!("line {line}: {what}");
    let mut values = Values::default();
    // A file of only blank lines and comments sets nothing.
    let lines = text.split(|&b| b == b'\n');
    if lines
        .map(|l| l.trim_ascii())
        .all(|l| l.is_empty() || l.starts_with(b"#"))
    {
        return Ok(values);
    }
    let root = mikros_yaml::parse(text).map_err(|e| at(e.line, &format!("{:?}", e.kind)))?;
    let map = root.map().map_err(|e| at(e.line, "not a map"))?;
    let known = ["accent", "scheme", "font", "font_size"];
    if let Some(entry) = map.unknown(&known).next() {
        return Err(at(entry.line, &format!("the unknown key {}", entry.key)));
    }
    let text = |key: &str| match map.get(key) {
        None => Ok(None),
        Some(node) => match node.string() {
            Ok(text) => Ok(Some((node.line, text))),
            Err(_) => Err(at(node.line, &format!("{key} is not text"))),
        },
    };
    if let Some((line, text)) = text("accent")? {
        values.accent = Some(colour(text).ok_or_else(|| at(line, "accent is not #rrggbb"))?);
    }
    if let Some((line, text)) = text("scheme")? {
        values.scheme = Some(match text {
            "light" => Scheme::Light,
            "dark" => Scheme::Dark,
            _ => return Err(at(line, "scheme is neither light nor dark")),
        });
    }
    if let Some((line, text)) = text("font")? {
        if text.is_empty() {
            return Err(at(line, "font is empty"));
        }
        values.font = Some(text.into());
    }
    if let Some((line, text)) = text("font_size")? {
        let size = hundredths(text).filter(|&s| (100..=100_000).contains(&s));
        values.font_size = Some(size.ok_or_else(|| at(line, "font_size is not 1 to 1000 points"))?);
    }
    Ok(values)
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
        let text = b"accent: \"#2279fb\"\nscheme: dark\nfont: DejaVu Sans\nfont_size: 10.5\n";
        let theme = Theme::default().with(&parse(text).unwrap());
        assert_eq!(
            theme,
            Theme {
                accent: [0x22, 0x79, 0xfb],
                scheme: Scheme::Dark,
                font: "DejaVu Sans".into(),
                font_size: 1050,
            }
        );
    }

    #[test]
    fn sets_only_the_values_a_file_has() {
        let system =
            parse(b"accent: \"#0066ff\"\nscheme: light\nfont: sans-serif\nfont_size: 10\n");
        let user = parse(b"scheme: dark\n").unwrap();
        let theme = Theme::default().with(&system.unwrap()).with(&user);
        assert_eq!(theme.scheme, Scheme::Dark);
        assert_eq!(theme.accent, [0x00, 0x66, 0xff]);
        assert_eq!(theme.font_size, 1000);
        assert_eq!(parse(b"").unwrap(), Values::default());
        assert_eq!(parse(b"# nothing\n\n").unwrap(), Values::default());
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
            b"colour: \"#000000\"\n",
            b"- accent\n",
        ] {
            assert!(parse(text).is_err(), "{}", String::from_utf8_lossy(text));
        }
    }
}
