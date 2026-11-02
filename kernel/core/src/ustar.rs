//! Finding members of the boot image, a ustar archive.

use core::fmt;

const BLOCK: usize = 512;
const NAME: core::ops::Range<usize> = 0..100;
const SIZE: core::ops::Range<usize> = 124..136;
const CHECKSUM: core::ops::Range<usize> = 148..156;
const TYPE: usize = 156;
const PREFIX: core::ops::Range<usize> = 345..500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The header at this offset has a wrong checksum or size field, or
    /// its member's data does not fit in the image.
    Malformed(usize),
    /// The archive ended without the member.
    NotFound,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Malformed(at) => write!(f, "malformed boot image header at offset {at:#x}"),
            Error::NotFound => write!(f, "the boot image has no init member"),
        }
    }
}

/// The value of an octal field: optional leading spaces, at least one
/// digit, then only NULs or spaces.
fn octal(field: &[u8]) -> Option<u64> {
    let start = field.iter().position(|&b| b != b' ')?;
    let digits = field[start..]
        .iter()
        .take_while(|b| b.is_ascii_digit() && **b < b'8')
        .count();
    if digits == 0 || field[start + digits..].iter().any(|&b| b != 0 && b != b' ') {
        return None;
    }
    field[start..start + digits].iter().try_fold(0u64, |v, &d| {
        v.checked_mul(8)?.checked_add(u64::from(d - b'0'))
    })
}

/// Whether the checksum field of `header` matches its bytes, counted with
/// the field itself as spaces.
fn checksum_ok(header: &[u8]) -> bool {
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| u64::from(if CHECKSUM.contains(&i) { b' ' } else { b }))
        .sum();
    octal(&header[CHECKSUM]) == Some(sum)
}

/// The bytes of a NUL-padded name field.
fn field_str(field: &[u8]) -> &[u8] {
    let len = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..len]
}

/// The data of the first regular file member of `image` named exactly
/// `name`.
pub fn find<'a>(image: &'a [u8], name: &[u8]) -> Result<&'a [u8], Error> {
    let mut at = 0;
    while let Some(header) = image.get(at..at + BLOCK) {
        if header.iter().all(|&b| b == 0) {
            let next = image.get(at + BLOCK..at + 2 * BLOCK);
            if next.is_none_or(|b| b.iter().all(|&b| b == 0)) {
                break;
            }
        }
        if !checksum_ok(header) {
            return Err(Error::Malformed(at));
        }
        let size = octal(&header[SIZE]).ok_or(Error::Malformed(at))?;
        let data = at + BLOCK;
        let end = usize::try_from(size)
            .ok()
            .and_then(|s| data.checked_add(s))
            .filter(|&end| end <= image.len())
            .ok_or(Error::Malformed(at))?;
        let regular = matches!(header[TYPE], b'0' | 0);
        if regular && field_str(&header[PREFIX]).is_empty() && field_str(&header[NAME]) == name {
            return Ok(&image[data..end]);
        }
        at = end.next_multiple_of(BLOCK);
    }
    Err(Error::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn header(name: &str, size: usize, kind: u8) -> Vec<u8> {
        let mut h = std::vec![0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[SIZE][..11].copy_from_slice(std::format!("{size:011o}").as_bytes());
        h[TYPE] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        seal(&mut h);
        h
    }

    /// Sets the checksum of the header at the start of `h`.
    fn seal(h: &mut [u8]) {
        h[CHECKSUM].fill(b' ');
        let sum: u32 = h[..BLOCK].iter().map(|&b| u32::from(b)).sum();
        h[CHECKSUM][..7].copy_from_slice(std::format!("{sum:06o}\0").as_bytes());
    }

    fn member(name: &str, data: &[u8], kind: u8) -> Vec<u8> {
        let mut m = header(name, data.len(), kind);
        m.extend_from_slice(data);
        m.resize(m.len().next_multiple_of(BLOCK), 0);
        m
    }

    fn archive(members: &[Vec<u8>]) -> Vec<u8> {
        let mut a = members.concat();
        a.extend_from_slice(&[0; 2 * BLOCK]);
        a
    }

    #[test]
    fn finds_the_first_regular_member() {
        let a = archive(&[
            member("lib", b"x", b'0'),
            member("init", b"", b'5'),
            member("init", &[7; 600], 0),
            member("init", b"second", b'0'),
        ]);
        assert_eq!(find(&a, b"init"), Ok(&[7u8; 600][..]));
    }

    #[test]
    fn needs_the_exact_name() {
        let mut prefixed = member("init", b"x", b'0');
        prefixed[PREFIX.start] = b'a';
        seal(&mut prefixed);
        let a = archive(&[member("./init", b"x", b'0'), member("init2", b"x", b'0')]);
        assert_eq!(find(&a, b"init"), Err(Error::NotFound));
        assert_eq!(find(&archive(&[prefixed]), b"init"), Err(Error::NotFound));
    }

    #[test]
    fn stops_at_the_end() {
        assert_eq!(find(&[], b"init"), Err(Error::NotFound));
        assert_eq!(find(&[0; 2 * BLOCK], b"init"), Err(Error::NotFound));
        let mut a = archive(&[]);
        a.extend(member("init", b"x", b'0'));
        assert_eq!(find(&a, b"init"), Err(Error::NotFound));
        // An image may end without the marker.
        let lone = member("lib", b"x", b'0');
        assert_eq!(find(&lone, b"init"), Err(Error::NotFound));
    }

    #[test]
    fn refuses_malformed_headers() {
        let mut bad_sum = member("init", b"x", b'0');
        bad_sum[0] = b'j';
        assert_eq!(
            find(&archive(&[bad_sum]), b"init"),
            Err(Error::Malformed(0))
        );
        let lib = member("lib", b"x", b'0');
        let mut zero_then_header = std::vec![0u8; BLOCK];
        zero_then_header.extend(member("init", b"x", b'0'));
        let a = archive(&[lib.clone(), zero_then_header]);
        assert_eq!(find(&a, b"init"), Err(Error::Malformed(lib.len())));
        // The size reaches past the image.
        let mut long = header("init", 600, b'0');
        long.extend_from_slice(&[0; 100]);
        assert_eq!(find(&long, b"init"), Err(Error::Malformed(0)));
        let mut not_octal = header("init", 1, b'0');
        not_octal[SIZE.start] = b'9';
        assert_eq!(
            find(&archive(&[not_octal]), b"init"),
            Err(Error::Malformed(0))
        );
    }

    #[test]
    fn reads_octal_fields() {
        assert_eq!(octal(b"0000017\0"), Some(15));
        assert_eq!(octal(b"  17 \0\0\0"), Some(15));
        assert_eq!(octal(b"\0\0\0\0"), None);
        assert_eq!(octal(b"17x\0"), None);
        assert_eq!(octal(b"1 7\0"), None);
        assert_eq!(octal(b"77777777777777777777777"), None);
    }
}
