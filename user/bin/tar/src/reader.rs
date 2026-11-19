//! Reading a tar archive entry by entry, with gzip found by itself
//! (MIKSPEC-0098.R65): pax extended and global headers and GNU long
//! names and links apply to the entries they come before. When the
//! reader is `limited`, as it is for extraction, it refuses those headers
//! when they are longer than 1 MiB (MIKSPEC-0098.R67), with the entry
//! they come before, without reading them.

use crate::header::{self, BLOCK, Block, Entry, LONGEST, Pax};
use flate2::read::MultiGzDecoder;
use std::io::{self, BufReader, Read};

/// `input`, an archive, gunzipped when it starts as gzip does.
pub fn unpacked<R: Read + 'static>(mut input: R) -> io::Result<Box<dyn Read>> {
    let mut head = [0u8; 2];
    let mut got = 0;
    while got < head.len() {
        match input.read(&mut head[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let gzip = got == 2 && head == [0x1f, 0x8b];
    let whole = io::Cursor::new(head[..got].to_vec()).chain(input);
    Ok(match gzip {
        true => Box::new(MultiGzDecoder::new(BufReader::new(whole))),
        false => Box::new(BufReader::with_capacity(64 * 1024, whole)),
    })
}

/// What comes next in an archive.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Entry(Entry),
    /// An entry, or a global header, that is refused, by its name, and
    /// why.
    Refused(String, String),
    End,
}

/// An archive, read entry by entry; reading it as a stream gives the
/// data of the last entry.
pub struct Reader<R: Read> {
    input: R,
    limited: bool,
    /// The bytes of data of the last entry not read yet, and the padding
    /// after them.
    left: u64,
    pad: u64,
    /// The modified time of a pax global header.
    global: Option<i64>,
}

impl<R: Read> Reader<R> {
    pub fn new(input: R, limited: bool) -> Reader<R> {
        Reader {
            input,
            limited,
            left: 0,
            pad: 0,
            global: None,
        }
    }

    /// The next entry, after the rest of the data of the last; an error
    /// when the archive is damaged or cut short, after which nothing
    /// more can be read.
    pub fn next(&mut self) -> Result<Next, String> {
        self.skip(self.left + self.pad)?;
        self.left = 0;
        self.pad = 0;
        let mut pax = Pax::default();
        let mut long_name = None;
        let mut long_link = None;
        // Why the next entry is refused, when it is.
        let mut refused: Option<String> = None;
        loop {
            let mut block = [0u8; BLOCK];
            match self.fill(&mut block)? {
                0 => return Ok(Next::End),
                BLOCK => {}
                _ => return Err("the archive ends inside a header".into()),
            }
            let (what, size) = match header::parse(&block)? {
                Block::End => return Ok(Next::End),
                Block::Entry(mut entry, data) => {
                    self.left = data;
                    self.pad = data.next_multiple_of(BLOCK as u64) - data;
                    if let Some(path) = pax.path.take().or(long_name.take()) {
                        entry.path = path;
                    }
                    if let Some(target) = pax.target.take().or(long_link.take()) {
                        entry.target = target;
                    }
                    if let (Some(size), header::Kind::File) = (pax.size, entry.kind) {
                        entry.size = size;
                        self.left = size;
                        self.pad = size.next_multiple_of(BLOCK as u64) - size;
                    }
                    if let Some(modified) = pax.modified.or(self.global) {
                        entry.modified = modified;
                    }
                    return Ok(match refused {
                        Some(why) => Next::Refused(entry.path, why),
                        None => Next::Entry(entry),
                    });
                }
                Block::Pax(size) => ("pax extended header", size),
                Block::Global(size) => ("pax global header", size),
                Block::LongName(size) => ("GNU long name", size),
                Block::LongLink(size) => ("GNU long link", size),
            };
            let padded = size.next_multiple_of(BLOCK as u64);
            if self.limited && size > LONGEST {
                self.skip(padded)?;
                let why = format!("its {what} of {size} bytes is longer than 1 MiB");
                if matches!(header::parse(&block)?, Block::Global(_)) {
                    return Ok(Next::Refused("pax global header".into(), why));
                }
                refused = Some(why);
                continue;
            }
            let mut data = Vec::new();
            (&mut self.input)
                .take(size)
                .read_to_end(&mut data)
                .map_err(|e| format!("cannot read the archive: {e}"))?;
            if (data.len() as u64) < size {
                return Err(format!("the archive ends inside a {what}"));
            }
            self.skip(padded - size)?;
            match header::parse(&block)? {
                Block::Pax(_) => header::pax(&data, &mut pax)?,
                Block::Global(_) => {
                    let mut global = Pax::default();
                    header::pax(&data, &mut global)?;
                    self.global = global.modified.or(self.global);
                }
                Block::LongName(_) => long_name = Some(header::long(&data)),
                _ => long_link = Some(header::long(&data)),
            }
        }
    }

    /// Reads a whole block, or less only at the end of the archive.
    fn fill(&mut self, block: &mut [u8]) -> Result<usize, String> {
        let mut got = 0;
        while got < block.len() {
            match self.input.read(&mut block[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("cannot read the archive: {e}")),
            }
        }
        Ok(got)
    }

    /// Skips `count` bytes of the archive, which must be there.
    fn skip(&mut self, count: u64) -> Result<(), String> {
        let skipped = io::copy(&mut (&mut self.input).take(count), &mut io::sink())
            .map_err(|e| format!("cannot read the archive: {e}"))?;
        match skipped == count {
            true => Ok(()),
            false => Err("the archive ends inside an entry's data".into()),
        }
    }
}

impl<R: Read> Read for Reader<R> {
    /// Reads the data of the last entry, and then nothing.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let want = usize::try_from(self.left).map_or(buffer.len(), |l| l.min(buffer.len()));
        if want == 0 {
            return Ok(0);
        }
        let got = self.input.read(&mut buffer[..want])?;
        self.left -= got as u64;
        Ok(got)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::header::{Kind, end, write};

    pub fn entry(path: &str, kind: Kind, size: u64) -> Entry {
        Entry {
            path: path.into(),
            kind,
            size,
            mode: match kind {
                Kind::Folder => 0o755,
                _ => 0o644,
            },
            modified: 1_700_000_000,
            target: String::new(),
        }
    }

    /// An archive of `entries`, each with its data.
    pub fn archive(entries: &[(Entry, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (entry, data) in entries {
            out.extend(write(entry));
            out.extend(*data);
            out.resize(out.len().next_multiple_of(BLOCK), 0);
        }
        let tail = end(out.len() as u64);
        out.extend(tail);
        out
    }

    /// A header block of `flag` with `size`, for crafted archives.
    pub fn raw(flag: u8, name: &str, size: u64) -> Vec<u8> {
        let mut file = entry(name, Kind::Other(flag), 0);
        file.size = 0;
        let mut block = write(&file);
        let text = format!("{size:011o}\0");
        block[124..136].copy_from_slice(text.as_bytes());
        block[148..156].fill(b' ');
        let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
        block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        block
    }

    fn all(bytes: Vec<u8>, limited: bool) -> Result<Vec<(Next, Vec<u8>)>, String> {
        let mut reader = Reader::new(io::Cursor::new(bytes), limited);
        let mut out = Vec::new();
        loop {
            let next = reader.next()?;
            if next == Next::End {
                return Ok(out);
            }
            let mut data = Vec::new();
            reader.read_to_end(&mut data).unwrap();
            out.push((next, data));
        }
    }

    #[test]
    fn reads_entries_and_their_data() {
        let bytes = archive(&[
            (entry("a", Kind::File, 3), b"abc"),
            (entry("d/", Kind::Folder, 0), b""),
            (entry("b", Kind::File, 600), &[7; 600]),
        ]);
        let read = all(bytes, true).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(
            read[0],
            (Next::Entry(entry("a", Kind::File, 3)), b"abc".to_vec())
        );
        assert_eq!(read[1].0, Next::Entry(entry("d/", Kind::Folder, 0)));
        assert_eq!(read[2].1, vec![7; 600]);
    }

    #[test]
    fn skips_data_that_is_not_read() {
        let bytes = archive(&[
            (entry("a", Kind::File, 1000), &[1; 1000]),
            (entry("b", Kind::File, 1), b"x"),
        ]);
        let mut reader = Reader::new(io::Cursor::new(bytes), true);
        assert!(matches!(reader.next().unwrap(), Next::Entry(_)));
        let Next::Entry(b) = reader.next().unwrap() else {
            panic!();
        };
        assert_eq!(b.path, "b");
    }

    #[test]
    fn applies_pax_and_gnu_headers_to_the_next_entry() {
        let long = "x/".repeat(100) + "end";
        let mut bytes = write(&entry(&long, Kind::File, 2));
        bytes.extend(*b"hi");
        bytes.resize(bytes.len().next_multiple_of(BLOCK), 0);
        let name = b"gnu-long-name\0";
        bytes.extend(raw(b'L', "././@LongLink", name.len() as u64));
        bytes.extend(name);
        bytes.resize(bytes.len().next_multiple_of(BLOCK), 0);
        let target = b"gnu-long-target";
        bytes.extend(raw(b'K', "././@LongLink", target.len() as u64));
        bytes.extend(target);
        bytes.resize(bytes.len().next_multiple_of(BLOCK), 0);
        bytes.extend(write(&entry("short", Kind::Link, 0)));
        let global = b"19 mtime=1234567.5\n";
        bytes.extend(raw(b'g', "global", global.len() as u64));
        bytes.extend(global);
        bytes.resize(bytes.len().next_multiple_of(BLOCK), 0);
        bytes.extend(write(&entry("later", Kind::Folder, 0)));
        bytes.extend([0; 1024]);
        let read = all(bytes, true).unwrap();
        let Next::Entry(first) = &read[0].0 else {
            panic!();
        };
        assert_eq!(
            (first.path.as_str(), read[0].1.as_slice()),
            (long.as_str(), &b"hi"[..])
        );
        let Next::Entry(second) = &read[1].0 else {
            panic!();
        };
        assert_eq!(
            (second.path.as_str(), second.target.as_str()),
            ("gnu-long-name", "gnu-long-target")
        );
        let Next::Entry(third) = &read[2].0 else {
            panic!();
        };
        assert_eq!(third.modified, 1_234_567);
    }

    #[test]
    fn refuses_headers_longer_than_1_mib_when_limited() {
        let size = LONGEST + 1;
        let mut bytes = raw(b'x', "PaxHeaders/big", size);
        bytes.extend(vec![b'a'; size.next_multiple_of(BLOCK as u64) as usize]);
        bytes.extend(archive(&[
            (entry("victim", Kind::File, 1), b"v"),
            (entry("next", Kind::File, 1), b"n"),
        ]));
        let read = all(bytes.clone(), true).unwrap();
        assert_eq!(read.len(), 2);
        let Next::Refused(name, why) = &read[0].0 else {
            panic!("{:?}", read[0].0);
        };
        assert_eq!(name, "victim");
        assert!(
            why.contains("pax extended header") && why.contains("1 MiB"),
            "{why}"
        );
        assert!(matches!(&read[1].0, Next::Entry(e) if e.path == "next"));
        // A header of 1 MiB exactly is taken.
        let mut bytes = raw(b'L', "././@LongLink", LONGEST);
        bytes.extend(vec![b'n'; LONGEST as usize]);
        bytes.extend(archive(&[(entry("short", Kind::File, 0), b"")]));
        let read = all(bytes, true).unwrap();
        assert!(matches!(&read[0].0, Next::Entry(e) if e.path.len() == LONGEST as usize));
    }

    #[test]
    fn refuses_long_gnu_names_and_global_headers_when_limited() {
        for flag in *b"LKg" {
            let size = LONGEST * 2;
            let mut bytes = raw(flag, "././@LongLink", size);
            let digits = size.to_string().len() as u64;
            let record = format!(
                "{size} comment={}\n",
                "a".repeat((size - digits - 10) as usize)
            );
            assert_eq!(record.len() as u64, size);
            bytes.extend(record.as_bytes());
            bytes.extend(archive(&[(entry("e", Kind::File, 0), b"")]));
            let read = all(bytes.clone(), true).unwrap();
            assert!(matches!(&read[0].0, Next::Refused(..)), "{}", flag as char);
            // Listing takes it.
            let read = all(bytes, false).unwrap();
            assert!(matches!(&read[0].0, Next::Entry(_)), "{}", flag as char);
        }
    }

    #[test]
    fn fails_archives_cut_short() {
        let bytes = archive(&[(entry("a", Kind::File, 1000), &[1; 1000])]);
        let mut reader = Reader::new(io::Cursor::new(bytes[..700].to_vec()), true);
        assert!(matches!(reader.next().unwrap(), Next::Entry(_)));
        let mut data = Vec::new();
        reader.read_to_end(&mut data).unwrap();
        assert_eq!(data.len(), 188);
        assert!(reader.next().unwrap_err().contains("ends inside"));
        let mut reader = Reader::new(io::Cursor::new(bytes[..100].to_vec()), true);
        assert!(reader.next().unwrap_err().contains("inside a header"));
        // An archive may end without its blocks of zeros.
        let mut reader = Reader::new(io::Cursor::new(bytes[..1536].to_vec()), true);
        assert!(matches!(reader.next().unwrap(), Next::Entry(_)));
        assert_eq!(reader.next().unwrap(), Next::End);
    }

    #[test]
    fn finds_gzip_by_itself() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;
        let bytes = archive(&[(entry("a", Kind::File, 3), b"abc")]);
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&bytes).unwrap();
        let gz = gz.finish().unwrap();
        for input in [gz, bytes] {
            let mut reader = Reader::new(unpacked(io::Cursor::new(input)).unwrap(), true);
            let Next::Entry(a) = reader.next().unwrap() else {
                panic!();
            };
            assert_eq!(a.path, "a");
        }
        let mut empty = unpacked(io::Cursor::new(vec![])).unwrap();
        assert_eq!(empty.read(&mut [0; 4]).unwrap(), 0);
    }
}
