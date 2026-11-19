//! What base64 takes, and its codec: an encoder that wraps its lines and
//! a decoder that skips line feeds, and other bytes on request, both fed
//! in pieces (MIKSPEC-0098.R51).

use mikros_cli::{Kind, Operand, Opt, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::optional(
        "file",
        Kind::File,
        "the file to encode or decode, or - for standard input",
    )],
    options: &[
        Opt::both("decode", 'd', "decode instead of encoding"),
        Opt::both(
            "ignore-garbage",
            'i',
            "when decoding, skip bytes that are not of the alphabet",
        ),
        Opt::both(
            "wrap",
            'w',
            "break lines after this many columns, 0 for none",
        )
        .takes("columns", Kind::Int),
    ],
    ..Spec::new(
        "base64",
        "Encodes a file, or standard input, as Base64, or decodes it with -d.",
    )
};

/// The columns of a line when `-w` is not given.
pub const COLUMNS: usize = 76;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The columns that `-w`'s `value` gives, `None` when it is not a
/// number.
pub fn columns(value: Option<&str>) -> Option<usize> {
    match value {
        None => Some(COLUMNS),
        Some(v) => v.parse().ok(),
    }
}

/// Encodes bytes given in pieces, breaking lines after `columns`
/// characters unless it is 0.
#[derive(Debug)]
pub struct Encoder {
    columns: usize,
    /// The bytes of a group of three not complete yet.
    pending: Vec<u8>,
    /// The characters on the current line.
    column: usize,
}

impl Encoder {
    pub fn new(columns: usize) -> Encoder {
        Encoder {
            columns,
            pending: Vec::new(),
            column: 0,
        }
    }

    /// Encodes `bytes`, the next piece, onto `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut bytes = bytes;
        while !self.pending.is_empty() && !bytes.is_empty() {
            self.pending.push(bytes[0]);
            bytes = &bytes[1..];
            if self.pending.len() == 3 {
                let group = std::mem::take(&mut self.pending);
                self.group(&group, out);
            }
        }
        let (groups, rest) = bytes.as_chunks::<3>();
        for group in groups {
            self.group(group, out);
        }
        self.pending.extend_from_slice(rest);
    }

    /// Encodes what remains, with padding, and ends the last line.
    pub fn finish(mut self, out: &mut Vec<u8>) {
        let pending = std::mem::take(&mut self.pending);
        if !pending.is_empty() {
            self.group(&pending, out);
        }
        if self.columns > 0 && self.column > 0 {
            out.push(b'\n');
        }
    }

    /// Encodes a group of one to three bytes.
    fn group(&mut self, group: &[u8], out: &mut Vec<u8>) {
        let b = [
            group[0],
            group.get(1).copied().unwrap_or(0),
            group.get(2).copied().unwrap_or(0),
        ];
        let sextets = [
            b[0] >> 2,
            (b[0] & 0x03) << 4 | b[1] >> 4,
            (b[1] & 0x0f) << 2 | b[2] >> 6,
            b[2] & 0x3f,
        ];
        for (i, sextet) in sextets.into_iter().enumerate() {
            let c = match i <= group.len() {
                true => ALPHABET[usize::from(sextet)],
                false => b'=',
            };
            self.put(c, out);
        }
    }

    fn put(&mut self, c: u8, out: &mut Vec<u8>) {
        out.push(c);
        self.column += 1;
        if self.columns > 0 && self.column == self.columns {
            out.push(b'\n');
            self.column = 0;
        }
    }
}

/// The input is not Base64.
#[derive(Debug, PartialEq, Eq)]
pub struct Invalid;

/// Decodes characters given in pieces, skipping line feeds, and with
/// `ignore` every byte that is not of the alphabet.
#[derive(Debug)]
pub struct Decoder {
    ignore: bool,
    /// The characters of a group of four so far, `=` as 64.
    group: Vec<u8>,
}

impl Decoder {
    pub fn new(ignore: bool) -> Decoder {
        Decoder {
            ignore,
            group: Vec::new(),
        }
    }

    /// Decodes `text`, the next piece, onto `out`, up to the first
    /// character that is not Base64.
    pub fn feed(&mut self, text: &[u8], out: &mut Vec<u8>) -> Result<(), Invalid> {
        for &c in text {
            let value = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => 64,
                b'\n' => continue,
                _ if self.ignore => continue,
                _ => return Err(Invalid),
            };
            self.group.push(value);
            if self.group.len() == 4 {
                let group = std::mem::take(&mut self.group);
                decode(&group, out)?;
            }
        }
        Ok(())
    }

    /// Fails when the input ended inside a group.
    pub fn finish(self) -> Result<(), Invalid> {
        match self.group.is_empty() {
            true => Ok(()),
            false => Err(Invalid),
        }
    }
}

/// Decodes a group of four characters, of which the last one or two may
/// be padding.
fn decode(group: &[u8], out: &mut Vec<u8>) -> Result<(), Invalid> {
    let padding = group.iter().rev().take_while(|&&v| v == 64).count();
    if padding > 2 || group[..4 - padding].contains(&64) {
        return Err(Invalid);
    }
    let v = |i: usize| match group[i] {
        64 => 0,
        v => u32::from(v),
    };
    let bits = v(0) << 18 | v(1) << 12 | v(2) << 6 | v(3);
    let bytes = [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8];
    out.extend_from_slice(&bytes[..3 - padding]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(pieces: &[&[u8]], columns: usize) -> String {
        let mut encoder = Encoder::new(columns);
        let mut out = Vec::new();
        for piece in pieces {
            encoder.feed(piece, &mut out);
        }
        encoder.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    fn decode(pieces: &[&[u8]], ignore: bool) -> (Vec<u8>, Result<(), Invalid>) {
        let mut decoder = Decoder::new(ignore);
        let mut out = Vec::new();
        for piece in pieces {
            if let Err(e) = decoder.feed(piece, &mut out) {
                return (out, Err(e));
            }
        }
        let end = decoder.finish();
        (out, end)
    }

    #[test]
    fn encodes_with_padding() {
        assert_eq!(encode(&[b""], 76), "");
        assert_eq!(encode(&[b"f"], 76), "Zg==\n");
        assert_eq!(encode(&[b"fo"], 76), "Zm8=\n");
        assert_eq!(encode(&[b"foo"], 76), "Zm9v\n");
        assert_eq!(encode(&[b"foobar"], 0), "Zm9vYmFy");
        assert_eq!(encode(&[b"\xff\xfe"], 0), "//4=");
    }

    #[test]
    fn wraps_lines_and_joins_pieces() {
        assert_eq!(encode(&[b"foobar"], 4), "Zm9v\nYmFy\n");
        assert_eq!(encode(&[b"foobar"], 5), "Zm9vY\nmFy\n");
        assert_eq!(encode(&[b"fo", b"o", b"b", b"ar"], 0), "Zm9vYmFy");
        let long = vec![0u8; 60];
        let text = encode(&[&long], COLUMNS);
        assert_eq!(text.lines().map(str::len).collect::<Vec<_>>(), [76, 4]);
    }

    #[test]
    fn decodes_and_round_trips() {
        assert_eq!(
            decode(&[b"Zm9v\nYmFy\n"], false),
            (b"foobar".to_vec(), Ok(()))
        );
        assert_eq!(decode(&[b"Zm", b"8=\n"], false), (b"fo".to_vec(), Ok(())));
        assert_eq!(decode(&[b"Zg==Zg=="], false), (b"ff".to_vec(), Ok(())));
        let bytes: Vec<u8> = (0..=255).collect();
        let text = encode(&[&bytes], 10);
        assert_eq!(decode(&[text.as_bytes()], false), (bytes, Ok(())));
    }

    #[test]
    fn refuses_what_is_not_base64_unless_ignoring_it() {
        assert_eq!(
            decode(&[b"Zm9v!YmFy"], false),
            (b"foo".to_vec(), Err(Invalid))
        );
        assert_eq!(decode(&[b"Zm9v!Ym Fy"], true), (b"foobar".to_vec(), Ok(())));
        assert_eq!(decode(&[b"Zm9vY"], false), (b"foo".to_vec(), Err(Invalid)));
        assert_eq!(decode(&[b"Z==="], false).1, Err(Invalid));
        assert_eq!(decode(&[b"Zm=v"], false).1, Err(Invalid));
    }

    #[test]
    fn reads_columns() {
        assert_eq!(columns(None), Some(76));
        assert_eq!(columns(Some("0")), Some(0));
        assert_eq!(columns(Some("-1")), None);
        assert_eq!(columns(Some("x")), None);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/base64.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
