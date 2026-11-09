//! The transfers of one endpoint: their TDs on its transfer ring, in
//! the order they were enqueued, their completion from transfer events,
//! cancelling them at their deadlines, and recovering the endpoint after
//! it halted.

use crate::controller::{CODE_SUCCESS, Controller};
use crate::ring::{
    CHAIN, IDT, IOC, ISP, Position, Ring, TRB_DATA, TRB_NORMAL, TRB_RESET_ENDPOINT,
    TRB_SET_DEQUEUE, TRB_SETUP, TRB_STATUS, TRB_STOP_ENDPOINT, Trb, trb_type,
};
use crate::wait::{Receiver, Sender, Waiters, oneshot};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_storage::{PAGE, now};

/// Completion codes.
const CODE_BABBLE: u8 = 3;
const CODE_TRANSACTION: u8 = 4;
const CODE_STALL: u8 = 6;
const CODE_SHORT_PACKET: u8 = 13;
const CODE_STOPPED: u8 = 26;
const CODE_STOPPED_LENGTH: u8 = 27;
const CODE_STOPPED_SHORT: u8 = 28;

/// The direction of a transfer, or of a control transfer's data stage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    In,
    Out,
}

/// A TD on the ring: its TRBs' addresses and data lengths, whether it
/// is a control transfer, the bytes of a short data stage, the enqueue
/// pointer after it, and where its result goes.
struct Td {
    addresses: Vec<u64>,
    lengths: Vec<u32>,
    control: bool,
    short: Option<u32>,
    after: Position,
    sender: Sender<Result<u32, Status>>,
}

impl Td {
    fn id(&self) -> u64 {
        self.addresses[0]
    }
}

struct State {
    pending: VecDeque<Td>,
    /// Set while the endpoint is being recovered after it halted: no new
    /// TD is enqueued meanwhile.
    recovering: bool,
    /// Set once the device is gone.
    gone: bool,
}

/// An endpoint of a device: its slot ID, its endpoint ID (the device
/// context index), and its transfer ring.
pub struct Endpoint {
    controller: Rc<Controller>,
    pub slot: u8,
    pub id: u8,
    pub ring: Ring,
    state: RefCell<State>,
    changed: Waiters,
}

/// A TD to enqueue: its TRBs, and their data lengths.
pub struct Request {
    pub trbs: Vec<Trb>,
    pub lengths: Vec<u32>,
    pub control: bool,
}

impl Request {
    /// The TRBs of a control transfer with the setup packet `setup`, and
    /// a data stage in `direction` of `length` bytes at `data` when
    /// `length` is not 0.
    pub fn control(setup: [u8; 8], direction: Direction, data: u64, length: u32) -> Request {
        let setup_low = u32::from_le_bytes([setup[0], setup[1], setup[2], setup[3]]);
        let setup_high = u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]);
        let input = direction == Direction::In;
        // The transfer type: no data stage, OUT data or IN data.
        let transfer_type = match (length, input) {
            (0, _) => 0,
            (_, false) => 2,
            (_, true) => 3,
        };
        let mut trbs = alloc::vec![[
            setup_low,
            setup_high,
            8,
            trb_type(TRB_SETUP) | IDT | transfer_type << 16,
        ]];
        let mut lengths = alloc::vec![0];
        if length > 0 {
            let dir = if input { 1 << 16 } else { 0 };
            trbs.push([
                data as u32,
                (data >> 32) as u32,
                length,
                trb_type(TRB_DATA) | ISP | dir,
            ]);
            lengths.push(length);
        }
        // The status stage goes the other way, and IN without data.
        let status_in = length == 0 || !input;
        let dir = if status_in { 1 << 16 } else { 0 };
        trbs.push([0, 0, 0, trb_type(TRB_STATUS) | IOC | dir]);
        lengths.push(0);
        Request {
            trbs,
            lengths,
            control: true,
        }
    }

    /// The normal TRBs of a bulk or interrupt transfer of `length`
    /// bytes, from byte `offset` of the pages at `pages`, a TRB for each
    /// page it touches, or one without data when `length` is 0, on an
    /// endpoint whose packets hold `max_packet` bytes.
    pub fn normal(pages: &[u64], offset: u64, length: u32, max_packet: u16) -> Request {
        let mut trbs = Vec::new();
        let mut lengths = Vec::new();
        let mut at = offset;
        let end = offset + u64::from(length);
        loop {
            let page_end = (at / PAGE + 1) * PAGE;
            let chunk = (end.min(page_end) - at) as u32;
            let address = pages.get((at / PAGE) as usize).map_or(0, |p| p + at % PAGE);
            // The TD size: the packets still to come after this TRB.
            let left = end - (at + u64::from(chunk));
            let packets = left.div_ceil(u64::from(max_packet.max(1))).min(31) as u32;
            trbs.push([
                address as u32,
                (address >> 32) as u32,
                chunk | packets << 17,
                trb_type(TRB_NORMAL) | ISP | CHAIN,
            ]);
            lengths.push(chunk);
            at += u64::from(chunk);
            if at >= end {
                break;
            }
        }
        let last = trbs.last_mut().expect("there is a TRB");
        last[3] = last[3] & !CHAIN | IOC;
        Request {
            trbs,
            lengths,
            control: false,
        }
    }
}

impl Endpoint {
    pub fn new(controller: Rc<Controller>, slot: u8, id: u8, ring: Ring) -> Rc<Endpoint> {
        let endpoint = Rc::new(Endpoint {
            controller: controller.clone(),
            slot,
            id,
            ring,
            state: RefCell::new(State {
                pending: VecDeque::new(),
                recovering: false,
                gone: false,
            }),
            changed: Waiters::default(),
        });
        controller.set_endpoint(slot, id, Some(endpoint.clone()));
        endpoint
    }

    /// Enqueues `request` once the ring has room for it and the endpoint
    /// is not being recovered, and rings its doorbell. TDs are enqueued
    /// in the order of the calls. Fails with `PEER_CLOSED` once the
    /// device is gone, and with `NO_RESOURCES` for a TD that the ring
    /// cannot hold at all. The receiver gives the bytes transferred.
    pub async fn enqueue(
        self: &Rc<Self>,
        request: Request,
    ) -> Result<(u64, Receiver<Result<u32, Status>>), Status> {
        let count = request.trbs.len();
        if count > self.ring.capacity() {
            return Err(Status::NoResources);
        }
        let ready = self
            .changed
            .until(|| {
                let state = self.state.borrow();
                match state.gone {
                    true => Some(false),
                    false => (!state.recovering && self.ring.free() >= count).then_some(true),
                }
            })
            .await;
        if !ready {
            return Err(Status::PeerClosed);
        }
        let (addresses, after) = self.ring.push(&request.trbs);
        let (sender, receiver) = oneshot();
        let id = addresses[0];
        self.state.borrow_mut().pending.push_back(Td {
            addresses,
            lengths: request.lengths,
            control: request.control,
            short: None,
            after,
            sender,
        });
        self.controller.regs.ring(self.slot, self.id);
        Ok((id, receiver))
    }

    /// Enqueues `request` and waits for it, cancelling it with
    /// `TIMED_OUT` when it has not completed by `deadline`.
    pub async fn transfer(self: &Rc<Self>, request: Request, deadline: i64) -> Result<u32, Status> {
        let (id, receiver) = self.enqueue(request).await?;
        self.wait(id, receiver, deadline).await
    }

    /// Waits for the TD `id`, which gives its result to `receiver`, and
    /// cancels it when it has not completed by `deadline`.
    pub async fn wait(
        self: &Rc<Self>,
        id: u64,
        receiver: Receiver<Result<u32, Status>>,
        deadline: i64,
    ) -> Result<u32, Status> {
        let endpoint = self.clone();
        let reactor = self.controller.reactor.clone();
        self.controller.spawner.spawn(async move {
            let pending = |e: &Endpoint| e.state.borrow().pending.iter().any(|td| td.id() == id);
            if deadline > now() {
                let _ = reactor.sleep_until(deadline).await;
            }
            if pending(&endpoint) {
                endpoint.cancel(id).await;
            }
        });
        receiver.recv().await
    }

    /// Stops the endpoint, takes TD `id` off the ring and fails it with
    /// `TIMED_OUT`, and starts the endpoint again.
    async fn cancel(self: &Rc<Self>, id: u64) {
        let stop = [0, 0, 0, self.command(TRB_STOP_ENDPOINT)];
        let _ = self.controller.command(stop).await;
        let td = {
            let mut state = self.state.borrow_mut();
            let Some(at) = state.pending.iter().position(|td| td.id() == id) else {
                return;
            };
            let td = state.pending.remove(at).expect("the TD is pending");
            if at > 0 {
                self.ring.cancel(&td.addresses);
            }
            (td, at == 0)
        };
        let (td, first) = td;
        if first {
            let _ = self.set_dequeue(td.after).await;
        }
        self.ring.release(td.addresses.len());
        td.sender.send(Err(Status::TimedOut));
        self.changed.wake();
        self.controller.regs.ring(self.slot, self.id);
    }

    /// The fourth dword of an endpoint command for this endpoint.
    fn command(&self, kind: u32) -> u32 {
        trb_type(kind) | u32::from(self.id) << 16 | u32::from(self.slot) << 24
    }

    /// Moves the endpoint's dequeue pointer to `to`.
    async fn set_dequeue(self: &Rc<Self>, to: Position) -> Result<(), Status> {
        let pointer = to.at | u64::from(to.cycle);
        let trb = [
            pointer as u32,
            (pointer >> 32) as u32,
            0,
            self.command(TRB_SET_DEQUEUE),
        ];
        self.controller.command_ok(trb).await.map(|_| ())
    }

    /// Recovers the endpoint after it halted on the TD that ends at
    /// `after`: resets it and moves its dequeue pointer past that TD,
    /// then lets new TDs be enqueued and starts it again.
    async fn recover(self: Rc<Self>, after: Position) {
        let reset = [0, 0, 0, self.command(TRB_RESET_ENDPOINT)];
        let _ = self.controller.command(reset).await;
        let _ = self.set_dequeue(after).await;
        self.state.borrow_mut().recovering = false;
        self.changed.wake();
        self.controller.regs.ring(self.slot, self.id);
    }

    /// Waits until the endpoint is not being recovered.
    pub async fn idle(&self) {
        self.changed
            .until(|| (!self.state.borrow().recovering).then_some(()))
            .await;
    }

    /// Fails every pending TD with `PEER_CLOSED`, and every later
    /// transfer, once the device is gone, and stops taking events.
    pub fn remove(&self) {
        let pending = {
            let mut state = self.state.borrow_mut();
            state.gone = true;
            core::mem::take(&mut state.pending)
        };
        for td in pending {
            td.sender.send(Err(Status::PeerClosed));
        }
        self.controller.set_endpoint(self.slot, self.id, None);
        self.changed.wake();
    }

    /// Fails every pending TD with `status`, and starts the ring from its
    /// first TRB again, after the endpoint was dropped or the device was
    /// reset.
    pub fn restart(&self, status: Status) {
        let pending = core::mem::take(&mut self.state.borrow_mut().pending);
        for td in pending {
            td.sender.send(Err(status));
        }
        self.ring.clear();
        self.changed.wake();
    }

    /// Completes the TD at `index`.
    fn complete(&self, index: usize, result: Result<u32, Status>) {
        let td = self.state.borrow_mut().pending.remove(index);
        if let Some(td) = td {
            self.ring.release(td.addresses.len());
            td.sender.send(result);
            self.changed.wake();
        }
    }
}

impl Endpoint {
    /// Takes a transfer event of the endpoint.
    pub fn event(&self, trb: &Trb) {
        let pointer = u64::from(trb[0]) | u64::from(trb[1]) << 32;
        let code = (trb[2] >> 24) as u8;
        let residual = trb[2] & 0xFF_FFFF;
        let found = {
            let state = self.state.borrow();
            state.pending.iter().enumerate().find_map(|(i, td)| {
                let at = td.addresses.iter().position(|&a| a == pointer)?;
                Some((i, at))
            })
        };
        // Events of TDs that were completed or cancelled already.
        let Some((index, at)) = found else {
            return;
        };
        let (last, actual, control, short) = {
            let state = self.state.borrow();
            let td = &state.pending[index];
            let before: u32 = td.lengths[..at].iter().sum();
            let actual = before + td.lengths[at].saturating_sub(residual);
            (at + 1 == td.addresses.len(), actual, td.control, td.short)
        };
        match code {
            CODE_SUCCESS if last => {
                let total = match control {
                    true => self.state.borrow().pending[index].lengths.iter().sum(),
                    false => actual,
                };
                self.complete(index, Ok(short.unwrap_or(total)));
            }
            CODE_SUCCESS => {}
            // A control transfer goes on to its status stage.
            CODE_SHORT_PACKET if control && !last => {
                // The data stage's bytes, without the setup stage.
                self.state.borrow_mut().pending[index].short = Some(actual);
            }
            CODE_SHORT_PACKET => self.complete(index, Ok(short.unwrap_or(actual))),
            CODE_STOPPED | CODE_STOPPED_LENGTH | CODE_STOPPED_SHORT => {}
            CODE_STALL | CODE_BABBLE | CODE_TRANSACTION => {
                let after = self.state.borrow().pending[index].after;
                self.state.borrow_mut().recovering = true;
                let status = match code {
                    CODE_STALL => Status::BadState,
                    _ => Status::Internal,
                };
                self.complete(index, Err(status));
                // The endpoint is halted until it is reset.
                let endpoint = self
                    .controller
                    .endpoint(self.slot, self.id)
                    .expect("the endpoint takes its events");
                self.controller.spawner.spawn(endpoint.recover(after));
            }
            _ => self.complete(index, Err(Status::Internal)),
        }
    }
}
