//! Runs the pending signals of a process and its threads as the kernel
//! does: signals made pending, some of them charged to a budget with a
//! small limit, taken under masks and discarded. Compares every result,
//! the masks, the shared count of queued real-time signals and the
//! budgets' usage with a model after each operation.

#![no_main]

use kernel_core::object::budget::{Budget, UNLIMITED};
use kernel_core::signal::{
    INFO_SIZE, Info, Pending, QUEUED_COST, RT_QUEUE_MAX, SIG_MAX, SIG_RT_MIN, bit,
};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::{BTreeMap, VecDeque};

/// The process's set and those of three threads.
const SETS: usize = 4;

#[derive(Arbitrary, Debug, Clone, Copy)]
struct Fields {
    code: u32,
    sender_koid: u64,
    value: u64,
    address: u64,
    exception_type: u32,
}

#[derive(Arbitrary, Debug)]
enum Op {
    Add {
        set: u8,
        signo: u8,
        fields: Fields,
        /// No budget, the unlimited one or the limited one.
        budget: u8,
    },
    /// Queues `count` copies of a real-time signal, to reach the limit.
    AddMany {
        set: u8,
        signo: u8,
        count: u16,
    },
    Take {
        set: u8,
        allowed: u64,
    },
    Discard {
        set: u8,
        signals: u64,
    },
}

#[derive(Default)]
struct Set {
    standard: BTreeMap<u32, Info>,
    /// Queued real-time signals and the budget charged for each.
    realtime: BTreeMap<u32, VecDeque<(Info, Option<usize>)>>,
}

impl Set {
    fn mask(&self) -> u64 {
        self.standard
            .keys()
            .chain(self.realtime.keys())
            .fold(0, |m, &s| m | bit(s))
    }
}

struct Model {
    sets: Vec<Set>,
    queued: usize,
    /// The queued signals charged to each budget.
    charged: [u64; 2],
    /// How many queued signals the limited budget has room for.
    room: u64,
}

impl Model {
    fn add(&mut self, s: usize, info: Info, budget: Option<usize>) -> Result<(), Error> {
        let set = &mut self.sets[s];
        if info.signo < SIG_RT_MIN {
            set.standard.entry(info.signo).or_insert(info);
            return Ok(());
        }
        if self.queued >= RT_QUEUE_MAX {
            return Err(Error::ShouldWait);
        }
        if budget == Some(1) && self.charged[1] == self.room {
            return Err(Error::NoResources);
        }
        set.realtime.entry(info.signo).or_default().push_back((info, budget));
        self.queued += 1;
        if let Some(b) = budget {
            self.charged[b] += 1;
        }
        Ok(())
    }

    fn take(&mut self, s: usize, allowed: u64) -> Option<Info> {
        let set = &mut self.sets[s];
        let signo = (1..=SIG_MAX).find(|&n| set.mask() & allowed & bit(n) != 0)?;
        if signo < SIG_RT_MIN {
            return set.standard.remove(&signo);
        }
        let queue = set.realtime.get_mut(&signo).unwrap();
        let (info, budget) = queue.pop_front().unwrap();
        if queue.is_empty() {
            set.realtime.remove(&signo);
        }
        self.queued -= 1;
        if let Some(b) = budget {
            self.charged[b] -= 1;
        }
        Some(info)
    }
}

fn info(signo: u32, f: Fields) -> Info {
    Info {
        signo,
        code: f.code,
        sender_koid: f.sender_koid,
        value: f.value,
        address: f.address,
        exception_type: f.exception_type,
    }
}

fn used(b: &Budget) -> u64 {
    u64::from_le_bytes(b.info()[0x08..0x10].try_into().unwrap())
}

fn check_bytes(i: &Info) {
    let b = i.bytes();
    let mut want = [0u8; INFO_SIZE];
    want[0..4].copy_from_slice(&i.signo.to_le_bytes());
    want[4..8].copy_from_slice(&i.code.to_le_bytes());
    want[8..16].copy_from_slice(&i.sender_koid.to_le_bytes());
    want[16..24].copy_from_slice(&i.value.to_le_bytes());
    want[24..32].copy_from_slice(&i.address.to_le_bytes());
    want[32..36].copy_from_slice(&i.exception_type.to_le_bytes());
    assert_eq!(b, want);
}

#[derive(Arbitrary, Debug)]
struct Input {
    /// How many queued signals the limited budget has room for.
    room: u8,
    ops: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let root = Budget::root();
    let room = u64::from(input.room % 16);
    let limited = Budget::child(&root, room * QUEUED_COST, UNLIMITED, None).unwrap();
    let budgets = [root.clone(), limited.clone()];
    let mut sets: Vec<Pending> = (0..SETS).map(|_| Pending::default()).collect();
    let mut queued = 0usize;
    let mut m = Model {
        sets: (0..SETS).map(|_| Set::default()).collect(),
        queued: 0,
        charged: [0; 2],
        room,
    };
    let signo = |n: u8| u32::from(n) % SIG_MAX + 1;
    for op in input.ops.iter().take(512) {
        match *op {
            Op::Add {
                set,
                signo: n,
                fields,
                budget,
            } => {
                let s = usize::from(set) % SETS;
                let i = info(signo(n), fields);
                let b = [None, Some(0), Some(1)][usize::from(budget) % 3];
                let result = sets[s].add(i, &mut queued, b.map(|b| &budgets[b]));
                assert_eq!(result, m.add(s, i, b));
            }
            Op::AddMany { set, signo: n, count } => {
                let s = usize::from(set) % SETS;
                let i = info(u32::from(n) % (SIG_MAX - SIG_RT_MIN + 1) + SIG_RT_MIN, Fields {
                    code: 0,
                    sender_koid: 0,
                    value: 0,
                    address: 0,
                    exception_type: 0,
                });
                for _ in 0..usize::from(count) % (RT_QUEUE_MAX + 2) {
                    let result = sets[s].add(i, &mut queued, None);
                    assert_eq!(result, m.add(s, i, None));
                    if result.is_err() {
                        break;
                    }
                }
            }
            Op::Take { set, allowed } => {
                let s = usize::from(set) % SETS;
                let taken = sets[s].take(allowed, &mut queued);
                assert_eq!(taken, m.take(s, allowed));
                if let Some(i) = taken {
                    assert_ne!(allowed & bit(i.signo), 0);
                    check_bytes(&i);
                }
            }
            Op::Discard { set, signals } => {
                let s = usize::from(set) % SETS;
                sets[s].discard(signals, &mut queued);
                while m.take(s, signals).is_some() {}
                assert_eq!(sets[s].mask() & signals, 0);
            }
        }
        for (p, set) in sets.iter().zip(&m.sets) {
            assert_eq!(p.mask(), set.mask());
        }
        assert_eq!(queued, m.queued);
        assert_eq!(queued, m.sets.iter().flat_map(|s| s.realtime.values()).map(VecDeque::len).sum());
        assert_eq!(used(&limited), m.charged[1] * QUEUED_COST);
        // The root also counts what is charged to its child.
        assert_eq!(used(&root), (m.charged[0] + m.charged[1]) * QUEUED_COST);
    }
    // Discarding every signal releases every charge.
    for set in &mut sets {
        set.discard(u64::MAX, &mut queued);
        assert_eq!(set.mask(), 0);
    }
    assert_eq!(queued, 0);
    assert_eq!(used(&root), 0);
});
