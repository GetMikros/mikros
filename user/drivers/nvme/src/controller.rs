//! The controller: bringing it up, its admin and I/O queue pairs, its
//! interrupts, and shutting it down.

use crate::queue::{Command, QueuePair};
use crate::regs::Regs;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::sys;
use mikros_storage::Mmio;

/// Admin opcodes.
const CREATE_IO_SQ: u8 = 0x01;
const CREATE_IO_CQ: u8 = 0x05;
const SET_FEATURES: u8 = 0x09;
/// The feature of the number of queues.
const FEATURE_QUEUES: u32 = 0x07;
/// The admin queues' entries, and the most I/O queue entries.
const ADMIN_ENTRIES: u16 = 32;
const IO_ENTRIES_MAX: u32 = 1024;
/// The ID of the I/O queue pair.
const IO_QUEUE: u16 = 1;
const SIGNALED: u32 = 1 << 3;

pub struct Controller {
    pub regs: Rc<Regs>,
    pub admin: Rc<QueuePair>,
    pub io: Rc<QueuePair>,
    /// The interrupt vector of the I/O completion queue.
    io_vector: u16,
    _dma: OwnedHandle,
    _bti: OwnedHandle,
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

impl Controller {
    /// Brings up the controller behind the bus channel `device` with
    /// memory from the `DMA` resource `dma`: maps BAR 0, disables it, sets
    /// up the admin queues, takes its interrupts, enables it, and creates
    /// the I/O queues.
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
        let controller = Rc::new(Controller {
            regs,
            admin,
            io,
            io_vector,
            _dma: dma,
            _bti: bti,
            _device: device,
            _interrupts: interrupts,
        });
        controller.create_io_queues().await?;
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
