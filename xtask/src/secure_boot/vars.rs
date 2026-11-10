//! Enrolling a certificate in an OVMF variable store, so that a virtual
//! machine starts with Secure Boot enabled and that certificate as its
//! platform key, key exchange key and only `db` entry.

use super::enroll::{self, ATTRIBUTES, VARIABLES};
use anyhow::{Result, bail};
use uuid::Uuid;

/// The signature of a store of authenticated variables.
const AUTHENTICATED_STORE: Uuid = Uuid::from_u128(0xaaf32c78_947b_439a_a180_2e144ec37792);
const SECURE_BOOT_ENABLE: Uuid = Uuid::from_u128(0xf0a30bc7_af08_4556_99c4_001009c93a44);
/// The size of the store header after the firmware volume header.
const STORE_HEADER: usize = 28;
const START_ID: u16 = 0x55aa;
/// The state of a variable that has been added in full.
const VAR_ADDED: u8 = 0x3f;
/// Non-volatile and boot service access.
const NV_BS: u32 = 0x3;

/// A variable as an AUTHENTICATED_VARIABLE_HEADER followed by its name and
/// data, padded to 4 bytes.
fn variable(name: &str, vendor: Uuid, attributes: u32, time: [u8; 16], data: &[u8]) -> Vec<u8> {
    let name: Vec<u8> = name
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut out = START_ID.to_le_bytes().to_vec();
    out.extend_from_slice(&[VAR_ADDED, 0]);
    out.extend_from_slice(&attributes.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&time);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&vendor.to_bytes_le());
    out.extend_from_slice(&name);
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(4), 0xff);
    out
}

/// The variable store `template`, which must hold no variables, with
/// `cert` enrolled as PK, KEK and db and Secure Boot enabled, at the time
/// `unix` seconds after 1970.
pub fn enroll(template: &[u8], cert: &[u8], unix: u64) -> Result<Vec<u8>> {
    let mut store = template.to_vec();
    if store.get(40..44) != Some(b"_FVH") {
        bail!("the variable store has no firmware volume header");
    }
    let header = usize::from(u16::from_le_bytes([store[48], store[49]]));
    if store.get(header..header + 16) != Some(&AUTHENTICATED_STORE.to_bytes_le()[..]) {
        bail!("the variable store does not hold authenticated variables");
    }
    let start = header + STORE_HEADER;
    if store.get(start..start + 2) != Some(&[0xff, 0xff]) {
        bail!("the variable store holds variables already");
    }
    let time = enroll::efi_time(unix);
    let list = enroll::signature_list(cert);
    let mut variables = Vec::new();
    for (name, vendor) in VARIABLES {
        variables.extend(variable(name, vendor, ATTRIBUTES, time, &list));
    }
    variables.extend(variable(
        "SecureBootEnable",
        SECURE_BOOT_ENABLE,
        NV_BS,
        [0; 16],
        &[1],
    ));
    let end = start + variables.len();
    let size = u32::from_le_bytes(store[header + 16..header + 20].try_into()?) as usize;
    if end > header + size {
        bail!("the variable store is too small for the certificate");
    }
    store[start..end].copy_from_slice(&variables);
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> Vec<u8> {
        let mut store = vec![0xff; 0x2000];
        store[40..44].copy_from_slice(b"_FVH");
        store[48..50].copy_from_slice(&72u16.to_le_bytes());
        store[72..88].copy_from_slice(&AUTHENTICATED_STORE.to_bytes_le());
        store[88..92].copy_from_slice(&0x1000u32.to_le_bytes());
        store
    }

    #[test]
    fn appends_the_keys_and_enables_secure_boot() {
        let store = enroll(&template(), b"certificate", 0).unwrap();
        let mut at = 100;
        let mut names = Vec::new();
        while store[at..at + 2] == START_ID.to_le_bytes() {
            assert_eq!(store[at + 2], VAR_ADDED);
            let size = |o: usize| {
                u32::from_le_bytes(store[at + o..at + o + 4].try_into().unwrap()) as usize
            };
            let (name_size, data_size) = (size(36), size(40));
            let name: Vec<u16> = store[at + 60..at + 60 + name_size - 2]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            names.push(String::from_utf16(&name).unwrap());
            let data = &store[at + 60 + name_size..][..data_size];
            if names.len() <= 3 {
                assert_eq!(size(4), 0x27);
                assert!(data.ends_with(b"certificate"));
            } else {
                assert_eq!(data, [1]);
            }
            at = (at + 60 + name_size + data_size).next_multiple_of(4);
        }
        assert_eq!(names, ["PK", "KEK", "db", "SecureBootEnable"]);
        assert!(store[at..].iter().all(|&b| b == 0xff));
    }

    #[test]
    fn refuses_stores_that_hold_variables() {
        let mut store = template();
        store[100..102].copy_from_slice(&START_ID.to_le_bytes());
        assert!(enroll(&store, b"certificate", 0).is_err());
    }
}
