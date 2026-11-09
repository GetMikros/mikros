//! The controller: taking it over from firmware, bringing it up with
//! its device context base address array, scratchpad buffers, command
//! ring and event ring, its interrupts, commands, the events of its
//! event ring, and stopping it.

use crate::regs::{Regs, USBCMD_INTE, USBCMD_RS, USBSTS_EINT};
use crate::ring::{
    EventRing, Ring, TRB_COMMAND_COMPLETION, TRB_PORT_STATUS_CHANGE, Trb, kind,
};
use crate::wait::{Sender, Waiters, oneshot};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;
use mikros_storage::{Contiguous, MS, Mmio, PAGE, PciBus, now};

/// Completion codes.
pub const CODE_SUCCESS: u8 = 1;
/// How long a command may take.
const COMMAND_LIMIT: i64 = 5000 * MS;
const SIGNALED: u32 = 1 << 3;

/// What a Command Completion Event says: its completion code and slot
/// ID.
#[derive(Clone, Copy, Debug)]
pub struct Completed {
    pub code: u8,
    pub slot: u8,
}

pub fn bus<T>(
    reply: Result<Result<T, Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Status> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::Internal),
    }
}

pub struct Controller {
    pub regs: Regs,
    pub pci: Rc<PciBus>,
    pub dma: OwnedHandle,
    pub bti: OwnedHandle,
    pub process: sys::Handle,
    /// Whether every structure the controller reaches must lie below
    /// 4 GiB.
    pub below_4g: bool,
    pub reactor: Reactor,
    pub spawner: Spawner,
    dcbaa: Contiguous,
    _scratchpads: Vec<Contiguous>,
    commands: Ring,
    command_room: Waiters,
    waiting: RefCell<BTreeMap<u64, Sender<Result<Completed, Status>>>>,
    events: EventRing,
    /// Root hub ports with a status change that was not looked at yet.
    changed: RefCell<BTreeSet<u8>>,
    port_changes: Waiters,
    _interrupts: RefCell<Vec<OwnedHandle>>,
    /// Pages for the data of control and interrupt transfers.
    pages: RefCell<Vec<Contiguous>>,
}

impl Controller {
    /// Brings up the controller behind the bus channel `pci` with memory
    /// from the `DMA` resource `dma`: maps BAR 0, takes it over from
    /// firmware, stops and resets it, sets up its structures and
    /// interrupts, and starts it.
    pub async fn start(
        pci: Rc<PciBus>,
        process: sys::Handle,
        dma: OwnedHandle,
        reactor: &Reactor,
        spawner: &Spawner,
    ) -> Result<Rc<Controller>, Status> {
        let device = pci.get();
        let bar = bus(device.get_bar(0).await)?.bar;
        let mmio = Mmio::map(process, &bar.resource, bar.base, bar.size)?;
        let regs = Regs::new(mmio);
        bus(device.set_bus_master(true).await)?;
        let bti = bus(device.get_bti().await)?.bti;
        if !regs.take_over(reactor).await {
            mikros_rt::log!(
                Warning,
                "xhci: firmware kept the controller; taking it anyway"
            );
        }
        let failed = |_| Status::Internal;
        regs.halt(reactor).await.map_err(failed)?;
        regs.reset(reactor).await.map_err(failed)?;
        let below_4g = !regs.ac64;
        let new = |size| Contiguous::new(&dma, &bti, process, size, below_4g);
        let dcbaa = new(8 * (u64::from(regs.max_slots) + 1))?;
        let mut scratchpads = Vec::new();
        if regs.scratchpads > 0 {
            let array = new(8 * u64::from(regs.scratchpads))?;
            for i in 0..u64::from(regs.scratchpads) {
                let page = new(PAGE)?;
                array.write64(8 * i, page.physical(0));
                scratchpads.push(page);
            }
            dcbaa.write64(0, array.physical(0));
            scratchpads.push(array);
        }
        let commands = Ring::new(&dma, &bti, process, 1, below_4g)?;
        let events = EventRing::new(&dma, &bti, process, below_4g)?;
        regs.configure(regs.max_slots, dcbaa.physical(0), commands.position().at);
        regs.set_event_ring(events.table.physical(0), events.start());
        let controller = Rc::new(Controller {
            regs,
            pci,
            dma,
            bti,
            process,
            below_4g,
            reactor: reactor.clone(),
            spawner: spawner.clone(),
            dcbaa,
            _scratchpads: scratchpads,
            commands,
            command_room: Waiters::default(),
            waiting: RefCell::new(BTreeMap::new()),
            events,
            changed: RefCell::new(BTreeSet::new()),
            port_changes: Waiters::default(),
            _interrupts: RefCell::new(Vec::new()),
            pages: RefCell::new(Vec::new()),
        });
        controller.take_interrupts().await?;
        let regs = &controller.regs;
        regs.set_command(regs.command() | USBCMD_INTE | USBCMD_RS);
        Ok(controller)
    }

    /// Memory for the controller to reach: `size` bytes, zeroed.
    pub fn memory(&self, size: u64) -> Result<Contiguous, Status> {
        Contiguous::new(&self.dma, &self.bti, self.process, size, self.below_4g)
    }

    /// A zeroed page for the data of a transfer.
    pub fn page(&self) -> Result<Contiguous, Status> {
        match self.pages.borrow_mut().pop() {
            Some(page) => {
                page.zero();
                Ok(page)
            }
            None => self.memory(PAGE),
        }
    }

    /// Gives back a page from `page`.
    pub fn give_page(&self, page: Contiguous) {
        self.pages.borrow_mut().push(page);
    }

    /// A transfer ring of `segments` pages.
    pub fn ring(&self, segments: usize) -> Result<Ring, Status> {
        Ring::new(&self.dma, &self.bti, self.process, segments, self.below_4g)
    }

    /// Takes one interrupt for interrupter 0 with `EnableMsi`, or the
    /// INTx event when that fails. Each interrupt processes the event
    /// ring and is then acknowledged.
    async fn take_interrupts(self: &Rc<Self>) -> Result<(), Status> {
        let device = self.pci.get();
        if let Ok(Ok(enabled)) = device.enable_msi(1).await
            && let Some(interrupt) = enabled.interrupts.into_iter().next()
        {
            let raw = interrupt.raw();
            self._interrupts.borrow_mut().push(interrupt);
            let controller = self.clone();
            self.spawner.spawn(async move {
                let reactor = controller.reactor.clone();
                while reactor.signals(raw, SIGNALED).await.is_ok() {
                    controller.regs.clear_pending();
                    controller.process_events();
                    // SAFETY: the call takes no pointers.
                    unsafe { sys::interrupt_ack(raw) };
                }
            });
            return Ok(());
        }
        let event = bus(device.get_interrupt().await)?.event;
        let raw = event.raw();
        self._interrupts.borrow_mut().push(event);
        let controller = self.clone();
        self.spawner.spawn(async move {
            let reactor = controller.reactor.clone();
            while reactor.signals(raw, SIGNALED).await.is_ok() {
                // The line may be shared: only an interrupt of this
                // controller has events to process.
                let regs = &controller.regs;
                if regs.status() & USBSTS_EINT != 0 {
                    regs.clear_status(USBSTS_EINT);
                    regs.clear_pending();
                    controller.process_events();
                }
                let _ = controller.pci.get().ack_interrupt().await;
            }
        });
        Ok(())
    }

    /// Takes every new event from the event ring, then moves the event
    /// ring dequeue pointer past them and clears the event handler busy
    /// bit.
    pub fn process_events(&self) {
        while let Some(trb) = self.events.next() {
            match kind(&trb) {
                TRB_COMMAND_COMPLETION => {
                    let at = u64::from(trb[0]) | u64::from(trb[1]) << 32;
                    let completed = Completed {
                        code: (trb[2] >> 24) as u8,
                        slot: (trb[3] >> 24) as u8,
                    };
                    if let Some(waiter) = self.waiting.borrow_mut().remove(&at) {
                        waiter.send(Ok(completed));
                    }
                    self.commands.release(1);
                    self.command_room.wake();
                }
                TRB_PORT_STATUS_CHANGE => {
                    self.changed.borrow_mut().insert((trb[0] >> 24) as u8);
                    self.port_changes.wake();
                }
                _ => {}
            }
        }
        self.regs.set_dequeue(self.events.dequeue());
    }

    /// Sends the command `trb` and waits for its completion. Fails with
    /// `TIMED_OUT` when the controller does not complete it in time.
    pub async fn command(self: &Rc<Self>, trb: Trb) -> Result<Completed, Status> {
        self.command_room
            .until(|| (self.commands.free() > 0).then_some(()))
            .await;
        let (addresses, _) = self.commands.push(&[trb]);
        let at = addresses[0];
        let (sender, receiver) = oneshot();
        self.waiting.borrow_mut().insert(at, sender);
        self.regs.ring(0, 0);
        let controller = self.clone();
        self.spawner.spawn(async move {
            let reactor = controller.reactor.clone();
            let _ = reactor.sleep_until(now() + COMMAND_LIMIT).await;
            if let Some(waiter) = controller.waiting.borrow_mut().remove(&at) {
                waiter.send(Err(Status::TimedOut));
            }
        });
        receiver.recv().await
    }

    /// Sends the command `trb` and fails with `INTERNAL` unless it
    /// succeeds.
    pub async fn command_ok(self: &Rc<Self>, trb: Trb) -> Result<Completed, Status> {
        let completed = self.command(trb).await?;
        match completed.code {
            CODE_SUCCESS => Ok(completed),
            _ => Err(Status::Internal),
        }
    }

    /// Gives the device context of `slot` to the controller, at `at`, or
    /// takes it away with 0.
    pub fn set_device_context(&self, slot: u8, at: u64) {
        self.dcbaa.write64(8 * u64::from(slot), at);
    }

    /// Waits for a root hub port with a status change and returns it.
    pub async fn port_change(&self) -> u8 {
        self.port_changes
            .until(|| self.changed.borrow_mut().pop_first())
            .await
    }

    /// Records a status change of root hub port `port`, as if an event
    /// had reported it.
    pub fn port_changed(&self, port: u8) {
        self.changed.borrow_mut().insert(port);
        self.port_changes.wake();
    }

    /// Stops the controller, before its memory is released when the
    /// driver ends.
    pub async fn stop(&self) {
        let _ = self.regs.halt(&self.reactor).await;
    }
}
