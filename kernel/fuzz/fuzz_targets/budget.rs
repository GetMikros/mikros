//! Runs a tree of budgets as the kernel does: creating children charged
//! to a budget, charging and releasing kernel memory and pages, held
//! charges, scratch room for vectors, limits, thresholds and process
//! counts, and dropping budgets. Compares the usage, signals and
//! information record of every budget, and which budgets are still
//! alive, with a model after each operation.

#![no_main]

use kernel_core::object::budget::{
    BUDGET_COST, Budget, Held, INFO_BUDGET_SIZE, Scratch, UNLIMITED,
};
use kernel_core::object::{Object, signals};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::sync::{Arc, Weak};

const MAX_BUDGETS: usize = 12;

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Amount {
    Small(u16),
    Raw(u64),
}

impl Amount {
    fn get(self) -> u64 {
        match self {
            Amount::Small(v) => u64::from(v),
            Amount::Raw(v) => v,
        }
    }
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Limit {
    Small(u16),
    Unlimited,
    Raw(u64),
}

impl Limit {
    fn get(self) -> u64 {
        match self {
            Limit::Small(v) => u64::from(v),
            Limit::Unlimited => UNLIMITED,
            Limit::Raw(v) => v,
        }
    }
}

#[derive(Arbitrary, Debug)]
enum Op {
    Child {
        parent: u8,
        kernel: Limit,
        pages: Limit,
        charge: Option<u8>,
    },
    /// Drops the handle of a budget without direct charges, other than
    /// the root, which the kernel keeps.
    DropHandle(u8),
    Charge {
        budget: u8,
        pages: bool,
        amount: Amount,
    },
    /// Releases part of a budget's direct charges.
    Release {
        budget: u8,
        pages: bool,
        part: u16,
    },
    Hold {
        budget: u8,
        amount: Amount,
    },
    DropHeld(u8),
    NewScratch(Option<u8>),
    ScratchCharge {
        scratch: u8,
        amount: Amount,
    },
    ScratchReserve {
        scratch: u8,
        additional: u16,
        exact: bool,
    },
    DropScratch(u8),
    SetLimits {
        budget: u8,
        kernel: Limit,
        pages: Limit,
    },
    SetThresholds {
        budget: u8,
        kernel: Limit,
        pages: Limit,
    },
    AddProcess(u8),
    RemoveProcess(u8),
}

/// The model of one budget, by creation index.
struct Node {
    parent: Option<usize>,
    /// The budget charged `BUDGET_COST` for this one.
    charged: Option<usize>,
    limits: [u64; 2],
    thresholds: [u64; 2],
    /// Charges made through the handle and not yet released.
    direct: [u64; 2],
    processes: u32,
    handle: Option<Arc<Budget>>,
    weak: Weak<Budget>,
}

struct HeldCharge {
    _held: Held,
    budget: usize,
    bytes: u64,
}

struct ScratchRoom {
    scratch: Scratch,
    budget: Option<usize>,
    /// The bytes charged, which count against the budget if there is one.
    bytes: u64,
    vec: Vec<u64>,
}

struct Model {
    nodes: Vec<Node>,
    held: Vec<HeldCharge>,
    scratch: Vec<ScratchRoom>,
}

impl Model {
    fn path(&self, mut b: usize) -> Vec<usize> {
        let mut out = vec![b];
        while let Some(p) = self.nodes[b].parent {
            out.push(p);
            b = p;
        }
        out
    }

    /// Which budgets must be alive: those with a handle, a live child, a
    /// held charge or scratch room, or a live budget charged to them.
    fn alive(&self) -> Vec<bool> {
        let mut alive = vec![false; self.nodes.len()];
        for b in &self.held {
            alive[b.budget] = true;
        }
        for s in &self.scratch {
            if let Some(b) = s.budget {
                alive[b] = true;
            }
        }
        // Parents and charged budgets come before the budgets that hold
        // them.
        for i in (0..self.nodes.len()).rev() {
            let n = &self.nodes[i];
            alive[i] |= n.handle.is_some();
            if alive[i] {
                for j in n.parent.into_iter().chain(n.charged) {
                    alive[j] = true;
                }
            }
        }
        alive
    }

    /// The charges each budget holds itself, by kind, with the cost of
    /// the live budgets charged to it.
    fn own(&self, alive: &[bool]) -> Vec<[u128; 2]> {
        let mut own: Vec<[u128; 2]> =
            self.nodes.iter().map(|n| n.direct.map(u128::from)).collect();
        for h in &self.held {
            own[h.budget][0] += u128::from(h.bytes);
        }
        for s in &self.scratch {
            if let Some(b) = s.budget {
                own[b][0] += u128::from(s.bytes);
            }
        }
        for (i, n) in self.nodes.iter().enumerate() {
            if let (true, Some(c)) = (alive[i], n.charged) {
                own[c][0] += u128::from(BUDGET_COST);
            }
        }
        own
    }

    /// The usage of each budget: its own charges and its descendants'.
    fn used(&self) -> Vec<[u128; 2]> {
        let alive = self.alive();
        let own = self.own(&alive);
        let mut used = vec![[0u128; 2]; self.nodes.len()];
        for (i, o) in own.iter().enumerate() {
            for b in self.path(i) {
                used[b][0] += o[0];
                used[b][1] += o[1];
            }
        }
        used
    }

    /// Whether charging `amount` of `kind` to `b` must succeed.
    fn fits(&self, b: usize, kind: usize, amount: u64) -> bool {
        let used = self.used();
        self.path(b).iter().all(|&a| {
            used[a][kind] + u128::from(amount) <= u128::from(self.nodes[a].limits[kind])
        })
    }

    /// The budget at `i`, if the model has a way to reach it.
    fn live(&self, i: u8) -> Option<(usize, Arc<Budget>)> {
        let n = self.nodes.len();
        let i = usize::from(i).checked_rem(n)?;
        self.nodes[i].weak.upgrade().map(|b| (i, b))
    }

    fn check(&self) {
        let alive = self.alive();
        let used = self.used();
        for (i, n) in self.nodes.iter().enumerate() {
            let budget = n.weak.upgrade();
            assert_eq!(budget.is_some(), alive[i], "budget {i} alive");
            let Some(b) = budget else { continue };
            let u = used[i].map(|v| u64::try_from(v).unwrap());
            let over = (0..2).any(|k| u[k] >= n.thresholds[k]);
            assert_eq!(b.header().signals() & signals::OVER_THRESHOLD != 0, over);
            let mut info = [0u8; INFO_BUDGET_SIZE];
            let mut put = |at: usize, v: u64| info[at..at + 8].copy_from_slice(&v.to_le_bytes());
            put(0x00, n.limits[0]);
            put(0x08, u[0]);
            put(0x10, n.limits[1]);
            put(0x18, u[1]);
            put(0x28, n.thresholds[0]);
            put(0x30, n.thresholds[1]);
            info[0x20..0x24].copy_from_slice(&n.processes.to_le_bytes());
            assert_eq!(b.info(), info, "budget {i}");
            assert_eq!(
                b.parent().map(Arc::as_ptr),
                n.parent.map(|p| self.nodes[p].weak.as_ptr())
            );
        }
    }

    fn charge(&mut self, i: usize, b: &Budget, kind: usize, amount: u64) {
        let fits = self.fits(i, kind, amount);
        let result = if kind == 0 {
            b.charge_kernel(amount)
        } else {
            b.charge_pages(amount)
        };
        assert_eq!(result, if fits { Ok(()) } else { Err(Error::NoResources) });
        if fits {
            self.nodes[i].direct[kind] += amount;
        }
    }

    fn reserve(&mut self, s: usize, additional: usize, exact: bool) {
        let room = &self.scratch[s];
        let (len, capacity) = (room.vec.len(), room.vec.capacity());
        let target = if exact {
            len + additional
        } else {
            (len + additional).max(2 * capacity).max(4)
        };
        let added = target.saturating_sub(capacity) as u64 * 8;
        if room.bytes.checked_add(added).is_none() {
            return;
        }
        let fits = added == 0 || room.budget.is_none_or(|b| self.fits(b, 0, added));
        let room = &mut self.scratch[s];
        let result = if exact {
            room.scratch.reserve_exact(&mut room.vec, additional)
        } else {
            room.scratch.reserve(&mut room.vec, additional)
        };
        assert_eq!(result, if fits { Ok(()) } else { Err(Error::NoResources) });
        if fits {
            assert!(room.vec.capacity() >= len + additional);
            room.bytes += added;
            room.vec.resize(len + additional / 2, 0);
        } else {
            assert_eq!(room.vec.capacity(), capacity);
        }
    }
}

fn limits(kernel: Limit, pages: Limit) -> [u64; 2] {
    [kernel.get(), pages.get()]
}

fuzz_target!(|ops: Vec<Op>| {
    let root = Budget::root();
    let mut m = Model {
        nodes: vec![Node {
            parent: None,
            charged: None,
            limits: [UNLIMITED; 2],
            thresholds: [UNLIMITED; 2],
            direct: [0; 2],
            processes: 0,
            weak: Arc::downgrade(&root),
            handle: Some(root),
        }],
        held: Vec::new(),
        scratch: Vec::new(),
    };
    for op in ops.iter().take(256) {
        match *op {
            Op::Child {
                parent,
                kernel,
                pages,
                charge,
            } => {
                let Some((p, parent)) = m.live(parent) else { continue };
                if m.nodes.len() == MAX_BUDGETS {
                    continue;
                }
                let charge = charge.and_then(|c| m.live(c));
                let fits = charge
                    .as_ref()
                    .is_none_or(|(c, _)| m.fits(*c, 0, BUDGET_COST));
                let limits = limits(kernel, pages);
                let child = Budget::child(&parent, limits[0], limits[1], charge.as_ref().map(|c| &c.1));
                let Ok(child) = child else {
                    assert_eq!(child.err(), Some(Error::NoResources));
                    assert!(!fits);
                    continue;
                };
                assert!(fits);
                m.nodes.push(Node {
                    parent: Some(p),
                    charged: charge.map(|c| c.0),
                    limits,
                    thresholds: [UNLIMITED; 2],
                    direct: [0; 2],
                    processes: 0,
                    weak: Arc::downgrade(&child),
                    handle: Some(child),
                });
            }
            Op::DropHandle(i) => {
                let i = usize::from(i) % m.nodes.len();
                if i > 0 && m.nodes[i].direct == [0, 0] {
                    m.nodes[i].handle = None;
                }
            }
            Op::Charge {
                budget,
                pages,
                amount,
            } => {
                let Some((i, b)) = m.live(budget) else { continue };
                if m.nodes[i].handle.is_some() {
                    m.charge(i, &b, usize::from(pages), amount.get());
                }
            }
            Op::Release { budget, pages, part } => {
                let Some((i, b)) = m.live(budget) else { continue };
                let kind = usize::from(pages);
                let direct = m.nodes[i].direct[kind];
                let amount = (u128::from(direct) * u128::from(part) / 0xFFFF) as u64;
                if kind == 0 {
                    b.release_kernel(amount);
                } else {
                    b.release_pages(amount);
                }
                m.nodes[i].direct[kind] -= amount;
            }
            Op::Hold { budget, amount } => {
                let Some((i, b)) = m.live(budget) else { continue };
                let amount = amount.get();
                let fits = m.fits(i, 0, amount);
                match b.hold_kernel(amount) {
                    Ok(held) => {
                        assert!(fits);
                        m.held.push(HeldCharge {
                            _held: held,
                            budget: i,
                            bytes: amount,
                        });
                    }
                    Err(e) => assert!(!fits && e == Error::NoResources),
                }
            }
            Op::DropHeld(i) => {
                if !m.held.is_empty() {
                    let i = usize::from(i) % m.held.len();
                    m.held.swap_remove(i);
                }
            }
            Op::NewScratch(budget) => {
                let budget = budget.and_then(|b| m.live(b));
                m.scratch.push(ScratchRoom {
                    scratch: Scratch::new(budget.as_ref().map(|b| &b.1)),
                    budget: budget.map(|b| b.0),
                    bytes: 0,
                    vec: Vec::new(),
                });
            }
            Op::ScratchCharge { scratch, amount } => {
                let Some(s) = usize::from(scratch).checked_rem(m.scratch.len()) else { continue };
                let amount = amount.get();
                let room = &m.scratch[s];
                // The bytes a scratch holds stay countable.
                if room.bytes.checked_add(amount).is_none() {
                    continue;
                }
                let fits = room.budget.is_none_or(|b| m.fits(b, 0, amount));
                let room = &mut m.scratch[s];
                let result = room.scratch.charge(amount);
                assert_eq!(result, if fits { Ok(()) } else { Err(Error::NoResources) });
                if fits {
                    room.bytes += amount;
                }
            }
            Op::ScratchReserve {
                scratch,
                additional,
                exact,
            } => {
                let Some(s) = usize::from(scratch).checked_rem(m.scratch.len()) else { continue };
                if m.scratch[s].vec.capacity() < 1 << 16 {
                    m.reserve(s, usize::from(additional % 4096), exact);
                }
            }
            Op::DropScratch(i) => {
                if !m.scratch.is_empty() {
                    let i = usize::from(i) % m.scratch.len();
                    m.scratch.swap_remove(i);
                }
            }
            Op::SetLimits {
                budget,
                kernel,
                pages,
            } => {
                let Some((i, b)) = m.live(budget) else { continue };
                let limits = limits(kernel, pages);
                b.set_limits(limits[0], limits[1]);
                m.nodes[i].limits = limits;
            }
            Op::SetThresholds {
                budget,
                kernel,
                pages,
            } => {
                let Some((i, b)) = m.live(budget) else { continue };
                let thresholds = limits(kernel, pages);
                b.set_thresholds(thresholds[0], thresholds[1]);
                m.nodes[i].thresholds = thresholds;
            }
            Op::AddProcess(i) => {
                let Some((i, b)) = m.live(i) else { continue };
                b.add_process();
                m.nodes[i].processes += 1;
            }
            Op::RemoveProcess(i) => {
                let Some((i, b)) = m.live(i) else { continue };
                if m.nodes[i].processes > 0 {
                    b.remove_process();
                    m.nodes[i].processes -= 1;
                }
            }
        }
        m.check();
    }
    // Dropping everything but the root releases every charge.
    m.held.clear();
    m.scratch.clear();
    for n in &mut m.nodes {
        if let (Some(b), [k, p]) = (&n.handle, n.direct) {
            b.release_kernel(k);
            b.release_pages(p);
        }
        n.direct = [0; 2];
    }
    for n in &mut m.nodes[1..] {
        n.handle = None;
    }
    m.check();
    assert!(m.nodes[1..].iter().all(|n| n.weak.upgrade().is_none()));
    let info = m.nodes[0].weak.upgrade().unwrap().info();
    assert_eq!(info[0x08..0x10], [0; 8]);
    assert_eq!(info[0x18..0x20], [0; 8]);
});
