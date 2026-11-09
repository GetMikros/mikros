//! A logical unit: finding out whether it is a disk, its capacity and
//! write protection, and its reads, writes and flushes as SCSI commands.

use crate::bot::{Bot, DATA_AT, DATA_MAX, Data, Failure};
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockFlags, BlockInfo};
use mikros_storage::{Disk, MS, Writer, now};

const TEST_UNIT_READY: u8 = 0x00;
const REQUEST_SENSE: u8 = 0x03;
const INQUIRY: u8 = 0x12;
const MODE_SENSE_6: u8 = 0x1A;
const READ_CAPACITY_10: u8 = 0x25;
const READ_10: u8 = 0x28;
const WRITE_10: u8 = 0x2A;
const SYNCHRONIZE_CACHE_10: u8 = 0x35;
const READ_16: u8 = 0x88;
const WRITE_16: u8 = 0x8A;
const SERVICE_ACTION_IN_16: u8 = 0x9E;
const READ_CAPACITY_16: u8 = 0x10;
/// The sense key of requests the unit does not support.
const ILLEGAL_REQUEST: u8 = 0x5;
/// The peripheral device type of direct-access block devices.
const DIRECT_ACCESS: u8 = 0;
/// How often TEST UNIT READY is sent until the unit is ready.
const READY_PERIOD: i64 = 100 * MS;
/// The largest transfer.
const TRANSFER_MAX: u64 = 1 << 20;

/// A logical unit that is a disk.
pub struct Unit {
    bot: Rc<Bot>,
    lun: u8,
    dma: Rc<OwnedHandle>,
    pub info: BlockInfo,
    pub writer: Writer,
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The text of an INQUIRY string field: ASCII, padded with spaces.
fn text(bytes: &[u8]) -> String {
    let text: String = bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { ' ' })
        .collect();
    String::from(text.trim())
}

/// Sends `command` to `lun` and reads `length` bytes of data into the
/// transport's page. On a failed command, reads the sense data. Returns
/// the data.
async fn data_in(
    bot: &Bot,
    lun: u8,
    command: &[u8],
    length: u32,
) -> Result<alloc::vec::Vec<u8>, Failure> {
    let moved = run(bot, lun, command, Data::In(length.min(DATA_MAX))).await?;
    Ok(bot.read(DATA_AT, moved as usize))
}

/// Sends `command` to `lun`. When the unit reports that it failed, reads
/// its sense data with REQUEST SENSE, and fails with its sense key.
async fn run(bot: &Bot, lun: u8, command: &[u8], data: Data<'_>) -> Result<u32, Failure> {
    let result = bot.command(lun, command, data).await;
    if result != Err(Failure::Failed) {
        return result;
    }
    let sense = [REQUEST_SENSE, 0, 0, 0, 18, 0];
    match bot.command(lun, &sense, Data::In(18)).await {
        Ok(moved) if moved >= 3 => Err(Failure::Sense(bot.read(DATA_AT, 3)[2] & 0xF)),
        _ => Err(Failure::Failed),
    }
}

impl Unit {
    /// The logical unit `lun` of `bot`, once it is ready, when it is a
    /// direct-access disk; `None` otherwise. Its serial number is
    /// `serial`.
    pub async fn probe(
        bot: Rc<Bot>,
        lun: u8,
        dma: Rc<OwnedHandle>,
        serial: String,
        reactor: &Reactor,
    ) -> Result<Option<Unit>, Status> {
        let inquiry = data_in(&bot, lun, &[INQUIRY, 0, 0, 0, 36, 0], 36).await?;
        if inquiry.len() < 36 || inquiry[0] & 0x1F != DIRECT_ACCESS {
            return Ok(None);
        }
        let model = alloc::format!("{} {}", text(&inquiry[8..16]), text(&inquiry[16..32]));
        loop {
            match run(&bot, lun, &[TEST_UNIT_READY, 0, 0, 0, 0, 0], Data::None).await {
                Ok(_) => break,
                Err(Failure::Transport(Status::PeerClosed)) => return Err(Status::PeerClosed),
                Err(_) => {
                    let _ = reactor.sleep_until(now() + READY_PERIOD).await;
                }
            }
        }
        let (blocks, block_size) = capacity(&bot, lun).await?;
        let mode = [MODE_SENSE_6, 0, 0x3F, 0, 192, 0];
        let protected = match data_in(&bot, lun, &mode, 192).await {
            Ok(header) => header.get(2).is_some_and(|b| b & 0x80 != 0),
            Err(Failure::Transport(Status::PeerClosed)) => return Err(Status::PeerClosed),
            Err(_) => false,
        };
        let mut flags = BlockFlags::REMOVABLE.0 | BlockFlags::FLUSH.0;
        if protected {
            flags |= BlockFlags::READ_ONLY.0;
        }
        if !block_size.is_power_of_two() || block_size < 512 {
            return Ok(None);
        }
        let info = BlockInfo {
            block_size,
            block_count: blocks,
            max_transfer_blocks: (TRANSFER_MAX / u64::from(block_size)) as u32,
            flags: BlockFlags(flags),
            model,
            serial,
        };
        Ok(Some(Unit {
            bot,
            lun,
            dma,
            info,
            writer: Writer::default(),
        }))
    }
}

/// The block count and block size, from READ CAPACITY (16), or READ
/// CAPACITY (10) when that fails.
async fn capacity(bot: &Bot, lun: u8) -> Result<(u64, u32), Status> {
    let mut command = [0u8; 16];
    command[0] = SERVICE_ACTION_IN_16;
    command[1] = READ_CAPACITY_16;
    command[13] = 32;
    match data_in(bot, lun, &command, 32).await {
        Ok(data) if data.len() >= 12 => {
            let last = u64::from(be32(&data, 0)) << 32 | u64::from(be32(&data, 4));
            return Ok((last + 1, be32(&data, 8)));
        }
        Err(Failure::Transport(Status::PeerClosed)) => return Err(Status::PeerClosed),
        _ => {}
    }
    let command = [READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let data = data_in(bot, lun, &command, 8).await?;
    if data.len() < 8 {
        return Err(Status::Internal);
    }
    Ok((u64::from(be32(&data, 0)) + 1, be32(&data, 4)))
}

impl Unit {
    /// Sends SYNCHRONIZE CACHE (10); a unit that rejects it as an
    /// illegal request has nothing to write back.
    async fn synchronize(&self) -> Result<(), Status> {
        let command = [SYNCHRONIZE_CACHE_10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        match run(&self.bot, self.lun, &command, Data::None).await {
            Ok(_) | Err(Failure::Sense(ILLEGAL_REQUEST)) => Ok(()),
            Err(failure) => Err(failure.into()),
        }
    }
}

impl Disk for Unit {
    fn info(&self) -> BlockInfo {
        let info = &self.info;
        BlockInfo {
            block_size: info.block_size,
            block_count: info.block_count,
            max_transfer_blocks: info.max_transfer_blocks,
            flags: info.flags,
            model: info.model.clone(),
            serial: info.serial.clone(),
        }
    }

    fn dma(&self) -> &OwnedHandle {
        &self.dma
    }

    fn bti(&self) -> Option<&OwnedHandle> {
        None
    }

    /// Not called: the disk has no bus transaction initiator.
    async fn transfer(
        &self,
        _write: bool,
        _block: u64,
        _pages: &[u64],
        _offset: u64,
        _len: u64,
        _fua: bool,
    ) -> Result<(), Status> {
        Err(Status::NotSupported)
    }

    async fn transfer_object(
        &self,
        write: bool,
        block: u64,
        memory: &OwnedHandle,
        offset: u64,
        len: u64,
        fua: bool,
    ) -> Result<(), Status> {
        let count = len / u64::from(self.info.block_size);
        let last = block + count - 1;
        let mut command = [0u8; 16];
        let size = if last > u64::from(u32::MAX) {
            command[0] = if write { WRITE_16 } else { READ_16 };
            command[2..10].copy_from_slice(&block.to_be_bytes());
            command[10..14].copy_from_slice(&(count as u32).to_be_bytes());
            16
        } else {
            command[0] = if write { WRITE_10 } else { READ_10 };
            command[2..6].copy_from_slice(&(block as u32).to_be_bytes());
            command[7..9].copy_from_slice(&(count as u16).to_be_bytes());
            10
        };
        let data = Data::Buffer {
            memory,
            offset,
            length: len as u32,
            write,
        };
        let moved = run(&self.bot, self.lun, &command[..size], data).await?;
        if u64::from(moved) != len {
            return Err(Status::Internal);
        }
        if write && fua {
            self.synchronize().await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), Status> {
        self.synchronize().await
    }

    async fn discard(&self, _block: u64, _count: u64) -> Result<(), Status> {
        Err(Status::NotSupported)
    }
}
