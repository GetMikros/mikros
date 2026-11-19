//! The central directory of a zip archive, which zip and unzip both
//! read (MIKSPEC-0098.R58, MIKSPEC-0098.R59): its end record, with the
//! ZIP64 ones where they are, and the record of each entry, with the
//! sizes and offset of its ZIP64 extra field and the modified time of
//! its extended timestamp; and the times of MS-DOS, in local time, that
//! every entry has.

use std::ffi::{c_char, c_int, c_long};
use std::io::{Read, Seek, SeekFrom};

pub const LOCAL: u32 = 0x0403_4b50;
pub const CENTRAL: u32 = 0x0201_4b50;
pub const END: u32 = 0x0605_4b50;
pub const END64: u32 = 0x0606_4b50;
pub const LOCATOR64: u32 = 0x0706_4b50;
pub const DESCRIPTOR: u32 = 0x0807_4b50;
/// The extra fields of ZIP64 and of the extended timestamp.
pub const ZIP64: u16 = 0x0001;
pub const TIMESTAMP: u16 = 0x5455;
/// The flags of encryption, of a data descriptor after the data, and of
/// names in UTF-8.
pub const ENCRYPTED: u16 = 1;
pub const DESCRIBED: u16 = 1 << 3;
pub const UTF8: u16 = 1 << 11;
pub const STORED: u16 = 0;
pub const DEFLATED: u16 = 8;
/// The host of `made_by` whose external attributes hold a mode.
pub const UNIX: u16 = 3;
pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

/// The record of an entry in the central directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub made_by: u16,
    pub needed: u16,
    pub flags: u16,
    pub method: u16,
    pub time: u16,
    pub date: u16,
    pub crc: u32,
    pub compressed: u64,
    pub size: u64,
    /// Where its local header is.
    pub offset: u64,
    pub internal: u16,
    pub external: u32,
    /// Its extra fields, without the ZIP64 one.
    pub extra: Vec<u8>,
    pub comment: Vec<u8>,
}

impl Record {
    /// The mode, with the type, when the entry stores one.
    pub fn mode(&self) -> Option<u32> {
        let mode = self.external >> 16;
        (self.made_by >> 8 == UNIX && mode != 0).then_some(mode)
    }

    pub fn is_folder(&self) -> bool {
        self.name.ends_with('/') || self.mode().is_some_and(|m| m & S_IFMT == S_IFDIR)
    }

    pub fn is_link(&self) -> bool {
        !self.is_folder() && self.mode().is_some_and(|m| m & S_IFMT == S_IFLNK)
    }

    /// The modified time, from the extended timestamp where it has one,
    /// and otherwise from the time of MS-DOS.
    pub fn modified(&self) -> i64 {
        let stamp = fields(&self.extra).find(|(id, _)| *id == TIMESTAMP);
        match stamp {
            Some((_, data)) if data.len() >= 5 && data[0] & 1 != 0 => {
                i64::from(i32::from_le_bytes([data[1], data[2], data[3], data[4]]))
            }
            _ => from_dos(self.time, self.date),
        }
    }
}

/// The extra fields of `extra`, by ID; those cut short are left out.
pub fn fields(extra: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut rest = extra;
    std::iter::from_fn(move || {
        if rest.len() < 4 {
            return None;
        }
        let id = u16::from_le_bytes([rest[0], rest[1]]);
        let length = usize::from(u16::from_le_bytes([rest[2], rest[3]]));
        let data = rest.get(4..4 + length)?;
        rest = &rest[4 + length..];
        Some((id, data))
    })
}

/// The central directory of an archive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directory {
    pub records: Vec<Record>,
    /// Where it starts and where it ends, which is where the end records
    /// start.
    pub start: u64,
    pub end: u64,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or_default())
}

/// Reads `count` bytes at `offset`, all of which must be there.
pub fn read_at<F: Read + Seek>(file: &mut F, offset: u64, count: u64) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("cannot read the archive: {e}"))?;
    let mut bytes = Vec::new();
    file.take(count)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read the archive: {e}"))?;
    match bytes.len() as u64 == count {
        true => Ok(bytes),
        false => Err("the archive is cut short".into()),
    }
}

/// Reads the central directory of the archive `file`.
pub fn read<F: Read + Seek>(file: &mut F) -> Result<Directory, String> {
    let not_zip = || String::from("it is not a zip archive, or it is damaged");
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("cannot read the archive: {e}"))?;
    let tail_length = length.min(22 + 0xffff);
    let tail = read_at(file, length - tail_length, tail_length)?;
    let found = (0..tail.len().saturating_sub(21)).rev().find(|&i| {
        u32_at(&tail, i) == END && i + 22 + usize::from(u16_at(&tail, i + 20)) <= tail.len()
    });
    let i = found.ok_or_else(not_zip)?;
    let at = length - tail_length + i as u64;
    let end = &tail[i..i + 22];
    let mut disks = (u16_at(end, 4), u16_at(end, 6));
    let mut here = u64::from(u16_at(end, 8));
    let mut count = u64::from(u16_at(end, 10));
    let mut size = u64::from(u32_at(end, 12));
    let mut start = u64::from(u32_at(end, 16));
    let mut directory_end = at;
    if at >= 20 {
        let locator = read_at(file, at - 20, 20)?;
        if u32_at(&locator, 0) == LOCATOR64 {
            let offset = u64_at(&locator, 8);
            let record = read_at(file, offset, 56)?;
            if u32_at(&record, 0) != END64 || u32_at(&locator, 16) > 1 {
                return Err(not_zip());
            }
            disks = (
                u16::try_from(u32_at(&record, 16)).unwrap_or(u16::MAX),
                u16::try_from(u32_at(&record, 20)).unwrap_or(u16::MAX),
            );
            here = u64_at(&record, 24);
            count = u64_at(&record, 32);
            size = u64_at(&record, 40);
            start = u64_at(&record, 48);
            directory_end = offset;
        }
    }
    if disks != (0, 0) || here != count {
        return Err("it spans several disks, which unzip does not take".into());
    }
    if start.checked_add(size).is_none_or(|e| e > directory_end) || count > size / 46 + 1 {
        return Err(not_zip());
    }
    let bytes = read_at(file, start, size)?;
    let mut records = Vec::new();
    let mut rest = &bytes[..];
    for _ in 0..count {
        let (record, used) = parse(rest).ok_or_else(not_zip)?;
        records.push(record);
        rest = &rest[used..];
    }
    Ok(Directory {
        records,
        start,
        end: directory_end,
    })
}

/// The record at the start of `bytes`, and its length.
fn parse(bytes: &[u8]) -> Option<(Record, usize)> {
    if bytes.len() < 46 || u32_at(bytes, 0) != CENTRAL {
        return None;
    }
    let name_length = usize::from(u16_at(bytes, 28));
    let extra_length = usize::from(u16_at(bytes, 30));
    let comment_length = usize::from(u16_at(bytes, 32));
    let used = 46 + name_length + extra_length + comment_length;
    let name = bytes.get(46..46 + name_length)?;
    let extra = bytes.get(46 + name_length..46 + name_length + extra_length)?;
    let comment = bytes.get(46 + name_length + extra_length..used)?;
    let mut record = Record {
        name: String::from_utf8_lossy(name).into_owned(),
        made_by: u16_at(bytes, 4),
        needed: u16_at(bytes, 6),
        flags: u16_at(bytes, 8),
        method: u16_at(bytes, 10),
        time: u16_at(bytes, 12),
        date: u16_at(bytes, 14),
        crc: u32_at(bytes, 16),
        compressed: u64::from(u32_at(bytes, 20)),
        size: u64::from(u32_at(bytes, 24)),
        offset: u64::from(u32_at(bytes, 42)),
        internal: u16_at(bytes, 36),
        external: u32_at(bytes, 38),
        extra: Vec::new(),
        comment: comment.to_vec(),
    };
    let disk = u16_at(bytes, 34);
    for (id, data) in fields(extra) {
        if id != ZIP64 {
            record.extra.extend(&(id.to_le_bytes()));
            record.extra.extend(&(data.len() as u16).to_le_bytes());
            record.extra.extend(data);
            continue;
        }
        let mut at = 0;
        let mut next = |field: &mut u64| -> Option<()> {
            if *field == 0xffff_ffff {
                *field = u64_at(data.get(at..at + 8)?, 0);
                at += 8;
            }
            Some(())
        };
        next(&mut record.size)?;
        next(&mut record.compressed)?;
        next(&mut record.offset)?;
    }
    if disk != 0 && disk != 0xffff {
        return None;
    }
    Some((record, used))
}

/// The C library's `struct tm`.
#[repr(C)]
struct Tm {
    sec: c_int,
    min: c_int,
    hour: c_int,
    mday: c_int,
    mon: c_int,
    year: c_int,
    wday: c_int,
    yday: c_int,
    isdst: c_int,
    gmtoff: c_long,
    zone: *const c_char,
}

unsafe extern "C" {
    fn localtime_r(time: *const i64, tm: *mut Tm) -> *mut Tm;
    fn mktime(tm: *mut Tm) -> i64;
}

/// The seconds since 1970 of a time and date of MS-DOS, in local time.
pub fn from_dos(time: u16, date: u16) -> i64 {
    let mut tm = Tm {
        sec: c_int::from(time & 0x1f) * 2,
        min: c_int::from((time >> 5) & 0x3f),
        hour: c_int::from(time >> 11),
        mday: c_int::from(date & 0x1f),
        mon: c_int::from((date >> 5) & 0x0f) - 1,
        year: c_int::from(date >> 9) + 80,
        wday: 0,
        yday: 0,
        isdst: -1,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    // SAFETY: the pointer is valid for the call.
    unsafe { mktime(&mut tm) }
}

/// The time and date of MS-DOS, in local time, of `seconds` since 1970,
/// no earlier than 1980, to two seconds.
pub fn to_dos(seconds: i64) -> (u16, u16) {
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: both pointers are valid for the call.
    if unsafe { localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return (0, 0x21);
    }
    // SAFETY: `localtime_r` filled it in.
    let tm = unsafe { tm.assume_init() };
    if tm.year < 80 {
        return (0, 0x21);
    }
    if tm.year > 207 {
        return (0xbf7d, 0xff9f);
    }
    let time = (tm.hour << 11) | (tm.min << 5) | (tm.sec / 2);
    let date = ((tm.year - 80) << 9) | ((tm.mon + 1) << 5) | tm.mday;
    (time as u16, date as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// A central record of `name`, ZIP64 when `big`.
    fn record(name: &str, big: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(CENTRAL.to_le_bytes());
        bytes.extend(((UNIX << 8) | 45).to_le_bytes());
        bytes.extend(45u16.to_le_bytes());
        bytes.extend(UTF8.to_le_bytes());
        bytes.extend(DEFLATED.to_le_bytes());
        bytes.extend([0, 0, 0x21, 0]);
        bytes.extend(7u32.to_le_bytes());
        let (small, extra) = match big {
            true => {
                let mut extra = Vec::new();
                extra.extend(ZIP64.to_le_bytes());
                extra.extend(24u16.to_le_bytes());
                extra.extend((5u64 << 32).to_le_bytes());
                extra.extend((6u64 << 32).to_le_bytes());
                extra.extend((7u64 << 32).to_le_bytes());
                (0xffff_ffffu32, extra)
            }
            false => (10, Vec::new()),
        };
        bytes.extend(small.to_le_bytes());
        bytes.extend(small.to_le_bytes());
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend((extra.len() as u16 + 9).to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend((0o100640u32 << 16).to_le_bytes());
        bytes.extend(small.to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(extra);
        bytes.extend(TIMESTAMP.to_le_bytes());
        bytes.extend(5u16.to_le_bytes());
        bytes.push(1);
        bytes.extend(1_700_000_000i32.to_le_bytes());
        bytes
    }

    fn end(count: u16, size: u32, start: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(END.to_le_bytes());
        bytes.extend([0; 4]);
        bytes.extend(count.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(size.to_le_bytes());
        bytes.extend(start.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        bytes.extend(b"hey");
        bytes
    }

    #[test]
    fn reads_records_and_their_zip64_fields() {
        let mut bytes = vec![0; 100];
        let records = [record("a/ä", false), record("big", true)].concat();
        bytes.extend(&records);
        bytes.extend(end(2, records.len() as u32, 100));
        let directory = read(&mut io::Cursor::new(&bytes)).unwrap();
        assert_eq!(
            (directory.start, directory.end),
            (100, 100 + records.len() as u64)
        );
        let [a, big] = &directory.records[..] else {
            panic!();
        };
        assert_eq!(
            (a.name.as_str(), a.size, a.compressed, a.offset),
            ("a/ä", 10, 10, 10)
        );
        assert_eq!(a.mode(), Some(0o100640));
        assert_eq!(a.modified(), 1_700_000_000);
        assert!(!a.is_folder() && !a.is_link());
        assert_eq!(
            (big.size, big.compressed, big.offset),
            (5 << 32, 6 << 32, 7 << 32)
        );
        // The ZIP64 field is taken out, the timestamp kept.
        assert_eq!(big.extra.len(), 9);
    }

    #[test]
    fn reads_zip64_end_records() {
        let records = record("x", false);
        let mut bytes = records.clone();
        let at = bytes.len() as u64;
        bytes.extend(END64.to_le_bytes());
        bytes.extend(44u64.to_le_bytes());
        bytes.extend([45, 3, 45, 0]);
        bytes.extend([0; 8]);
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend((records.len() as u64).to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(LOCATOR64.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(at.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(end(0xffff, 0xffff_ffff, 0xffff_ffff));
        let directory = read(&mut io::Cursor::new(&bytes)).unwrap();
        assert_eq!(directory.records.len(), 1);
        assert_eq!(directory.end, at);
    }

    #[test]
    fn refuses_what_is_not_a_zip_archive() {
        assert!(read(&mut io::Cursor::new(b"not a zip archive at all")).is_err());
        assert!(read(&mut io::Cursor::new(Vec::new())).is_err());
        let records = record("x", false);
        // More entries than the directory holds.
        let mut bytes = records.clone();
        bytes.extend(end(2, records.len() as u32, 0));
        assert!(read(&mut io::Cursor::new(&bytes)).is_err());
        // A directory beyond its end record.
        let mut bytes = records.clone();
        bytes.extend(end(1, records.len() as u32 + 1, 0));
        assert!(read(&mut io::Cursor::new(&bytes)).is_err());
        // Several disks.
        let mut bytes = records.clone();
        let mut tail = end(1, records.len() as u32, 0);
        tail[4] = 1;
        bytes.extend(tail);
        assert!(
            read(&mut io::Cursor::new(&bytes))
                .unwrap_err()
                .contains("disks")
        );
    }

    #[test]
    fn converts_times_of_ms_dos() {
        let now = 1_763_500_000;
        let (time, date) = to_dos(now);
        assert!((now - from_dos(time, date)).abs() <= 1);
        assert_eq!(to_dos(0), (0, 0x21));
    }
}
