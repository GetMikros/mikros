//! Writing a zip archive (MIKSPEC-0098.R58): for each entry a local
//! header, its data, stored or deflated, and then the central directory
//! and its end records. Names are in UTF-8 and marked so; each entry has
//! its modified time, as the time of MS-DOS and as an extended
//! timestamp, and its stored mode with its type in the external
//! attributes of Unix. ZIP64 fields and end records are written where a
//! size, an offset or the number of entries needs them. Entries kept
//! from an archive are copied as they are compressed.

use crate::central::{
    self, CENTRAL, DEFLATED, DESCRIBED, DESCRIPTOR, ENCRYPTED, END, END64, LOCAL, LOCATOR64,
    Record, STORED, TIMESTAMP, UNIX, UTF8, ZIP64,
};
use flate2::{Compression, write::DeflateEncoder};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// The size above which a file's local header has room for ZIP64 sizes,
/// with a margin for what deflating may add.
const ROOMY: u64 = 0xf000_0000;
const MOST32: u64 = 0xffff_ffff;
const VERSION: u16 = (UNIX << 8) | 63;

/// What an entry is made of.
pub enum Data<'a> {
    /// The contents of a file, of which no more than its size when it
    /// was looked at is read.
    File(&'a mut dyn Read, u64),
    /// A few bytes: a link's target, or nothing for a folder.
    Bytes(&'a [u8]),
}

/// An archive being written to `out`.
pub struct Writer<W: Write + Seek> {
    out: W,
    at: u64,
    records: Vec<Record>,
}

/// Little-endian bytes, appended.
trait Put {
    fn u16(&mut self, value: u16);
    fn u32(&mut self, value: u32);
    fn u64(&mut self, value: u64);
}

impl Put for Vec<u8> {
    fn u16(&mut self, value: u16) {
        self.extend(value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.extend(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.extend(value.to_le_bytes());
    }
}

fn small(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The extended timestamp of `modified`, when it fits one.
fn timestamp(modified: i64) -> Vec<u8> {
    let mut extra = Vec::new();
    if let Ok(seconds) = i32::try_from(modified) {
        extra.u16(TIMESTAMP);
        extra.u16(5);
        extra.push(1);
        extra.extend(seconds.to_le_bytes());
    }
    extra
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(out: W) -> Writer<W> {
        Writer {
            out,
            at: 0,
            records: Vec::new(),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.at += bytes.len() as u64;
        Ok(())
    }

    /// Writes the local header of `record`, with room for ZIP64 sizes
    /// when `roomy`.
    fn local(&mut self, record: &Record, roomy: bool) -> io::Result<()> {
        let mut header = Vec::new();
        header.u32(LOCAL);
        header.u16(record.needed);
        header.u16(record.flags);
        header.u16(record.method);
        header.u16(record.time);
        header.u16(record.date);
        let described = record.flags & DESCRIBED != 0;
        let (crc, compressed, size) = match (described, roomy) {
            (true, _) => (0, 0, 0),
            (false, true) => (record.crc, u32::MAX, u32::MAX),
            (false, false) => (record.crc, small(record.compressed), small(record.size)),
        };
        header.u32(crc);
        header.u32(compressed);
        header.u32(size);
        let mut extra = Vec::new();
        if roomy {
            extra.u16(ZIP64);
            extra.u16(16);
            extra.u64(record.size);
            extra.u64(record.compressed);
        }
        extra.extend(&record.extra);
        header.u16(record.name.len() as u16);
        header.u16(extra.len() as u16);
        header.extend(record.name.as_bytes());
        header.extend(extra);
        self.put(&header)
    }

    /// Adds an entry named `name`, with `mode`, its type included, and
    /// the `modified` time, of `data`, deflated at `level` unless it is
    /// 0; gives back its record. When the data cannot be read the entry
    /// is left out, and the error given back inside the result.
    pub fn add(
        &mut self,
        name: &str,
        mode: u32,
        modified: i64,
        data: Data,
        level: u32,
    ) -> io::Result<Result<Record, io::Error>> {
        let (time, date) = central::to_dos(modified);
        let length = match &data {
            Data::File(_, size) => *size,
            Data::Bytes(bytes) => bytes.len() as u64,
        };
        let method = match (level, &data) {
            (0, _) | (_, Data::Bytes(_)) => STORED,
            _ => DEFLATED,
        };
        let roomy = length >= ROOMY;
        let mut record = Record {
            name: name.into(),
            made_by: VERSION,
            needed: match (roomy, method) {
                (true, _) => 45,
                (false, DEFLATED) => 20,
                (false, _) if mode & central::S_IFMT == central::S_IFDIR => 20,
                _ => 10,
            },
            flags: UTF8,
            method,
            time,
            date,
            offset: self.at,
            external: (mode << 16) | u32::from(mode & central::S_IFMT == central::S_IFDIR) << 4,
            extra: timestamp(modified),
            ..Record::default()
        };
        let start = self.at;
        self.local(&record, roomy)?;
        let mut hasher = crc32fast::Hasher::new();
        let size;
        let began = self.at;
        let failed: Option<io::Error>;
        match data {
            Data::Bytes(bytes) => {
                hasher.update(bytes);
                size = bytes.len() as u64;
                failed = None;
                self.put(bytes)?;
            }
            Data::File(read, length) => {
                let mut read = read.take(length);
                let mut sink = Counted {
                    out: &mut self.out,
                    count: 0,
                };
                let pumped = match method {
                    DEFLATED => {
                        let mut encoder = DeflateEncoder::new(&mut sink, Compression::new(level));
                        let pumped = pump(&mut read, &mut encoder, &mut hasher)?;
                        encoder.finish()?;
                        pumped
                    }
                    _ => pump(&mut read, &mut sink, &mut hasher)?,
                };
                (size, failed) = pumped;
                self.at += sink.count;
            }
        }
        if let Some(e) = failed {
            self.out.seek(SeekFrom::Start(start))?;
            self.at = start;
            return Ok(Err(e));
        }
        record.crc = hasher.finalize();
        record.size = size;
        record.compressed = self.at - began;
        // The sizes and CRC-32, now known, in the local header.
        let end = self.at;
        self.out.seek(SeekFrom::Start(start + 14))?;
        self.out.write_all(&record.crc.to_le_bytes())?;
        if roomy {
            let extra = start + 30 + record.name.len() as u64 + 4;
            self.out.seek(SeekFrom::Start(extra))?;
            self.out.write_all(&record.size.to_le_bytes())?;
            self.out.write_all(&record.compressed.to_le_bytes())?;
        } else {
            self.out
                .write_all(&small(record.compressed).to_le_bytes())?;
            self.out.write_all(&small(record.size).to_le_bytes())?;
        }
        self.out.seek(SeekFrom::Start(end))?;
        self.records.push(record.clone());
        Ok(Ok(record))
    }

    /// Copies the entry of `record` from the archive `from` as it is
    /// compressed, with a new local header, keeping its data descriptor
    /// only where it is encrypted.
    pub fn copy<F: Read + Seek>(&mut self, from: &mut F, record: &Record) -> Result<(), String> {
        let header = central::read_at(from, record.offset, 30)?;
        if header[..4] != LOCAL.to_le_bytes() {
            return Err(format!(
                "{}: its local header is not where the central directory says",
                record.name
            ));
        }
        let skip = u64::from(u16::from_le_bytes([header[26], header[27]]))
            + u64::from(u16::from_le_bytes([header[28], header[29]]));
        let mut record = record.clone();
        if record.flags & ENCRYPTED == 0 {
            record.flags &= !DESCRIBED;
        }
        record.flags |= UTF8;
        let roomy = record.size >= MOST32 || record.compressed >= MOST32;
        if roomy {
            record.needed = record.needed.max(45);
        }
        let write = |e: io::Error| format!("cannot write the archive: {e}");
        from.seek(SeekFrom::Start(record.offset + 30 + skip))
            .map_err(|e| format!("cannot read the archive: {e}"))?;
        record.offset = self.at;
        self.local(&record, roomy).map_err(write)?;
        let mut data = from.take(record.compressed);
        let copied = io::copy(&mut data, &mut self.out).map_err(write)?;
        self.at += copied;
        if copied != record.compressed {
            return Err(format!("{}: the archive is cut short", record.name));
        }
        if record.flags & DESCRIBED != 0 {
            let mut descriptor = Vec::new();
            descriptor.u32(DESCRIPTOR);
            descriptor.u32(record.crc);
            match roomy {
                true => {
                    descriptor.u64(record.compressed);
                    descriptor.u64(record.size);
                }
                false => {
                    descriptor.u32(small(record.compressed));
                    descriptor.u32(small(record.size));
                }
            }
            self.put(&descriptor).map_err(write)?;
        }
        self.records.push(record);
        Ok(())
    }

    /// Writes the central directory and its end records, and gives back
    /// the output and the length of the archive.
    pub fn finish(mut self) -> io::Result<(W, u64)> {
        let tail = directory(&self.records, self.at);
        self.put(&tail)?;
        self.out.flush()?;
        Ok((self.out, self.at))
    }
}

/// Copies `read` to `out`, counting its CRC-32 in `hasher`, until it
/// ends or fails; gives back how many bytes it read, and why it failed
/// when it did. Only errors writing are errors of the result.
fn pump(
    read: &mut dyn Read,
    out: &mut dyn Write,
    hasher: &mut crc32fast::Hasher,
) -> io::Result<(u64, Option<io::Error>)> {
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0;
    loop {
        let got = match read.read(&mut buffer) {
            Ok(0) => return Ok((size, None)),
            Ok(got) => got,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Ok((size, Some(e))),
        };
        hasher.update(&buffer[..got]);
        size += got as u64;
        out.write_all(&buffer[..got])?;
    }
}

/// The central directory of `records`, starting at `start`, and its end
/// records.
fn directory(records: &[Record], start: u64) -> Vec<u8> {
    let mut directory = Vec::new();
    for record in records {
        let mut zip64 = Vec::new();
        let mut field = |value: u64| -> u32 {
            match value >= MOST32 {
                true => {
                    zip64.u64(value);
                    u32::MAX
                }
                false => value as u32,
            }
        };
        let size = field(record.size);
        let compressed = field(record.compressed);
        let offset = field(record.offset);
        let mut extra = Vec::new();
        if !zip64.is_empty() {
            extra.u16(ZIP64);
            extra.u16(zip64.len() as u16);
            extra.extend(&zip64);
        }
        extra.extend(&record.extra);
        directory.u32(CENTRAL);
        directory.u16(record.made_by);
        let needed = match zip64.is_empty() {
            true => record.needed,
            false => record.needed.max(45),
        };
        directory.u16(needed);
        directory.u16(record.flags);
        directory.u16(record.method);
        directory.u16(record.time);
        directory.u16(record.date);
        directory.u32(record.crc);
        directory.u32(compressed);
        directory.u32(size);
        directory.u16(record.name.len() as u16);
        directory.u16(extra.len() as u16);
        directory.u16(record.comment.len() as u16);
        directory.u16(0);
        directory.u16(record.internal);
        directory.u32(record.external);
        directory.u32(offset);
        directory.extend(record.name.as_bytes());
        directory.extend(extra);
        directory.extend(&record.comment);
    }
    let size = directory.len() as u64;
    let count = records.len() as u64;
    if count >= 0xffff || size >= MOST32 || start >= MOST32 {
        let at = start + size;
        directory.u32(END64);
        directory.u64(44);
        directory.u16(VERSION);
        directory.u16(45);
        directory.u32(0);
        directory.u32(0);
        directory.u64(count);
        directory.u64(count);
        directory.u64(size);
        directory.u64(start);
        directory.u32(LOCATOR64);
        directory.u32(0);
        directory.u64(at);
        directory.u32(1);
    }
    let entries = u16::try_from(count).unwrap_or(u16::MAX);
    directory.u32(END);
    directory.u32(0);
    directory.u16(entries);
    directory.u16(entries);
    directory.u32(small(size));
    directory.u32(small(start));
    directory.u16(0);
    directory
}

/// A writer that counts what goes through it.
struct Counted<'a, W: Write> {
    out: &'a mut W,
    count: u64,
}

impl<W: Write> Write for Counted<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.out.write(bytes)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::central::{S_IFDIR, S_IFLNK, S_IFREG};
    use flate2::read::DeflateDecoder;

    /// The archive that `write` makes.
    pub fn written(write: impl FnOnce(&mut Writer<io::Cursor<Vec<u8>>>)) -> Vec<u8> {
        let mut writer = Writer::new(io::Cursor::new(Vec::new()));
        write(&mut writer);
        let (out, length) = writer.finish().unwrap();
        let mut bytes = out.into_inner();
        bytes.truncate(length as usize);
        bytes
    }

    /// The data of `record` in `bytes`, inflated.
    pub fn data(bytes: &[u8], record: &Record) -> Vec<u8> {
        let name = u16::from_le_bytes([
            bytes[record.offset as usize + 26],
            bytes[record.offset as usize + 27],
        ]);
        let extra = u16::from_le_bytes([
            bytes[record.offset as usize + 28],
            bytes[record.offset as usize + 29],
        ]);
        let start = record.offset as usize + 30 + name as usize + extra as usize;
        let raw = &bytes[start..start + record.compressed as usize];
        let mut out = Vec::new();
        match record.method {
            DEFLATED => {
                DeflateDecoder::new(raw).read_to_end(&mut out).unwrap();
            }
            _ => out.extend(raw),
        }
        assert_eq!(crc32fast::hash(&out), record.crc);
        out
    }

    #[test]
    fn writes_entries_that_read_back() {
        let text = b"some text that compresses, some text that compresses".repeat(50);
        let bytes = written(|w| {
            let mut file = &text[..];
            let length = text.len() as u64;
            w.add("dir/", S_IFDIR | 0o755, 1_700_000_000, Data::Bytes(b""), 6)
                .unwrap()
                .unwrap();
            w.add(
                "dir/ä.txt",
                S_IFREG | 0o640,
                1_700_000_100,
                Data::File(&mut file, length),
                6,
            )
            .unwrap()
            .unwrap();
            let mut stored = &b"plain"[..];
            w.add("s", S_IFREG | 0o600, 0, Data::File(&mut stored, 5), 0)
                .unwrap()
                .unwrap();
            w.add(
                "l",
                S_IFLNK | 0o777,
                5,
                Data::Bytes("dir/ä.txt".as_bytes()),
                6,
            )
            .unwrap()
            .unwrap();
        });
        let directory = central::read(&mut io::Cursor::new(&bytes)).unwrap();
        let [folder, file, stored, link] = &directory.records[..] else {
            panic!();
        };
        assert!(folder.is_folder() && folder.external & 0x10 != 0);
        assert_eq!((file.name.as_str(), file.method), ("dir/ä.txt", DEFLATED));
        assert!(file.flags & UTF8 != 0);
        assert_eq!(file.mode(), Some(S_IFREG | 0o640));
        assert_eq!(file.modified(), 1_700_000_100);
        assert!(file.compressed < file.size);
        assert_eq!(data(&bytes, file), text);
        assert_eq!(
            (stored.method, data(&bytes, stored)),
            (STORED, b"plain".to_vec())
        );
        assert!(link.is_link());
        assert_eq!(data(&bytes, link), "dir/ä.txt".as_bytes());
    }

    #[test]
    fn leaves_out_what_cannot_be_read() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("broken"))
            }
        }
        let bytes = written(|w| {
            let failed = w
                .add("bad", S_IFREG, 0, Data::File(&mut Broken, 10), 6)
                .unwrap();
            assert!(failed.is_err());
            w.add("good", S_IFREG, 0, Data::Bytes(b"g"), 6)
                .unwrap()
                .unwrap();
        });
        let directory = central::read(&mut io::Cursor::new(&bytes)).unwrap();
        assert_eq!(directory.records.len(), 1);
        assert_eq!(directory.records[0].offset, 0);
    }

    #[test]
    fn copies_entries_of_another_archive() {
        let first = written(|w| {
            w.add("a", S_IFREG | 0o644, 7, Data::Bytes(b"aaa"), 6)
                .unwrap()
                .unwrap();
            w.add("b", S_IFREG | 0o644, 8, Data::Bytes(b"bbb"), 6)
                .unwrap()
                .unwrap();
        });
        let mut from = io::Cursor::new(&first);
        let old = central::read(&mut from).unwrap();
        let second = written(|w| {
            w.add("c", S_IFREG, 0, Data::Bytes(b"new"), 6)
                .unwrap()
                .unwrap();
            w.copy(&mut from, &old.records[1]).unwrap();
        });
        let directory = central::read(&mut io::Cursor::new(&second)).unwrap();
        let b = &directory.records[1];
        assert_eq!((b.name.as_str(), b.modified()), ("b", 8));
        assert_eq!(data(&second, b), b"bbb");
    }

    #[test]
    fn writes_zip64_end_records_for_many_entries() {
        let bytes = written(|w| {
            for i in 0..0x10000u32 {
                w.add(&format!("{i}"), S_IFREG, 0, Data::Bytes(b""), 0)
                    .unwrap()
                    .unwrap();
            }
        });
        let directory = central::read(&mut io::Cursor::new(&bytes)).unwrap();
        assert_eq!(directory.records.len(), 0x10000);
        assert_eq!(directory.records[0xffff].name, "65535");
    }

    #[test]
    fn writes_zip64_fields_for_large_offsets() {
        let record = Record {
            name: "far".into(),
            offset: 5 << 30,
            size: 6 << 30,
            compressed: 1,
            made_by: VERSION,
            ..Record::default()
        };
        let tail = directory(&[record], 7 << 30);
        assert_eq!(u16::from_le_bytes([tail[30], tail[31]]), 20);
        assert_eq!(&tail[24..28], &[0xff; 4]);
        assert_eq!(
            u64::from_le_bytes(tail[53..61].try_into().unwrap()),
            6 << 30
        );
        assert_eq!(
            u64::from_le_bytes(tail[61..69].try_into().unwrap()),
            5 << 30
        );
        // The directory starts past 4 GiB: ZIP64 end records.
        let records = central::read(&mut Offset(7 << 30, tail, 0)).unwrap();
        assert_eq!(records.records[0].offset, 5 << 30);
        assert_eq!(records.start, 7 << 30);
    }

    /// Bytes that stand at `.0` of a file of zeros before them.
    struct Offset(u64, Vec<u8>, u64);

    impl Read for Offset {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let end = self.0 + self.1.len() as u64;
            let n = buffer.len().min((end.saturating_sub(self.2)) as usize);
            for (i, b) in buffer[..n].iter_mut().enumerate() {
                let at = self.2 + i as u64;
                *b = match at.checked_sub(self.0) {
                    Some(i) => self.1[i as usize],
                    None => 0,
                };
            }
            self.2 += n as u64;
            Ok(n)
        }
    }

    impl Seek for Offset {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.2 = match to {
                SeekFrom::Start(at) => at,
                SeekFrom::End(back) => (self.0 + self.1.len() as u64).saturating_add_signed(back),
                SeekFrom::Current(by) => self.2.saturating_add_signed(by),
            };
            Ok(self.2)
        }
    }
}
