//! The headers of tar archives (MIKSPEC-0098.R65): reading the blocks
//! of ustar, pax and GNU archives, and of older ones without a magic
//! number, and writing those of pax archives, a ustar header after a pax
//! extended header for what does not fit it.

/// The size of a block.
pub const BLOCK: usize = 512;
/// How many blocks make a record, which the archive is padded to.
pub const RECORD: usize = 20;
/// The longest pax extended header or GNU long name or long link that
/// extraction takes (MIKSPEC-0098.R67).
pub const LONGEST: u64 = 1 << 20;

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    /// A symbolic link.
    Link,
    Hard,
    /// A character or block device.
    Device,
    Fifo,
    /// Another type, by its flag.
    Other(u8),
}

impl Kind {
    /// The letter of a long line.
    pub fn letter(self) -> char {
        match self {
            Kind::File => '-',
            Kind::Folder => 'd',
            Kind::Link => 'l',
            Kind::Hard => 'h',
            Kind::Device => 'c',
            Kind::Fifo => 'p',
            Kind::Other(_) => '?',
        }
    }

    /// The word of JSON and messages.
    pub fn word(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Folder => "folder",
            Kind::Link => "link",
            Kind::Hard => "hardlink",
            Kind::Device => "device",
            Kind::Fifo => "fifo",
            Kind::Other(_) => "other",
        }
    }
}

/// An entry of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    /// The size of its data.
    pub size: u64,
    /// The stored mode, without the type.
    pub mode: u32,
    /// Seconds since 1970.
    pub modified: i64,
    /// What a link or hard link leads to, empty otherwise.
    pub target: String,
}

/// What a header block holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A block of zeros, which ends the archive.
    End,
    /// A pax extended header for the next entry, of this size.
    Pax(u64),
    /// A pax global header, of this size.
    Global(u64),
    /// A GNU long name for the next entry, of this size.
    LongName(u64),
    /// A GNU long link for the next entry, of this size.
    LongLink(u64),
    /// An entry, with how many bytes of data follow it.
    Entry(Entry, u64),
}

/// Reads one header block.
pub fn parse(block: &[u8; BLOCK]) -> Result<Block, String> {
    if block.iter().all(|&b| b == 0) {
        return Ok(Block::End);
    }
    let stored = octal(&block[148..156]).ok_or("a header has an invalid checksum field")?;
    let unsigned: i64 = block
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                i64::from(b)
            }
        })
        .sum();
    let signed: i64 = block
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                i64::from(b as i8)
            }
        })
        .sum();
    if stored != unsigned && stored != signed {
        return Err(
            "a header's checksum does not match: this is not a tar archive or it is damaged".into(),
        );
    }
    let size = number(&block[124..136]).ok_or("a header has an invalid size")?;
    let size = u64::try_from(size).map_err(|_| "a header has a negative size")?;
    let flag = block[156];
    match flag {
        b'x' => return Ok(Block::Pax(size)),
        b'g' => return Ok(Block::Global(size)),
        b'L' => return Ok(Block::LongName(size)),
        b'K' => return Ok(Block::LongLink(size)),
        _ => {}
    }
    let ustar = &block[257..263] == b"ustar\0";
    let mut name = text(&block[..100]);
    if ustar {
        let prefix = text(&block[345..500]);
        if !prefix.is_empty() {
            name = [prefix, b"/".to_vec(), name].concat();
        }
    }
    let path = String::from_utf8_lossy(&name).into_owned();
    let kind = match flag {
        b'0' | 0 | b'7' if path.ends_with('/') => Kind::Folder,
        b'0' | 0 | b'7' => Kind::File,
        b'1' => Kind::Hard,
        b'2' => Kind::Link,
        b'3' | b'4' => Kind::Device,
        // GNU's dump folders are folders, with a list of names as data.
        b'5' | b'D' => Kind::Folder,
        b'6' => Kind::Fifo,
        other => Kind::Other(other),
    };
    // Only files, dump folders and types not known have data.
    let data = match (kind, flag) {
        (Kind::File | Kind::Other(_), _) | (_, b'D') => size,
        _ => 0,
    };
    let mode = number(&block[100..108]).ok_or("a header has an invalid mode")?;
    let modified = number(&block[136..148]).ok_or("a header has an invalid modified time")?;
    let entry = Entry {
        path,
        kind,
        size: match kind {
            Kind::File => size,
            _ => 0,
        },
        mode: (mode & 0o7777) as u32,
        modified: i64::try_from(modified).map_err(|_| "a header has an invalid modified time")?,
        target: String::from_utf8_lossy(&text(&block[157..257])).into_owned(),
    };
    Ok(Block::Entry(entry, data))
}

/// The bytes of a field up to its first NUL.
fn text(field: &[u8]) -> Vec<u8> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    field[..end].to_vec()
}

/// The octal number of a field, with spaces and NULs around it.
fn octal(field: &[u8]) -> Option<i64> {
    let start = field.iter().position(|&b| b != b' ' && b != 0)?;
    let mut value: i64 = 0;
    for &b in &field[start..] {
        match b {
            b'0'..=b'7' => value = value.checked_mul(8)?.checked_add(i64::from(b - b'0'))?,
            b' ' | 0 => break,
            _ => return None,
        }
    }
    Some(value)
}

/// The number of a field: octal, or GNU's base 256, two's complement,
/// when its first byte has the high bit set; 0 when it is empty.
fn number(field: &[u8]) -> Option<i128> {
    if field[0] & 0x80 != 0 {
        if field.len() > 15 {
            return None;
        }
        let first = i128::from(field[0] & 0x7f);
        let mut value = match first & 0x40 {
            0 => first,
            _ => (-1 << 7) | first,
        };
        for &b in &field[1..] {
            value = (value << 8) | i128::from(b);
        }
        return Some(value);
    }
    match field.iter().all(|&b| b == 0 || b == b' ') {
        true => Some(0),
        false => octal(field).map(i128::from),
    }
}

/// What a pax extended header gives for the next entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pax {
    pub path: Option<String>,
    pub target: Option<String>,
    pub size: Option<u64>,
    pub modified: Option<i64>,
}

/// Reads the records of a pax extended header, `<length> <key>=<value>`
/// and a newline each, keeping those for the path, the link's target,
/// the size and the modified time.
pub fn pax(mut data: &[u8], into: &mut Pax) -> Result<(), String> {
    let bad = || String::from("a pax extended header is malformed");
    while !data.is_empty() && data[0] != 0 {
        let space = data.iter().position(|&b| b == b' ').ok_or_else(bad)?;
        let length: usize = std::str::from_utf8(&data[..space])
            .ok()
            .and_then(|l| l.parse().ok())
            .ok_or_else(bad)?;
        if length <= space + 1 || length > data.len() || data[length - 1] != b'\n' {
            return Err(bad());
        }
        let record = &data[space + 1..length - 1];
        let equals = record.iter().position(|&b| b == b'=').ok_or_else(bad)?;
        let key = &record[..equals];
        let value = String::from_utf8_lossy(&record[equals + 1..]).into_owned();
        match key {
            b"path" => into.path = Some(value),
            b"linkpath" => into.target = Some(value),
            b"size" => into.size = Some(value.parse().map_err(|_| bad())?),
            b"mtime" => into.modified = Some(seconds(&value).ok_or_else(bad)?),
            _ => {}
        }
        data = &data[length..];
    }
    Ok(())
}

/// The whole seconds of a pax time, which may have a fraction and a
/// sign, rounded down.
fn seconds(text: &str) -> Option<i64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let negative = whole.starts_with('-');
    let digits = whole.strip_prefix('-').unwrap_or(whole);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i64 = digits.parse().ok()?;
    match negative {
        false => Some(value),
        true if fraction.bytes().any(|b| b != b'0') => value.checked_neg()?.checked_sub(1),
        true => value.checked_neg(),
    }
}

/// The NUL-terminated name of a GNU long name or long link.
pub fn long(data: &[u8]) -> String {
    String::from_utf8_lossy(&text(data)).into_owned()
}

/// The blocks that store `entry` before its data: a pax extended header
/// when its path, target, size or time do not fit a ustar header, or the
/// path or target are not ASCII, and then the ustar header. A folder's
/// path ends with `/`.
pub fn write(entry: &Entry) -> Vec<u8> {
    let mut records = Vec::new();
    let path = entry.path.as_bytes();
    let split = split(path);
    if split.is_none() || !path.is_ascii() {
        records.extend(record("path", &entry.path));
    }
    let target = entry.target.as_bytes();
    if target.len() > 100 || !target.is_ascii() {
        records.extend(record("linkpath", &entry.target));
    }
    const MOST: u64 = 0o77777777777;
    let size = match entry.kind {
        Kind::File => entry.size,
        _ => 0,
    };
    if size > MOST {
        records.extend(record("size", &size.to_string()));
    }
    let modified = match u64::try_from(entry.modified) {
        Ok(m) if m <= MOST => m,
        _ => {
            records.extend(record("mtime", &entry.modified.to_string()));
            entry.modified.clamp(0, MOST as i64) as u64
        }
    };
    let mut out = Vec::new();
    if !records.is_empty() {
        let base = entry.path.trim_end_matches('/');
        let base = base.rsplit('/').next().unwrap_or(base);
        let mut name = format!("PaxHeaders/{base}").into_bytes();
        name.truncate(100);
        let mut block = ustar(&name, &[], b'x', 0o644, records.len() as u64, modified, &[]);
        out.append(&mut block);
        out.extend(&records);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
    }
    let (prefix, name) = split.unwrap_or((&[][..], &path[..path.len().min(100)]));
    let flag = match entry.kind {
        Kind::File => b'0',
        Kind::Folder => b'5',
        Kind::Link => b'2',
        Kind::Hard => b'1',
        Kind::Device => b'3',
        Kind::Fifo => b'6',
        Kind::Other(flag) => flag,
    };
    let target = &target[..target.len().min(100)];
    let size = if size > MOST { 0 } else { size };
    out.append(&mut ustar(
        name,
        prefix,
        flag,
        entry.mode & 0o7777,
        size,
        modified,
        target,
    ));
    out
}

/// The ustar header block of an entry, with owner and group 0 and no
/// names for them.
fn ustar(
    name: &[u8],
    prefix: &[u8],
    flag: u8,
    mode: u32,
    size: u64,
    modified: u64,
    target: &[u8],
) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK];
    block[..name.len()].copy_from_slice(name);
    put_octal(&mut block[100..108], u64::from(mode));
    put_octal(&mut block[108..116], 0);
    put_octal(&mut block[116..124], 0);
    put_octal(&mut block[124..136], size);
    put_octal(&mut block[136..148], modified);
    block[156] = flag;
    block[157..157 + target.len()].copy_from_slice(target);
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[345..345 + prefix.len()].copy_from_slice(prefix);
    block[148..156].fill(b' ');
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    block
}

/// Writes `value` in octal with leading zeros and a NUL.
fn put_octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(&text.as_bytes()[text.len() - digits..]);
    field[digits] = 0;
}

/// `path` as the prefix and name of a ustar header, when it fits them.
fn split(path: &[u8]) -> Option<(&[u8], &[u8])> {
    if path.len() <= 100 {
        return Some((&[], path));
    }
    (1..path.len().min(156))
        .rev()
        .filter(|&i| path[i] == b'/')
        .map(|i| (&path[..i], &path[i + 1..]))
        .find(|(_, name)| !name.is_empty() && name.len() <= 100)
}

/// A pax record, whose length counts its own digits.
fn record(key: &str, value: &str) -> Vec<u8> {
    let rest = key.len() + value.len() + 3;
    let mut length = rest + 1;
    while length != rest + length.to_string().len() {
        length = rest + length.to_string().len();
    }
    format!("{length} {key}={value}\n").into_bytes()
}

/// The blocks that end an archive of `written` bytes: two of zeros, and
/// then zeros to the end of the record.
pub fn end(written: u64) -> Vec<u8> {
    let after = written + 2 * BLOCK as u64;
    let record = (RECORD * BLOCK) as u64;
    vec![0; (after.next_multiple_of(record) - written) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: Kind, size: u64) -> Entry {
        Entry {
            path: path.into(),
            kind,
            size,
            mode: 0o644,
            modified: 1_700_000_000,
            target: String::new(),
        }
    }

    fn block(bytes: &[u8]) -> [u8; BLOCK] {
        bytes[..BLOCK].try_into().unwrap()
    }

    #[test]
    fn writes_and_reads_ustar_headers() {
        let mut file = entry("dir/file.txt", Kind::File, 1234);
        file.mode = 0o4755;
        let bytes = write(&file);
        assert_eq!(bytes.len(), BLOCK);
        assert_eq!(&bytes[257..265], b"ustar\x0000");
        assert_eq!(&bytes[108..116], b"0000000\0");
        assert_eq!(&bytes[265..329], &[0; 64][..]);
        assert_eq!(parse(&block(&bytes)).unwrap(), Block::Entry(file, 1234));
        let mut link = entry("l", Kind::Link, 0);
        link.target = "../x".into();
        let read = parse(&block(&write(&link))).unwrap();
        assert_eq!(read, Block::Entry(link, 0));
        let folder = entry("d/", Kind::Folder, 0);
        assert_eq!(
            parse(&block(&write(&folder))).unwrap(),
            Block::Entry(folder, 0)
        );
        assert_eq!(parse(&[0; BLOCK]).unwrap(), Block::End);
    }

    #[test]
    fn splits_long_paths_into_prefix_and_name() {
        let path = format!("{}/{}", "a".repeat(120), "b".repeat(90));
        let file = entry(&path, Kind::File, 0);
        let bytes = write(&file);
        assert_eq!(bytes.len(), BLOCK, "no pax header needed");
        assert_eq!(parse(&block(&bytes)).unwrap(), Block::Entry(file, 0));
    }

    #[test]
    fn writes_pax_headers_for_what_does_not_fit() {
        let mut file = entry(&"n".repeat(150), Kind::File, 1 << 34);
        file.target = String::new();
        file.modified = -5;
        let bytes = write(&file);
        assert_eq!(bytes.len(), 3 * BLOCK);
        let Block::Pax(size) = parse(&block(&bytes)).unwrap() else {
            panic!("no pax header");
        };
        let mut pax_read = Pax::default();
        pax(&bytes[BLOCK..BLOCK + size as usize], &mut pax_read).unwrap();
        assert_eq!(pax_read.path.as_deref(), Some(&*"n".repeat(150)));
        assert_eq!(pax_read.size, Some(1 << 34));
        assert_eq!(pax_read.modified, Some(-5));
        let mut unicode = entry("ä", Kind::Link, 0);
        unicode.target = "ö".repeat(60);
        let bytes = write(&unicode);
        let Block::Pax(size) = parse(&block(&bytes)).unwrap() else {
            panic!("no pax header");
        };
        let mut pax_read = Pax::default();
        pax(&bytes[BLOCK..BLOCK + size as usize], &mut pax_read).unwrap();
        assert_eq!(pax_read.path.as_deref(), Some("ä"));
        assert_eq!(pax_read.target.as_deref(), Some(&*"ö".repeat(60)));
    }

    #[test]
    fn counts_the_digits_of_pax_records() {
        assert_eq!(record("path", "a"), b"9 path=a\n");
        assert_eq!(record("path", &"x".repeat(91)).len(), 101);
        let long = record("path", &"x".repeat(92));
        assert_eq!(long.len(), 102);
        assert!(long.starts_with(b"102 "));
        let mut read = Pax::default();
        pax(&long, &mut read).unwrap();
        assert_eq!(read.path.unwrap().len(), 92);
    }

    #[test]
    fn reads_pax_times_and_refuses_malformed_records() {
        assert_eq!(seconds("12.5"), Some(12));
        assert_eq!(seconds("-12.5"), Some(-13));
        assert_eq!(seconds("-12"), Some(-12));
        assert_eq!(seconds("x"), None);
        let mut read = Pax::default();
        assert!(pax(b"30 path=a\n", &mut read).is_err());
        assert!(pax(b"9 path=a", &mut read).is_err());
        assert!(pax(b"x path=a\n", &mut read).is_err());
        assert!(pax(b"11 size=x1\n", &mut read).is_err());
        pax(b"12 uid=1000\n9 path=b\n\0\0\0", &mut read).unwrap();
        assert_eq!(read.path.as_deref(), Some("b"));
    }

    #[test]
    fn reads_gnu_headers_and_base_256_numbers() {
        let mut bytes = write(&entry("f", Kind::File, 0));
        bytes[257..265].copy_from_slice(b"ustar  \0");
        bytes[345] = b'x';
        bytes[124] = 0x80;
        bytes[125..136].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]);
        bytes[136..148].fill(0xff);
        bytes[148..156].fill(b' ');
        let sum: u64 = bytes.iter().map(|&b| u64::from(b)).sum();
        bytes[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let Block::Entry(read, data) = parse(&block(&bytes)).unwrap() else {
            panic!();
        };
        // No prefix in GNU headers.
        assert_eq!(read.path, "f");
        assert_eq!((read.size, data), (1 << 25, 1 << 25));
        assert_eq!(read.modified, -1);
        let mut long = crate::reader::tests::raw(b'L', "././@LongLink", 300);
        assert_eq!(parse(&block(&long)).unwrap(), Block::LongName(300));
        long[156] = b'K';
        long[148..156].fill(b' ');
        let sum: u64 = long.iter().map(|&b| u64::from(b)).sum();
        long[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        assert_eq!(parse(&block(&long)).unwrap(), Block::LongLink(300));
    }

    #[test]
    fn reads_old_headers_and_refuses_bad_checksums() {
        let mut bytes = write(&entry("old/", Kind::File, 0));
        bytes[257..265].fill(0);
        bytes[156] = 0;
        bytes[148..156].fill(b' ');
        let sum: u64 = bytes.iter().map(|&b| u64::from(b)).sum();
        bytes[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let Block::Entry(read, _) = parse(&block(&bytes)).unwrap() else {
            panic!();
        };
        assert_eq!(read.kind, Kind::Folder);
        bytes[0] = b'x';
        assert!(parse(&block(&bytes)).unwrap_err().contains("checksum"));
    }

    #[test]
    fn gives_no_data_to_links_and_folders() {
        let mut bytes = write(&entry("l", Kind::Link, 0));
        put_octal(&mut bytes[124..136], 4096);
        bytes[148..156].fill(b' ');
        let sum: u64 = bytes.iter().map(|&b| u64::from(b)).sum();
        bytes[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let Block::Entry(read, data) = parse(&block(&bytes)).unwrap() else {
            panic!();
        };
        assert_eq!((read.size, data), (0, 0));
    }

    #[test]
    fn pads_to_the_end_of_a_record() {
        assert_eq!(end(0).len(), 10240);
        assert_eq!(end(512).len(), 10240 - 512);
        assert_eq!(end(9216).len(), 1024);
        assert_eq!(end(9728).len(), 1024 + 10240 - 512);
    }
}
