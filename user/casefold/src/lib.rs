//! Unicode simple case folding, the mappings of status C and S of
//! `CaseFolding.txt` of Unicode 16.0.0, for the servers that look names
//! up case-insensitively (MIKSPEC-0071.R16). `cargo xtask casefold`
//! generates the table.

#![no_std]

mod table;

use table::FOLDINGS;
pub use table::UNICODE_VERSION;

/// The simple case folding of `c`: `c` itself unless it has one.
pub fn fold(c: char) -> char {
    match FOLDINGS.binary_search_by_key(&u32::from(c), |&(from, _)| from) {
        Ok(at) => char::from_u32(FOLDINGS[at].1).unwrap_or(c),
        Err(_) => c,
    }
}

/// Whether `a` and `b` are equal under simple case folding, which maps
/// each character to one character.
pub fn equal(a: &str, b: &str) -> bool {
    let mut b = b.chars();
    for x in a.chars() {
        match b.next() {
            Some(y) if fold(x) == fold(y) => {}
            _ => return false,
        }
    }
    b.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_as_unicode_16() {
        assert_eq!(UNICODE_VERSION, "16.0.0");
        assert_eq!(FOLDINGS.len(), 1484);
        assert!(FOLDINGS.windows(2).all(|w| w[0].0 < w[1].0));
        for (from, to) in [
            ('A', 'a'),
            ('a', 'a'),
            ('Σ', 'σ'),
            ('ς', 'σ'),
            // Kelvin sign, long s and capital sharp s, a simple folding.
            ('\u{212A}', 'k'),
            ('ſ', 's'),
            ('ẞ', 'ß'),
            // Only full foldings or Turkic ones: none.
            ('ß', 'ß'),
            ('İ', 'İ'),
            // Cherokee small letters fold to the capitals.
            ('\u{AB70}', '\u{13A0}'),
            ('7', '7'),
        ] {
            assert_eq!(fold(from), to, "{from:?}");
        }
    }

    #[test]
    fn compares_names() {
        assert!(equal("Program Files", "PROGRAM FILES"));
        assert!(equal("straße", "STRAẞE"));
        assert!(!equal("strasse", "straße"));
        assert!(!equal("a", "ab") && !equal("ab", "a"));
        assert!(equal("", ""));
    }
}
