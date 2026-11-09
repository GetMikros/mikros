//! The Bulk-Only Transport: command block wrappers, the data stage and
//! command status wrappers on the interface's bulk endpoints, one
//! command at a time, with the class requests and the reset recovery.

use crate::lock::Lock;
use alloc::vec::Vec;
use core::cell::Cell;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::usb::{InterfaceAsyncClient, Setup};
use mikros_rt::sys;
use mikros_storage::{MS, now};

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const CBW_SIZE: u32 = 31;
const CSW_SIZE: u32 = 13;
/// Where the wrappers and the driver's own data lie in its page.
const CBW_AT: u64 = 0;
const CSW_AT: u64 = 64;
pub const DATA_AT: u64 = 1024;
pub const DATA_MAX: u32 = 3072;
const PAGE: u64 = 4096;
/// The class requests.
const MASS_STORAGE_RESET: u8 = 0xFF;
const GET_MAX_LUN: u8 = 0xFE;
/// How long a command may take.
const COMMAND_LIMIT: i64 = 20_000 * MS;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const SAME_RIGHTS: u32 = 1 << 31;

/// Why a command did not succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The device reported that the command failed.
    Failed,
    /// It failed, and its sense data gave this sense key.
    Sense(u8),
    /// The transport failed with this status: `PEER_CLOSED` once the
    /// device is gone, `INTERNAL` for anything else.
    Transport(Status),
}

impl From<Failure> for Status {
    fn from(failure: Failure) -> Status {
        match failure {
            Failure::Transport(Status::PeerClosed) => Status::PeerClosed,
            _ => Status::Internal,
        }
    }
}

/// Where a command's data stage goes.
pub enum Data<'a> {
    None,
    /// `length` bytes from the device into the driver's page.
    In(u32),
    /// From the device into, or to the device from (`write`), the bytes
    /// at `offset` in a client's buffer.
    Buffer {
        memory: &'a OwnedHandle,
        offset: u64,
        length: u32,
        write: bool,
    },
}

/// The transport of one interface: its client, its bulk endpoints, the
/// page that holds the wrappers, and the lock that keeps one command at
/// a time.
pub struct Bot {
    pub interface: InterfaceAsyncClient,
    pub number: u8,
    bulk_in: u8,
    bulk_out: u8,
    page: OwnedHandle,
    at: u64,
    tag: Cell<u32>,
    lock: Lock,
}

fn kernel(status: i64) -> Result<(), Status> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(Status::from_raw(status as i32)),
    }
}

fn duplicate(handle: &OwnedHandle) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    kernel(unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) })?;
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// The status of a reply on the interface's channel.
fn reply<T>(
    reply: Result<Result<T, Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Status> {
    match reply {
        Ok(result) => result,
        Err(_) => Err(Status::PeerClosed),
    }
}

impl Bot {
    /// The transport of interface `number` through `interface`, on the
    /// bulk endpoints `bulk_in` and `bulk_out`, with a page of pinned
    /// memory from the `DMA` resource `dma` mapped into `process`.
    pub fn new(
        interface: InterfaceAsyncClient,
        number: u8,
        bulk_in: u8,
        bulk_out: u8,
        dma: &OwnedHandle,
        process: sys::Handle,
    ) -> Result<Bot, Status> {
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe { sys::mem_create_pinned(dma.raw(), PAGE, 0, &mut mem) })?;
        // SAFETY: the kernel just created the handle for us.
        let page = unsafe { OwnedHandle::from_raw(mem) };
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        kernel(unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                page.raw(),
                0,
                PAGE,
                &mut at,
            )
        })?;
        Ok(Bot {
            interface,
            number,
            bulk_in,
            bulk_out,
            page,
            at,
            tag: Cell::new(0),
            lock: Lock::default(),
        })
    }

    fn write(&self, offset: u64, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            // SAFETY: the page is mapped and `offset` lies in it.
            unsafe { core::ptr::write_volatile((self.at + offset + i as u64) as *mut u8, *byte) };
        }
    }

    /// `len` bytes of the page from `offset`.
    pub fn read(&self, offset: u64, len: usize) -> Vec<u8> {
        (0..len as u64)
            // SAFETY: the page is mapped and the bytes lie in it.
            .map(|i| unsafe { core::ptr::read_volatile((self.at + offset + i) as *const u8) })
            .collect()
    }

    async fn class_request(
        &self,
        request_type: u8,
        request: u8,
        length: u16,
    ) -> Result<Vec<u8>, Status> {
        let setup = Setup {
            request_type,
            request,
            value: 0,
            index: u16::from(self.number),
            length,
        };
        let sent = self
            .interface
            .control_transfer(setup, Vec::new(), now() + COMMAND_LIMIT)
            .await;
        reply(sent).map(|r| r.data)
    }

    /// The number of logical units, from Get Max LUN: 1 when the device
    /// stalls the request.
    pub async fn units(&self) -> Result<u8, Status> {
        match self.class_request(0xA1, GET_MAX_LUN, 1).await {
            Ok(data) => Ok(data.first().map_or(1, |&max| (max & 0xF) + 1)),
            Err(Status::BadState) => Ok(1),
            Err(status) => Err(status),
        }
    }

    async fn bulk(
        &self,
        endpoint: u8,
        memory: &OwnedHandle,
        offset: u64,
        length: u32,
    ) -> Result<u32, Status> {
        let buffer = duplicate(memory)?;
        let sent = self
            .interface
            .bulk_transfer(endpoint, buffer, offset, length, now() + COMMAND_LIMIT)
            .await;
        reply(sent).map(|r| r.actual)
    }

    async fn clear_halt(&self, endpoint: u8) -> Result<(), Status> {
        reply(self.interface.clear_halt(endpoint).await).map(|_| ())
    }

    /// The reset recovery: a Bulk-Only Mass Storage Reset, and clearing
    /// the halts of both bulk endpoints.
    async fn reset_recovery(&self) {
        let _ = self.class_request(0x21, MASS_STORAGE_RESET, 0).await;
        let _ = self.clear_halt(self.bulk_in).await;
        let _ = self.clear_halt(self.bulk_out).await;
    }

    /// Sends the command block `command` to logical unit `lun` with the
    /// data stage `data`, and reads its status. Returns the bytes the
    /// data stage moved. A stalled data stage has its endpoint's halt
    /// cleared; an invalid status wrapper leads to the reset recovery.
    pub async fn command(&self, lun: u8, command: &[u8], data: Data<'_>) -> Result<u32, Failure> {
        let _held = self.lock.lock().await;
        let transport = Failure::Transport;
        let tag = self.tag.get().wrapping_add(1);
        self.tag.set(tag);
        let (length, input) = match &data {
            Data::None => (0, false),
            Data::In(length) => (*length, true),
            Data::Buffer { length, write, .. } => (*length, !write),
        };
        let mut cbw = [0u8; CBW_SIZE as usize];
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&length.to_le_bytes());
        cbw[12] = if input { 0x80 } else { 0 };
        cbw[13] = lun;
        cbw[14] = command.len() as u8;
        cbw[15..15 + command.len()].copy_from_slice(command);
        self.write(CBW_AT, &cbw);
        let sent = self.bulk(self.bulk_out, &self.page, CBW_AT, CBW_SIZE).await;
        if let Err(status) = sent {
            if status != Status::PeerClosed {
                self.reset_recovery().await;
            }
            return Err(transport(status));
        }
        let endpoint = if input { self.bulk_in } else { self.bulk_out };
        let moved = match data {
            Data::None => Ok(0),
            Data::In(length) => self.bulk(endpoint, &self.page, DATA_AT, length).await,
            Data::Buffer {
                memory,
                offset,
                length,
                ..
            } => self.bulk(endpoint, memory, offset, length).await,
        };
        let moved = match moved {
            Ok(moved) => moved,
            Err(Status::BadState) => {
                self.clear_halt(endpoint).await.map_err(transport)?;
                0
            }
            Err(status) => {
                if status != Status::PeerClosed {
                    self.reset_recovery().await;
                }
                return Err(transport(status));
            }
        };
        // A stalled status stage is retried once after its halt is
        // cleared.
        let mut read = self.bulk(self.bulk_in, &self.page, CSW_AT, CSW_SIZE).await;
        if read == Err(Status::BadState) {
            self.clear_halt(self.bulk_in).await.map_err(transport)?;
            read = self.bulk(self.bulk_in, &self.page, CSW_AT, CSW_SIZE).await;
        }
        let got = match read {
            Ok(got) => got,
            Err(Status::PeerClosed) => return Err(transport(Status::PeerClosed)),
            Err(status) => {
                self.reset_recovery().await;
                return Err(transport(status));
            }
        };
        let csw = self.read(CSW_AT, CSW_SIZE as usize);
        let le32 = |at: usize| u32::from_le_bytes([csw[at], csw[at + 1], csw[at + 2], csw[at + 3]]);
        let valid = got == CSW_SIZE
            && le32(0) == CSW_SIGNATURE
            && le32(4) == tag
            && le32(8) <= length
            && csw[12] <= 1;
        if !valid {
            self.reset_recovery().await;
            return Err(transport(Status::Internal));
        }
        match csw[12] {
            0 => Ok(moved.min(length - le32(8))),
            _ => Err(Failure::Failed),
        }
    }
}
