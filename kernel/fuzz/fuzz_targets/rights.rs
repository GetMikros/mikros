//! Checks handles of every object type against accepted types and
//! required rights.

#![no_main]

use kernel_core::object::handles::HandleTable;
use kernel_core::object::rights::ALL;
use kernel_core::object::{Header, Object, ObjectType, default_rights};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

struct Plain(Header);

impl Object for Plain {
    fn header(&self) -> &Header {
        &self.0
    }
}

const TYPES: &[ObjectType] = &ObjectType::ALL;

#[derive(Arbitrary, Debug)]
struct Case {
    kind: u8,
    /// The handle's rights, or the type's defaults when absent.
    rights: Option<u32>,
    accepted: Vec<u8>,
    required: u32,
}

fuzz_target!(|cases: Vec<Case>| {
    let mut table = HandleTable::new();
    for c in cases.iter().take(256) {
        let kind = TYPES[usize::from(c.kind) % TYPES.len()];
        assert_eq!(default_rights(kind) & !ALL, 0);
        let rights = c.rights.map_or(default_rights(kind), |r| r & ALL);
        let object: Arc<dyn Object> = Arc::new(Plain(Header::new(kind)));
        let h = table.insert(object, rights, None).unwrap();
        let accepted: Vec<ObjectType> =
            c.accepted.iter().take(4).map(|&k| TYPES[usize::from(k) % TYPES.len()]).collect();
        let type_ok = accepted.is_empty() || accepted.contains(&kind);
        let expected = type_ok && rights & c.required == c.required;
        assert_eq!(table.check(h, &accepted, c.required).is_ok(), expected);
        table.close(h).unwrap();
    }
});
