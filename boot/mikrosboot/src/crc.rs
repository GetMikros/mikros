//! The CRC-32 checksums of GPT and btrfs.

/// The reflected polynomial of CRC-32, which GPT uses.
const IEEE: u32 = 0xEDB8_8320;
/// The reflected polynomial of CRC-32C, which btrfs uses.
const CASTAGNOLI: u32 = 0x82F6_3B78;

const fn table(poly: u32) -> [u32; 256] {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static IEEE_TABLE: [u32; 256] = table(IEEE);
static CASTAGNOLI_TABLE: [u32; 256] = table(CASTAGNOLI);

fn update(table: &[u32; 256], mut state: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        state = table[((state ^ u32::from(b)) & 0xff) as usize] ^ (state >> 8);
    }
    state
}

/// The CRC-32 of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    !update(&IEEE_TABLE, !0, bytes)
}

/// The CRC-32C of `bytes`.
pub fn crc32c(bytes: &[u8]) -> u32 {
    !update(&CASTAGNOLI_TABLE, !0, bytes)
}

/// CRC-32C register arithmetic from `seed` over `bytes`, without the
/// final inversion, as btrfs hashes directory entry names.
pub fn crc32c_raw(seed: u32, bytes: &[u8]) -> u32 {
    update(&CASTAGNOLI_TABLE, seed, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_check_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn raw_form_is_the_uninverted_register() {
        assert_eq!(!crc32c_raw(!0, b"123456789"), crc32c(b"123456789"));
    }
}
