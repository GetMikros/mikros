//! ATA commands for the AHCI driver: what IDENTIFY DEVICE's data says
//! about a disk, and the commands that read and write its sectors.
//!
//! Nothing here touches hardware, so that the host tests can check it.

#![no_std]

extern crate alloc;

use alloc::string::String;

#[cfg(test)]
mod tests;
pub mod timeouts;

/// ATA commands.
pub const READ_DMA_EXT: u8 = 0x25;
pub const WRITE_DMA_EXT: u8 = 0x35;
pub const WRITE_DMA_FUA_EXT: u8 = 0x3D;
pub const READ_FPDMA_QUEUED: u8 = 0x60;
pub const WRITE_FPDMA_QUEUED: u8 = 0x61;
pub const FLUSH_CACHE_EXT: u8 = 0xEA;
pub const IDENTIFY_DEVICE: u8 = 0xEC;
pub const DATA_SET_MANAGEMENT: u8 = 0x06;
/// The LBA mode bit of the device register, and the bit of WRITE FPDMA
/// QUEUED's device register that forces unit access.
pub const DEVICE_LBA: u8 = 1 << 6;
pub const FPDMA_FUA: u8 = 1 << 7;
/// The bit of IDENTIFY DEVICE's words 84 and 87 that reports WRITE DMA
/// FUA EXT.
const FUA_SUPPORTED: u16 = 1 << 6;

/// An ATA command for a host to device register FIS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ata {
    pub command: u8,
    pub features: u16,
    pub lba: u64,
    pub count: u16,
    pub device: u8,
}

/// What IDENTIFY DEVICE says about a disk.
#[derive(Debug, PartialEq, Eq)]
pub struct Identity {
    pub model: String,
    pub serial: String,
    pub lba48: bool,
    pub sectors: u64,
    pub sector_size: u32,
    /// Native command queuing, and its queue depth.
    pub ncq: bool,
    pub queue_depth: u32,
    pub trim: bool,
    pub write_cache: bool,
    /// Forced unit access writes.
    pub fua: bool,
}

impl Identity {
    /// What the IDENTIFY DEVICE words `w` say, or `None` when the logical
    /// sector size is not a power of two of at least 512.
    pub fn parse(w: &[u16; 256]) -> Option<Identity> {
        let sector_size = sector_size(w)?;
        let sectors = (0..4).fold(0u64, |n, i| n | u64::from(w[100 + i]) << (16 * i));
        Some(Identity {
            model: text(&w[27..47]),
            serial: text(&w[10..20]),
            lba48: w[83] & 1 << 10 != 0,
            sectors,
            sector_size,
            ncq: w[76] & 1 << 8 != 0,
            queue_depth: u32::from(w[75] & 0x1F) + 1,
            trim: w[169] & 1 != 0,
            write_cache: w[85] & 1 << 5 != 0,
            fua: (w[84] | w[87]) & FUA_SUPPORTED != 0,
        })
    }
}

/// The logical sector size in bytes that the IDENTIFY DEVICE words `w`
/// give: words 117 and 118, in words of two bytes, when word 106 says
/// that logical sectors are longer than 256 words, and otherwise 512.
/// `None` when it is not a power of two of at least 512, which the disk's
/// block size must be.
fn sector_size(w: &[u16; 256]) -> Option<u32> {
    let size = if w[106] & 0xC000 == 0x4000 && w[106] & 1 << 12 != 0 {
        (u32::from(w[117]) | u32::from(w[118]) << 16).checked_mul(2)?
    } else {
        512
    };
    (size >= 512 && size.is_power_of_two()).then_some(size)
}

/// An identify string: ASCII with the bytes of each word swapped, padded
/// with spaces.
fn text(words: &[u16]) -> String {
    let text: String = words
        .iter()
        .flat_map(|w| [(w >> 8) as u8, *w as u8])
        .map(|b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                ' '
            }
        })
        .collect();
    String::from(text.trim())
}

/// The command that reads, or writes when `write` is true, `count`
/// sectors from `block` (0 for 65536), queued with native command queuing
/// when `queued` is true, and whether a flush must follow it. A write
/// with `fua` forces unit access when the disk supports it, as
/// `identity_fua` says, with WRITE FPDMA QUEUED's FUA bit or WRITE DMA
/// FUA EXT, and is followed by a flush otherwise.
pub fn read_write(
    queued: bool,
    write: bool,
    fua: bool,
    identity_fua: bool,
    block: u64,
    count: u16,
) -> (Ata, bool) {
    let forced = write && fua && identity_fua;
    let ata = if queued {
        Ata {
            command: if write {
                WRITE_FPDMA_QUEUED
            } else {
                READ_FPDMA_QUEUED
            },
            features: count,
            lba: block,
            device: if forced {
                DEVICE_LBA | FPDMA_FUA
            } else {
                DEVICE_LBA
            },
            ..Ata::default()
        }
    } else {
        Ata {
            command: match (write, forced) {
                (true, true) => WRITE_DMA_FUA_EXT,
                (true, false) => WRITE_DMA_EXT,
                (false, _) => READ_DMA_EXT,
            },
            lba: block,
            count,
            device: DEVICE_LBA,
            ..Ata::default()
        }
    };
    (ata, write && fua && !forced)
}
