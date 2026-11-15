//! Reading the boot image memory object: the index it starts with, and
//! the boot modules whose contents follow it.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// The magic and version that the index starts with.
pub const MAGIC: [u8; 8] = *b"MIKROSBM";
pub const VERSION: u32 = 1;

const HEADER: usize = 16;
const ENTRY: usize = 24;

/// A boot module: its path, where its contents start in the image and
/// the contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Module<'a> {
    pub path: &'a [u8],
    pub offset: usize,
    pub data: &'a [u8],
}

/// Why an image has no valid index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The image does not start with the magic and version.
    Header,
    /// The entry of this number, or its path or contents, lies outside the
    /// image.
    Entry(usize),
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    let bytes = b.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_le_bytes(bytes.try_into().ok()?)).ok()
}

fn u64_at(b: &[u8], at: usize) -> Option<usize> {
    let bytes = b.get(at..at.checked_add(8)?)?;
    usize::try_from(u64::from_le_bytes(bytes.try_into().ok()?)).ok()
}

/// The modules of `image`, the whole boot image object, in the order of
/// its index.
pub fn modules(image: &[u8]) -> Result<Vec<Module<'_>>, Error> {
    if image.get(..8) != Some(&MAGIC[..]) || u32_at(image, 8) != Some(VERSION as usize) {
        return Err(Error::Header);
    }
    let count = u32_at(image, 12).ok_or(Error::Header)?;
    let mut modules = Vec::new();
    for i in 0..count {
        let module = (|| {
            let e = HEADER.checked_add(i.checked_mul(ENTRY)?)?;
            let offset = u64_at(image, e)?;
            let size = u64_at(image, e + 8)?;
            let path_at = u32_at(image, e + 16)?;
            let path_len = u32_at(image, e + 20)?;
            Some(Module {
                path: image.get(path_at..path_at.checked_add(path_len)?)?,
                offset,
                data: image.get(offset..offset.checked_add(size)?)?,
            })
        })();
        modules.push(module.ok_or(Error::Entry(i))?);
    }
    Ok(modules)
}

/// The contents of the module of `image` whose path is `path`.
pub fn find<'a>(image: &'a [u8], path: &[u8]) -> Option<&'a [u8]> {
    modules(image)
        .ok()?
        .into_iter()
        .find(|m| m.path == path)
        .map(|m| m.data)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec;

    /// An image with an index page for `modules`, each followed by zeros to
    /// the next page.
    fn image(modules: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut index = vec![0u8; 4096];
        index[..8].copy_from_slice(&MAGIC);
        index[8..12].copy_from_slice(&1u32.to_le_bytes());
        index[12..16].copy_from_slice(&(modules.len() as u32).to_le_bytes());
        let mut contents = Vec::new();
        let mut path_at = HEADER + ENTRY * modules.len();
        for (i, (path, data)) in modules.iter().enumerate() {
            let offset = if data.is_empty() {
                0
            } else {
                4096 + contents.len()
            };
            let e = HEADER + ENTRY * i;
            index[e..e + 8].copy_from_slice(&(offset as u64).to_le_bytes());
            index[e + 8..e + 16].copy_from_slice(&(data.len() as u64).to_le_bytes());
            index[e + 16..e + 20].copy_from_slice(&(path_at as u32).to_le_bytes());
            index[e + 20..e + 24].copy_from_slice(&(path.len() as u32).to_le_bytes());
            index[path_at..path_at + path.len()].copy_from_slice(path);
            path_at += path.len();
            contents.extend_from_slice(data);
            contents.resize(contents.len().next_multiple_of(4096), 0);
        }
        index.extend(contents);
        index
    }

    #[test]
    fn reads_the_modules() {
        let image = image(&[(b"init", b"\x7fELF"), (b"empty", b""), (b"bin/x", b"xx")]);
        let modules = modules(&image).unwrap();
        assert_eq!(modules.len(), 3);
        assert_eq!(
            modules[0],
            Module {
                path: b"init",
                offset: 4096,
                data: b"\x7fELF"
            }
        );
        assert_eq!(modules[1].data, b"");
        assert_eq!(modules[2].offset, 8192);
        assert_eq!(find(&image, b"bin/x"), Some(&b"xx"[..]));
        assert_eq!(find(&image, b"bin"), None);
    }

    #[test]
    fn refuses_bad_indices() {
        let good = image(&[(b"init", b"data")]);
        let mut bad = good.clone();
        bad[0] = b'X';
        assert_eq!(modules(&bad), Err(Error::Header));
        let mut bad = good.clone();
        bad[8] = 2;
        assert_eq!(modules(&bad), Err(Error::Header));
        let mut bad = good.clone();
        bad[12] = 2;
        assert_eq!(modules(&bad), Err(Error::Entry(1)));
        let mut bad = good.clone();
        bad[HEADER + 8..HEADER + 16].copy_from_slice(&5000u64.to_le_bytes());
        assert_eq!(modules(&bad), Err(Error::Entry(0)));
        assert_eq!(modules(&good[..4096 + 3]), Err(Error::Entry(0)));
        assert_eq!(modules(&good[..10]), Err(Error::Header));
    }
}
