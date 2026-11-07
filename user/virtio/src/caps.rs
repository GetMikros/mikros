//! Finding a device's configuration structures through its virtio
//! vendor-specific PCI capabilities, and mapping them.

use crate::Error;
use crate::mmio::Mmio;
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::sys;

/// The PCI status register's capability list bit, and the list pointer.
const STATUS: u16 = 0x06;
const STATUS_CAPABILITIES: u32 = 1 << 4;
const CAPABILITIES: u16 = 0x34;
const CAPABILITIES_MAX: usize = 48;
/// The vendor-specific capability ID, and the virtio structure types.
const CAP_VENDOR: u32 = 0x09;
const COMMON: u8 = 1;
const NOTIFY: u8 = 2;
const ISR: u8 = 3;
const DEVICE: u8 = 4;
const SHARED_MEMORY: u8 = 8;

/// A shared memory region of the device: its ID, and where it lies in
/// which BAR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedMemory {
    pub id: u8,
    pub bar: u8,
    pub offset: u64,
    pub length: u64,
}

/// Where one structure lies: its BAR, offset and length.
#[derive(Clone, Copy)]
struct Place {
    bar: u8,
    offset: u64,
    length: u64,
}

/// The mapped structures of a device, the notification offset
/// multiplier, and the shared memory regions it describes.
pub struct Structures {
    pub common: Mmio,
    pub notify: Mmio,
    pub notify_multiplier: u32,
    pub isr: Mmio,
    pub device: Mmio,
    pub shared_memory: Vec<SharedMemory>,
}

async fn read(device: &DeviceAsyncClient, offset: u16, width: u8) -> Result<u32, Error> {
    match device.read_config(offset, width).await {
        Ok(Ok(read)) => Ok(read.value),
        Ok(Err(status)) => Err(Error::Bus(status)),
        Err(_) => Err(Error::Channel),
    }
}

/// Finds the common configuration, notification, ISR status and device
/// configuration structures and the shared memory regions of `device`,
/// using the first capability of each type, and maps the four
/// structures into `process`. Fails when one of them is missing.
pub async fn find(device: &DeviceAsyncClient, process: sys::Handle) -> Result<Structures, Error> {
    let mut places: [Option<Place>; 5] = [None; 5];
    let mut multiplier = 0;
    let mut shared_memory = Vec::new();
    if read(device, STATUS, 2).await? & STATUS_CAPABILITIES != 0 {
        let mut next = read(device, CAPABILITIES, 1).await? as u16 & 0xFC;
        let mut seen = 0;
        while next >= 0x40 && seen < CAPABILITIES_MAX {
            seen += 1;
            let at = next;
            next = read(device, at + 1, 1).await? as u16 & 0xFC;
            if read(device, at, 1).await? != CAP_VENDOR {
                continue;
            }
            let kind = read(device, at + 3, 1).await? as u8;
            let bar = read(device, at + 4, 1).await? as u8;
            let offset = u64::from(read(device, at + 8, 4).await?);
            let length = u64::from(read(device, at + 12, 4).await?);
            match kind {
                SHARED_MEMORY => {
                    let high_offset = u64::from(read(device, at + 16, 4).await?);
                    let high_length = u64::from(read(device, at + 20, 4).await?);
                    shared_memory.push(SharedMemory {
                        id: read(device, at + 5, 1).await? as u8,
                        bar,
                        offset: high_offset << 32 | offset,
                        length: high_length << 32 | length,
                    });
                }
                COMMON | NOTIFY | ISR | DEVICE
                    if places[usize::from(kind)].is_none() && bar < 6 =>
                {
                    if kind == NOTIFY {
                        multiplier = read(device, at + 16, 4).await?;
                    }
                    places[usize::from(kind)] = Some(Place {
                        bar,
                        offset,
                        length,
                    });
                }
                _ => {}
            }
        }
    }
    let mut mapped = Vec::new();
    for kind in [COMMON, NOTIFY, ISR, DEVICE] {
        let place = places[usize::from(kind)].ok_or(Error::MissingStructure(kind))?;
        mapped.push(map(device, process, place).await?);
    }
    let mut mapped = mapped.into_iter();
    let mut take = || mapped.next().expect("four structures");
    Ok(Structures {
        common: take(),
        notify: take(),
        notify_multiplier: multiplier,
        isr: take(),
        device: take(),
        shared_memory,
    })
}

/// Maps the structure at `place` from the resource of its BAR.
async fn map(
    device: &DeviceAsyncClient,
    process: sys::Handle,
    place: Place,
) -> Result<Mmio, Error> {
    let bar = match device.get_bar(place.bar).await {
        Ok(Ok(got)) => got.bar,
        Ok(Err(status)) => return Err(Error::Bus(status)),
        Err(_) => return Err(Error::Channel),
    };
    Mmio::map(
        process,
        &bar.resource,
        bar.base + place.offset,
        place.length,
    )
}
