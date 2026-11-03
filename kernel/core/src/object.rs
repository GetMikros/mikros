//! Kernel objects: their types, IDs, rights and signals.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub mod budget;
pub mod channel;
pub mod event;
pub mod handles;
pub mod memory;
pub mod resource;
pub mod stream;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ObjectType {
    Process = 1,
    Thread = 2,
    MemoryObject = 3,
    Channel = 4,
    Event = 5,
    Port = 6,
    Timer = 7,
    Interrupt = 8,
    Resource = 9,
    Log = 10,
    Budget = 11,
    LogReader = 12,
    Pager = 13,
    Timeline = 14,
    Stream = 15,
    Bti = 16,
    Pmt = 17,
}

impl ObjectType {
    /// Every object type, in the order of their values, which run from 1.
    pub const ALL: [ObjectType; 17] = [
        ObjectType::Process,
        ObjectType::Thread,
        ObjectType::MemoryObject,
        ObjectType::Channel,
        ObjectType::Event,
        ObjectType::Port,
        ObjectType::Timer,
        ObjectType::Interrupt,
        ObjectType::Resource,
        ObjectType::Log,
        ObjectType::Budget,
        ObjectType::LogReader,
        ObjectType::Pager,
        ObjectType::Timeline,
        ObjectType::Stream,
        ObjectType::Bti,
        ObjectType::Pmt,
    ];
}

/// Handle rights.
pub mod rights {
    pub const DUPLICATE: u32 = 1 << 0;
    pub const TRANSFER: u32 = 1 << 1;
    pub const READ: u32 = 1 << 2;
    pub const WRITE: u32 = 1 << 3;
    pub const EXECUTE: u32 = 1 << 4;
    pub const MAP: u32 = 1 << 5;
    pub const WAIT: u32 = 1 << 6;
    pub const SIGNAL: u32 = 1 << 7;
    pub const MANAGE: u32 = 1 << 8;
    pub const INSPECT: u32 = 1 << 9;
    /// Every defined right.
    pub const ALL: u32 = (1 << 10) - 1;
    /// Stands for the rights of the source handle where a call takes
    /// rights.
    pub const SAME_RIGHTS: u32 = 0x8000_0000;
}

use rights::*;

/// The rights of a handle returned by a call that creates an object of
/// type `t`. For memory objects these are the rights of anonymous and
/// paged ones; see `PHYSICAL_MEMORY_DEFAULT_RIGHTS` for the others.
pub const fn default_rights(t: ObjectType) -> u32 {
    use ObjectType::*;
    let base = DUPLICATE | TRANSFER;
    match t {
        Process => base | MAP | WAIT | SIGNAL | MANAGE | INSPECT,
        Thread => base | WAIT | SIGNAL | MANAGE | INSPECT,
        MemoryObject => base | READ | WRITE | EXECUTE | MAP | INSPECT,
        Channel | Stream => base | READ | WRITE | WAIT | INSPECT,
        Event => base | WAIT | SIGNAL | INSPECT,
        Port => base | READ | WRITE | INSPECT,
        Timer | Interrupt => base | WRITE | WAIT | INSPECT,
        Resource | Pager | Bti => base | MANAGE | INSPECT,
        Log => base | READ | WRITE | MANAGE | INSPECT,
        Budget => base | WRITE | WAIT | MANAGE | INSPECT,
        LogReader => base | READ | WAIT | INSPECT,
        Timeline => base | READ | WAIT | SIGNAL | INSPECT,
        Pmt => MANAGE | INSPECT,
    }
}

/// The default rights of physical, contiguous and pinned memory objects.
pub const PHYSICAL_MEMORY_DEFAULT_RIGHTS: u32 =
    DUPLICATE | TRANSFER | READ | WRITE | MAP | MANAGE | INSPECT;

/// Object signals.
pub mod signals {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const PEER_CLOSED: u32 = 1 << 2;
    pub const SIGNALED: u32 = 1 << 3;
    pub const TERMINATED: u32 = 1 << 4;
    pub const SUSPENDED: u32 = 1 << 5;
    pub const OVER_THRESHOLD: u32 = 1 << 6;
    /// `USER0` to `USER7`.
    pub const USER_ALL: u32 = 0xFF << 24;
}

/// The signals an object of type `t` can have set.
pub const fn allowed_signals(t: ObjectType) -> u32 {
    use ObjectType::*;
    use signals::*;
    match t {
        Channel | Stream => READABLE | WRITABLE | PEER_CLOSED,
        LogReader => READABLE,
        Event => SIGNALED | USER_ALL,
        Timer | Interrupt => SIGNALED,
        Process | Thread => TERMINATED | SUSPENDED,
        Budget => OVER_THRESHOLD,
        _ => 0,
    }
}

static NEXT_KOID: AtomicU64 = AtomicU64::new(1);

/// A new object ID, never 0 and never returned before.
fn new_koid() -> u64 {
    let koid = NEXT_KOID.fetch_add(1, Ordering::Relaxed);
    // At one ID per nanosecond this takes 584 years.
    assert!(koid != 0, "object IDs ran out");
    koid
}

/// Called with an object's header and its new signal state whenever the
/// signals of an object with waiters change. It may run with other
/// kernel locks held, so it must not allocate, charge budgets or drop
/// objects.
pub type SignalHook = fn(&Header, u32);

/// The installed `SignalHook`, as an address, or 0 before one is set.
static SIGNAL_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Installs `hook` as the signal hook.
pub fn set_signal_hook(hook: SignalHook) {
    SIGNAL_HOOK.store(hook as usize, Ordering::Release);
}

/// The state every object has.
#[derive(Debug)]
pub struct Header {
    koid: u64,
    kind: ObjectType,
    signals: AtomicU32,
    /// The number of waits registered on the object.
    watchers: AtomicU32,
}

impl Header {
    /// The header of a new object of type `kind`, with no signals set.
    pub fn new(kind: ObjectType) -> Header {
        Header {
            koid: new_koid(),
            kind,
            signals: AtomicU32::new(0),
            watchers: AtomicU32::new(0),
        }
    }

    /// Registers a wait, so that signal changes reach the signal hook.
    pub fn watch(&self) {
        self.watchers.fetch_add(1, Ordering::AcqRel);
    }

    /// Ends a wait registered with `watch`.
    pub fn unwatch(&self) {
        self.watchers.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn koid(&self) -> u64 {
        self.koid
    }

    pub fn kind(&self) -> ObjectType {
        self.kind
    }

    pub fn signals(&self) -> u32 {
        self.signals.load(Ordering::Acquire)
    }

    /// Clears the signals in `clear`, then sets those in `set`, leaving
    /// out signals the object's type cannot have. Returns the new state.
    pub fn update_signals(&self, clear: u32, set: u32) -> u32 {
        let allowed = allowed_signals(self.kind);
        let update = |s: u32| (s & !clear) | (set & allowed);
        let mut old = self.signals.load(Ordering::Acquire);
        loop {
            let new = update(old);
            match self
                .signals
                .compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    if new != old && self.watchers.load(Ordering::Acquire) != 0 {
                        let hook = SIGNAL_HOOK.load(Ordering::Acquire);
                        if hook != 0 {
                            // SAFETY: only `set_signal_hook` stores a
                            // value, which is a `SignalHook`.
                            let hook: SignalHook = unsafe { core::mem::transmute(hook) };
                            hook(self, new);
                        }
                    }
                    return new;
                }
                Err(current) => old = current,
            }
        }
    }
}

/// A kernel object. Handles and kernel references hold it through an
/// `Arc`, so it is destroyed when the last of them is dropped.
pub trait Object: Any + Send + Sync {
    fn header(&self) -> &Header;

    /// The `related_koid` of the object's basic information.
    fn related_koid(&self) -> u64 {
        0
    }
}

/// The object behind `object` as a `T`, if it is one.
pub fn downcast<T: Object>(object: Arc<dyn Object>) -> Option<Arc<T>> {
    let any: Arc<dyn Any + Send + Sync> = object;
    any.downcast::<T>().ok()
}

/// The topic of the basic information record.
pub const INFO_BASIC: u32 = 1;
/// The size of the basic information record.
pub const INFO_BASIC_SIZE: usize = 24;

/// The basic information record of `object` seen through a handle with
/// `rights`.
pub fn basic_info(object: &dyn Object, rights: u32) -> [u8; INFO_BASIC_SIZE] {
    let h = object.header();
    let mut record = [0u8; INFO_BASIC_SIZE];
    record[0..8].copy_from_slice(&h.koid().to_le_bytes());
    record[8..16].copy_from_slice(&object.related_koid().to_le_bytes());
    record[16..20].copy_from_slice(&(h.kind() as u32).to_le_bytes());
    record[20..24].copy_from_slice(&rights.to_le_bytes());
    record
}

/// The longest object name in bytes.
pub const NAME_MAX: usize = 32;

/// An object's name, used only for diagnostics: valid UTF-8 of at most
/// `NAME_MAX` bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; NAME_MAX],
    len: u8,
}

impl Name {
    /// The empty name.
    pub const EMPTY: Name = Name {
        bytes: [0; NAME_MAX],
        len: 0,
    };

    /// The name made of `bytes`, which fails with `InvalidArgs` when they
    /// are too long or not UTF-8.
    pub fn new(bytes: &[u8]) -> Result<Name, crate::status::Error> {
        if bytes.len() > NAME_MAX || core::str::from_utf8(bytes).is_err() {
            return Err(crate::status::Error::InvalidArgs);
        }
        let mut name = Name::EMPTY;
        name.bytes[..bytes.len()].copy_from_slice(bytes);
        name.len = bytes.len() as u8;
        Ok(name)
    }

    pub fn as_str(&self) -> &str {
        // `new` accepts only UTF-8.
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

impl core::fmt::Debug for Name {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

/// The signals user space may change with `object_signal`.
pub const USER_SETTABLE: u32 = signals::SIGNALED | signals::USER_ALL;

/// Changes the user signals of `object`: the checks and the effect of
/// `object_signal` after the handle checks.
pub fn signal_user(object: &dyn Object, clear: u32, set: u32) -> Result<(), crate::status::Error> {
    use crate::status::Error;
    if (clear | set) & !USER_SETTABLE != 0 {
        return Err(Error::InvalidArgs);
    }
    let header = object.header();
    if header.kind() != ObjectType::Event {
        return Err(Error::WrongType);
    }
    header.update_signals(clear, set);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::status::Error;

    pub struct Plain(pub Header);

    impl Object for Plain {
        fn header(&self) -> &Header {
            &self.0
        }
    }

    #[test]
    fn assigns_unique_nonzero_ids() {
        let a = Header::new(ObjectType::Event);
        let b = Header::new(ObjectType::Event);
        assert!(a.koid() != 0 && b.koid() > a.koid());
    }

    #[test]
    fn lists_every_object_type_by_value() {
        assert_eq!(
            ObjectType::ALL.len(),
            core::mem::variant_count::<ObjectType>()
        );
        for (i, t) in ObjectType::ALL.into_iter().enumerate() {
            assert_eq!(t as usize, i + 1, "{t:?}");
        }
    }

    #[test]
    fn default_rights_stay_within_the_defined_bits() {
        for t in ObjectType::ALL {
            assert_eq!(default_rights(t) & !ALL, 0);
        }
        assert_eq!(default_rights(ObjectType::Pmt), MANAGE | INSPECT);
        assert_eq!(default_rights(ObjectType::Event), 0b10_1100_0011);
        assert_eq!(PHYSICAL_MEMORY_DEFAULT_RIGHTS, 0b11_0010_1111);
    }

    #[test]
    fn keeps_signals_to_those_of_the_type() {
        let h = Header::new(ObjectType::Timer);
        assert_eq!(
            h.update_signals(0, signals::SIGNALED | signals::READABLE),
            signals::SIGNALED
        );
        assert_eq!(h.update_signals(signals::SIGNALED, 0), 0);
        assert_eq!(Header::new(ObjectType::Port).update_signals(0, u32::MAX), 0);
    }

    #[test]
    fn writes_the_basic_record() {
        let event = Plain(Header::new(ObjectType::Event));
        let r = basic_info(&event, 0x2C3);
        assert_eq!(
            u64::from_le_bytes(r[0..8].try_into().unwrap()),
            event.0.koid()
        );
        assert_eq!(&r[8..16], &[0; 8]);
        assert_eq!(&r[16..24], &[5, 0, 0, 0, 0xC3, 2, 0, 0]);
    }

    #[test]
    fn checks_names() {
        assert_eq!(Name::new(b"init").unwrap().as_str(), "init");
        assert_eq!(Name::new(&[b'a'; 32]).unwrap().as_str().len(), 32);
        assert!(Name::new(&[b'a'; 33]).is_err());
        assert!(Name::new(&[0xC3]).is_err());
        assert_eq!(Name::new("ü".as_bytes()).unwrap().as_str(), "ü");
        assert_eq!(Name::new(b"").unwrap(), Name::EMPTY);
    }

    #[test]
    fn signals_events_only_with_user_bits() {
        let event = Plain(Header::new(ObjectType::Event));
        assert_eq!(
            signal_user(&event, 0, signals::READABLE),
            Err(Error::InvalidArgs)
        );
        signal_user(&event, 0, signals::SIGNALED | 1 << 31).unwrap();
        signal_user(&event, signals::SIGNALED, 1 << 24).unwrap();
        assert_eq!(event.0.signals(), 1 << 31 | 1 << 24);
        let timer = Plain(Header::new(ObjectType::Timer));
        assert_eq!(
            signal_user(&timer, 0, signals::SIGNALED),
            Err(Error::WrongType)
        );
    }
}
