//! The controller: bringing it up, its admin and I/O queue pairs, its
//! interrupts, identifying it and its namespaces, the command timeout,
//! resetting it, and shutting it down.

use crate::queue::{Abort, Command, Failure, QueuePair};
use crate::regs::Regs;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::sys;
use mikros_storage::{Contiguous, MS, Mmio, PAGE, now};

/// Admin opcodes.
const CREATE_IO_SQ: u8 = 0x01;
const CREATE_IO_CQ: u8 = 0x05;
const IDENTIFY: u8 = 0x06;
const ABORT: u8 = 0x08;
const SET_FEATURES: u8 = 0x09;
/// Identify's CNS values, and the feature of the number of queues.
const CNS_NAMESPACE: u32 = 0x00;
const CNS_CONTROLLER: u32 = 0x01;
const CNS_ACTIVE_NAMESPACES: u32 = 0x02;
const FEATURE_QUEUES: u32 = 0x07;
/// The admin queues' entries, and the most I/O queue entries.
const ADMIN_ENTRIES: u16 = 32;
const IO_ENTRIES_MAX: u32 = 1024;
/// The ID of the I/O queue pair.
const IO_QUEUE: u16 = 1;
/// How long a command may take before it is aborted, and how often the
/// driver looks for such commands.
pub const COMMAND_LIMIT: i64 = 30_000 * MS;
const WATCH_PERIOD: i64 = 1000 * MS;
/// The largest transfer, which a PRP list of one page always covers.
const TRANSFER_MAX: u64 = 2 << 20;
const SIGNALED: u32 = 1 << 3;

/// What Identify Controller says.
pub struct Identity {
    pub serial: String,
    pub model: String,
    /// The largest transfer in bytes.
    pub transfer_max: u64,
    /// Whether it has a volatile write cache, and supports Dataset
    /// Management.
    pub write_cache: bool,
    pub dataset_management: bool,
}

/// An active namespace, from Identify Namespace.
pub struct NamespaceInfo {
    pub nsid: u32,
    pub blocks: u64,
    /// The data size of the current LBA format, and its metadata size.
    pub block_size: u64,
    pub metadata: u16,
}

pub struct Controller {
    pub regs: Rc<Regs>,
    pub admin: Rc<QueuePair>,
    pub io: Rc<QueuePair>,
    pub identity: Identity,
    /// The interrupt vector of the I/O completion queue.
    io_vector: u16,
    pub dma: OwnedHandle,
    pub bti: OwnedHandle,
    process: sys::Handle,
    /// Pages for PRP lists and Dataset Management ranges.
    pages: RefCell<Vec<Contiguous>>,
    resetting: Cell<bool>,
    /// Set when a reset could not bring the I/O queues back.
    pub failed: Cell<bool>,
    _device: Rc<DeviceAsyncClient>,
    _interrupts: Vec<OwnedHandle>,
}

fn bus<T>(
    reply: Result<Result<T, Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Status> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::Internal),
    }
}

/// The text of an identify string field: ASCII, padded with spaces.
fn text(bytes: &[u8]) -> String {
    let text: String = bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                ' '
            }
        })
        .collect();
    String::from(text.trim())
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

impl Controller {
    /// Brings up the controller behind the bus channel `device` with
    /// memory from the `DMA` resource `dma`: maps BAR 0, disables it, sets
    /// up the admin queues, enables it, takes its interrupts, identifies
    /// it and creates the I/O queues.
    pub async fn start(
        device: DeviceAsyncClient,
        process: sys::Handle,
        dma: OwnedHandle,
        reactor: &Reactor,
        spawner: &Spawner,
    ) -> Result<Rc<Controller>, Status> {
        let device = Rc::new(device);
        let bar = bus(device.get_bar(0).await)?.bar;
        let regs = Rc::new(Regs::new(Mmio::map(
            process,
            &bar.resource,
            bar.base,
            bar.size,
        )?));
        bus(device.set_bus_master(true).await)?;
        let bti = bus(device.get_bti().await)?.bti;
        regs.disable(reactor).await.map_err(|_| Status::Internal)?;
        let admin = Rc::new(QueuePair::new(
            0,
            ADMIN_ENTRIES,
            regs.clone(),
            &dma,
            &bti,
            process,
        )?);
        let entries = (u32::from(regs.mqes) + 1).min(IO_ENTRIES_MAX) as u16;
        let io = Rc::new(QueuePair::new(
            IO_QUEUE,
            entries,
            regs.clone(),
            &dma,
            &bti,
            process,
        )?);
        let (interrupts, io_vector) = interrupts(&device, &admin, &io, reactor, spawner).await?;
        enable(&regs, &admin, reactor).await?;
        let page = Contiguous::new(&dma, &bti, process, PAGE, false)?;
        let identity = identify_controller(&admin, &page, regs.page_min).await?;
        let controller = Rc::new(Controller {
            regs,
            admin,
            io,
            identity,
            io_vector,
            dma,
            bti,
            process,
            pages: RefCell::new(alloc::vec![page]),
            resetting: Cell::new(false),
            failed: Cell::new(false),
            _device: device,
            _interrupts: interrupts,
        });
        controller.create_io_queues().await?;
        spawner.spawn(watch(controller.clone(), reactor.clone(), spawner.clone()));
        Ok(controller)
    }

    /// Asks for one I/O queue pair, and creates its completion queue and
    /// then its submission queue.
    async fn create_io_queues(&self) -> Result<(), Status> {
        let io = &self.io;
        let size = u32::from(io.entries - 1) << 16 | u32::from(io.id);
        let set = Command {
            opcode: SET_FEATURES,
            cdw: [FEATURE_QUEUES, 0, 0, 0, 0, 0],
            ..Command::default()
        };
        self.admin.submit(set).await?;
        let cq = Command {
            opcode: CREATE_IO_CQ,
            prp1: io.completion.physical(0),
            // Interrupts enabled, physically contiguous.
            cdw: [size, u32::from(self.io_vector) << 16 | 0b11, 0, 0, 0, 0],
            ..Command::default()
        };
        self.admin.submit(cq).await?;
        let sq = Command {
            opcode: CREATE_IO_SQ,
            prp1: io.submission.physical(0),
            // Its completion queue, physically contiguous.
            cdw: [size, u32::from(io.id) << 16 | 0b1, 0, 0, 0, 0],
            ..Command::default()
        };
        self.admin.submit(sq).await?;
        Ok(())
    }

    /// The active namespaces.
    pub async fn namespaces(&self) -> Result<Vec<NamespaceInfo>, Status> {
        let page = self.page()?;
        let mut ids = Vec::new();
        let mut after = 0;
        loop {
            identify(&self.admin, &page, CNS_ACTIVE_NAMESPACES, after).await?;
            let mut bytes = [0u8; PAGE as usize];
            page.read_bytes(0, &mut bytes);
            let found: Vec<u32> = (0..1024)
                .map(|i| le32(&bytes, 4 * i))
                .take_while(|&nsid| nsid != 0)
                .collect();
            let full = found.len() == 1024;
            ids.extend_from_slice(&found);
            match found.last() {
                Some(&last) if full => after = last,
                _ => break,
            }
        }
        let mut namespaces = Vec::new();
        for nsid in ids {
            identify(&self.admin, &page, CNS_NAMESPACE, nsid).await?;
            let blocks = page.read64(0);
            let flbas = page.read8(26);
            let format = u64::from(flbas & 0xF | (flbas >> 1) & 0x30);
            let lbaf = page.read32(128 + 4 * format);
            namespaces.push(NamespaceInfo {
                nsid,
                blocks,
                block_size: 1u64.checked_shl((lbaf >> 16) & 0xFF).unwrap_or(0),
                metadata: lbaf as u16,
            });
        }
        self.give_page(page);
        Ok(namespaces)
    }

    /// A zeroed page for a PRP list or Dataset Management ranges.
    pub fn page(&self) -> Result<Contiguous, Status> {
        match self.pages.borrow_mut().pop() {
            Some(page) => {
                page.zero();
                Ok(page)
            }
            None => Contiguous::new(&self.dma, &self.bti, self.process, PAGE, false),
        }
    }

    /// Gives back a page from `page`.
    pub fn give_page(&self, page: Contiguous) {
        self.pages.borrow_mut().push(page);
    }

    /// The largest transfer in bytes.
    pub fn transfer_max(&self) -> u64 {
        self.identity.transfer_max
    }

    /// Resets the controller and enables it again, failing every command
    /// in flight, and creates the I/O queues again. Sets `failed` when
    /// that does not work.
    async fn reset(&self, reactor: &Reactor) {
        if self.resetting.replace(true) {
            return;
        }
        self.io.pause();
        self.admin.pause();
        let _ = self.regs.disable(reactor).await;
        self.admin.restart();
        self.io.restart();
        let enabled = enable(&self.regs, &self.admin, reactor).await;
        self.admin.resume();
        let created = match enabled {
            Ok(()) => self.create_io_queues().await,
            Err(status) => Err(status),
        };
        self.failed.set(created.is_err());
        if created.is_ok() {
            self.io.resume();
        }
        self.resetting.set(false);
    }

    /// Shuts the controller down normally, before the driver ends.
    pub async fn shutdown(&self, reactor: &Reactor) {
        let _ = self.regs.shutdown(reactor).await;
    }
}

/// Enables the controller with the admin queues of `admin`.
async fn enable(regs: &Regs, admin: &QueuePair, reactor: &Reactor) -> Result<(), Status> {
    let (sq, cq) = (admin.submission.physical(0), admin.completion.physical(0));
    regs.enable(reactor, admin.entries, sq, cq)
        .await
        .map_err(|_| Status::Internal)
}

/// Sends Identify with `cns` for `nsid`, into `page`.
async fn identify(admin: &QueuePair, page: &Contiguous, cns: u32, nsid: u32) -> Result<(), Status> {
    let command = Command {
        opcode: IDENTIFY,
        nsid,
        prp1: page.physical(0),
        cdw: [cns, 0, 0, 0, 0, 0],
        ..Command::default()
    };
    admin.submit(command).await?;
    Ok(())
}

/// Sends Identify Controller. The largest transfer is the smaller of
/// 2 MiB and the `MDTS` limit, which counts in pages of `page_min`
/// bytes; an `MDTS` of 0 sets no limit of its own.
async fn identify_controller(
    admin: &QueuePair,
    page: &Contiguous,
    page_min: u64,
) -> Result<Identity, Status> {
    identify(admin, page, CNS_CONTROLLER, 0).await?;
    let mut bytes = [0u8; PAGE as usize];
    page.read_bytes(0, &mut bytes);
    let mdts = bytes[77];
    let transfer_max = match mdts {
        0 => TRANSFER_MAX,
        // 4 KiB << 9 is already 2 MiB, so larger shifts change nothing.
        n => (page_min << n.min(9)).min(TRANSFER_MAX),
    };
    let oncs = u16::from_le_bytes([bytes[520], bytes[521]]);
    Ok(Identity {
        serial: text(&bytes[4..24]),
        model: text(&bytes[24..64]),
        transfer_max,
        write_cache: bytes[525] & 1 != 0,
        dataset_management: oncs & 1 << 2 != 0,
    })
}

/// Takes the controller's interrupts: one per completion queue through
/// `EnableMsi`, the I/O queue sharing the admin queue's when there is
/// only one, or else the INTx event. Each interrupt processes its
/// completion queues and is then acknowledged. Returns the handles to
/// keep and the I/O queue's vector.
async fn interrupts(
    device: &Rc<DeviceAsyncClient>,
    admin: &Rc<QueuePair>,
    io: &Rc<QueuePair>,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Vec<OwnedHandle>, u16), Status> {
    if let Ok(Ok(enabled)) = device.enable_msi(2).await {
        let list = enabled.interrupts;
        let io_vector = list.len().min(2) as u16 - 1;
        for (vector, interrupt) in list.iter().enumerate() {
            let mut mine = Vec::new();
            if vector == 0 {
                mine.push(admin.clone());
            }
            if vector == usize::from(io_vector) {
                mine.push(io.clone());
            }
            let raw = interrupt.raw();
            let reactor = reactor.clone();
            spawner.spawn(async move {
                while reactor.signals(raw, SIGNALED).await.is_ok() {
                    for queue in &mine {
                        queue.process();
                    }
                    // SAFETY: the call takes no pointers.
                    unsafe { sys::interrupt_ack(raw) };
                }
            });
        }
        return Ok((list, io_vector));
    }
    let event = bus(device.get_interrupt().await)?.event;
    let (device, queues) = (device.clone(), [admin.clone(), io.clone()]);
    let raw = event.raw();
    let reactor = reactor.clone();
    spawner.spawn(async move {
        while reactor.signals(raw, SIGNALED).await.is_ok() {
            for queue in &queues {
                queue.process();
            }
            if device.ack_interrupt().await.is_err() {
                return;
            }
        }
    });
    Ok((alloc::vec![event], 0))
}

/// Looks for commands in flight for longer than `COMMAND_LIMIT` once a
/// period. Each one is aborted. When the abort fails, does not report
/// the command aborted, or does not complete in time itself, or the
/// command still has not completed `COMMAND_LIMIT` after its abort, the
/// controller is reset.
async fn watch(controller: Rc<Controller>, reactor: Reactor, spawner: Spawner) {
    loop {
        let _ = reactor.sleep_until(now() + WATCH_PERIOD).await;
        if controller.resetting.get() {
            continue;
        }
        let mut reset = false;
        for queue in [&controller.admin, &controller.io] {
            for (cid, opcode, abort) in queue.overdue(COMMAND_LIMIT) {
                match abort {
                    _ if queue.id == 0 && opcode == ABORT => reset = true,
                    Abort::Done(at) if now() - at > COMMAND_LIMIT => reset = true,
                    Abort::Done(_) | Abort::Sent => {}
                    Abort::None => {
                        queue.set_abort(cid, Abort::Sent);
                        let (controller, queue) = (controller.clone(), queue.clone());
                        let reactor = reactor.clone();
                        spawner.spawn(async move {
                            let command = Command {
                                opcode: ABORT,
                                cdw: [u32::from(queue.id) | u32::from(cid) << 16, 0, 0, 0, 0, 0],
                                ..Command::default()
                            };
                            match controller.admin.submit(command).await {
                                Ok(dw0) if dw0 & 1 == 0 => queue.set_abort(cid, Abort::Done(now())),
                                Err(Failure::Reset) => {}
                                _ => controller.reset(&reactor).await,
                            }
                        });
                    }
                }
            }
        }
        if reset {
            controller.reset(&reactor).await;
        }
    }
}
