//! The HBA's global registers: taking it over from firmware, resetting
//! it, and its capabilities.

use mikros_rt::exec::Reactor;
use mikros_storage::{MS, Mmio, now};

const CAP: u64 = 0x00;
const GHC: u64 = 0x04;
pub const IS: u64 = 0x08;
const PI: u64 = 0x0C;
const CAP2: u64 = 0x24;
const BOHC: u64 = 0x28;
/// `CAP`: 64-bit addressing, native command queuing, staggered spin-up,
/// and the number of command slots minus 1.
const CAP_S64A: u32 = 1 << 31;
const CAP_SNCQ: u32 = 1 << 30;
const CAP_SSS: u32 = 1 << 27;
const CAP_NCS_SHIFT: u32 = 8;
/// `GHC`: AHCI enable, interrupt enable, and HBA reset.
const GHC_AE: u32 = 1 << 31;
const GHC_IE: u32 = 1 << 1;
const GHC_HR: u32 = 1 << 0;
/// `CAP2.BOH`, and `BOHC`'s OS and BIOS ownership bits.
const CAP2_BOH: u32 = 1 << 0;
const BOHC_OOS: u32 = 1 << 1;
const BOHC_BOS: u32 = 1 << 0;
/// How long the firmware may take to hand over, and the HBA to reset.
const HANDOFF_LIMIT: i64 = 2000 * MS;
const RESET_LIMIT: i64 = 1000 * MS;

/// The HBA did not reach the state it waited for in time.
#[derive(Clone, Copy, Debug)]
pub struct TimedOut;

/// What `CAP` says.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub addressing_64: bool,
    pub ncq: bool,
    pub staggered_spin_up: bool,
    pub slots: u32,
}

/// Waits until `done` holds, polling once a millisecond, for at most
/// `limit` nanoseconds.
pub async fn wait(reactor: &Reactor, limit: i64, done: impl Fn() -> bool) -> Result<(), TimedOut> {
    let deadline = now() + limit;
    loop {
        if done() {
            return Ok(());
        }
        if now() >= deadline {
            return Err(TimedOut);
        }
        let _ = reactor.sleep_until(now() + MS).await;
    }
}

pub fn capabilities(mmio: &Mmio) -> Capabilities {
    let cap = mmio.read32(CAP);
    Capabilities {
        addressing_64: cap & CAP_S64A != 0,
        ncq: cap & CAP_SNCQ != 0,
        staggered_spin_up: cap & CAP_SSS != 0,
        slots: ((cap >> CAP_NCS_SHIFT) & 0x1F) + 1,
    }
}

/// The ports that `PI` lists.
pub fn ports(mmio: &Mmio) -> impl Iterator<Item = u32> {
    let pi = mmio.read32(PI);
    (0..32).filter(move |p| pi & 1 << p != 0)
}

/// Takes the HBA over from the firmware when it supports BIOS/OS
/// handoff: sets `BOHC.OOS` and waits up to 2 seconds for the firmware
/// to stop using it. Going on after the wait is how the handoff ends
/// when the firmware does not answer.
pub async fn take_over(mmio: &Mmio, reactor: &Reactor) {
    if mmio.read32(CAP2) & CAP2_BOH == 0 {
        return;
    }
    mmio.write32(BOHC, mmio.read32(BOHC) | BOHC_OOS);
    let _ = wait(reactor, HANDOFF_LIMIT, || mmio.read32(BOHC) & BOHC_BOS == 0).await;
}

/// Sets `GHC.AE`, resets the HBA with `GHC.HR`, and sets `GHC.AE` again.
pub async fn reset(mmio: &Mmio, reactor: &Reactor) -> Result<(), TimedOut> {
    mmio.write32(GHC, mmio.read32(GHC) | GHC_AE);
    mmio.write32(GHC, mmio.read32(GHC) | GHC_HR);
    wait(reactor, RESET_LIMIT, || mmio.read32(GHC) & GHC_HR == 0).await?;
    mmio.write32(GHC, mmio.read32(GHC) | GHC_AE);
    Ok(())
}

/// Lets the HBA raise interrupts.
pub fn enable_interrupts(mmio: &Mmio) {
    mmio.write32(GHC, mmio.read32(GHC) | GHC_IE);
}
