//! The disk of a virtio block device: its configuration, the limits of
//! its requests, and the requests themselves.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockFlags, BlockInfo};
use mikros_rt::sys;
use mikros_storage::{PAGE, pages, pinned};
use mikros_virtio::{Buffer, Mmio, Queue, Transport};

/// The features the driver implements.
pub const F_SIZE_MAX: u64 = 1 << 1;
pub const F_SEG_MAX: u64 = 1 << 2;
pub const F_RO: u64 = 1 << 5;
pub const F_BLK_SIZE: u64 = 1 << 6;
pub const F_FLUSH: u64 = 1 << 9;
pub const F_DISCARD: u64 = 1 << 13;
pub const FEATURES: u64 = F_SIZE_MAX | F_SEG_MAX | F_RO | F_BLK_SIZE | F_FLUSH | F_DISCARD;
/// Request types and statuses.
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const T_GET_ID: u32 = 8;
const T_DISCARD: u32 = 11;
const S_OK: u8 = 0;
const S_UNSUPP: u8 = 2;
/// The fields of the device configuration.
const CAPACITY: u64 = 0;
const SIZE_MAX: u64 = 8;
const SEG_MAX: u64 = 12;
const BLK_SIZE: u64 = 20;
const MAX_DISCARD_SECTORS: u64 = 36;
const MAX_DISCARD_SEG: u64 = 40;
/// The size of a sector.
const SECTOR: u64 = 512;
/// Where a request slot's page holds the header, the status byte and
/// the payload, the bytes of the payload, and the bytes of a discard
/// segment and a device ID.
const STATUS: u64 = 16;
const PAYLOAD: u64 = 64;
const PAYLOAD_SIZE: u64 = PAGE - PAYLOAD;
const DISCARD_SEGMENT: u64 = 16;
const ID_SIZE: u32 = 20;
/// The most requests in flight at once, one slot each.
const SLOTS_MAX: u16 = 64;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;

/// The disk, with what the device configuration and the accepted
/// features say about it, and the pages that requests use for their
/// headers, statuses and payloads.
pub struct Disk {
    pub transport: Rc<Transport>,
    pub dma: OwnedHandle,
    queue: Rc<Queue>,
    pub block_size: u32,
    pub blocks: u64,
    pub read_only: bool,
    pub flush: bool,
    pub discard: bool,
    pub max_transfer_blocks: u32,
    pub serial: String,
    /// The most bytes one data descriptor covers.
    segment: u64,
    max_discard_sectors: u64,
    max_discard_segments: u64,
    slots: Slots,
}

/// One pinned page for each request in flight: its mapping, its
/// physical address, and which pages are free.
struct Slots {
    at: u64,
    pages: Vec<u64>,
    free: RefCell<Vec<usize>>,
    waiting: RefCell<Vec<Waker>>,
    _mem: OwnedHandle,
    _pmt: OwnedHandle,
}

fn kernel(status: i64) -> Result<(), Status> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(Status::from_raw(status as i32)),
    }
}

fn read64(config: &Mmio, offset: u64) -> u64 {
    u64::from(config.read32(offset)) | u64::from(config.read32(offset + 4)) << 32
}

/// The most blocks of `block_size` bytes whose data fits, at any offset
/// in a buffer, in `descriptors` data descriptors of at most `segment`
/// bytes that do not cross pages.
pub fn transfer_limit(block_size: u32, descriptors: u64, segment: u64) -> u32 {
    let worst = |bytes: u64| {
        let pages = (PAGE - 1 + bytes).div_ceil(PAGE);
        if segment >= PAGE {
            pages
        } else {
            bytes.div_ceil(segment) + pages
        }
    };
    let mut blocks = (PAGE * descriptors / u64::from(block_size)).min(u64::from(u32::MAX));
    while blocks > 0 && worst(blocks * u64::from(block_size)) > descriptors {
        blocks -= 1;
    }
    blocks as u32
}

impl Disk {
    /// The disk of the device that `transport` drives with `features`,
    /// whose requests go on `queue`, with request pages made from `dma`.
    /// Reads its serial number with a `GET_ID` request; it is empty when
    /// that request fails.
    pub async fn new(
        transport: Rc<Transport>,
        features: u64,
        queue: Rc<Queue>,
        dma: OwnedHandle,
        process: sys::Handle,
    ) -> Result<Disk, Status> {
        let has = |f: u64| features & f != 0;
        let config = transport.read_config(|c| {
            (
                read64(c, CAPACITY),
                c.read32(SIZE_MAX),
                c.read32(SEG_MAX),
                c.read32(BLK_SIZE),
                c.read32(MAX_DISCARD_SECTORS),
                c.read32(MAX_DISCARD_SEG),
            )
        });
        let (capacity, size_max, seg_max, blk_size, discard_sectors, discard_segments) = config;
        let block_size = if has(F_BLK_SIZE) && blk_size >= 512 && blk_size.is_power_of_two() {
            blk_size
        } else {
            512
        };
        let descriptors = if has(F_SEG_MAX) {
            u64::from(seg_max)
        } else {
            u64::from(queue.size()) - 2
        };
        let segment = if has(F_SIZE_MAX) && size_max > 0 {
            u64::from(size_max).min(PAGE)
        } else {
            PAGE
        };
        let count = (queue.size() / 2).clamp(1, SLOTS_MAX);
        let (mem, pmt) = pinned(&dma, transport.bti(), PAGE * u64::from(count))?;
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        kernel(unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                mem.raw(),
                0,
                PAGE * u64::from(count),
                &mut at,
            )
        })?;
        let slots = Slots {
            at,
            pages: pages(&mem, u64::from(count))?,
            free: RefCell::new((0..usize::from(count)).collect()),
            waiting: RefCell::new(Vec::new()),
            _mem: mem,
            _pmt: pmt,
        };
        let unlimited = |n: u32| {
            if n == 0 {
                u64::from(u32::MAX)
            } else {
                u64::from(n)
            }
        };
        let mut disk = Disk {
            transport,
            dma,
            queue,
            block_size,
            blocks: capacity * SECTOR / u64::from(block_size),
            read_only: has(F_RO),
            flush: has(F_FLUSH),
            discard: has(F_DISCARD),
            max_transfer_blocks: transfer_limit(block_size, descriptors, segment),
            serial: String::new(),
            segment,
            max_discard_sectors: unlimited(discard_sectors),
            max_discard_segments: unlimited(discard_segments).min(PAYLOAD_SIZE / DISCARD_SEGMENT),
            slots,
        };
        if let Ok(id) = disk.request(T_GET_ID, 0, &[], &[], ID_SIZE).await {
            let end = id.iter().position(|&b| b == 0).unwrap_or(id.len());
            disk.serial = String::from_utf8_lossy(&id[..end]).into();
        }
        Ok(disk)
    }

    /// The data descriptors for `len` bytes at `offset` in a buffer whose
    /// pages are at `pages`, written by the device when `device_writes`:
    /// one for each part of a page, of at most `segment` bytes.
    fn data(&self, pages: &[u64], offset: u64, len: u64, device_writes: bool) -> Vec<Buffer> {
        let mut data = Vec::new();
        let (mut at, end) = (offset, offset + len);
        while at < end {
            let in_page = at % PAGE;
            let piece = (PAGE - in_page).min(end - at).min(self.segment);
            data.push(Buffer {
                address: pages[(at / PAGE) as usize] + in_page,
                len: piece as u32,
                device_writes,
            });
            at += piece;
        }
        data
    }

    /// Reads (`write` false) or writes `count` blocks from `block`, with
    /// the data descriptors `data`.
    async fn transfer_data(&self, write: bool, block: u64, data: &[Buffer]) -> Result<(), Status> {
        let kind = if write { T_OUT } else { T_IN };
        let sector = block * u64::from(self.block_size) / SECTOR;
        self.request(kind, sector, data, &[], 0).await.map(|_| ())
    }

    /// Has the device write its cache to stable storage, when it has one.
    async fn flush_cache(&self) -> Result<(), Status> {
        if !self.flush {
            return Ok(());
        }
        self.request(T_FLUSH, 0, &[], &[], 0).await.map(|_| ())
    }

    /// Discards `count` blocks from `block`, in as few requests as the
    /// device's limits allow, stopping at the first that fails.
    async fn discard_blocks(&self, block: u64, count: u64) -> Result<(), Status> {
        let per_block = u64::from(self.block_size) / SECTOR;
        let (mut sector, end) = (block * per_block, (block + count) * per_block);
        while sector < end {
            let mut segments = Vec::new();
            while sector < end
                && (segments.len() as u64) < self.max_discard_segments * DISCARD_SEGMENT
            {
                let sectors = (end - sector).min(self.max_discard_sectors);
                segments.extend_from_slice(&sector.to_le_bytes());
                segments.extend_from_slice(&(sectors as u32).to_le_bytes());
                segments.extend_from_slice(&0u32.to_le_bytes());
                sector += sectors;
            }
            self.request(T_DISCARD, 0, &[], &segments, 0).await?;
        }
        Ok(())
    }

    /// Makes a request of `kind` at `sector`: its header, `payload` for
    /// the device to read, the data descriptors `data`, `reply` bytes for
    /// the device to write, and the status byte. Returns those bytes.
    async fn request(
        &self,
        kind: u32,
        sector: u64,
        data: &[Buffer],
        payload: &[u8],
        reply: u32,
    ) -> Result<Vec<u8>, Status> {
        let slot = self.take().await;
        let (page, at) = (self.slots.pages[slot], self.slots.at + PAGE * slot as u64);
        // SAFETY: the slot's page is mapped at `at`, and only this
        // request uses it.
        unsafe {
            core::ptr::write_volatile(at as *mut u32, kind);
            core::ptr::write_volatile((at + 4) as *mut u32, 0);
            core::ptr::write_volatile((at + 8) as *mut u64, sector);
            core::ptr::write_volatile((at + STATUS) as *mut u8, 0xFF);
            core::ptr::copy_nonoverlapping(
                payload.as_ptr(),
                (at + PAYLOAD) as *mut u8,
                payload.len(),
            );
        }
        let mut buffers = vec![Buffer {
            address: page,
            len: 16,
            device_writes: false,
        }];
        if !payload.is_empty() {
            buffers.push(Buffer {
                address: page + PAYLOAD,
                len: payload.len() as u32,
                device_writes: false,
            });
        }
        buffers.extend_from_slice(data);
        if reply > 0 {
            buffers.push(Buffer {
                address: page + PAYLOAD,
                len: reply,
                device_writes: true,
            });
        }
        buffers.push(Buffer {
            address: page + STATUS,
            len: 1,
            device_writes: true,
        });
        self.queue.submit(&buffers).await;
        // SAFETY: as above; the device is done with the slot.
        let (status, replied) = unsafe {
            let status = core::ptr::read_volatile((at + STATUS) as *const u8);
            let replied =
                core::slice::from_raw_parts((at + PAYLOAD) as *const u8, reply as usize).to_vec();
            (status, replied)
        };
        self.give_back(slot);
        match status {
            S_OK => Ok(replied),
            S_UNSUPP => Err(Status::NotSupported),
            _ => Err(Status::Internal),
        }
    }

    async fn take(&self) -> usize {
        poll_fn(|cx| match self.slots.free.borrow_mut().pop() {
            Some(slot) => Poll::Ready(slot),
            None => {
                self.slots.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    fn give_back(&self, slot: usize) {
        self.slots.free.borrow_mut().push(slot);
        let waiting = core::mem::take(&mut *self.slots.waiting.borrow_mut());
        for waker in waiting {
            waker.wake();
        }
    }
}

impl mikros_storage::Disk for Disk {
    fn info(&self) -> BlockInfo {
        let mut flags = 0;
        if self.read_only {
            flags |= BlockFlags::READ_ONLY.0;
        }
        if self.flush {
            flags |= BlockFlags::FLUSH.0;
        }
        if self.discard {
            flags |= BlockFlags::DISCARD.0;
        }
        BlockInfo {
            block_size: self.block_size,
            block_count: self.blocks,
            max_transfer_blocks: self.max_transfer_blocks,
            flags: BlockFlags(flags),
            model: String::from("virtio-blk"),
            serial: self.serial.clone(),
        }
    }

    fn dma(&self) -> &OwnedHandle {
        &self.dma
    }

    fn bti(&self) -> &OwnedHandle {
        self.transport.bti()
    }

    /// One request, and for a `fua` write a flush after it.
    async fn transfer(
        &self,
        write: bool,
        block: u64,
        pages: &[u64],
        offset: u64,
        len: u64,
        fua: bool,
    ) -> Result<(), Status> {
        let data = self.data(pages, offset, len, !write);
        self.transfer_data(write, block, &data).await?;
        if write && fua {
            self.flush_cache().await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), Status> {
        self.flush_cache().await
    }

    async fn discard(&self, block: u64, count: u64) -> Result<(), Status> {
        self.discard_blocks(block, count).await
    }
}
