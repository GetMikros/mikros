//! Finding members of the boot image, a ustar archive.

#![no_std]

const BLOCK: usize = 512;

/// The size field of a header.
fn size(header: &[u8]) -> Option<usize> {
    let field = &header[124..136];
    let mut digits = field.iter().take_while(|b| (b'0'..=b'7').contains(b));
    digits.try_fold(0usize, |v, &d| {
        v.checked_mul(8)?.checked_add(usize::from(d - b'0'))
    })
}

/// The data of the first regular member of `image` named `name`. The
/// kernel has checked the archive before it started init.
pub fn find<'a>(image: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut at = 0;
    while let Some(header) = image.get(at..at + BLOCK) {
        if header.iter().all(|&b| b == 0) {
            return None;
        }
        let len = size(header)?;
        let data = image.get(at + BLOCK..at + BLOCK + len)?;
        let name_len = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let regular = matches!(header[156], b'0' | 0);
        if regular && header[345] == 0 && &header[..name_len] == name {
            return Some(data);
        }
        at += BLOCK + len.next_multiple_of(BLOCK);
    }
    None
}
