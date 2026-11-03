//! Decodes system call arguments with the dispatch generated from the
//! system call table, checking handles against a handle table, and
//! compares what each handler receives, or the status of a refused call,
//! with a model of the table. Also decodes `vm_map` and `vm_protect`
//! options and signal actions and compares them with models.

#![no_main]

use kernel_core::object::handles::{Handle, HandleTable};
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::regions::MapOptions;
use kernel_core::signal::{ACTION_SIZE, Action, HANDLER_IGNORE};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The environment the generated dispatch and harness are built in.
mod env {
    use kernel_core::object::handles::HandleTable;
    #[allow(unused_imports)]
    use kernel_core::object::{ObjectType, rights};
    use kernel_core::status::Error;
    use std::cell::RefCell;
    use std::marker::PhantomData;

    thread_local! {
        /// The calling process's handle table.
        pub static TABLE: RefCell<HandleTable> = const { RefCell::new(HandleTable::new()) };
        /// The call a handler received and its arguments.
        pub static CALLED: RefCell<Option<(u64, Vec<Value>)>> = const { RefCell::new(None) };
    }

    /// An argument as a handler received it.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Int(u64),
        /// A checked handle's value and rights.
        Handle(Option<(u32, u32)>),
        Pointer(u64),
    }

    pub struct Checked {
        value: u32,
        rights: u32,
    }

    pub fn handle(raw: u32, types: Option<&[ObjectType]>, rights: u32) -> Result<Checked, Error> {
        TABLE.with_borrow(|table| {
            let entry = table.check(raw, types.unwrap_or(&[]), rights)?;
            Ok(Checked {
                value: raw,
                rights: entry.rights,
            })
        })
    }

    pub struct UserIn<T>(u64, PhantomData<T>);
    pub struct UserOut<T>(u64, PhantomData<T>);
    pub struct UserInOut<T>(u64, PhantomData<T>);

    impl<T> UserIn<T> {
        pub fn new(addr: u64) -> Self {
            UserIn(addr, PhantomData)
        }
    }

    impl<T> UserOut<T> {
        pub fn new(addr: u64) -> Self {
            UserOut(addr, PhantomData)
        }
    }

    impl<T> UserInOut<T> {
        pub fn new(addr: u64) -> Self {
            UserInOut(addr, PhantomData)
        }
    }

    pub trait Record {
        fn record(&self) -> Value;
    }

    impl Record for u32 {
        fn record(&self) -> Value {
            Value::Int(u64::from(*self))
        }
    }

    impl Record for i32 {
        fn record(&self) -> Value {
            Value::Int(i64::from(*self) as u64)
        }
    }

    impl Record for u64 {
        fn record(&self) -> Value {
            Value::Int(*self)
        }
    }

    impl Record for i64 {
        fn record(&self) -> Value {
            Value::Int(*self as u64)
        }
    }

    impl Record for Checked {
        fn record(&self) -> Value {
            Value::Handle(Some((self.value, self.rights)))
        }
    }

    impl Record for Option<Checked> {
        fn record(&self) -> Value {
            Value::Handle(self.as_ref().map(|c| (c.value, c.rights)))
        }
    }

    impl<T> Record for UserIn<T> {
        fn record(&self) -> Value {
            Value::Pointer(self.0)
        }
    }

    impl<T> Record for UserOut<T> {
        fn record(&self) -> Value {
            Value::Pointer(self.0)
        }
    }

    impl<T> Record for UserInOut<T> {
        fn record(&self) -> Value {
            Value::Pointer(self.0)
        }
    }

    pub fn record(number: u64, args: &[Value]) {
        CALLED.set(Some((number, args.to_vec())));
    }

    pub struct CallModel {
        pub number: u64,
        pub fuzz_only: bool,
        pub args: &'static [ArgModel],
    }

    pub enum ArgModel {
        U32,
        U64,
        I32,
        I64,
        Handle,
        Pointer,
        Checked {
            types: Option<&'static [ObjectType]>,
            rights: u32,
            allow_invalid: bool,
        },
    }

    include!(concat!(env!("OUT_DIR"), "/syscalls.rs"));
    include!(concat!(env!("OUT_DIR"), "/harness.rs"));
}

use env::{ArgModel, CALLED, CALLS, TABLE, Value};

const TYPES: [ObjectType; 17] = [
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

struct Plain(Header);

impl Object for Plain {
    fn header(&self) -> &Header {
        &self.0
    }
}

#[derive(Arbitrary, Debug)]
enum Number {
    /// The number of a call of the table, by index.
    Known(u8),
    Raw(u64),
}

#[derive(Arbitrary, Debug)]
enum Arg {
    Raw(u64),
    /// A live handle by index, with `high` in the upper half.
    Live(u8, u32),
}

#[derive(Arbitrary, Debug)]
enum Op {
    /// Adds a handle to an object of a type, by index, with the rights.
    Insert(u8, u32),
    /// Closes a live handle by index.
    Close(u8),
    Call(Number, [Arg; 8]),
    MapOptions(u32, u64, bool),
    Protection(u32, bool),
    /// An action, with the end of the user range.
    Action([u8; ACTION_SIZE], u64),
}

/// The handles in the table, with their object types and rights.
type Model = BTreeMap<Handle, (ObjectType, u32)>;

fn live(model: &Model, index: u8) -> Handle {
    match model.len() {
        0 => 0,
        n => *model.keys().nth(usize::from(index) % n).unwrap(),
    }
}

/// What the handler of call `number` receives with the registers `raw`,
/// or the status of the refused call, as the model sees it.
fn expected(model: &Model, number: u64, raw: &[u64; 8]) -> Result<Vec<Value>, i64> {
    let call = CALLS
        .iter()
        .find(|c| c.number == number && (!c.fuzz_only || cfg!(feature = "fuzz")))
        .ok_or(Error::BadSyscall as i64)?;
    let mut values = Vec::new();
    for (arg, &raw) in call.args.iter().zip(raw) {
        values.push(match *arg {
            ArgModel::U32 | ArgModel::Handle => Value::Int(raw & 0xFFFF_FFFF),
            ArgModel::I32 => Value::Int(i64::from(raw as u32 as i32) as u64),
            ArgModel::U64 | ArgModel::I64 => Value::Int(raw),
            ArgModel::Pointer => Value::Pointer(raw),
            ArgModel::Checked {
                types,
                rights,
                allow_invalid,
            } => {
                let value = raw as u32;
                if allow_invalid && value == 0 {
                    Value::Handle(None)
                } else {
                    // The checks run in order for each argument in turn,
                    // and the first failure is the call's status.
                    let &(kind, held) = model.get(&value).ok_or(Error::BadHandle as i64)?;
                    if types.is_some_and(|t| !t.contains(&kind)) {
                        return Err(Error::WrongType as i64);
                    }
                    if held & rights != rights {
                        return Err(Error::AccessDenied as i64);
                    }
                    Value::Handle(Some((value, held)))
                }
            }
        });
    }
    Ok(values)
}

fn check_call(model: &Model, number: &Number, args: &[Arg; 8]) {
    let number = match *number {
        Number::Known(i) => CALLS[usize::from(i) % CALLS.len()].number,
        Number::Raw(n) => n,
    };
    let raw = args.each_ref().map(|a| match *a {
        Arg::Raw(v) => v,
        Arg::Live(i, high) => u64::from(high) << 32 | u64::from(live(model, i)),
    });
    let status = env::dispatch(number, &raw);
    let called = CALLED.take();
    match expected(model, number, &raw) {
        Ok(values) => {
            assert_eq!(status, 0, "call {number}");
            assert_eq!(called, Some((number, values)));
        }
        Err(e) => {
            assert_eq!(status, e, "call {number}");
            assert_eq!(called, None);
        }
    }
}

/// The bits of `vm_map` options with a meaning.
const PERMS: u32 = 0b111;
const SPECIFIC: u32 = 1 << 8;
const INHERITANCE: u32 = 3 << 9;
const BELOW_4G: u32 = 1 << 11;

/// Whether the permission bits `perms` are allowed: each of write and
/// execute needs read, and both together need `write_execute`.
fn perms_allowed(perms: u32, write_execute: bool) -> bool {
    let readable = perms & 0b110 == 0 || perms & 0b001 != 0;
    readable && (write_execute || perms & 0b110 != 0b110)
}

fn check_map_options(options: u32, addr: u64, write_execute: bool) {
    let specific = options & SPECIFIC != 0;
    let valid = options & !(PERMS | SPECIFIC | INHERITANCE | BELOW_4G) == 0
        && perms_allowed(options & PERMS, write_execute)
        && options & INHERITANCE != INHERITANCE
        && !(specific && options & BELOW_4G != 0)
        && if specific {
            addr % 4096 == 0
        } else {
            addr == 0
        };
    match MapOptions::parse(options, addr, write_execute) {
        Ok(m) => {
            assert!(valid, "accepted {options:#x} at {addr:#x}");
            let back = m.perms
                | u32::from(m.specific) << 8
                | m.inheritance << 9
                | u32::from(m.below_4g) << 11;
            assert_eq!(back, options);
        }
        Err(e) => {
            assert!(!valid, "refused {options:#x} at {addr:#x}");
            assert_eq!(e, Error::InvalidArgs);
        }
    }
}

fn check_protection(options: u32, write_execute: bool) {
    let valid = options & !PERMS == 0 && perms_allowed(options, write_execute);
    match MapOptions::parse_protection(options, write_execute) {
        Ok(perms) => assert!(valid && perms == options),
        Err(e) => assert!(!valid && e == Error::InvalidArgs),
    }
}

fn check_action(bytes: &[u8; ACTION_SIZE], user_end: u64) {
    let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let user = |addr: u64| addr < user_end;
    let (handler, restorer, rest) = (word(0x00), word(0x10), word(0x18));
    // Only the four flag bits may be set, and the reserved word is zero.
    let valid =
        rest & !0xF == 0 && (handler <= HANDLER_IGNORE || (user(handler) && user(restorer)));
    match Action::parse(bytes, user) {
        Ok(action) => {
            assert!(valid);
            assert_eq!(&action.bytes(), bytes);
        }
        Err(e) => assert!(!valid && e == Error::InvalidArgs),
    }
}

fuzz_target!(|ops: Vec<Op>| {
    TABLE.set(HandleTable::new());
    let mut model = Model::new();
    for op in ops.iter().take(256) {
        match op {
            Op::Insert(kind, rights) => {
                let kind = TYPES[usize::from(*kind) % TYPES.len()];
                let object: Arc<dyn Object> = Arc::new(Plain(Header::new(kind)));
                let inserted = TABLE.with_borrow_mut(|t| t.insert(object, *rights, None));
                if let Ok(handle) = inserted {
                    model.insert(handle, (kind, *rights));
                }
            }
            Op::Close(index) => {
                let handle = live(&model, *index);
                if model.remove(&handle).is_some() {
                    TABLE.with_borrow_mut(|t| t.close(handle)).unwrap();
                }
            }
            Op::Call(number, args) => check_call(&model, number, args),
            Op::MapOptions(options, addr, wx) => check_map_options(*options, *addr, *wx),
            Op::Protection(options, wx) => check_protection(*options, *wx),
            Op::Action(bytes, end) => check_action(bytes, *end),
        }
    }
});
