//! Process signals: numbers, actions, information records and the sets
//! of pending signals.

use crate::nodes::{self, Map};
use crate::object::budget::Budget;
use crate::status::Error;
use alloc::collections::VecDeque;
use alloc::sync::Arc;

/// The highest signal number.
pub const SIG_MAX: u32 = 64;
/// The first real-time signal.
pub const SIG_RT_MIN: u32 = 32;

pub const ILL: u32 = 4;
pub const TRAP: u32 = 5;
pub const BUS: u32 = 7;
pub const FPE: u32 = 8;
pub const KILL: u32 = 9;
pub const SEGV: u32 = 11;
pub const CHLD: u32 = 17;
pub const CONT: u32 = 18;
pub const STOP: u32 = 19;
pub const TSTP: u32 = 20;
pub const TTIN: u32 = 21;
pub const TTOU: u32 = 22;
pub const URG: u32 = 23;
pub const WINCH: u32 = 28;

/// The most real-time signals a process and its threads hold queued.
pub const RT_QUEUE_MAX: usize = 1024;

/// The exit code of a process terminated by a signal.
pub const EXIT_CODE: i64 = -3;

/// The `code` of an information record.
pub const CODE_PROCESS: u32 = 0;
pub const CODE_THREAD: u32 = 1;
pub const CODE_EXCEPTION: u32 = 2;
pub const CODE_KERNEL: u32 = 3;

/// The mask bit of signal `signo`, from 1 to 64.
pub const fn bit(signo: u32) -> u64 {
    1 << (signo - 1)
}

/// The signals that cannot be caught or blocked.
pub const UNBLOCKABLE: u64 = bit(KILL) | bit(STOP);
/// The signals whose default action is stop.
pub const STOP_SIGNALS: u64 = bit(STOP) | bit(TSTP) | bit(TTIN) | bit(TTOU);

/// Whether `signo` is a signal number.
pub fn valid(signo: u32) -> bool {
    (1..=SIG_MAX).contains(&signo)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Default {
    Terminate,
    Ignore,
    Stop,
    Continue,
}

/// The default action of `signo`.
pub fn default_action(signo: u32) -> Default {
    match signo {
        CHLD | URG | WINCH => Default::Ignore,
        CONT => Default::Continue,
        STOP | TSTP | TTIN | TTOU => Default::Stop,
        _ => Default::Terminate,
    }
}

/// The handler values with a fixed meaning.
pub const HANDLER_DEFAULT: u64 = 0;
pub const HANDLER_IGNORE: u64 = 1;

pub const RESTART: u32 = 1 << 0;
pub const ONSTACK: u32 = 1 << 1;
pub const NODEFER: u32 = 1 << 2;
pub const RESETHAND: u32 = 1 << 3;
const FLAGS: u32 = RESTART | ONSTACK | NODEFER | RESETHAND;

/// The size of an action.
pub const ACTION_SIZE: usize = 32;

/// What happens when a signal is delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Action {
    pub handler: u64,
    pub mask: u64,
    pub restorer: u64,
    pub flags: u32,
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}

impl Action {
    /// The action in `bytes`, or `InvalidArgs` when it sets an undefined
    /// flag or its reserved field, or when a handler or restorer address
    /// is one `user` refuses.
    pub fn parse(bytes: &[u8; ACTION_SIZE], user: impl Fn(u64) -> bool) -> Result<Action, Error> {
        let action = Action {
            handler: u64_at(bytes, 0x00),
            mask: u64_at(bytes, 0x08),
            restorer: u64_at(bytes, 0x10),
            flags: u32_at(bytes, 0x18),
        };
        let handler = action.handler > HANDLER_IGNORE;
        if action.flags & !FLAGS != 0
            || u32_at(bytes, 0x1C) != 0
            || (handler && (!user(action.handler) || !user(action.restorer)))
        {
            return Err(Error::InvalidArgs);
        }
        Ok(action)
    }

    pub fn bytes(&self) -> [u8; ACTION_SIZE] {
        let mut b = [0u8; ACTION_SIZE];
        b[0x00..0x08].copy_from_slice(&self.handler.to_le_bytes());
        b[0x08..0x10].copy_from_slice(&self.mask.to_le_bytes());
        b[0x10..0x18].copy_from_slice(&self.restorer.to_le_bytes());
        b[0x18..0x1C].copy_from_slice(&self.flags.to_le_bytes());
        b
    }

    /// Whether delivering `signo` with this action discards it.
    pub fn ignores(&self, signo: u32) -> bool {
        match self.handler {
            HANDLER_IGNORE => true,
            HANDLER_DEFAULT => default_action(signo) == Default::Ignore,
            _ => false,
        }
    }
}

/// The size of an information record.
pub const INFO_SIZE: usize = 40;

/// The information a pending signal carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub signo: u32,
    pub code: u32,
    pub sender_koid: u64,
    pub value: u64,
    pub address: u64,
    pub exception_type: u32,
}

impl Info {
    pub fn bytes(&self) -> [u8; INFO_SIZE] {
        let mut b = [0u8; INFO_SIZE];
        b[0x00..0x04].copy_from_slice(&self.signo.to_le_bytes());
        b[0x04..0x08].copy_from_slice(&self.code.to_le_bytes());
        b[0x08..0x10].copy_from_slice(&self.sender_koid.to_le_bytes());
        b[0x10..0x18].copy_from_slice(&self.value.to_le_bytes());
        b[0x18..0x20].copy_from_slice(&self.address.to_le_bytes());
        b[0x20..0x24].copy_from_slice(&self.exception_type.to_le_bytes());
        b
    }
}

/// A queued real-time signal and the budget charged for it, if any,
/// which it releases when it goes.
struct Queued {
    info: Info,
    charged: Option<Arc<Budget>>,
}

/// The kernel memory charged for a queued real-time signal: its place in
/// its queue and that place's share of the queue and map storage.
pub const QUEUED_COST: u64 = 2 * size_of::<Queued>() as u64;

impl Drop for Queued {
    fn drop(&mut self) {
        if let Some(budget) = &self.charged {
            budget.release_kernel(QUEUED_COST);
        }
    }
}

/// The signals pending for a process or a thread.
pub struct Pending {
    /// Signals 1 to 31, at index `signo - 1`.
    standard: [Option<Info>; (SIG_RT_MIN - 1) as usize],
    /// The queued real-time signals, by number. No queue is empty.
    realtime: Map<u32, VecDeque<Queued>>,
}

impl core::default::Default for Pending {
    fn default() -> Self {
        Pending {
            standard: [None; (SIG_RT_MIN - 1) as usize],
            realtime: nodes::map(),
        }
    }
}

impl Pending {
    /// The mask of the pending signals.
    pub fn mask(&self) -> u64 {
        let standard = self.standard.iter().enumerate();
        let standard = standard
            .filter(|(_, i)| i.is_some())
            .map(|(n, _)| 1u64 << n);
        let realtime = self.realtime.keys().map(|&s| bit(s));
        standard.chain(realtime).fold(0, |m, b| m | b)
    }

    /// Makes `info` pending. A standard signal that is pending already is
    /// not queued again. A real-time signal is counted in `queued`, the
    /// count of the whole process, and fails with `ShouldWait` when
    /// `RT_QUEUE_MAX` are queued; it is charged to `budget`, if given,
    /// until it is taken or discarded. Nothing changes on failure.
    pub fn add(
        &mut self,
        info: Info,
        queued: &mut usize,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), Error> {
        if info.signo < SIG_RT_MIN {
            let slot = &mut self.standard[info.signo as usize - 1];
            if slot.is_none() {
                *slot = Some(info);
            }
            return Ok(());
        }
        if *queued >= RT_QUEUE_MAX {
            return Err(Error::ShouldWait);
        }
        if let Some(budget) = budget {
            budget.charge_kernel(QUEUED_COST)?;
        }
        // From here on, dropping the entry releases the charge.
        let entry = Queued {
            info,
            charged: budget.cloned(),
        };
        if let Some(queue) = self.realtime.get_mut(&info.signo) {
            queue.try_reserve(1).map_err(|_| Error::NoMemory)?;
            queue.push_back(entry);
        } else {
            let mut queue = VecDeque::new();
            queue.try_reserve(1).map_err(|_| Error::NoMemory)?;
            queue.push_back(entry);
            let _claim = nodes::claim_insert()?;
            self.realtime.insert(info.signo, queue);
        }
        *queued += 1;
        Ok(())
    }

    /// Removes the pending signal with the lowest number in `allowed`,
    /// the oldest one for a real-time signal.
    pub fn take(&mut self, allowed: u64, queued: &mut usize) -> Option<Info> {
        let signo = (self.mask() & allowed).trailing_zeros() + 1;
        if !valid(signo) {
            return None;
        }
        if signo < SIG_RT_MIN {
            return self.standard[signo as usize - 1].take();
        }
        let queue = self.realtime.get_mut(&signo)?;
        let entry = queue.pop_front();
        if queue.is_empty() {
            self.realtime.remove(&signo);
        }
        *queued -= 1;
        entry.map(|entry| entry.info)
    }

    /// Moves the oldest pending instance of `signo` to `to`, keeping its
    /// information and its charge. A standard signal already pending in
    /// `to` is not queued there again. Returns whether `signo` is
    /// pending in `to` now; when memory runs out, nothing moves.
    pub fn move_one(&mut self, signo: u32, to: &mut Pending) -> bool {
        if !valid(signo) {
            return false;
        }
        if signo < SIG_RT_MIN {
            let index = signo as usize - 1;
            match self.standard[index].take() {
                Some(info) => {
                    to.standard[index].get_or_insert(info);
                    return true;
                }
                None => return to.standard[index].is_some(),
            }
        }
        let Some(queue) = self.realtime.get_mut(&signo) else {
            return to.realtime.contains_key(&signo);
        };
        let target = match to.realtime.get_mut(&signo) {
            Some(target) => target,
            None => {
                let Ok(_claim) = nodes::claim_insert() else {
                    return false;
                };
                to.realtime.insert(signo, VecDeque::new());
                to.realtime.get_mut(&signo).expect("just inserted")
            }
        };
        if target.try_reserve(1).is_err() {
            if target.is_empty() {
                to.realtime.remove(&signo);
            }
            return false;
        }
        let entry = queue.pop_front().expect("no queue is empty");
        if queue.is_empty() {
            self.realtime.remove(&signo);
        }
        target.push_back(entry);
        true
    }

    /// Discards every pending signal in `signals`.
    pub fn discard(&mut self, signals: u64, queued: &mut usize) {
        while self.take(signals, queued).is_some() {}
    }
}

/// The signals that making `signo` pending discards from the process
/// and all its threads.
pub fn discarded_by(signo: u32) -> u64 {
    match signo {
        CONT => STOP_SIGNALS,
        STOP | TSTP | TTIN | TTOU => bit(CONT),
        _ => 0,
    }
}

/// The signal an exception of type `kind` becomes.
pub fn for_exception(kind: u32) -> u32 {
    use crate::exception::kind::*;
    match kind {
        UNDEFINED_INSTRUCTION => ILL,
        ARITHMETIC | FLOATING_POINT => FPE,
        BREAKPOINT | DEBUG => TRAP,
        ALIGNMENT | OUT_OF_MEMORY => BUS,
        _ => SEGV,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(signo: u32, value: u64) -> Info {
        Info {
            signo,
            value,
            ..Info::default()
        }
    }

    #[test]
    fn knows_default_actions() {
        assert_eq!(default_action(KILL), Default::Terminate);
        assert_eq!(default_action(CHLD), Default::Ignore);
        assert_eq!(default_action(CONT), Default::Continue);
        assert_eq!(default_action(TTOU), Default::Stop);
        assert_eq!(default_action(40), Default::Terminate);
        assert_eq!(UNBLOCKABLE, 0x4_0100);
        assert!(!valid(0) && valid(64) && !valid(65));
    }

    #[test]
    fn parses_actions() {
        let user = |a: u64| (0x1_0000..0x7FFF_FFFF_F000).contains(&a);
        let a = Action {
            handler: 0x40_0000,
            mask: 3,
            restorer: 0x40_1000,
            flags: ONSTACK | RESETHAND,
        };
        assert_eq!(Action::parse(&a.bytes(), user), Ok(a));
        let mut b = a.bytes();
        b[0x1C] = 1;
        assert_eq!(Action::parse(&b, user), Err(Error::InvalidArgs));
        let bad_restorer = Action { restorer: 0, ..a };
        assert_eq!(
            Action::parse(&bad_restorer.bytes(), user),
            Err(Error::InvalidArgs)
        );
        let ignore = Action {
            handler: HANDLER_IGNORE,
            restorer: 0,
            ..a
        };
        assert!(Action::parse(&ignore.bytes(), user).unwrap().ignores(SEGV));
        let flags = Action { flags: 1 << 4, ..a };
        assert_eq!(Action::parse(&flags.bytes(), user), Err(Error::InvalidArgs));
        // TERM, signal 15, terminates by default.
        assert!(Action::default().ignores(CHLD) && !Action::default().ignores(15));
    }

    #[test]
    fn queues_real_time_signals_and_merges_standard_ones() {
        let mut p = Pending::default();
        let mut queued = 0;
        p.add(info(SEGV, 1), &mut queued, None).unwrap();
        p.add(info(SEGV, 2), &mut queued, None).unwrap();
        p.add(info(40, 1), &mut queued, None).unwrap();
        p.add(info(33, 1), &mut queued, None).unwrap();
        p.add(info(40, 2), &mut queued, None).unwrap();
        assert_eq!(queued, 3);
        assert_eq!(p.mask(), bit(SEGV) | bit(33) | bit(40));
        assert_eq!(p.take(!0, &mut queued), Some(info(SEGV, 1)));
        assert_eq!(p.take(!bit(33), &mut queued), Some(info(40, 1)));
        assert_eq!(p.take(!0, &mut queued), Some(info(33, 1)));
        assert_eq!(p.take(!0, &mut queued), Some(info(40, 2)));
        assert_eq!((p.take(!0, &mut queued), queued), (None, 0));
    }

    #[test]
    fn moves_one_signal_to_a_thread() {
        let (mut process, mut thread) = (Pending::default(), Pending::default());
        let mut queued = 0;
        process.add(info(SEGV, 1), &mut queued, None).unwrap();
        process.add(info(40, 1), &mut queued, None).unwrap();
        process.add(info(40, 2), &mut queued, None).unwrap();
        thread.add(info(40, 0), &mut queued, None).unwrap();
        assert!(process.move_one(SEGV, &mut thread));
        assert!(process.move_one(40, &mut thread));
        assert_eq!(process.mask(), bit(40));
        // Nothing to move, but pending in the thread already.
        assert!(process.move_one(SEGV, &mut thread));
        assert!(!process.move_one(33, &mut thread));
        assert_eq!(queued, 3);
        assert_eq!(thread.take(!0, &mut queued), Some(info(SEGV, 1)));
        assert_eq!(thread.take(!0, &mut queued), Some(info(40, 0)));
        assert_eq!(thread.take(!0, &mut queued), Some(info(40, 1)));
        assert_eq!(process.take(!0, &mut queued), Some(info(40, 2)));
        assert_eq!(queued, 0);
    }

    #[test]
    fn limits_queued_real_time_signals() {
        let mut p = Pending::default();
        let mut queued = RT_QUEUE_MAX - 1;
        p.add(info(50, 0), &mut queued, None).unwrap();
        assert_eq!(
            p.add(info(50, 0), &mut queued, None),
            Err(Error::ShouldWait)
        );
        p.add(info(SEGV, 0), &mut queued, None).unwrap();
        p.discard(!0, &mut queued);
        assert_eq!((p.mask(), queued), (0, RT_QUEUE_MAX - 1));
    }

    #[test]
    fn leaves_nothing_pending_without_room() {
        let mut p = Pending::default();
        let mut queued = 0;
        let added = crate::nodes::tests::without_claims(|| p.add(info(40, 0), &mut queued, None));
        assert_eq!(added, Err(Error::NoMemory));
        assert_eq!((p.mask(), queued), (0, 0));
        assert_eq!(p.take(!0, &mut queued), None);
    }

    #[test]
    fn charges_queued_signals_until_they_go() {
        use crate::object::budget::UNLIMITED;
        let root = Budget::root();
        let small = Budget::child(&root, QUEUED_COST, UNLIMITED, None).unwrap();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[8..16].try_into().unwrap());
        let mut p = Pending::default();
        let mut queued = 0;
        p.add(info(40, 1), &mut queued, Some(&small)).unwrap();
        assert_eq!(used(&small), QUEUED_COST);
        // Over the limit, nothing is queued.
        assert_eq!(
            p.add(info(41, 1), &mut queued, Some(&small)),
            Err(Error::NoResources)
        );
        assert_eq!((p.mask(), queued), (bit(40), 1));
        // Standard signals are not queued and not charged.
        p.add(info(SEGV, 1), &mut queued, Some(&small)).unwrap();
        assert_eq!(p.take(bit(40), &mut queued), Some(info(40, 1)));
        assert_eq!(used(&small), 0);
        p.add(info(42, 1), &mut queued, Some(&small)).unwrap();
        p.discard(!0, &mut queued);
        assert_eq!((used(&small), used(&root), queued), (0, 0, 0));
    }

    #[test]
    fn maps_exceptions_and_discards() {
        use crate::exception::kind;
        assert_eq!(for_exception(kind::PAGE_FAULT), SEGV);
        assert_eq!(for_exception(kind::GENERAL), SEGV);
        assert_eq!(for_exception(kind::UNDEFINED_INSTRUCTION), ILL);
        assert_eq!(for_exception(kind::FLOATING_POINT), FPE);
        assert_eq!(for_exception(kind::DEBUG), TRAP);
        assert_eq!(for_exception(kind::OUT_OF_MEMORY), BUS);
        assert_eq!(discarded_by(CONT), STOP_SIGNALS);
        assert_eq!(discarded_by(TSTP), bit(CONT));
        assert_eq!(discarded_by(KILL), 0);
    }

    #[test]
    fn writes_information_records() {
        let i = Info {
            signo: 11,
            code: CODE_EXCEPTION,
            sender_koid: 0,
            value: 0,
            address: 0x1234,
            exception_type: 2,
        };
        let b = i.bytes();
        assert_eq!(&b[0..8], &[11, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(
            u64::from_le_bytes(b[0x18..0x20].try_into().unwrap()),
            0x1234
        );
        assert_eq!(&b[0x20..0x28], &[2, 0, 0, 0, 0, 0, 0, 0]);
    }
}
