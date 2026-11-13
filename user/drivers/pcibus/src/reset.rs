//! Resetting a function before its driver is started again.

use crate::bus::{Bus, Function};
use crate::pci::{
    BAR0, BRIDGE_CONTROL, COMMAND, COMMAND_BUS_MASTER, COMMAND_IO, COMMAND_MEMORY, Config, Reset,
    VENDOR_ID, reset_method,
};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

/// The `Interrupt Line` register.
const INTERRUPT_LINE: u16 = 0x3C;
/// The bits that start a reset: `Initiate Function Level Reset` in the
/// PCI Express Device Control register and in the Advanced Features
/// control register, and the bridge control register's `Secondary Bus
/// Reset`.
const PCIE_INITIATE_FLR: u32 = 1 << 15;
const AF_INITIATE_FLR: u32 = 1 << 0;
const SECONDARY_BUS_RESET: u32 = 1 << 6;
/// The power states in the power management control register.
const POWER_STATE: u32 = 0b11;
const D3HOT: u32 = 0b11;
/// The vendor ID that a function answers with while it is not ready.
const RETRY: u32 = 0x0001;
const MS: i64 = 1_000_000;

/// Resets `function` of `bus`: disables its bus mastering and decoding,
/// resets it with the first method that `reset_method` finds, waits
/// 100 ms, and waits up to 1 s more for it to stop answering with
/// Configuration Request Retry Status. It then restores its BARs, its
/// `Interrupt Line` register and its command register without bus
/// mastering. The quarantine of its bus transaction initiator is
/// released in every case. Fails with `NOT_SUPPORTED` when no method
/// applies, and with `TIMED_OUT` when the function does not come back.
pub async fn reset(bus: &Bus, function: &Function) -> Result<(), Status> {
    let at = function.at;
    let mut config = &bus.ecam;
    let command = config.read(at, COMMAND, 2);
    let decoding = u32::from(COMMAND_IO | COMMAND_MEMORY | COMMAND_BUS_MASTER);
    config.write(at, COMMAND, 2, command & !decoding);
    let bars: [u32; 6] = core::array::from_fn(|i| config.read(at, BAR0 + 4 * i as u16, 4));
    let line = config.read(at, INTERRUPT_LINE, 1);
    let method = reset_method(&config, at, bus.ecam.bus_start());
    let result = match method {
        None => Err(Status::NotSupported),
        Some(method) => {
            start(bus, function, method).await;
            sleep(bus, 100 * MS).await;
            ready(bus, function).await
        }
    };
    if result.is_ok() {
        for (i, bar) in bars.iter().enumerate() {
            config.write(at, BAR0 + 4 * i as u16, 4, *bar);
        }
        config.write(at, INTERRUPT_LINE, 1, line);
        config.write(at, COMMAND, 2, command & !u32::from(COMMAND_BUS_MASTER));
    }
    if let Ok(bti) = &function.bti {
        // SAFETY: the call takes no pointers.
        unsafe { sys::bti_release_quarantine(bti.raw()) };
    }
    result
}

/// Starts `method` on `function`.
async fn start(bus: &Bus, function: &Function, method: Reset) {
    let at = function.at;
    let mut config = &bus.ecam;
    let mut set = |at, offset, width, bits: u32| {
        let value = config.read(at, offset, width);
        config.write(at, offset, width, value | bits);
    };
    match method {
        Reset::PcieFlr(cap) => set(at, cap + 8, 2, PCIE_INITIATE_FLR),
        Reset::AfFlr(cap) => set(at, cap + 4, 1, AF_INITIATE_FLR),
        Reset::PowerCycle(cap) => {
            let control = config.read(at, cap + 4, 2);
            config.write(at, cap + 4, 2, (control & !POWER_STATE) | D3HOT);
            sleep(bus, 10 * MS).await;
            config.write(at, cap + 4, 2, control & !POWER_STATE);
        }
        Reset::SecondaryBus(bridge) => {
            let control = config.read(bridge, BRIDGE_CONTROL, 2);
            config.write(bridge, BRIDGE_CONTROL, 2, control | SECONDARY_BUS_RESET);
            sleep(bus, 2 * MS).await;
            config.write(bridge, BRIDGE_CONTROL, 2, control & !SECONDARY_BUS_RESET);
        }
    }
}

/// Waits up to 1 s for `function` to answer with a vendor ID other than
/// that of Configuration Request Retry Status.
async fn ready(bus: &Bus, function: &Function) -> Result<(), Status> {
    let deadline = now() + 1000 * MS;
    loop {
        if (&bus.ecam).read(function.at, VENDOR_ID, 2) != RETRY {
            return Ok(());
        }
        if now() >= deadline {
            return Err(Status::TimedOut);
        }
        sleep(bus, 10 * MS).await;
    }
}

async fn sleep(bus: &Bus, ns: i64) {
    let _ = bus.reactor.sleep_until(now() + ns).await;
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
