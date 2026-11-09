//! The checksum of the up-case table.

/// The checksum of the stored table `bytes`.
pub fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |sum, &b| {
        sum.rotate_right(1).wrapping_add(u32::from(b))
    })
}
