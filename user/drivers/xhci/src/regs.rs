//! The controller's registers: the capability, operational and runtime
//! registers of interrupter 0, the port registers, the doorbells, and
//! the extended capabilities.

use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_storage::{MS, Mmio, now};

/// Capability registers.
const CAPLENGTH: u64 = 0x00;
const HCSPARAMS1: u64 = 0x04;
const HCSPARAMS2: u64 = 0x08;
const HCCPARAMS1: u64 = 0x10;
const DBOFF: u64 = 0x14;
const RTSOFF: u64 = 0x18;
/// Operational registers, from `CAPLENGTH`.
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;
const PORTSC: u64 = 0x400;
/// Interrupter 0's registers, from the runtime registers.
const IMAN: u64 = 0x20;
const ERSTSZ: u64 = 0x28;
const ERSTBA: u64 = 0x30;
const ERDP: u64 = 0x38;

pub const USBCMD_RS: u32 = 1 << 0;
pub const USBCMD_HCRST: u32 = 1 << 1;
pub const USBCMD_INTE: u32 = 1 << 2;
pub const USBSTS_HCH: u32 = 1 << 0;
pub const USBSTS_EINT: u32 = 1 << 3;
pub const USBSTS_CNR: u32 = 1 << 11;
const IMAN_IP: u32 = 1 << 0;
const IMAN_IE: u32 = 1 << 1;
const ERDP_EHB: u64 = 1 << 3;
/// The command ring's cycle state, in `CRCR`.
const CRCR_RCS: u64 = 1 << 0;

/// `PORTSC` bits.
pub const PORT_CCS: u32 = 1 << 0;
pub const PORT_PED: u32 = 1 << 1;
pub const PORT_PR: u32 = 1 << 4;
pub const PORT_PP: u32 = 1 << 9;
pub const PORT_CSC: u32 = 1 << 17;
pub const PORT_PRC: u32 = 1 << 21;
/// The change bits, which writing 1 clears.
pub const PORT_CHANGES: u32 = 0x7F << 17;
/// The bits that a write must give back as they are: port power, the
/// port indicator and the wake enables. The others either read as 0
/// when written as 0, or act when written as 1.
const PORT_KEEP: u32 = PORT_PP | 3 << 14 | 7 << 25;

/// Extended capability IDs.
const CAP_LEGACY: u32 = 1;
const CAP_PROTOCOL: u32 = 2;
/// `USBLEGSUP`: the BIOS and OS owned semaphores.
const LEGACY_BIOS_OWNED: u32 = 1 << 16;
const LEGACY_OS_OWNED: u32 = 1 << 24;
/// `USBLEGCTLSTS`: the SMI enable bits.
const LEGACY_SMI_ENABLES: u32 = 1 << 0 | 1 << 4 | 1 << 13 | 1 << 14 | 1 << 15;

/// How long the controller may take to halt, reset or become ready, and
/// firmware to give up the controller.
const STATE_LIMIT: i64 = 1000 * MS;

/// The controller did not reach the state it waited for in time.
#[derive(Clone, Copy, Debug)]
pub struct TimedOut;

/// The root hub ports that one supported protocol capability describes:
/// its major USB revision, and its first port, from 1, and port count.
#[derive(Clone, Copy, Debug)]
pub struct Protocol {
    pub major: u8,
    pub first: u8,
    pub count: u8,
}

/// The mapped registers, with what the capability registers say.
pub struct Regs {
    mmio: Mmio,
    op: u64,
    runtime: u64,
    doorbells: u64,
    pub max_slots: u8,
    pub max_ports: u8,
    /// The scratchpad buffers the controller asks for.
    pub scratchpads: u32,
    /// `HCCPARAMS1.AC64`, and the context size from `HCCPARAMS1.CSZ`.
    pub ac64: bool,
    pub context_size: u64,
    /// Where the extended capabilities start, 0 when there are none.
    extended: u64,
}

impl Regs {
    pub fn new(mmio: Mmio) -> Regs {
        let caplength = u64::from(mmio.read32(CAPLENGTH) as u8);
        let hcs1 = mmio.read32(HCSPARAMS1);
        let hcs2 = mmio.read32(HCSPARAMS2);
        let hcc1 = mmio.read32(HCCPARAMS1);
        Regs {
            op: caplength,
            runtime: u64::from(mmio.read32(RTSOFF) & !0x1F),
            doorbells: u64::from(mmio.read32(DBOFF) & !0x3),
            max_slots: hcs1 as u8,
            max_ports: (hcs1 >> 24) as u8,
            scratchpads: (hcs2 >> 27) & 0x1F | ((hcs2 >> 21) & 0x1F) << 5,
            ac64: hcc1 & 1 != 0,
            context_size: if hcc1 & 1 << 2 != 0 { 64 } else { 32 },
            extended: u64::from(hcc1 >> 16) * 4,
            mmio,
        }
    }

    pub fn command(&self) -> u32 {
        self.mmio.read32(self.op + USBCMD)
    }

    pub fn set_command(&self, value: u32) {
        self.mmio.write32(self.op + USBCMD, value);
    }

    pub fn status(&self) -> u32 {
        self.mmio.read32(self.op + USBSTS)
    }

    /// Clears the status bits of `bits` that writing 1 clears.
    pub fn clear_status(&self, bits: u32) {
        self.mmio.write32(self.op + USBSTS, bits);
    }

    /// The extended capabilities: the offset and first dword of each.
    fn capabilities(&self) -> Vec<(u64, u32)> {
        let mut found = Vec::new();
        let mut at = self.extended;
        while at != 0 && at + 4 <= self.mmio.len() && found.len() < 256 {
            let dword = self.mmio.read32(at);
            found.push((at, dword));
            match (dword >> 8) & 0xFF {
                0 => break,
                next => at += u64::from(next) * 4,
            }
        }
        found
    }

    /// Takes the controller over from firmware through its USB legacy
    /// support capability, when it has one: sets the OS owned
    /// semaphore, waits for firmware to clear the BIOS owned one, and
    /// disables the SMIs. Returns false when firmware did not let go in
    /// time; the SMIs are disabled anyway.
    pub async fn take_over(&self, reactor: &Reactor) -> bool {
        let Some((at, _)) = self
            .capabilities()
            .into_iter()
            .find(|(_, dword)| dword & 0xFF == CAP_LEGACY)
        else {
            return true;
        };
        let legsup = self.mmio.read32(at);
        self.mmio.write32(at, legsup | LEGACY_OS_OWNED);
        let released = self
            .wait(reactor, || self.mmio.read32(at) & LEGACY_BIOS_OWNED == 0)
            .await
            .is_ok();
        let control = self.mmio.read32(at + 4);
        // The status bits above the enables clear when written as 1, so
        // they are written as 0.
        self.mmio
            .write32(at + 4, control & !LEGACY_SMI_ENABLES & 0x1FFF_FFFF);
        released
    }

    /// The supported protocol capabilities.
    pub fn protocols(&self) -> Vec<Protocol> {
        let caps = self.capabilities().into_iter();
        caps.filter(|(_, dword)| dword & 0xFF == CAP_PROTOCOL)
            .map(|(at, dword)| {
                let ports = self.mmio.read32(at + 8);
                Protocol {
                    major: (dword >> 24) as u8,
                    first: ports as u8,
                    count: (ports >> 8) as u8,
                }
            })
            .collect()
    }

    /// Clears `USBCMD.RS` and waits for `USBSTS.HCH`.
    pub async fn halt(&self, reactor: &Reactor) -> Result<(), TimedOut> {
        self.set_command(self.command() & !USBCMD_RS);
        self.wait(reactor, || self.status() & USBSTS_HCH != 0).await
    }

    /// Sets `USBCMD.HCRST` and waits until it and `USBSTS.CNR` clear.
    pub async fn reset(&self, reactor: &Reactor) -> Result<(), TimedOut> {
        self.set_command(USBCMD_HCRST);
        // Some controllers hang when their registers are read right
        // after the reset starts.
        let _ = reactor.sleep_until(now() + MS).await;
        self.wait(reactor, || {
            self.command() & USBCMD_HCRST == 0 && self.status() & USBSTS_CNR == 0
        })
        .await
    }

    /// Enables `slots` device slots, and gives the controller the device
    /// context base address array at `dcbaa` and the command ring at
    /// `ring`, whose cycle state starts as 1.
    pub fn configure(&self, slots: u8, dcbaa: u64, ring: u64) {
        let config = self.mmio.read32(self.op + CONFIG);
        self.mmio
            .write32(self.op + CONFIG, config & !0xFF | u32::from(slots));
        self.mmio.write64(self.op + DCBAAP, dcbaa);
        self.mmio.write64(self.op + CRCR, ring | CRCR_RCS);
    }

    /// Gives interrupter 0 the event ring segment table of one entry at
    /// `table`, whose segment starts at `ring`, and enables its
    /// interrupts.
    pub fn set_event_ring(&self, table: u64, ring: u64) {
        let base = self.runtime;
        self.mmio.write32(base + ERSTSZ, 1);
        self.mmio.write64(base + ERDP, ring);
        self.mmio.write64(base + ERSTBA, table);
        self.mmio.write32(base + IMAN, IMAN_IE | IMAN_IP);
    }

    /// Clears interrupter 0's pending bit.
    pub fn clear_pending(&self) {
        self.mmio.write32(self.runtime + IMAN, IMAN_IE | IMAN_IP);
    }

    /// Moves interrupter 0's event ring dequeue pointer to `at` and
    /// clears its event handler busy bit.
    pub fn set_dequeue(&self, at: u64) {
        self.mmio.write64(self.runtime + ERDP, at | ERDP_EHB);
    }

    /// The `PORTSC` of root hub port `port`, from 1.
    pub fn port(&self, port: u8) -> u32 {
        self.mmio.read32(self.portsc(port))
    }

    /// Writes `set` to `PORTSC` of `port`, with the bits that must be
    /// kept as they are, which clears the change bits of `set` and acts
    /// on its others.
    pub fn write_port(&self, port: u8, set: u32) {
        let keep = self.port(port) & PORT_KEEP;
        self.mmio.write32(self.portsc(port), keep | set);
    }

    fn portsc(&self, port: u8) -> u64 {
        self.op + PORTSC + 0x10 * (u64::from(port) - 1)
    }

    /// Rings doorbell `slot`, 0 for the command ring, with `target`.
    pub fn ring(&self, slot: u8, target: u8) {
        let at = self.doorbells + 4 * u64::from(slot);
        self.mmio.write32(at, u32::from(target));
    }

    /// Waits until `done` returns true, for at most a second.
    pub async fn wait(&self, reactor: &Reactor, done: impl Fn() -> bool) -> Result<(), TimedOut> {
        let deadline = now() + STATE_LIMIT;
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
}
