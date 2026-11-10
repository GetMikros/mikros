//! Authenticode signatures of PE images, which UEFI firmware checks
//! against its `db` before it starts an image under Secure Boot.

use super::der;
use super::key::{self, Key};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

const SIGNED_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 2];
const SPC_INDIRECT_DATA: &[u64] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 4];
const SPC_PE_IMAGE_DATA: &[u64] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 15];
const SPC_SP_OPUS_INFO: &[u64] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 12];
const CONTENT_TYPE: &[u64] = &[1, 2, 840, 113549, 1, 9, 3];
const MESSAGE_DIGEST: &[u64] = &[1, 2, 840, 113549, 1, 9, 4];
/// The index of the certificate table among the data directories.
const CERT_TABLE: usize = 4;
const WIN_CERT_REVISION_2_0: u16 = 0x0200;
const WIN_CERT_TYPE_PKCS_SIGNED_DATA: u16 = 0x0002;

/// Where the parts of a PE image that the signature leaves out are.
struct Layout {
    checksum: usize,
    cert_entry: usize,
    headers: usize,
    /// The file ranges of the sections, ordered by their offsets.
    sections: Vec<(usize, usize)>,
}

fn u16_at(image: &[u8], at: usize) -> Result<u16> {
    match image.get(at..at + 2) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => bail!("the PE image ends early"),
    }
}

fn u32_at(image: &[u8], at: usize) -> Result<usize> {
    match image.get(at..at + 4) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize),
        None => bail!("the PE image ends early"),
    }
}

fn layout(image: &[u8]) -> Result<Layout> {
    if image.get(..2) != Some(b"MZ") {
        bail!("not a PE image");
    }
    let pe = u32_at(image, 0x3c)?;
    if image.get(pe..pe + 4) != Some(b"PE\0\0") {
        bail!("not a PE image");
    }
    let coff = pe + 4;
    let sections = usize::from(u16_at(image, coff + 2)?);
    let optional = coff + 20;
    let optional_size = usize::from(u16_at(image, coff + 16)?);
    let (directories, count) = match u16_at(image, optional)? {
        0x10b => (optional + 96, u32_at(image, optional + 92)?),
        0x20b => (optional + 112, u32_at(image, optional + 108)?),
        magic => bail!("unknown PE optional header magic {magic:#x}"),
    };
    if count <= CERT_TABLE {
        bail!("the PE image has no certificate table entry");
    }
    let table = optional + optional_size;
    let mut ranges = Vec::new();
    for i in 0..sections {
        let header = table + i * 40;
        let size = u32_at(image, header + 16)?;
        let offset = u32_at(image, header + 20)?;
        if size != 0 {
            if offset + size > image.len() {
                bail!("a section of the PE image lies past its end");
            }
            ranges.push((offset, offset + size));
        }
    }
    ranges.sort();
    Ok(Layout {
        checksum: optional + 64,
        cert_entry: directories + CERT_TABLE * 8,
        headers: u32_at(image, optional + 60)?,
        sections: ranges,
    })
}

/// The Authenticode digest of the unsigned `image`.
fn digest(image: &[u8], layout: &Layout) -> Result<[u8; 32]> {
    if layout.headers > image.len() || layout.cert_entry + 8 > layout.headers {
        bail!("the PE headers are malformed");
    }
    let mut hash = Sha256::new();
    hash.update(&image[..layout.checksum]);
    hash.update(&image[layout.checksum + 4..layout.cert_entry]);
    hash.update(&image[layout.cert_entry + 8..layout.headers]);
    let mut hashed = layout.headers;
    for &(start, end) in &layout.sections {
        hash.update(&image[start..end]);
        hashed += end - start;
    }
    if image.len() > hashed {
        hash.update(&image[hashed..]);
    }
    Ok(hash.finalize().into())
}

/// The PKCS #7 SignedData of `content` of the type `content_type`, signed
/// by `key` with the authenticated attributes `attributes`, as a
/// ContentInfo. Without attributes, the signature covers `signed` alone.
pub fn signed_data(
    key: &Key,
    content_type: &[u64],
    content: Option<&[u8]>,
    attributes: Option<&[&[u8]]>,
    signed: &[u8],
) -> Result<Vec<u8>> {
    let sha256 = key::algorithm(key::SHA256);
    let (attributes, signature) = match attributes {
        Some(attributes) => {
            let set = der::set_of(attributes);
            let signature = key.sign(&set)?;
            // In the SignerInfo, the SET OF is tagged [0] IMPLICIT.
            let mut tagged = set;
            tagged[0] = 0xa0;
            (tagged, signature)
        }
        None => (Vec::new(), key.sign(signed)?),
    };
    let signer = der::seq(&[
        &der::small_uint(1),
        &der::seq(&[&key.issuer, &key.serial]),
        &sha256,
        &attributes,
        &key::algorithm(key::RSA_ENCRYPTION),
        &der::octets(&signature),
    ]);
    let content = match content {
        Some(content) => der::context(0, &[content]),
        None => Vec::new(),
    };
    let signed_data = der::seq(&[
        &der::small_uint(1),
        &der::set_of(&[&sha256]),
        &der::seq(&[&der::oid(content_type), &content]),
        &der::context(0, &[&key.cert]),
        &der::set_of(&[&signer]),
    ]);
    Ok(der::seq(&[
        &der::oid(SIGNED_DATA),
        &der::context(0, &[&signed_data]),
    ]))
}

/// An authenticated attribute of the type `arcs` with the one value
/// `value`.
fn attribute(arcs: &[u64], value: &[u8]) -> Vec<u8> {
    der::seq(&[&der::oid(arcs), &der::set_of(&[value])])
}

/// The PE image `image`, signed by `key` with an Authenticode signature
/// using SHA-256.
pub fn sign(image: &[u8], key: &Key) -> Result<Vec<u8>> {
    let mut image = image.to_vec();
    let layout = layout(&image)?;
    if u32_at(&image, layout.cert_entry + 4)? != 0 {
        bail!("the PE image is signed already");
    }
    // The certificate table starts on an 8-byte boundary; the padding
    // before it is part of the digest.
    image.resize(image.len().next_multiple_of(8), 0);
    let digest = digest(&image, &layout)?;

    // SpcPeImageData with no flags and the file link "<<<Obsolete>>>".
    let obsolete: Vec<u8> = "<<<Obsolete>>>"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    let link = der::context(0, &[&der::context(2, &[&der::tlv(0x80, &obsolete)])]);
    let pe_image_data = der::seq(&[&der::tlv(0x03, &[0]), &link]);
    let indirect = der::seq(&[
        &der::seq(&[&der::oid(SPC_PE_IMAGE_DATA), &pe_image_data]),
        &der::seq(&[&key::algorithm(key::SHA256), &der::octets(&digest)]),
    ]);
    // The message digest covers the content's value, without its tag and
    // length.
    let value = der::Reader(&indirect).expect(der::SEQUENCE)?.content;
    let attributes = [
        attribute(CONTENT_TYPE, &der::oid(SPC_INDIRECT_DATA)),
        attribute(MESSAGE_DIGEST, &der::octets(&Sha256::digest(value))),
        attribute(SPC_SP_OPUS_INFO, &der::seq(&[])),
    ];
    let attributes: Vec<&[u8]> = attributes.iter().map(Vec::as_slice).collect();
    let pkcs7 = signed_data(
        key,
        SPC_INDIRECT_DATA,
        Some(&indirect),
        Some(&attributes),
        value,
    )?;

    let offset = image.len();
    let length = 8 + pkcs7.len();
    image.extend_from_slice(&(length as u32).to_le_bytes());
    image.extend_from_slice(&WIN_CERT_REVISION_2_0.to_le_bytes());
    image.extend_from_slice(&WIN_CERT_TYPE_PKCS_SIGNED_DATA.to_le_bytes());
    image.extend_from_slice(&pkcs7);
    image.resize(image.len().next_multiple_of(8), 0);
    let size = image.len() - offset;
    let entry = layout.cert_entry;
    image[entry..entry + 4].copy_from_slice(&(offset as u32).to_le_bytes());
    image[entry + 4..entry + 8].copy_from_slice(&(size as u32).to_le_bytes());
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_boot::key::{load_or_create, tests::TempDir};

    /// A PE32+ image with two sections, the second before the first in
    /// the file, and two trailing bytes.
    fn image() -> Vec<u8> {
        let mut image = vec![0u8; 0x400];
        image[..2].copy_from_slice(b"MZ");
        image[0x3c] = 0x40;
        image[0x40..0x44].copy_from_slice(b"PE\0\0");
        let coff = 0x44;
        image[coff + 2] = 2;
        image[coff + 16] = 240;
        let optional = coff + 20;
        image[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        image[optional + 60..optional + 64].copy_from_slice(&0x200u32.to_le_bytes());
        image[optional + 108] = 16;
        let table = optional + 240;
        for (i, offset) in [0x300u32, 0x200].into_iter().enumerate() {
            let header = table + i * 40;
            image[header + 16..header + 20].copy_from_slice(&0x100u32.to_le_bytes());
            image[header + 20..header + 24].copy_from_slice(&offset.to_le_bytes());
        }
        image[0x200..0x400].fill(0x5a);
        image.extend_from_slice(b"xy");
        image
    }

    #[test]
    fn digest_leaves_out_checksum_and_certificate_entry() {
        let original = image();
        let layout = layout(&original).unwrap();
        assert_eq!(layout.sections, [(0x200, 0x300), (0x300, 0x400)]);
        let mut changed = original.clone();
        changed[layout.checksum] = 1;
        changed[layout.cert_entry + 7] = 1;
        assert_eq!(
            digest(&original, &layout).unwrap(),
            digest(&changed, &layout).unwrap()
        );
        changed[layout.cert_entry + 8] = 1;
        assert_ne!(
            digest(&original, &layout).unwrap(),
            digest(&changed, &layout).unwrap()
        );
    }

    #[test]
    fn appends_an_aligned_certificate_table() {
        let dir = TempDir::new("authenticode");
        let key = load_or_create(&dir.0).unwrap();
        let unsigned = image();
        let signed = sign(&unsigned, &key).unwrap();
        let layout = layout(&signed).unwrap();
        let offset = u32_at(&signed, layout.cert_entry).unwrap();
        let size = u32_at(&signed, layout.cert_entry + 4).unwrap();
        assert_eq!(offset, unsigned.len().next_multiple_of(8));
        assert_eq!(offset + size, signed.len());
        assert_eq!(size % 8, 0);
        let length = u32_at(&signed, offset).unwrap();
        assert!(length <= size && length + 8 > size);
        assert_eq!(&signed[offset + 4..offset + 8], [0, 2, 2, 0]);
        der::Reader(&signed[offset + 8..offset + length])
            .expect(der::SEQUENCE)
            .unwrap();
        // The digest of the signed image, as the firmware computes it,
        // is the one in the signature.
        let mut unpadded = signed[..offset].to_vec();
        unpadded[layout.cert_entry..layout.cert_entry + 8].fill(0);
        let digest = digest(&unpadded, &layout).unwrap();
        assert!(signed.windows(32).any(|w| w == digest));
        assert!(sign(&signed, &key).is_err());
    }
}
