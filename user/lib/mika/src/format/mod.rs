//! The `.mika` file format (MIKSPEC-0101.R1) and the content checksum of
//! a package (MIKSPEC-0101.R2): reading a package, checked, and writing
//! one from a folder tree.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 8] = b"MIKAPKG1";
pub const VERSION: u32 = 1;
/// The sizes of the header and of an index entry.
pub const HEADER: usize = 0x20;
pub const ENTRY: usize = 80;
/// The manifest's path, at the package's root.
pub const MANIFEST: &str = "manifest.yaml";
/// What the data of a file that is not compressed starts at a multiple
/// of, so that it can be mapped from the package.
pub const ALIGN: u64 = 4096;
/// The `executable` bit of an entry's `flags`.
pub const EXECUTABLE: u16 = 1;

/// Why a package was refused, or could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The package breaks a rule of its format; what it breaks.
    Invalid(&'static str),
    /// Reading the package failed.
    Io,
}

pub type Result<T> = core::result::Result<T, Error>;

fn invalid<T>(what: &'static str) -> Result<T> {
    Err(Error::Invalid(what))
}

/// Where a package's bytes come from.
pub trait Source {
    /// The package's size in bytes.
    fn size(&self) -> u64;
    /// Fills `buf` with the bytes from `offset`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;
}

impl Source for [u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let at = usize::try_from(offset).map_err(|_| Error::Io)?;
        let bytes = self.get(at..at + buf.len()).ok_or(Error::Io)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    Symlink,
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Kind::File => 1,
            Kind::Folder => 2,
            Kind::Symlink => 3,
        }
    }
}

/// An entry of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub data_offset: u64,
    pub size: u64,
    pub stored_size: u64,
    pub compressed: bool,
    pub flags: u16,
    pub sha256: [u8; 32],
}

impl Entry {
    pub fn executable(&self) -> bool {
        self.flags & EXECUTABLE != 0
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Whether `path` is relative, with components that are not empty, `.`
/// or `..`.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Whether the symbolic link at `path` with `target` stays within the
/// package: a relative target whose `..` never leave its root.
fn stays_within(path: &str, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    let mut depth = path.split('/').count() - 1;
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            _ => depth += 1,
        }
    }
    true
}

/// A package's index, read and checked.
#[derive(Debug)]
pub struct Package {
    pub entries: Vec<Entry>,
}

impl Package {
    /// Reads the index of the package in `source` and checks that the
    /// package keeps the rules of its format, the contents of its
    /// symbolic links among them. File contents are checked as they are
    /// read.
    pub fn open<S: Source + ?Sized>(source: &S) -> Result<Package> {
        let len = source.size();
        let mut header = [0u8; HEADER];
        source.read_at(0, &mut header)?;
        if &header[..8] != MAGIC {
            return invalid("no .mika magic");
        }
        if u32_at(&header, 8) != VERSION {
            return invalid("an unknown version");
        }
        let count = u32_at(&header, 0xC) as u64;
        let index_offset = u64_at(&header, 0x10);
        let strings_offset = u64_at(&header, 0x18);
        let index_end = count
            .checked_mul(ENTRY as u64)
            .and_then(|n| n.checked_add(index_offset))
            .filter(|&end| end <= len)
            .ok_or(Error::Invalid("an index outside the file"))?;
        if strings_offset > len {
            return invalid("paths outside the file");
        }
        let mut index = vec![0u8; (index_end - index_offset) as usize];
        source.read_at(index_offset, &mut index)?;
        let mut entries: Vec<Entry> = Vec::with_capacity(count as usize);
        for raw in index.as_chunks::<ENTRY>().0 {
            let path_offset = strings_offset + u64::from(u32_at(raw, 0x18));
            let path_len = u32_at(raw, 0x1C) as usize;
            if path_offset + path_len as u64 > len {
                return invalid("a path outside the file");
            }
            let mut path = vec![0u8; path_len];
            source.read_at(path_offset, &mut path)?;
            let path =
                String::from_utf8(path).map_err(|_| Error::Invalid("a path that is not UTF-8"))?;
            if !valid_path(&path) {
                return invalid("a path that is absolute or has an empty, . or .. component");
            }
            if let Some(last) = entries.last()
                && last.path.as_bytes() >= path.as_bytes()
            {
                return invalid("paths out of order, or twice");
            }
            let kind = match raw[0x20] {
                1 => Kind::File,
                2 => Kind::Folder,
                3 => Kind::Symlink,
                _ => return invalid("an unknown entry type"),
            };
            let compressed = match raw[0x21] {
                0 => false,
                1 => true,
                _ => return invalid("an unknown compression"),
            };
            let entry = Entry {
                path,
                kind,
                data_offset: u64_at(raw, 0),
                size: u64_at(raw, 8),
                stored_size: u64_at(raw, 0x10),
                compressed,
                flags: u16_at(raw, 0x22),
                sha256: raw[0x28..0x48].try_into().unwrap(),
            };
            if entry.kind != Kind::Folder {
                let end = entry.data_offset.checked_add(entry.stored_size);
                if end.is_none_or(|end| end > len) {
                    return invalid("data outside the file");
                }
            }
            if entry.kind == Kind::Folder && entry.sha256 != [0; 32] {
                return invalid("a folder with a checksum");
            }
            if entry.kind == Kind::File && !entry.compressed {
                if !entry.data_offset.is_multiple_of(ALIGN) {
                    return invalid("file data that is not page-aligned");
                }
                if entry.stored_size != entry.size {
                    return invalid("a stored size that is not the size");
                }
            }
            entries.push(entry);
        }
        let package = Package { entries };
        match package.find(MANIFEST) {
            Some(e) if e.kind == Kind::File => {}
            _ => return invalid("no manifest.yaml at the root"),
        }
        for entry in package.entries.iter().filter(|e| e.kind == Kind::Symlink) {
            let target = package.read(source, entry)?;
            let target = core::str::from_utf8(&target)
                .map_err(|_| Error::Invalid("a link target that is not UTF-8"))?;
            if !stays_within(&entry.path, target) {
                return invalid("a link that is absolute or leads out of the package");
            }
        }
        Ok(package)
    }

    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.path.as_bytes().cmp(path.as_bytes()))
            .ok()
            .map(|at| &self.entries[at])
    }

    /// The contents of `entry`, decompressed and checked against its
    /// `sha256`.
    pub fn read<S: Source + ?Sized>(&self, source: &S, entry: &Entry) -> Result<Vec<u8>> {
        if entry.kind == Kind::Folder {
            return Ok(Vec::new());
        }
        let size = usize::try_from(entry.size).map_err(|_| Error::Io)?;
        let mut stored = vec![0u8; usize::try_from(entry.stored_size).map_err(|_| Error::Io)?];
        source.read_at(entry.data_offset, &mut stored)?;
        let contents = match entry.compressed {
            false => stored,
            true => decompress(&stored, size)?,
        };
        if contents.len() != size || <[u8; 32]>::from(Sha256::digest(&contents)) != entry.sha256 {
            return invalid("contents that do not match their checksum");
        }
        Ok(contents)
    }

    /// The content checksum of the package: of every entry but the
    /// manifest and `signature`, the signature file that it names.
    pub fn checksum(&self, signature: Option<&str>) -> [u8; 32] {
        let mut sum = Sha256::new();
        for entry in &self.entries {
            if entry.path == MANIFEST || Some(entry.path.as_str()) == signature {
                continue;
            }
            sum.update(entry.path.as_bytes());
            sum.update([0, entry.kind.byte()]);
            sum.update(entry.flags.to_le_bytes());
            sum.update(entry.sha256);
        }
        sum.finalize().into()
    }
}

/// The contents of `stored`, one zstd frame of `size` bytes.
fn decompress(stored: &[u8], size: usize) -> Result<Vec<u8>> {
    use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
    use ruzstd::io::Read;
    let mut input = stored;
    let mut decoder = FrameDecoder::new();
    decoder
        .init(&mut input)
        .map_err(|_| Error::Invalid("compressed data that is not a zstd frame"))?;
    decoder
        .decode_blocks(&mut input, BlockDecodingStrategy::All)
        .map_err(|_| Error::Invalid("a zstd frame that cannot be decoded"))?;
    if !decoder.is_finished() || !input.is_empty() {
        return invalid("compressed data that is not one zstd frame");
    }
    let mut contents = vec![0u8; size];
    let mut at = 0;
    while at < size {
        match decoder.read(&mut contents[at..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => at += n,
        }
    }
    let rest = decoder.can_collect();
    if at != size || rest != 0 {
        return invalid("a zstd frame of another size");
    }
    Ok(contents)
}

/// The text of `checksum` as the manifest gives it: `sha256:` and 64
/// lowercase hexadecimal digits.
pub fn checksum_text(checksum: &[u8; 32]) -> String {
    let mut text = String::from("sha256:");
    for byte in checksum {
        text.push(char::from_digit(u32::from(byte >> 4), 16).unwrap());
        text.push(char::from_digit(u32::from(byte & 15), 16).unwrap());
    }
    text
}

/// A package being written: its entries, by path.
#[derive(Default)]
pub struct Builder {
    entries: Vec<(String, Kind, u16, Vec<u8>)>,
}

impl Builder {
    pub fn folder(&mut self, path: &str) {
        self.entries
            .push((path.into(), Kind::Folder, 0, Vec::new()));
    }

    pub fn file(&mut self, path: &str, contents: Vec<u8>, executable: bool) {
        let flags = if executable { EXECUTABLE } else { 0 };
        self.entries
            .push((path.into(), Kind::File, flags, contents));
    }

    pub fn symlink(&mut self, path: &str, target: &str) {
        self.entries
            .push((path.into(), Kind::Symlink, 0, target.as_bytes().to_vec()));
    }

    /// The content checksum that the package will have, for its manifest.
    pub fn checksum(&self, signature: Option<&str>) -> [u8; 32] {
        let mut sorted: Vec<&(String, Kind, u16, Vec<u8>)> = self.entries.iter().collect();
        sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut sum = Sha256::new();
        for (path, kind, flags, contents) in sorted {
            if path == MANIFEST || Some(path.as_str()) == signature {
                continue;
            }
            let sha: [u8; 32] = match kind {
                Kind::Folder => [0; 32],
                _ => Sha256::digest(contents).into(),
            };
            sum.update(path.as_bytes());
            sum.update([0, kind.byte()]);
            sum.update(flags.to_le_bytes());
            sum.update(sha);
        }
        sum.finalize().into()
    }

    /// The package's bytes, its entries in the order of their paths, with
    /// no compression and every file's data at a multiple of `ALIGN`.
    pub fn finish(mut self) -> Vec<u8> {
        self.entries
            .sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let count = self.entries.len();
        let index_offset = HEADER;
        let strings_offset = index_offset + ENTRY * count;
        let mut strings = Vec::new();
        let mut offsets = Vec::with_capacity(count);
        for (path, ..) in &self.entries {
            offsets.push(strings.len());
            strings.extend_from_slice(path.as_bytes());
        }
        let mut data_at = (strings_offset + strings.len()) as u64;
        let mut index = Vec::with_capacity(ENTRY * count);
        let mut data: Vec<(u64, &[u8])> = Vec::new();
        for (at, (path, kind, flags, contents)) in self.entries.iter().enumerate() {
            let offset = match kind {
                Kind::Folder => 0,
                Kind::File => data_at.next_multiple_of(ALIGN),
                Kind::Symlink => data_at,
            };
            if *kind != Kind::Folder {
                data.push((offset, contents));
                data_at = offset + contents.len() as u64;
            }
            let sha: [u8; 32] = match kind {
                Kind::Folder => [0; 32],
                _ => Sha256::digest(contents).into(),
            };
            let size = contents.len() as u64;
            index.extend_from_slice(&offset.to_le_bytes());
            index.extend_from_slice(&size.to_le_bytes());
            index.extend_from_slice(&size.to_le_bytes());
            index.extend_from_slice(&(offsets[at] as u32).to_le_bytes());
            index.extend_from_slice(&(path.len() as u32).to_le_bytes());
            index.extend_from_slice(&[kind.byte(), 0]);
            index.extend_from_slice(&flags.to_le_bytes());
            index.extend_from_slice(&[0; 4]);
            index.extend_from_slice(&sha);
            index.extend_from_slice(&[0; 8]);
        }
        let mut bytes = Vec::with_capacity(data_at as usize);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        bytes.extend_from_slice(&(index_offset as u64).to_le_bytes());
        bytes.extend_from_slice(&(strings_offset as u64).to_le_bytes());
        bytes.extend_from_slice(&index);
        bytes.extend_from_slice(&strings);
        for (offset, contents) in data {
            bytes.resize(offset as usize, 0);
            bytes.extend_from_slice(contents);
        }
        bytes
    }
}

#[cfg(test)]
mod tests;
