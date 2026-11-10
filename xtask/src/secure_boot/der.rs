//! The few DER encodings that certificates and PKCS #7 signatures need,
//! and a reader for the elements of a DER structure.

use anyhow::{Result, bail};

pub const INTEGER: u8 = 0x02;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// An element with the tag `tag` and the contents `content`.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(content);
    out
}

/// An element with the tag `tag` whose contents are `parts` one after
/// the other.
pub fn cons(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
    tlv(tag, &parts.concat())
}

pub fn seq(parts: &[&[u8]]) -> Vec<u8> {
    cons(SEQUENCE, parts)
}

/// A SET OF `parts`, which DER orders by their encodings.
pub fn set_of(parts: &[&[u8]]) -> Vec<u8> {
    let mut sorted = parts.to_vec();
    sorted.sort();
    cons(SET, &sorted)
}

/// The constructed context-specific tag `[n]`.
pub fn context(n: u8, parts: &[&[u8]]) -> Vec<u8> {
    cons(0xa0 | n, parts)
}

/// An object identifier, given by its arcs.
pub fn oid(arcs: &[u64]) -> Vec<u8> {
    let mut content = Vec::new();
    let first = [arcs[0] * 40 + arcs[1]];
    for &arc in first.iter().chain(&arcs[2..]) {
        let mut groups = vec![(arc & 0x7f) as u8];
        let mut rest = arc >> 7;
        while rest != 0 {
            groups.push(0x80 | (rest & 0x7f) as u8);
            rest >>= 7;
        }
        content.extend(groups.iter().rev());
    }
    tlv(0x06, &content)
}

/// The non-negative INTEGER whose big-endian bytes are `bytes`.
pub fn uint(bytes: &[u8]) -> Vec<u8> {
    let skip = bytes.iter().take_while(|&&b| b == 0).count();
    let mut content = bytes[skip..].to_vec();
    if content.first().is_none_or(|&b| b & 0x80 != 0) {
        content.insert(0, 0);
    }
    tlv(INTEGER, &content)
}

pub fn small_uint(n: u64) -> Vec<u8> {
    uint(&n.to_be_bytes())
}

pub fn null() -> Vec<u8> {
    vec![0x05, 0x00]
}

pub fn octets(bytes: &[u8]) -> Vec<u8> {
    tlv(0x04, bytes)
}

/// A BIT STRING of whole bytes.
pub fn bits(bytes: &[u8]) -> Vec<u8> {
    cons(0x03, &[&[0], bytes])
}

pub fn utf8(text: &str) -> Vec<u8> {
    tlv(0x0c, text.as_bytes())
}

pub fn boolean(value: bool) -> Vec<u8> {
    vec![0x01, 0x01, if value { 0xff } else { 0 }]
}

/// The time `unix` seconds after 1970 in UTC, `years` years later, as a
/// UTCTime before 2050 and a GeneralizedTime from then on. 29 February
/// becomes 28 February in years that have no such day.
pub fn time(unix: u64, years: u64) -> Vec<u8> {
    let (year, month, mut day) = date(unix / 86400);
    let year = year + years;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    if month == 2 && day == 29 && !leap {
        day = 28;
    }
    let secs = unix % 86400;
    let clock = format!(
        "{month:02}{day:02}{:02}{:02}{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    if year < 2050 {
        tlv(0x17, format!("{:02}{clock}", year % 100).as_bytes())
    } else {
        tlv(0x18, format!("{year:04}{clock}").as_bytes())
    }
}

/// The year, month and day of the day `days` after 1 January 1970.
pub fn date(days: u64) -> (u64, u64, u64) {
    // Howard Hinnant's civil_from_days, for days on or after 1970.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// An element read from DER input.
pub struct Element<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    /// The whole element, with its tag and length.
    pub raw: &'a [u8],
}

/// Reads the elements of `input` one after the other.
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    /// The next element, which must have the tag `tag`.
    pub fn expect(&mut self, tag: u8) -> Result<Element<'a>> {
        let element = self.next()?;
        if element.tag != tag {
            bail!("expected DER tag {tag:#x}, found {:#x}", element.tag);
        }
        Ok(element)
    }

    pub fn next(&mut self) -> Result<Element<'a>> {
        let input = self.0;
        let (&tag, rest) = input.split_first().ok_or_else(too_short)?;
        let (&first, rest) = rest.split_first().ok_or_else(too_short)?;
        let (len, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else {
            let n = usize::from(first & 0x7f);
            if n == 0 || n > 4 || rest.len() < n {
                bail!("bad DER length");
            }
            let len = rest[..n].iter().fold(0, |l, &b| l << 8 | usize::from(b));
            (len, &rest[n..])
        };
        if rest.len() < len {
            return Err(too_short());
        }
        let header = input.len() - rest.len();
        self.0 = &rest[len..];
        Ok(Element {
            tag,
            content: &rest[..len],
            raw: &input[..header + len],
        })
    }
}

fn too_short() -> anyhow::Error {
    anyhow::anyhow!("DER input ends early")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_object_identifiers() {
        // sha256WithRSAEncryption
        assert_eq!(
            oid(&[1, 2, 840, 113549, 1, 1, 11]),
            [6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 0x0b]
        );
    }

    #[test]
    fn encodes_lengths_and_integers() {
        assert_eq!(tlv(4, &[0; 200])[..3], [4, 0x81, 200]);
        assert_eq!(tlv(4, &[0; 300])[..4], [4, 0x82, 1, 44]);
        assert_eq!(uint(&[0, 0, 0x80]), [2, 2, 0, 0x80]);
        assert_eq!(small_uint(0), [2, 1, 0]);
        assert_eq!(small_uint(2), [2, 1, 2]);
    }

    #[test]
    fn encodes_times() {
        assert_eq!(date(0), (1970, 1, 1));
        assert_eq!(date(20_454), (2026, 1, 1));
        assert_eq!(time(951_782_400, 0), tlv(0x17, b"000229000000Z"));
        assert_eq!(time(951_782_400, 10), tlv(0x17, b"100228000000Z"));
        assert_eq!(time(951_782_400 + 3661, 4), tlv(0x17, b"040229010101Z"));
        assert_eq!(time(2_524_608_000, 0), tlv(0x18, b"20500101000000Z"));
    }

    #[test]
    fn reads_back_what_it_writes() {
        let encoded = seq(&[&small_uint(5), &octets(&[7; 300])]);
        let mut outer = Reader(&encoded);
        let sequence = outer.expect(SEQUENCE).unwrap();
        assert!(outer.0.is_empty());
        let mut inner = Reader(sequence.content);
        assert_eq!(inner.expect(INTEGER).unwrap().content, [5]);
        let data = inner.expect(4).unwrap();
        assert_eq!(data.content.len(), 300);
        assert_eq!(data.raw.len(), 304);
        assert!(inner.next().is_err());
    }
}
