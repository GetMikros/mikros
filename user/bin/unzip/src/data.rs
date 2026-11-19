//! The data of an entry (MIKSPEC-0098.R59): found after its local
//! header, read stored or inflated and no further than its compressed
//! size, with its CRC-32 counted as it is read; and the check that the
//! entries' data overlap neither one another nor the central directory
//! (MIKSPEC-0098.R61).

use crate::central::{self, DEFLATED, Directory, ENCRYPTED, LOCAL, Record, STORED};
use flate2::read::DeflateDecoder;
use std::io::{self, Read, Seek};

/// Where the data of `record` starts, after its local header.
pub fn start<F: Read + Seek>(file: &mut F, record: &Record) -> Result<u64, String> {
    let header = central::read_at(file, record.offset, 30)?;
    if u32::from_le_bytes([header[0], header[1], header[2], header[3]]) != LOCAL {
        return Err("its local header is not where the central directory says".into());
    }
    let name = u64::from(u16::from_le_bytes([header[26], header[27]]));
    let extra = u64::from(u16::from_le_bytes([header[28], header[29]]));
    Ok(record.offset + 30 + name + extra)
}

/// Refuses an archive in which the local header and data of an entry
/// overlap those of another or the central directory.
pub fn overlapping<F: Read + Seek>(file: &mut F, directory: &Directory) -> Result<(), String> {
    let mut spans = Vec::with_capacity(directory.records.len());
    for record in &directory.records {
        let begin = start(file, record).map_err(|why| format!("{}: {why}", record.name))?;
        let end = begin
            .checked_add(record.compressed)
            .ok_or_else(|| format!("{}: its compressed size is too large", record.name))?;
        spans.push((record.offset, end, &record.name));
    }
    spans.sort();
    for pair in spans.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(format!(
                "the data of {} and {} overlap, as in an archive made to expand beyond its size",
                pair[0].2, pair[1].2
            ));
        }
    }
    if let Some((_, end, name)) = spans.last()
        && *end > directory.start
    {
        return Err(format!("the data of {name} overlaps the central directory"));
    }
    Ok(())
}

/// The data of `record`, inflated where it is deflated, or why it cannot
/// be read.
pub fn open<'a, F: Read + Seek>(
    file: &'a mut F,
    record: &Record,
) -> Result<Box<dyn Read + 'a>, String> {
    if record.flags & ENCRYPTED != 0 {
        return Err("it is encrypted, which unzip does not take".into());
    }
    if record.method != STORED && record.method != DEFLATED {
        return Err(format!(
            "it is compressed with method {}, which unzip does not take",
            record.method
        ));
    }
    let begin = start(file, record)?;
    file.seek(io::SeekFrom::Start(begin))
        .map_err(|e| format!("cannot read the archive: {e}"))?;
    let raw = file.take(record.compressed);
    Ok(match record.method {
        STORED => Box::new(raw),
        _ => Box::new(DeflateDecoder::new(raw)),
    })
}

/// A reader that counts the CRC-32 of what it reads.
pub struct Crc<R> {
    inner: R,
    hasher: crc32fast::Hasher,
    pub count: u64,
}

impl<R: Read> Crc<R> {
    pub fn new(inner: R) -> Crc<R> {
        Crc {
            inner,
            hasher: crc32fast::Hasher::new(),
            count: 0,
        }
    }

    pub fn crc(&self) -> u32 {
        self.hasher.clone().finalize()
    }
}

impl<R: Read> Read for Crc<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let got = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..got]);
        self.count += got as u64;
        Ok(got)
    }
}

/// Why the CRC-32 of `record`'s data, `crc`, fails it, if it does.
pub fn checked(record: &Record, crc: u32) -> Result<(), String> {
    match crc == record.crc {
        true => Ok(()),
        false => Err(format!(
            "its CRC-32 is {crc:08x}, not the {:08x} that the archive gives",
            record.crc
        )),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::central::{CENTRAL, END};
    use flate2::{Compression, write::DeflateEncoder};
    use std::io::Write;

    /// An entry of a crafted archive.
    pub struct Crafted {
        pub name: String,
        pub data: Vec<u8>,
        pub deflate: bool,
        /// The mode, with its type, when it stores one.
        pub mode: Option<u32>,
        /// What the central directory says instead of the truth.
        pub size: Option<u64>,
        pub crc: Option<u32>,
        pub offset: Option<u64>,
        pub flags: u16,
        pub method: Option<u16>,
    }

    pub fn crafted(name: &str, data: &[u8]) -> Crafted {
        Crafted {
            name: name.into(),
            data: data.to_vec(),
            deflate: true,
            mode: Some(0o100644),
            size: None,
            crc: None,
            offset: None,
            flags: 0,
            method: None,
        }
    }

    /// An archive of `entries`, with what they say instead of the truth.
    pub fn archive(entries: &[Crafted]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for e in entries {
            let packed = match e.deflate {
                true => {
                    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                    encoder.write_all(&e.data).unwrap();
                    encoder.finish().unwrap()
                }
                false => e.data.clone(),
            };
            let method = e.method.unwrap_or(if e.deflate { 8 } else { 0 });
            let crc = e.crc.unwrap_or_else(|| crc32fast::hash(&e.data));
            let size = e.size.unwrap_or(e.data.len() as u64) as u32;
            let offset = e.offset.unwrap_or(out.len() as u64) as u32;
            let fixed = |out: &mut Vec<u8>| {
                out.extend(20u16.to_le_bytes());
                out.extend(e.flags.to_le_bytes());
                out.extend(method.to_le_bytes());
                out.extend([0, 0, 0x21, 0x5b]);
                out.extend(crc.to_le_bytes());
                out.extend((packed.len() as u32).to_le_bytes());
                out.extend(size.to_le_bytes());
                out.extend((e.name.len() as u16).to_le_bytes());
                out.extend(0u16.to_le_bytes());
            };
            out.extend(LOCAL.to_le_bytes());
            fixed(&mut out);
            out.extend(e.name.as_bytes());
            out.extend(&packed);
            central.extend(CENTRAL.to_le_bytes());
            let host: u16 = if e.mode.is_some() { 3 } else { 0 };
            central.extend(((host << 8) | 20).to_le_bytes());
            fixed(&mut central);
            central.extend([0; 6]);
            central.extend((e.mode.unwrap_or(0) << 16).to_le_bytes());
            central.extend(offset.to_le_bytes());
            central.extend(e.name.as_bytes());
        }
        let start = out.len() as u32;
        let size = central.len() as u32;
        out.extend(central);
        out.extend(END.to_le_bytes());
        out.extend([0; 4]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend([0; 2]);
        out
    }

    fn read_all(bytes: &[u8]) -> Vec<Result<(Vec<u8>, u32), String>> {
        let mut file = io::Cursor::new(bytes);
        let directory = central::read(&mut file).unwrap();
        directory
            .records
            .iter()
            .map(|record| {
                let mut data = Crc::new(open(&mut file, record)?);
                let mut out = Vec::new();
                data.read_to_end(&mut out).map_err(|e| e.to_string())?;
                Ok((out, data.crc()))
            })
            .collect()
    }

    #[test]
    fn reads_stored_and_deflated_data() {
        let mut stored = crafted("s", b"stored data");
        stored.deflate = false;
        let bytes = archive(&[crafted("d", &[b'z'; 5000]), stored]);
        let read = read_all(&bytes);
        let (data, crc) = read[0].as_ref().unwrap();
        assert_eq!((data.len(), *crc), (5000, crc32fast::hash(&[b'z'; 5000])));
        assert_eq!(read[1].as_ref().unwrap().0, b"stored data");
        let mut file = io::Cursor::new(&bytes);
        let directory = central::read(&mut file).unwrap();
        assert!(overlapping(&mut file, &directory).is_ok());
    }

    #[test]
    fn refuses_encrypted_entries_and_other_methods() {
        let mut encrypted = crafted("e", b"x");
        encrypted.flags = ENCRYPTED;
        let mut bzip = crafted("b", b"x");
        bzip.method = Some(12);
        let read = read_all(&archive(&[encrypted, bzip]));
        assert!(read[0].as_ref().unwrap_err().contains("encrypted"));
        assert!(read[1].as_ref().unwrap_err().contains("method 12"));
    }

    #[test]
    fn refuses_overlapping_entries() {
        // Two records whose data are the same bytes.
        let mut second = crafted("b", b"same");
        second.offset = Some(0);
        let bytes = archive(&[crafted("a", b"same"), second]);
        let mut file = io::Cursor::new(&bytes);
        let directory = central::read(&mut file).unwrap();
        let why = overlapping(&mut file, &directory).unwrap_err();
        assert!(why.contains("overlap"), "{why}");
        // A record whose local header lies in another's data.
        let mut quoted = crafted("q", &[0; 200]);
        quoted.deflate = false;
        let mut inner = crafted("i", b"");
        inner.offset = Some(40);
        let mut bytes = archive(&[quoted, inner]);
        bytes[40..44].copy_from_slice(&LOCAL.to_le_bytes());
        let mut file = io::Cursor::new(&bytes);
        let directory = central::read(&mut file).unwrap();
        assert!(overlapping(&mut file, &directory).is_err());
    }

    #[test]
    fn refuses_data_that_runs_into_the_central_directory() {
        let mut first = crafted("a", b"abc");
        first.deflate = false;
        let mut bytes = archive(&[first]);
        // The compressed size reaches past where the directory starts.
        let at = bytes.len() - 22 - 46 - 1 + 20;
        bytes[at..at + 4].copy_from_slice(&100u32.to_le_bytes());
        let mut file = io::Cursor::new(&bytes);
        let directory = central::read(&mut file).unwrap();
        let why = overlapping(&mut file, &directory).unwrap_err();
        assert!(why.contains("central directory"), "{why}");
    }

    #[test]
    fn names_crc_mismatches() {
        let record = Record {
            crc: 5,
            ..Record::default()
        };
        assert!(checked(&record, 5).is_ok());
        assert!(checked(&record, 6).unwrap_err().contains("00000006"));
    }
}
