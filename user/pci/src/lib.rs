//! PCI configuration space, for the PCI bus driver: finding the
//! functions behind a host bridge, their hardware IDs, the sizes of
//! their BARs, and their capabilities.
//!
//! The configuration space is reached through `Config`, which the bus
//! driver implements over its ECAM range and the tests over memory.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

#[cfg(test)]
mod tests;

/// Offsets in the configuration space of a function.
pub const VENDOR_ID: u16 = 0x00;
pub const DEVICE_ID: u16 = 0x02;
pub const COMMAND: u16 = 0x04;
pub const STATUS: u16 = 0x06;
pub const REVISION: u16 = 0x08;
pub const PROG_IF: u16 = 0x09;
pub const SUBCLASS: u16 = 0x0A;
pub const CLASS: u16 = 0x0B;
pub const HEADER_TYPE: u16 = 0x0E;
pub const BAR0: u16 = 0x10;
pub const SUBSYSTEM_VENDOR_ID: u16 = 0x2C;
pub const SUBSYSTEM_ID: u16 = 0x2E;
pub const CAPABILITIES: u16 = 0x34;
pub const INTERRUPT_PIN: u16 = 0x3D;

/// The command register's memory and I/O decoding bits, and its bus
/// mastering bit.
pub const COMMAND_IO: u16 = 1 << 0;
pub const COMMAND_MEMORY: u16 = 1 << 1;
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;
/// The status register's bit for a capability list.
const STATUS_CAPABILITIES: u16 = 1 << 4;
/// The IDs of the MSI and MSI-X capabilities.
pub const CAP_MSI: u8 = 0x05;
pub const CAP_MSIX: u8 = 0x11;
/// The bits of the MSI message control register that make the
/// capability longer: 64-bit addresses and per-vector masking.
const MSI_64: u16 = 1 << 7;
const MSI_MASKING: u16 = 1 << 8;
/// The most capabilities a function's list is followed for.
const CAPABILITIES_MAX: usize = 48;
/// The vendor ID that no function has.
const NO_VENDOR: u16 = 0xFFFF;
/// The header type bit of a device with more than one function, and
/// the type of the rest of the header.
const MULTI_FUNCTION: u8 = 1 << 7;
const HEADER_LAYOUT: u8 = 0x7F;
/// A type 0 header has six BARs.
const BARS: u8 = 6;
/// The low bits of a BAR: the I/O space bit, the memory type, 64 bits
/// among them, and the prefetchable bit.
const BAR_IO: u32 = 1 << 0;
const BAR_TYPE: u32 = 0b11 << 1;
const BAR_64: u32 = 0b10 << 1;
const BAR_PREFETCHABLE: u32 = 1 << 3;
const BAR_IO_FLAGS: u32 = 0b11;
const BAR_MEMORY_FLAGS: u32 = 0b1111;

/// The address of a function: its bus, device and function numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// Access to the configuration space of the functions behind one host
/// bridge, with accesses of 1, 2 or 4 bytes at offsets aligned to them.
pub trait Config {
    fn read(&self, at: Address, offset: u16, width: u8) -> u32;
    fn write(&mut self, at: Address, offset: u16, width: u8, value: u32);
}

/// The offset of `offset` in the configuration space of `at` within an
/// ECAM range that starts at bus `bus_start`: 1 MiB for each bus, 32 KiB
/// for each device and 4 KiB for each function.
pub fn ecam_offset(bus_start: u8, at: Address, offset: u16) -> usize {
    (usize::from(at.bus - bus_start) << 20)
        | (usize::from(at.device) << 15)
        | (usize::from(at.function) << 12)
        | usize::from(offset & 0xFFF)
}

/// What the header of a function says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub revision: u8,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub interrupt_pin: u8,
}

impl Header {
    /// Reads the header of the function at `at`.
    pub fn read(config: &impl Config, at: Address) -> Header {
        let byte = |offset| config.read(at, offset, 1) as u8;
        let word = |offset| config.read(at, offset, 2) as u16;
        Header {
            vendor_id: word(VENDOR_ID),
            device_id: word(DEVICE_ID),
            subsystem_vendor_id: word(SUBSYSTEM_VENDOR_ID),
            subsystem_id: word(SUBSYSTEM_ID),
            revision: byte(REVISION),
            class: byte(CLASS),
            subclass: byte(SUBCLASS),
            prog_if: byte(PROG_IF),
            interrupt_pin: byte(INTERRUPT_PIN),
        }
    }

    /// The function's hardware IDs, from the most specific to the
    /// least, in lowercase hexadecimal.
    pub fn hardware_ids(&self) -> [String; 5] {
        let ids = format!("v{:04x}d{:04x}", self.vendor_id, self.device_id);
        let subsystem = format!(
            "{ids}sv{:04x}sd{:04x}",
            self.subsystem_vendor_id, self.subsystem_id
        );
        let class = format!("c{:02x}{:02x}", self.class, self.subclass);
        [
            format!("pci:{subsystem}r{:02x}", self.revision),
            format!("pci:{subsystem}"),
            format!("pci:{ids}"),
            format!("pci:{class}{:02x}", self.prog_if),
            format!("pci:{class}"),
        ]
    }
}

/// The functions with a type 0 header on the buses from `bus_start` to
/// `bus_end`: every device of each bus, and every function of a device
/// whose function 0 has the multi-function bit set.
pub fn scan(config: &impl Config, bus_start: u8, bus_end: u8) -> Vec<Address> {
    let mut found = Vec::new();
    for bus in bus_start..=bus_end {
        for device in 0..32 {
            let first = Address {
                bus,
                device,
                function: 0,
            };
            if config.read(first, VENDOR_ID, 2) as u16 == NO_VENDOR {
                continue;
            }
            let header_type = config.read(first, HEADER_TYPE, 1) as u8;
            let functions = if header_type & MULTI_FUNCTION != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let at = Address {
                    bus,
                    device,
                    function,
                };
                let present = config.read(at, VENDOR_ID, 2) as u16 != NO_VENDOR;
                let layout = config.read(at, HEADER_TYPE, 1) as u8 & HEADER_LAYOUT;
                if present && layout == 0 {
                    found.push(at);
                }
            }
        }
    }
    found
}

/// The address space a BAR decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    Mmio,
    Io,
}

/// An implemented BAR: the index of its first register, what it
/// decodes, where and how much.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    pub index: u8,
    pub kind: BarKind,
    pub prefetchable: bool,
    pub base: u64,
    pub size: u64,
}

/// The implemented BARs of the function at `at`. Each is sized by
/// writing all ones to it and reading it back while memory and I/O
/// decoding are disabled, and is then restored, as the command register
/// is at the end. A 64-bit BAR takes two registers and counts as one.
pub fn size_bars(config: &mut impl Config, at: Address) -> Vec<Bar> {
    let command = config.read(at, COMMAND, 2);
    let decoding = u32::from(COMMAND_IO | COMMAND_MEMORY);
    config.write(at, COMMAND, 2, command & !decoding);
    let mut bars = Vec::new();
    let mut index = 0;
    while index < BARS {
        let offset = BAR0 + 4 * u16::from(index);
        let original = config.read(at, offset, 4);
        let wide = original & BAR_IO == 0 && original & BAR_TYPE == BAR_64 && index + 1 < BARS;
        config.write(at, offset, 4, u32::MAX);
        let low = config.read(at, offset, 4);
        config.write(at, offset, 4, original);
        let (high_original, high) = if wide {
            let high_original = config.read(at, offset + 4, 4);
            config.write(at, offset + 4, 4, u32::MAX);
            let high = config.read(at, offset + 4, 4);
            config.write(at, offset + 4, 4, high_original);
            (high_original, high)
        } else {
            (0, 0)
        };
        let io = original & BAR_IO != 0;
        let flags = if io { BAR_IO_FLAGS } else { BAR_MEMORY_FLAGS };
        let mask = (u64::from(high) << 32) | u64::from(low & !flags);
        // The lowest address bit that stays set is the size.
        let size = mask.isolate_lowest_one();
        if size != 0 {
            bars.push(Bar {
                index,
                kind: if io { BarKind::Io } else { BarKind::Mmio },
                prefetchable: !io && original & BAR_PREFETCHABLE != 0,
                base: (u64::from(high_original) << 32) | u64::from(original & !flags),
                size,
            });
        }
        index += if wide { 2 } else { 1 };
    }
    config.write(at, COMMAND, 2, command);
    bars
}

/// The capabilities of the function at `at`, by ID and offset, in the
/// order of its list. A list that loops, or points below the header, is
/// followed no further.
pub fn capabilities(config: &impl Config, at: Address) -> Vec<(u8, u16)> {
    let mut found = Vec::new();
    if config.read(at, STATUS, 2) as u16 & STATUS_CAPABILITIES == 0 {
        return found;
    }
    let mut next = config.read(at, CAPABILITIES, 1) as u16 & 0xFC;
    while next >= 0x40 && found.len() < CAPABILITIES_MAX {
        let id = config.read(at, next, 1) as u8;
        if found.iter().any(|&(_, offset)| offset == next) {
            break;
        }
        found.push((id, next));
        next = config.read(at, next + 1, 1) as u16 & 0xFC;
    }
    found
}

/// The registers of the MSI and MSI-X capabilities of the function at
/// `at`, which only the bus driver writes.
pub fn msi_registers(config: &impl Config, at: Address) -> Vec<Range<u16>> {
    let mut ranges = Vec::new();
    for (id, offset) in capabilities(config, at) {
        let len = match id {
            CAP_MSI => {
                let control = config.read(at, offset + 2, 2) as u16;
                let address = if control & MSI_64 != 0 { 14 } else { 10 };
                address + if control & MSI_MASKING != 0 { 10 } else { 0 }
            }
            CAP_MSIX => 12,
            _ => continue,
        };
        ranges.push(offset..offset + len);
    }
    ranges
}
