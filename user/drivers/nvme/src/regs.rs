//! The controller's registers: its capabilities, enabling and disabling
//! it, shutting it down, and the doorbells.

use mikros_rt::exec::Reactor;
use mikros_storage::{MS, Mmio, now};

const CAP: u64 = 0x00;
const CC: u64 = 0x14;
const CSTS: u64 = 0x1C;
const AQA: u64 = 0x24;
const ASQ: u64 = 0x28;
const ACQ: u64 = 0x30;
/// Where the doorbells start.
const DOORBELLS: u64 = 0x1000;
/// `CC`: enable, the I/O submission and completion entry sizes as powers
/// of two (64 and 16 bytes), and a normal shutdown. The command set and
/// the memory page size fields stay 0: the NVM command set and 4 KiB.
const CC_EN: u32 = 1 << 0;
const CC_IOSQES: u32 = 6 << 16;
const CC_IOCQES: u32 = 4 << 20;
const CC_SHN_MASK: u32 = 3 << 14;
const CC_SHN_NORMAL: u32 = 1 << 14;
/// `CSTS`: ready, and the shutdown status field, with its value once
/// the shutdown is complete.
const CSTS_RDY: u32 = 1 << 0;
const CSTS_SHST_MASK: u32 = 3 << 2;
const CSTS_SHST_COMPLETE: u32 = 2 << 2;
/// What a register reads as when the device is gone.
const GONE: u32 = u32::MAX;

/// The controller did not reach the state it waited for in time.
#[derive(Clone, Copy, Debug)]
pub struct TimedOut;

/// The mapped registers, with what `CAP` says.
pub struct Regs {
    mmio: Mmio,
    /// `CAP.MQES`: the largest I/O queue, as entries minus 1.
    pub mqes: u16,
    /// The time `CAP.TO` gives for the controller's state changes, in
    /// nanoseconds.
    pub timeout: i64,
    /// The distance between doorbells, from `CAP.DSTRD`.
    stride: u64,
    /// The smallest memory page size, from `CAP.MPSMIN`, in which
    /// `MDTS` counts.
    pub page_min: u64,
}

impl Regs {
    pub fn new(mmio: Mmio) -> Regs {
        let cap = mmio.read64(CAP);
        Regs {
            mqes: cap as u16,
            timeout: ((cap >> 24) & 0xFF).max(1) as i64 * 500 * MS,
            stride: 4 << ((cap >> 32) & 0xF),
            page_min: 4096 << ((cap >> 48) & 0xF),
            mmio,
        }
    }

    /// Clears `CC.EN` and waits until `CSTS.RDY` clears.
    pub async fn disable(&self, reactor: &Reactor) -> Result<(), TimedOut> {
        let cc = self.mmio.read32(CC);
        self.mmio.write32(CC, cc & !CC_EN);
        self.wait(reactor, |csts| csts & CSTS_RDY == 0).await
    }

    /// Gives the controller the admin queues of `entries` entries at
    /// `submission` and `completion`, sets `CC` and `CC.EN`, and waits
    /// until `CSTS.RDY` is set.
    pub async fn enable(
        &self,
        reactor: &Reactor,
        entries: u16,
        submission: u64,
        completion: u64,
    ) -> Result<(), TimedOut> {
        let size = u32::from(entries - 1);
        self.mmio.write32(AQA, size << 16 | size);
        self.mmio.write64(ASQ, submission);
        self.mmio.write64(ACQ, completion);
        self.mmio.write32(CC, CC_IOCQES | CC_IOSQES);
        self.mmio.write32(CC, CC_IOCQES | CC_IOSQES | CC_EN);
        self.wait(reactor, |csts| csts & CSTS_RDY != 0).await
    }

    /// Asks for a normal shutdown through `CC.SHN` and waits until
    /// `CSTS.SHST` says that it is complete.
    pub async fn shutdown(&self, reactor: &Reactor) -> Result<(), TimedOut> {
        let cc = self.mmio.read32(CC);
        self.mmio.write32(CC, (cc & !CC_SHN_MASK) | CC_SHN_NORMAL);
        self.wait(reactor, |csts| csts & CSTS_SHST_MASK == CSTS_SHST_COMPLETE)
            .await
    }

    /// Waits until `CSTS` satisfies `done`, for at most `CAP.TO`.
    async fn wait(&self, reactor: &Reactor, done: impl Fn(u32) -> bool) -> Result<(), TimedOut> {
        let deadline = now() + self.timeout;
        loop {
            let csts = self.mmio.read32(CSTS);
            if csts != GONE && done(csts) {
                return Ok(());
            }
            if now() >= deadline {
                return Err(TimedOut);
            }
            let _ = reactor.sleep_until(now() + MS).await;
        }
    }

    /// Writes `tail` to the submission queue doorbell of queue `id`.
    pub fn ring_submission(&self, id: u16, tail: u16) {
        let at = DOORBELLS + 2 * u64::from(id) * self.stride;
        self.mmio.write32(at, u32::from(tail));
    }

    /// Writes `head` to the completion queue doorbell of queue `id`.
    pub fn ring_completion(&self, id: u16, head: u16) {
        let at = DOORBELLS + (2 * u64::from(id) + 1) * self.stride;
        self.mmio.write32(at, u32::from(head));
    }
}
