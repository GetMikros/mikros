//! Reading the members of the boot image, a ustar archive.

#![no_std]

const BLOCK: usize = 512;

/// What a member is, from its type flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    /// Another name for an earlier member, named by `link_name`.
    HardLink,
    /// A symbolic link to `link_name`.
    Symlink,
    Directory,
    /// Any other type flag, such as devices and FIFOs.
    Other(u8),
}

/// One member of an archive.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    header: &'a [u8],
    /// The member's data.
    pub data: &'a [u8],
    /// Where the data starts in the archive.
    pub offset: usize,
}

/// The NUL-terminated text at the start of `field`.
fn text(field: &[u8]) -> &[u8] {
    let len = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..len]
}

/// The octal number in `field`, after any leading spaces.
fn octal(field: &[u8]) -> Option<u64> {
    let digits = field.iter().skip_while(|&&b| b == b' ');
    let mut digits = digits.take_while(|b| (b'0'..=b'7').contains(b)).peekable();
    digits.peek()?;
    digits.try_fold(0u64, |v, &d| {
        v.checked_mul(8)?.checked_add(u64::from(d - b'0'))
    })
}

impl<'a> Entry<'a> {
    pub fn kind(&self) -> Kind {
        match self.header[156] {
            b'0' | 0 | b'7' => Kind::File,
            b'1' => Kind::HardLink,
            b'2' => Kind::Symlink,
            b'5' => Kind::Directory,
            other => Kind::Other(other),
        }
    }

    /// The prefix field, which comes before the name with a `/`.
    pub fn prefix(&self) -> &'a [u8] {
        text(&self.header[345..500])
    }

    /// The name field.
    pub fn name(&self) -> &'a [u8] {
        text(&self.header[..100])
    }

    /// The components of the member's path, leaving out empty and `.`
    /// components, so that `./usr//bin/` gives `usr` and `bin`.
    pub fn components(&self) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        let (prefix, name) = (self.prefix(), self.name());
        prefix
            .split(|&b| b == b'/')
            .chain(name.split(|&b| b == b'/'))
            .filter(|c| !c.is_empty() && *c != b".")
    }

    /// The target of a link.
    pub fn link_name(&self) -> &'a [u8] {
        text(&self.header[157..257])
    }

    /// The permission bits.
    pub fn mode(&self) -> u32 {
        octal(&self.header[100..108]).map_or(0, |m| (m & 0o7777) as u32)
    }

    pub fn uid(&self) -> u32 {
        octal(&self.header[108..116]).map_or(0, |u| u as u32)
    }

    /// The modification time, in seconds since the Unix epoch.
    pub fn mtime(&self) -> u64 {
        octal(&self.header[136..148]).unwrap_or(0)
    }
}

/// The members of an archive, in order.
pub struct Entries<'a> {
    image: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Entry<'a>> {
        let header = self.image.get(self.at..self.at + BLOCK)?;
        if header.iter().all(|&b| b == 0) {
            return None;
        }
        // Links and directories have no data, whatever their size says.
        let len = match header[156] {
            b'1' | b'2' | b'5' => 0,
            _ => usize::try_from(octal(&header[124..136])?).ok()?,
        };
        let offset = self.at + BLOCK;
        let data = self.image.get(offset..offset.checked_add(len)?)?;
        self.at = offset + len.next_multiple_of(BLOCK);
        Some(Entry {
            header,
            data,
            offset,
        })
    }
}

/// The members of `image`. Iteration ends at the end-of-archive block,
/// or at the first member that does not fit the image.
pub fn entries(image: &[u8]) -> Entries<'_> {
    Entries { image, at: 0 }
}

/// The data of the first regular member of `image` with no prefix named
/// `name`.
pub fn find<'a>(image: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    entries(image)
        .find(|e| e.kind() == Kind::File && e.prefix().is_empty() && e.name() == name)
        .map(|e| e.data)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// A header for `name` with `flag`, `data` and `link`.
    fn member(prefix: &str, name: &str, flag: u8, data: &[u8], link: &str) -> Vec<u8> {
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000012\0");
        h[124..136].copy_from_slice(std::format!("{:011o}\0", data.len()).as_bytes());
        h[136..148].copy_from_slice(b"00000000144\0");
        h[156] = flag;
        h[157..157 + link.len()].copy_from_slice(link.as_bytes());
        h[257..263].copy_from_slice(b"ustar\0");
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
        out
    }

    fn archive(members: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = members.concat();
        out.extend_from_slice(&[0; 2 * BLOCK]);
        out
    }

    #[test]
    fn lists_members_with_their_fields() {
        let image = archive(&[
            member("", "./usr/bin/", b'5', b"", ""),
            member("usr/lib", "libc.so", b'0', b"code", ""),
            member("", "usr/bin/sh", b'2', b"", "../../bin/sh"),
            member("", "usr/bin/ash", b'1', b"", "usr/lib/libc.so"),
        ]);
        let all: Vec<Entry> = entries(&image).collect();
        assert_eq!(all.len(), 4);
        let kinds: Vec<Kind> = all.iter().map(Entry::kind).collect();
        assert_eq!(
            kinds,
            [Kind::Directory, Kind::File, Kind::Symlink, Kind::HardLink]
        );
        let path = |e: &Entry| e.components().collect::<Vec<_>>().join(&b'/');
        assert_eq!(path(&all[0]), b"usr/bin");
        assert_eq!(path(&all[1]), b"usr/lib/libc.so");
        assert_eq!(all[1].data, b"code");
        assert_eq!(&image[all[1].offset..all[1].offset + 4], b"code");
        assert_eq!(all[2].link_name(), b"../../bin/sh");
        assert_eq!(
            (all[1].mode(), all[1].uid(), all[1].mtime()),
            (0o644, 10, 100)
        );
    }

    #[test]
    fn finds_regular_members_without_a_prefix() {
        let image = archive(&[
            member("a", "init", b'0', b"prefixed", ""),
            member("", "init", b'5', b"", ""),
            member("", "init", b'0', b"found", ""),
        ]);
        assert_eq!(find(&image, b"init"), Some(&b"found"[..]));
        assert_eq!(find(&image, b"other"), None);
    }

    #[test]
    fn stops_at_a_member_past_the_end() {
        let mut image = member("", "big", b'0', &[1; 600], "");
        image.truncate(BLOCK + 100);
        assert_eq!(entries(&image).count(), 0);
    }
}
