//! TRB rings: command and transfer rings, which the driver produces and
//! which end each segment with a link TRB, the last of which toggles the
//! cycle bit, and the event ring, which the controller produces.

use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{Ordering, fence};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;
use mikros_storage::{Contiguous, PAGE};

/// A TRB: four dwords.
pub type Trb = [u32; 4];

/// The TRBs in one segment, a page; the last one of a producer ring's
/// segment is its link TRB.
const SEGMENT_TRBS: usize = (PAGE / 16) as usize;
const USABLE: usize = SEGMENT_TRBS - 1;

/// TRB types.
pub const TRB_NORMAL: u32 = 1;
pub const TRB_SETUP: u32 = 2;
pub const TRB_DATA: u32 = 3;
pub const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_NOOP: u32 = 8;
pub const TRB_ENABLE_SLOT: u32 = 9;
pub const TRB_DISABLE_SLOT: u32 = 10;
pub const TRB_ADDRESS_DEVICE: u32 = 11;
pub const TRB_CONFIGURE_ENDPOINT: u32 = 12;
pub const TRB_EVALUATE_CONTEXT: u32 = 13;
pub const TRB_RESET_ENDPOINT: u32 = 14;
pub const TRB_STOP_ENDPOINT: u32 = 15;
pub const TRB_SET_DEQUEUE: u32 = 16;
pub const TRB_TRANSFER_EVENT: u32 = 32;
pub const TRB_COMMAND_COMPLETION: u32 = 33;
pub const TRB_PORT_STATUS_CHANGE: u32 = 34;

/// Control bits in the fourth dword.
pub const CYCLE: u32 = 1 << 0;
const TOGGLE_CYCLE: u32 = 1 << 1;
pub const ISP: u32 = 1 << 2;
pub const CHAIN: u32 = 1 << 4;
pub const IOC: u32 = 1 << 5;
pub const IDT: u32 = 1 << 6;

pub fn trb_type(kind: u32) -> u32 {
    kind << 10
}

/// The type of `trb`.
pub fn kind(trb: &Trb) -> u32 {
    (trb[3] >> 10) & 0x3F
}

/// Where a producer ring's enqueue pointer stands: a TRB's address and
/// the cycle state there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub at: u64,
    pub cycle: bool,
}

struct State {
    segment: usize,
    index: usize,
    cycle: bool,
    /// TRBs that may still be enqueued before ones the controller has
    /// not finished with would be overwritten.
    free: usize,
}

/// A command or transfer ring of one or more segments, linked in a
/// cycle.
pub struct Ring {
    segments: Vec<Contiguous>,
    state: RefCell<State>,
}

impl Ring {
    /// A ring of `segments` pages from the `DMA` resource `dma`, below
    /// 4 GiB when `below_4g` is true.
    pub fn new(
        dma: &OwnedHandle,
        bti: &OwnedHandle,
        process: sys::Handle,
        segments: usize,
        below_4g: bool,
    ) -> Result<Ring, Status> {
        let segments = (0..segments)
            .map(|_| Contiguous::new(dma, bti, process, PAGE, below_4g))
            .collect::<Result<Vec<_>, _>>()?;
        let ring = Ring {
            segments,
            state: RefCell::new(State {
                segment: 0,
                index: 0,
                cycle: true,
                free: 0,
            }),
        };
        ring.clear();
        Ok(ring)
    }

    /// Empties the ring: no TRB belongs to the controller, every link
    /// TRB points to the next segment, and the enqueue pointer is at the
    /// start with cycle state 1.
    pub fn clear(&self) {
        let count = self.segments.len();
        for (i, segment) in self.segments.iter().enumerate() {
            segment.zero();
            let link = 16 * USABLE as u64;
            segment.write64(link, self.segments[(i + 1) % count].physical(0));
            let toggle = if i + 1 == count { TOGGLE_CYCLE } else { 0 };
            segment.write32(link + 12, trb_type(TRB_LINK) | toggle);
        }
        *self.state.borrow_mut() = State {
            segment: 0,
            index: 0,
            cycle: true,
            free: USABLE * count - 1,
        };
    }

    /// The enqueue pointer.
    pub fn position(&self) -> Position {
        let state = self.state.borrow();
        Position {
            at: self.segments[state.segment].physical(16 * state.index as u64),
            cycle: state.cycle,
        }
    }

    /// The most TRBs that one call of `push` may enqueue.
    pub fn capacity(&self) -> usize {
        USABLE * self.segments.len() - 1
    }

    pub fn free(&self) -> usize {
        self.state.borrow().free
    }

    /// Gives back `count` TRBs that the controller has finished with.
    pub fn release(&self, count: usize) {
        let mut state = self.state.borrow_mut();
        state.free = (state.free + count).min(self.capacity());
    }

    /// Enqueues `trbs`, whose cycle bits are set here, as one TD, and
    /// returns their addresses and the enqueue pointer after them. The
    /// first TRB is handed to the controller last, so that it never
    /// starts on a TD that is not complete. The caller made sure that
    /// `free` covers them.
    pub fn push(&self, trbs: &[Trb]) -> (Vec<u64>, Position) {
        let mut state = self.state.borrow_mut();
        assert!(trbs.len() <= state.free, "the ring has room for the TD");
        state.free -= trbs.len();
        let mut addresses = Vec::with_capacity(trbs.len());
        let mut first = None;
        for (i, trb) in trbs.iter().enumerate() {
            let segment = &self.segments[state.segment];
            let at = 16 * state.index as u64;
            let cycle = if state.cycle { CYCLE } else { 0 };
            segment.write32(at, trb[0]);
            segment.write32(at + 4, trb[1]);
            segment.write32(at + 8, trb[2]);
            let control = trb[3] & !CYCLE;
            if i == 0 {
                first = Some((state.segment, at, control | cycle));
                segment.write32(at + 12, control | (cycle ^ CYCLE));
            } else {
                fence(Ordering::SeqCst);
                segment.write32(at + 12, control | cycle);
            }
            addresses.push(segment.physical(at));
            state.index += 1;
            if state.index == USABLE {
                // The link TRB belongs to the TD when the TD goes on
                // past it.
                let link = 16 * USABLE as u64;
                let old = segment.read32(link + 12) & !(CYCLE | CHAIN);
                let chain = if i + 1 < trbs.len() { CHAIN } else { 0 };
                fence(Ordering::SeqCst);
                segment.write32(link + 12, old | chain | cycle);
                if old & TOGGLE_CYCLE != 0 {
                    state.cycle = !state.cycle;
                }
                state.segment = (state.segment + 1) % self.segments.len();
                state.index = 0;
            }
        }
        if let Some((segment, at, control)) = first {
            fence(Ordering::SeqCst);
            self.segments[segment].write32(at + 12, control);
        }
        fence(Ordering::SeqCst);
        let segment = &self.segments[state.segment];
        let after = Position {
            at: segment.physical(16 * state.index as u64),
            cycle: state.cycle,
        };
        (addresses, after)
    }

    /// Turns the TRBs at `addresses` into no-op TRBs, keeping their
    /// cycle and chain bits, so that the controller skips their TD.
    pub fn cancel(&self, addresses: &[u64]) {
        for &address in addresses {
            for segment in &self.segments {
                let start = segment.physical(0);
                if (start..start + PAGE).contains(&address) {
                    let at = address - start;
                    let control = segment.read32(at + 12) & (CYCLE | CHAIN);
                    segment.write64(at, 0);
                    segment.write32(at + 8, 0);
                    fence(Ordering::SeqCst);
                    segment.write32(at + 12, control | trb_type(TRB_NOOP));
                }
            }
        }
    }
}

/// The event ring of interrupter 0: one segment, and its segment table.
pub struct EventRing {
    segment: Contiguous,
    pub table: Contiguous,
    state: RefCell<(usize, bool)>,
}

impl EventRing {
    pub fn new(
        dma: &OwnedHandle,
        bti: &OwnedHandle,
        process: sys::Handle,
        below_4g: bool,
    ) -> Result<EventRing, Status> {
        let segment = Contiguous::new(dma, bti, process, PAGE, below_4g)?;
        let table = Contiguous::new(dma, bti, process, 64, below_4g)?;
        table.write64(0, segment.physical(0));
        table.write32(8, SEGMENT_TRBS as u32);
        Ok(EventRing {
            segment,
            table,
            state: RefCell::new((0, true)),
        })
    }

    /// The start of the segment.
    pub fn start(&self) -> u64 {
        self.segment.physical(0)
    }

    /// The next event, when its cycle bit matches the consumer cycle
    /// state.
    pub fn next(&self) -> Option<Trb> {
        let mut state = self.state.borrow_mut();
        let at = 16 * state.0 as u64;
        let control = self.segment.read32(at + 12);
        if (control & CYCLE != 0) != state.1 {
            return None;
        }
        fence(Ordering::SeqCst);
        let trb = [
            self.segment.read32(at),
            self.segment.read32(at + 4),
            self.segment.read32(at + 8),
            control,
        ];
        state.0 += 1;
        if state.0 == SEGMENT_TRBS {
            state.0 = 0;
            state.1 = !state.1;
        }
        Some(trb)
    }

    /// The address of the next event the driver will take.
    pub fn dequeue(&self) -> u64 {
        self.segment.physical(16 * self.state.borrow().0 as u64)
    }
}
