//! A port: its command list, received-FIS area and command tables,
//! setting it up, starting it, and taking its interrupts.

use crate::hba::{TimedOut, wait};
use alloc::rc::Rc;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;
use mikros_storage::{Contiguous, MS, Mmio};

/// The port registers, from the port's base.
const CLB: u64 = 0x00;
const FB: u64 = 0x08;
const IS: u64 = 0x10;
const IE: u64 = 0x14;
const CMD: u64 = 0x18;
const TFD: u64 = 0x20;
const SSTS: u64 = 0x28;
const SERR: u64 = 0x30;
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
/// `PxSSTS`: an established connection.
const DET_ESTABLISHED: u32 = 3;
/// Where the command list, the received-FIS area and the command tables
/// lie in the port's memory; a command table's size; and the most PRD
/// entries in one table.
const COMMAND_LIST: u64 = 0;
const RECEIVED_FIS: u64 = 1024;
const TABLES: u64 = 2048;
const PRD_MAX: usize = 264;
const TABLE_SIZE: u64 = 0x80 + 16 * PRD_MAX as u64;
/// How long a port may take to stop, a link to come up, and the device
/// to stop being busy.
const STOP_LIMIT: i64 = 500 * MS;
const LINK_LIMIT: i64 = 1000 * MS;
const BUSY_LIMIT: i64 = 1000 * MS;

pub struct Port {
    pub number: u32,
    hba: Rc<Mmio>,
    base: u64,
    memory: Contiguous,
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
        self.write(CMD, self.read(CMD) & !CMD_ST);
        wait(reactor, STOP_LIMIT, || self.read(CMD) & CMD_CR == 0).await?;
        self.write(CMD, self.read(CMD) & !CMD_FRE);
        wait(reactor, STOP_LIMIT, || self.read(CMD) & CMD_FR == 0).await?;
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

    /// Takes what the port reports: clears `PxIS`, and says whether it
    /// had anything set.
    pub fn interrupt(&self) -> bool {
        let is = self.read(IS);
        if is != 0 {
            self.write(IS, is);
        }
        is != 0
    }
}

/// Waits until every port in `ports` has a connection established, for
/// at most `LINK_LIMIT`.
pub async fn wait_for_links(ports: &[Port], reactor: &Reactor) {
    let _ = wait(reactor, LINK_LIMIT, || ports.iter().all(Port::linked)).await;
}
