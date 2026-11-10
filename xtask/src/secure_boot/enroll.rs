//! The files on a signed image for enrolling its key in the firmware.

use super::authenticode;
use super::key::Key;
use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Where the enrollment files are on the EFI system partition.
pub const DIR: &str = "EFI/Mikros/keys";

pub const GLOBAL_VARIABLE: Uuid = Uuid::from_u128(0x8be4df61_93ca_11d2_aa0d_00e098032b8c);
pub const IMAGE_SECURITY_DATABASE: Uuid = Uuid::from_u128(0xd719b2cb_3d3a_4596_a3bc_dad00e67656f);
const CERT_X509: Uuid = Uuid::from_u128(0xa5c059a1_94e4_4aa7_87b5_ab155c2bf072);
const CERT_TYPE_PKCS7: Uuid = Uuid::from_u128(0x4aafd29d_68df_49ee_8aa9_347d375665a7);
/// The owner that the signature lists name for the certificate.
pub const OWNER: Uuid = Uuid::from_u128(0x6d696b72_6f73_4d4b_a5ee_6b6579730001);
const PKCS7_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 1];

/// Non-volatile, boot service and runtime access, time-based
/// authenticated writes.
pub const ATTRIBUTES: u32 = 0x27;
const WIN_CERT_TYPE_EFI_GUID: u16 = 0x0ef1;

/// The variables that hold the certificate: the platform key, the key
/// exchange key and the signature database.
pub const VARIABLES: [(&str, Uuid); 3] = [
    ("PK", GLOBAL_VARIABLE),
    ("KEK", GLOBAL_VARIABLE),
    ("db", IMAGE_SECURITY_DATABASE),
];

/// An EFI_SIGNATURE_LIST holding the X.509 certificate `cert`.
pub fn signature_list(cert: &[u8]) -> Vec<u8> {
    let signature_size = 16 + cert.len();
    let mut list = CERT_X509.to_bytes_le().to_vec();
    list.extend_from_slice(&((28 + signature_size) as u32).to_le_bytes());
    list.extend_from_slice(&0u32.to_le_bytes());
    list.extend_from_slice(&(signature_size as u32).to_le_bytes());
    list.extend_from_slice(&OWNER.to_bytes_le());
    list.extend_from_slice(cert);
    list
}

/// The EFI_TIME of `unix` seconds after 1970 in UTC.
pub fn efi_time(unix: u64) -> [u8; 16] {
    let (year, month, day) = super::der::date(unix / 86400);
    let secs = unix % 86400;
    let mut time = [0u8; 16];
    time[..2].copy_from_slice(&(year as u16).to_le_bytes());
    time[2] = month as u8;
    time[3] = day as u8;
    time[4] = (secs / 3600) as u8;
    time[5] = (secs / 60 % 60) as u8;
    time[6] = (secs % 60) as u8;
    time
}

/// The payload for setting the variable `name` of `vendor` to `data` with
/// a time-based authenticated write at `time`: an
/// EFI_VARIABLE_AUTHENTICATION_2 followed by `data`.
fn authenticated(
    key: &Key,
    name: &str,
    vendor: Uuid,
    time: [u8; 16],
    data: &[u8],
) -> Result<Vec<u8>> {
    let mut signed: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    signed.extend_from_slice(&vendor.to_bytes_le());
    signed.extend_from_slice(&ATTRIBUTES.to_le_bytes());
    signed.extend_from_slice(&time);
    signed.extend_from_slice(data);
    let pkcs7 = authenticode::signed_data(key, PKCS7_DATA, None, None, &signed)?;

    let mut out = time.to_vec();
    out.extend_from_slice(&((24 + pkcs7.len()) as u32).to_le_bytes());
    out.extend_from_slice(&0x0200u16.to_le_bytes());
    out.extend_from_slice(&WIN_CERT_TYPE_EFI_GUID.to_le_bytes());
    out.extend_from_slice(&CERT_TYPE_PKCS7.to_bytes_le());
    out.extend_from_slice(&pkcs7);
    out.extend_from_slice(data);
    Ok(out)
}

/// The enrollment files for `key`, as paths on the EFI system partition
/// and their contents: the certificate in DER, and the authenticated
/// variables that set it as PK, KEK and in db.
pub fn files(key: &Key) -> Result<Vec<(String, Vec<u8>)>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let list = signature_list(&key.cert);
    let mut files = vec![(format!("{DIR}/mikros.cer"), key.cert.clone())];
    for (name, vendor) in VARIABLES {
        let auth = authenticated(key, name, vendor, efi_time(now), &list)?;
        files.push((format!("{DIR}/{name}.auth"), auth));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_boot::der;
    use crate::secure_boot::key::{load_or_create, tests::TempDir};

    #[test]
    fn writes_the_time_as_efi_time() {
        // 2026-11-10 20:44:05
        let time = efi_time(1_794_343_445);
        assert_eq!(time[..7], [0xea, 0x07, 11, 10, 20, 44, 5]);
        assert!(time[7..].iter().all(|&b| b == 0));
    }

    #[test]
    fn signature_list_holds_the_certificate() {
        let list = signature_list(b"cert");
        assert_eq!(list.len(), 28 + 16 + 4);
        assert_eq!(&list[16..20], &48u32.to_le_bytes());
        assert_eq!(&list[24..28], &20u32.to_le_bytes());
        assert!(list.ends_with(b"cert"));
    }

    #[test]
    fn authenticated_variables_end_in_the_signature_list() {
        let dir = TempDir::new("enroll");
        let key = load_or_create(&dir.0).unwrap();
        let files = files(&key).unwrap();
        let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "EFI/Mikros/keys/mikros.cer",
                "EFI/Mikros/keys/PK.auth",
                "EFI/Mikros/keys/KEK.auth",
                "EFI/Mikros/keys/db.auth",
            ]
        );
        assert_eq!(files[0].1, key.cert);
        let list = signature_list(&key.cert);
        for (_, auth) in &files[1..] {
            let length = u32::from_le_bytes(auth[16..20].try_into().unwrap()) as usize;
            assert_eq!(&auth[20..24], [0, 2, 0xf1, 0x0e]);
            assert_eq!(&auth[24..40], CERT_TYPE_PKCS7.to_bytes_le());
            let pkcs7 = der::Reader(&auth[40..16 + length])
                .expect(der::SEQUENCE)
                .unwrap();
            assert_eq!(pkcs7.raw.len(), length - 24);
            assert_eq!(&auth[16 + length..], list);
        }
    }
}
