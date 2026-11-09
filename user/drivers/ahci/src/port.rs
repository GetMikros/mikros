//! A port: its command list, received-FIS area and command tables, setting
//! it up, issuing commands in its slots, taking their completions, and
//! recovering from errors with COMRESET.

use crate::hba::{TimedOut, wait};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::sync::atomic::{Ordering, fence};
use core::task::{Poll, Waker};
use mikros_ata::Ata;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;
use mikros_storage::{Contiguous, MS, Mmio, now};

/// The port registers, from the port's base.
const CLB: u64 = 0x00;
const FB: u64 = 0x08;
const IS: u64 = 0x10;
const IE: u64 = 0x14;
const CMD: u64 = 0x18;
const TFD: u64 = 0x20;
const SIG: u64 = 0x24;
const SSTS: u64 = 0x28;
const SCTL: u64 = 0x2C;
const SERR: u64 = 0x30;
const SACT: u64 = 0x34;
const CI: u64 = 0x38;
/// `PxCMD`: start, spin-up device, FIS receive enable, FIS receive
/// running, and command list running.
const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
/// `PxTFD`: busy and data request.
const TFD_BUSY: u32 = 1 << 7 | 1 << 3;
/// `PxIS` and `PxIE`: the errors that stop the port (task file, host bus
/// fatal, host bus data and interface fatal errors), and the interrupts
/// the driver takes: those, a D2H register FIS, a PIO setup FIS, a DMA
/// setup FIS, a set device bits FIS, and a descriptor processed.
const IS_FATAL: u32 = 1 << 30 | 1 << 29 | 1 << 28 | 1 << 27;
const IE_ALL: u32 = IS_FATAL | 1 << 5 | 1 << 3 | 1 << 2 | 1 << 1 | 1 << 0;
/// `PxSSTS`: an established connection, and the active state.
const DET_ESTABLISHED: u32 = 3;
const IPM_ACTIVE: u32 = 1 << 8;
/// The signature of a SATA disk.
const SIG_SATA: u32 = 0x0000_0101;
/// Where the command list, the received-FIS area and the command tables
/// lie in the port's memory; a command table's size; and the most PRD
/// entries in one table.
const COMMAND_LIST: u64 = 0;
const RECEIVED_FIS: u64 = 1024;
const TABLES: u64 = 2048;
pub const PRD_MAX: usize = 264;
const TABLE_SIZE: u64 = 0x80 + 16 * PRD_MAX as u64;
/// How long a port may take to stop, a link to come up, and the device
/// to stop being busy.
const STOP_LIMIT: i64 = 500 * MS;
const LINK_LIMIT: i64 = 1000 * MS;
const BUSY_LIMIT: i64 = 1000 * MS;
/// The type and command bit of a host to device register FIS.
const FIS_H2D: u8 = 0x27;
const FIS_COMMAND: u8 = 0x80;

/// What an interrupt found on a port.
pub enum Event {
    /// Nothing; its `PxIS` was clear.
    None,
    /// Commands completed.
    Handled,
    /// It reported an error; its commands have failed and it must be
    /// recovered.
    Error,
}

struct State {
    /// The slots that tasks hold, and those issued to the HBA and not
    /// complete.
    held: u32,
    outstanding: u32,
    /// Set while a command that is not queued holds the port, and while
    /// it is being recovered.
    exclusive: bool,
    recovering: bool,
    results: [Option<Result<(), Status>>; 32],
    wakers: [Option<Waker>; 32],
    waiting: Vec<Waker>,
}

pub struct Port {
    pub number: u32,
    hba: Rc<Mmio>,
    base: u64,
    memory: Contiguous,
    slots: u32,
    /// How many slots queued commands may use.
    depth: Cell<u32>,
    state: RefCell<State>,
}

impl Port {
    /// Port `number` of the HBA, with `slots` command slots and memory
    /// from the `DMA` resource `dma`, below 4 GiB when `below_4g` is true.
    pub fn new(
        number: u32,
        hba: Rc<Mmio>,
        slots: u32,
        dma: &OwnedHandle,
        bti: &OwnedHandle,
        process: sys::Handle,
        below_4g: bool,
    ) -> Result<Port, Status> {
        let size = TABLES + TABLE_SIZE * u64::from(slots);
        let memory = Contiguous::new(dma, bti, process, size, below_4g)?;
        for slot in 0..u64::from(slots) {
            let header = COMMAND_LIST + 32 * slot;
            memory.write64(header + 8, memory.physical(TABLES + TABLE_SIZE * slot));
        }
        Ok(Port {
            number,
            hba,
            base: 0x100 + 0x80 * u64::from(number),
            memory,
            slots,
            depth: Cell::new(1),
            state: RefCell::new(State {
                held: 0,
                outstanding: 0,
                exclusive: false,
                recovering: false,
                results: [None; 32],
                wakers: Default::default(),
                waiting: Vec::new(),
            }),
        })
    }

    fn read(&self, register: u64) -> u32 {
        self.hba.read32(self.base + register)
    }

    fn write(&self, register: u64, value: u32) {
        self.hba.write32(self.base + register, value);
    }

    /// Stops the port, clearing `PxCMD.ST` and `PxCMD.FRE` and waiting
    /// for `PxCMD.CR` and `PxCMD.FR` to clear; gives it its command list
    /// and received-FIS area; clears its errors and interrupts; and
    /// enables FIS reception and its interrupts.
    pub async fn set_up(&self, reactor: &Reactor, spin_up: bool) -> Result<(), TimedOut> {
        self.stop(reactor).await?;
        let (list, fis) = (
            self.memory.physical(COMMAND_LIST),
            self.memory.physical(RECEIVED_FIS),
        );
        self.hba.write64(self.base + CLB, list);
        self.hba.write64(self.base + FB, fis);
        self.write(SERR, u32::MAX);
        self.write(IS, u32::MAX);
        self.write(IE, IE_ALL);
        let mut cmd = self.read(CMD) | CMD_FRE;
        if spin_up {
            cmd |= CMD_SUD;
        }
        self.write(CMD, cmd);
        Ok(())
    }

    /// Stops the port: clears `PxCMD.ST` and waits for `PxCMD.CR` to
    /// clear, then clears `PxCMD.FRE` and waits for `PxCMD.FR` to clear.
    pub async fn stop(&self, reactor: &Reactor) -> Result<(), TimedOut> {
        self.write(CMD, self.read(CMD) & !CMD_ST);
        wait(reactor, STOP_LIMIT, || self.read(CMD) & CMD_CR == 0).await?;
        self.write(CMD, self.read(CMD) & !CMD_FRE);
        wait(reactor, STOP_LIMIT, || self.read(CMD) & CMD_FR == 0).await
    }

    /// Whether the link has a connection established.
    pub fn linked(&self) -> bool {
        self.read(SSTS) & 0xF == DET_ESTABLISHED
    }

    /// Starts the port once its device is not busy. A port whose device
    /// stays busy is left stopped.
    pub async fn start(&self, reactor: &Reactor) -> Result<(), TimedOut> {
        if self.linked() {
            wait(reactor, BUSY_LIMIT, || self.read(TFD) & TFD_BUSY == 0).await?;
        }
        self.write(CMD, self.read(CMD) | CMD_ST);
        Ok(())
    }

    /// Whether the port has a SATA disk: an established connection in the
    /// active state, with the signature of a disk.
    pub fn has_disk(&self) -> bool {
        let ssts = self.read(SSTS);
        ssts & 0xF == DET_ESTABLISHED && ssts & 0xF00 == IPM_ACTIVE && self.read(SIG) == SIG_SATA
    }

    /// Lets queued commands use `depth` slots.
    pub fn set_depth(&self, depth: u32) {
        self.depth.set(depth.clamp(1, self.slots));
    }

    /// Issues `ata`, which moves the bytes at the physical addresses and
    /// lengths `data` to the device when `write` is true and from it
    /// otherwise, and waits until it completes. A `queued` command, of
    /// native command queuing, gets its tag in the FIS's count field and
    /// runs alongside other queued commands; any other command runs on
    /// its own.
    pub async fn issue(
        &self,
        ata: Ata,
        data: &[(u64, u32)],
        write: bool,
        queued: bool,
    ) -> Result<(), Status> {
        if data.len() > PRD_MAX {
            return Err(Status::InvalidArgs);
        }
        let slot = poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            let free = if state.recovering || state.exclusive {
                None
            } else if queued {
                (0..self.depth.get()).find(|s| state.held & 1 << s == 0)
            } else if state.held == 0 {
                Some(0)
            } else {
                None
            };
            match free {
                Some(slot) => {
                    state.held |= 1 << slot;
                    state.exclusive = !queued;
                    state.results[slot as usize] = None;
                    Poll::Ready(slot)
                }
                None => {
                    state.waiting.push(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await;
        self.fill(slot, ata, data, write, queued);
        {
            let mut state = self.state.borrow_mut();
            state.outstanding |= 1 << slot;
            fence(Ordering::SeqCst);
            if queued {
                self.write(SACT, 1 << slot);
            }
            self.write(CI, 1 << slot);
        }
        let result = poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            match state.results[slot as usize] {
                Some(result) => Poll::Ready(result),
                None => {
                    state.wakers[slot as usize] = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await;
        let waiting = {
            let mut state = self.state.borrow_mut();
            state.held &= !(1 << slot);
            if !queued {
                state.exclusive = false;
            }
            core::mem::take(&mut state.waiting)
        };
        for waker in waiting {
            waker.wake();
        }
        result
    }

    /// Writes the command header and table of `slot`.
    fn fill(&self, slot: u32, ata: Ata, data: &[(u64, u32)], write: bool, queued: bool) {
        let m = &self.memory;
        let header = COMMAND_LIST + 32 * u64::from(slot);
        // The FIS is 5 dwords.
        let dw0 = 5 | u32::from(write) << 6 | (data.len() as u32) << 16;
        m.write32(header, dw0);
        m.write32(header + 4, 0);
        let table = TABLES + TABLE_SIZE * u64::from(slot);
        let count = if queued {
            (slot as u16) << 3
        } else {
            ata.count
        };
        let fis = [
            FIS_H2D,
            FIS_COMMAND,
            ata.command,
            ata.features as u8,
            ata.lba as u8,
            (ata.lba >> 8) as u8,
            (ata.lba >> 16) as u8,
            ata.device,
            (ata.lba >> 24) as u8,
            (ata.lba >> 32) as u8,
            (ata.lba >> 40) as u8,
            (ata.features >> 8) as u8,
            count as u8,
            (count >> 8) as u8,
            0,
            0,
        ];
        for (i, byte) in fis.iter().enumerate() {
            m.write8(table + i as u64, *byte);
        }
        for i in fis.len() as u64..20 {
            m.write8(table + i, 0);
        }
        for (i, (address, len)) in data.iter().enumerate() {
            let entry = table + 0x80 + 16 * i as u64;
            m.write64(entry, *address);
            m.write32(entry + 8, 0);
            m.write32(entry + 12, len - 1);
        }
    }

    /// Takes what the port reports: clears `PxIS`, and completes the
    /// commands that are done, or fails all outstanding ones when the
    /// port reports an error.
    pub fn interrupt(&self) -> Event {
        let is = self.read(IS);
        if is == 0 {
            return Event::None;
        }
        self.write(IS, is);
        let mut state = self.state.borrow_mut();
        let failed = is & IS_FATAL != 0;
        let done = match failed {
            true => state.outstanding,
            false => state.outstanding & !(self.read(CI) | self.read(SACT)),
        };
        for slot in (0..32).filter(|s| done & 1 << s != 0) {
            state.results[slot] = Some(if failed {
                Err(Status::Internal)
            } else {
                Ok(())
            });
            if let Some(waker) = state.wakers[slot].take() {
                waker.wake();
            }
        }
        state.outstanding &= !done;
        if failed {
            state.recovering = true;
            Event::Error
        } else {
            Event::Handled
        }
    }

    /// Recovers the port after an error: stops it, resets it with
    /// COMRESET through `PxSCTL`, and sets it up and starts it again.
    pub async fn recover(&self, reactor: &Reactor, spin_up: bool) {
        self.write(CMD, self.read(CMD) & !CMD_ST);
        let _ = wait(reactor, STOP_LIMIT, || self.read(CMD) & CMD_CR == 0).await;
        let sctl = self.read(SCTL) & !0xF;
        self.write(SCTL, sctl | 1);
        let _ = reactor.sleep_until(now() + 2 * MS).await;
        self.write(SCTL, sctl);
        let _ = wait(reactor, LINK_LIMIT, || self.linked()).await;
        if self.set_up(reactor, spin_up).await.is_ok() {
            let _ = self.start(reactor).await;
        }
        let waiting = {
            let mut state = self.state.borrow_mut();
            state.recovering = false;
            core::mem::take(&mut state.waiting)
        };
        for waker in waiting {
            waker.wake();
        }
    }
}

/// Waits until every port in `ports` has a connection established, for
/// at most `LINK_LIMIT`.
pub async fn wait_for_links(ports: &[Port], reactor: &Reactor) {
    let _ = wait(reactor, LINK_LIMIT, || ports.iter().all(Port::linked)).await;
}
