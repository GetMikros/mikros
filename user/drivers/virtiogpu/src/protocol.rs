//! The virtio-gpu commands that the driver sends and the responses it
//! reads, as the little-endian bytes the device exchanges with it.

use alloc::vec::Vec;

/// `VIRTIO_GPU_F_VIRGL`, `VIRTIO_GPU_F_EDID`, `VIRTIO_GPU_F_RESOURCE_BLOB`
/// and `VIRTIO_GPU_F_CONTEXT_INIT`.
pub const F_VIRGL: u64 = 1 << 0;
pub const F_EDID: u64 = 1 << 1;
pub const F_RESOURCE_BLOB: u64 = 1 << 3;
pub const F_CONTEXT_INIT: u64 = 1 << 4;

/// The control and cursor queues.
pub const CONTROL_QUEUE: u16 = 0;
pub const CURSOR_QUEUE: u16 = 1;

/// The device configuration: `events_read`, `events_clear` and
/// `num_scanouts`, and the display event.
pub const CONFIG_EVENTS_READ: u64 = 0;
pub const CONFIG_EVENTS_CLEAR: u64 = 4;
pub const CONFIG_NUM_SCANOUTS: u64 = 8;
pub const EVENT_DISPLAY: u32 = 1 << 0;

const GET_DISPLAY_INFO: u32 = 0x0100;
const RESOURCE_CREATE_2D: u32 = 0x0101;
const RESOURCE_UNREF: u32 = 0x0102;
const SET_SCANOUT: u32 = 0x0103;
const RESOURCE_FLUSH: u32 = 0x0104;
const TRANSFER_TO_HOST_2D: u32 = 0x0105;
const RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESOURCE_DETACH_BACKING: u32 = 0x0107;
const GET_EDID: u32 = 0x010a;
const UPDATE_CURSOR: u32 = 0x0300;
const MOVE_CURSOR: u32 = 0x0301;

pub const RESP_OK_NODATA: u32 = 0x1100;
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const RESP_OK_EDID: u32 = 0x1104;

/// `VIRTIO_GPU_FLAG_FENCE`.
const FLAG_FENCE: u32 = 1 << 0;

/// `VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM` and `VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM`,
/// the formats of `ARGB8888` and `XRGB8888`.
pub const FORMAT_B8G8R8A8: u32 = 1;
pub const FORMAT_B8G8R8X8: u32 = 2;

/// The size of the control header, and of the responses the driver reads.
pub const HEADER_SIZE: usize = 24;
pub const MAX_SCANOUTS: usize = 16;
pub const DISPLAY_INFO_SIZE: usize = HEADER_SIZE + MAX_SCANOUTS * 24;
pub const EDID_SIZE: usize = HEADER_SIZE + 8 + 1024;

/// A rectangle as the device takes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One scanout of the display information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scanout {
    pub rect: Rect,
    pub enabled: bool,
}

/// One entry of a resource's backing: a physical address and a length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub address: u64,
    pub length: u32,
}

/// A command being written.
struct Command(Vec<u8>);

impl Command {
    /// A command of `kind`, fenced with `fence` when there is one.
    fn new(kind: u32, fence: Option<u64>) -> Command {
        let mut c = Command(Vec::with_capacity(64));
        c.u32(kind);
        c.u32(if fence.is_some() { FLAG_FENCE } else { 0 });
        c.u64(fence.unwrap_or(0));
        // `ctx_id`, `ring_idx` and its padding.
        c.u32(0);
        c.u32(0);
        c
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn u64(&mut self, value: u64) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn rect(&mut self, r: Rect) -> &mut Self {
        self.u32(r.x).u32(r.y).u32(r.width).u32(r.height)
    }

    fn done(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.0)
    }
}

pub fn get_display_info() -> Vec<u8> {
    Command::new(GET_DISPLAY_INFO, None).done()
}

pub fn get_edid(scanout: u32) -> Vec<u8> {
    Command::new(GET_EDID, None).u32(scanout).u32(0).done()
}

pub fn resource_create_2d(resource: u32, format: u32, width: u32, height: u32) -> Vec<u8> {
    Command::new(RESOURCE_CREATE_2D, None)
        .u32(resource)
        .u32(format)
        .u32(width)
        .u32(height)
        .done()
}

pub fn resource_unref(resource: u32) -> Vec<u8> {
    Command::new(RESOURCE_UNREF, None)
        .u32(resource)
        .u32(0)
        .done()
}

/// Attaches `entries`, which the caller keeps within one command's
/// worth, as the backing of `resource`.
pub fn attach_backing(resource: u32, entries: &[Entry]) -> Vec<u8> {
    let mut c = Command::new(RESOURCE_ATTACH_BACKING, None);
    c.u32(resource).u32(entries.len() as u32);
    for e in entries {
        c.u64(e.address).u32(e.length).u32(0);
    }
    c.done()
}

pub fn detach_backing(resource: u32) -> Vec<u8> {
    Command::new(RESOURCE_DETACH_BACKING, None)
        .u32(resource)
        .u32(0)
        .done()
}

pub fn set_scanout(scanout: u32, resource: u32, rect: Rect) -> Vec<u8> {
    Command::new(SET_SCANOUT, None)
        .rect(rect)
        .u32(scanout)
        .u32(resource)
        .done()
}

/// Copies `rect` of `resource`'s backing, which starts `offset` bytes
/// in, to the host.
pub fn transfer_to_host_2d(resource: u32, rect: Rect, offset: u64) -> Vec<u8> {
    Command::new(TRANSFER_TO_HOST_2D, None)
        .rect(rect)
        .u64(offset)
        .u32(resource)
        .u32(0)
        .done()
}

pub fn resource_flush(resource: u32, rect: Rect, fence: Option<u64>) -> Vec<u8> {
    Command::new(RESOURCE_FLUSH, fence)
        .rect(rect)
        .u32(resource)
        .u32(0)
        .done()
}

/// A cursor command: a new image, `resource` with its hot spot, or only
/// a move.
fn cursor(kind: u32, scanout: u32, x: u32, y: u32, resource: u32, hot: (u32, u32)) -> Vec<u8> {
    Command::new(kind, None)
        .u32(scanout)
        .u32(x)
        .u32(y)
        .u32(0)
        .u32(resource)
        .u32(hot.0)
        .u32(hot.1)
        .u32(0)
        .done()
}

pub fn update_cursor(scanout: u32, x: u32, y: u32, resource: u32, hot: (u32, u32)) -> Vec<u8> {
    cursor(UPDATE_CURSOR, scanout, x, y, resource, hot)
}

pub fn move_cursor(scanout: u32, x: u32, y: u32) -> Vec<u8> {
    cursor(MOVE_CURSOR, scanout, x, y, 0, (0, 0))
}

/// The backing entries of the pages of `page` bytes at `addresses`,
/// those that follow each other merged into one.
pub fn entries(addresses: &[u64], page: u32) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for &address in addresses {
        match entries.last_mut() {
            Some(last) if last.address + u64::from(last.length) == address => {
                last.length += page;
            }
            _ => entries.push(Entry {
                address,
                length: page,
            }),
        }
    }
    entries
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The type of the response in `bytes`.
pub fn response_type(bytes: &[u8]) -> Option<u32> {
    u32_at(bytes, 0)
}

/// The first `count` scanouts of a display information response.
pub fn scanouts(bytes: &[u8], count: usize) -> Option<Vec<Scanout>> {
    if response_type(bytes)? != RESP_OK_DISPLAY_INFO {
        return None;
    }
    (0..count.min(MAX_SCANOUTS))
        .map(|i| {
            let at = HEADER_SIZE + i * 24;
            Some(Scanout {
                rect: Rect {
                    x: u32_at(bytes, at)?,
                    y: u32_at(bytes, at + 4)?,
                    width: u32_at(bytes, at + 8)?,
                    height: u32_at(bytes, at + 12)?,
                },
                enabled: u32_at(bytes, at + 16)? != 0,
            })
        })
        .collect()
}

/// The EDID blob of an EDID response.
pub fn edid(bytes: &[u8]) -> Option<&[u8]> {
    if response_type(bytes)? != RESP_OK_EDID {
        return None;
    }
    let size = (u32_at(bytes, HEADER_SIZE)? as usize).min(1024);
    bytes.get(HEADER_SIZE + 8..HEADER_SIZE + 8 + size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fenced_flush_carries_its_fence() {
        let rect = Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        let c = resource_flush(7, rect, Some(9));
        assert_eq!(c.len(), 48);
        assert_eq!(u32_at(&c, 0), Some(0x0104));
        assert_eq!(u32_at(&c, 4), Some(1));
        assert_eq!(u64::from_le_bytes(c[8..16].try_into().unwrap()), 9);
        assert_eq!(
            (u32_at(&c, 24), u32_at(&c, 36), u32_at(&c, 40)),
            (Some(1), Some(4), Some(7))
        );
        assert_eq!(u32_at(&resource_flush(7, rect, None), 4), Some(0));
    }

    #[test]
    fn commands_have_the_device_layouts() {
        let rect = Rect::default();
        assert_eq!(get_display_info().len(), 24);
        assert_eq!(get_edid(0).len(), 32);
        assert_eq!(resource_create_2d(1, FORMAT_B8G8R8X8, 640, 480).len(), 40);
        assert_eq!(resource_unref(1).len(), 32);
        assert_eq!(detach_backing(1).len(), 32);
        assert_eq!(set_scanout(0, 1, rect).len(), 48);
        assert_eq!(transfer_to_host_2d(1, rect, 0).len(), 56);
        assert_eq!(update_cursor(0, 1, 2, 3, (4, 5)).len(), 56);
        assert_eq!(move_cursor(0, 1, 2).len(), 56);
        let entries = [
            Entry {
                address: 0x1000,
                length: 0x2000,
            },
            Entry {
                address: 0x8000,
                length: 0x1000,
            },
        ];
        let c = attach_backing(5, &entries);
        assert_eq!(c.len(), 32 + 2 * 16);
        assert_eq!((u32_at(&c, 24), u32_at(&c, 28)), (Some(5), Some(2)));
        assert_eq!(u64::from_le_bytes(c[48..56].try_into().unwrap()), 0x8000);
    }

    #[test]
    fn merges_pages_that_follow_each_other() {
        let e = entries(&[0x1000, 0x2000, 0x5000, 0x6000, 0x8000], 0x1000);
        let pairs: Vec<(u64, u32)> = e.iter().map(|e| (e.address, e.length)).collect();
        assert_eq!(
            pairs,
            vec![(0x1000, 0x2000), (0x5000, 0x2000), (0x8000, 0x1000)]
        );
        assert!(entries(&[], 0x1000).is_empty());
    }

    #[test]
    fn reads_the_scanouts_and_the_edid() {
        let mut info = vec![0u8; DISPLAY_INFO_SIZE];
        info[..4].copy_from_slice(&RESP_OK_DISPLAY_INFO.to_le_bytes());
        info[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&1280u32.to_le_bytes());
        info[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&800u32.to_le_bytes());
        info[HEADER_SIZE + 16..HEADER_SIZE + 20].copy_from_slice(&1u32.to_le_bytes());
        let scanouts = scanouts(&info, 2).unwrap();
        assert_eq!(scanouts.len(), 2);
        assert_eq!(
            (scanouts[0].rect.width, scanouts[0].rect.height),
            (1280, 800)
        );
        assert!(scanouts[0].enabled && !scanouts[1].enabled);
        info[..4].copy_from_slice(&0x1200u32.to_le_bytes());
        assert_eq!(super::scanouts(&info, 1), None);

        let mut edid_response = vec![0u8; EDID_SIZE];
        edid_response[..4].copy_from_slice(&RESP_OK_EDID.to_le_bytes());
        edid_response[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&128u32.to_le_bytes());
        edid_response[HEADER_SIZE + 8] = 0xff;
        let blob = edid(&edid_response).unwrap();
        assert_eq!((blob.len(), blob[0]), (128, 0xff));
    }
}
