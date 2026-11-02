//! Runs handle table operations and compares the table with a map from
//! handle values to rights.

#![no_main]

use kernel_core::object::handles::{HANDLE_INVALID, Handle, HandleTable};
use kernel_core::object::rights::{ALL, SAME_RIGHTS};
use kernel_core::object::{Header, Object, ObjectType};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use std::sync::Arc;

struct Plain(Header);

impl Object for Plain {
    fn header(&self) -> &Header {
        &self.0
    }
}

#[derive(Arbitrary, Debug)]
enum Op {
    Insert(u32),
    /// Picks a handle by index among the live ones, or any value.
    Close(Pick),
    Duplicate(Pick, u32),
    Replace(Pick, u32),
    Check(Pick, u32),
}

#[derive(Arbitrary, Debug)]
enum Pick {
    Live(u8),
    Raw(u32),
}

fn pick(p: &Pick, model: &HashMap<Handle, u32>) -> Handle {
    match *p {
        Pick::Live(i) if !model.is_empty() => {
            let mut keys: Vec<_> = model.keys().copied().collect();
            keys.sort_unstable();
            keys[usize::from(i) % keys.len()]
        }
        Pick::Live(_) => HANDLE_INVALID,
        Pick::Raw(v) => v,
    }
}

/// The rights a derived handle gets, as the model sees it.
fn derived(source: u32, requested: u32) -> Option<u32> {
    let rights = if requested == SAME_RIGHTS { source } else { requested };
    (rights & !source == 0).then_some(rights)
}

fuzz_target!(|ops: Vec<Op>| {
    let mut table = HandleTable::new();
    let mut model: HashMap<Handle, u32> = HashMap::new();
    let object: Arc<dyn Object> = Arc::new(Plain(Header::new(ObjectType::Event)));
    for op in ops.iter().take(512) {
        match op {
            Op::Insert(rights) => {
                let rights = rights & ALL;
                let h = table.insert(object.clone(), rights, None).unwrap();
                assert!(h != HANDLE_INVALID && model.insert(h, rights).is_none());
            }
            Op::Close(p) => {
                let h = pick(p, &model);
                let expected = h == HANDLE_INVALID || model.remove(&h).is_some();
                assert_eq!(table.close(h).is_ok(), expected);
            }
            Op::Duplicate(p, rights) => {
                let h = pick(p, &model);
                let allowed = model.get(&h).filter(|&&r| r & kernel_core::object::rights::DUPLICATE != 0);
                let expected = allowed.and_then(|&r| derived(r, *rights));
                match table.duplicate(h, *rights, None) {
                    Ok(new) => {
                        assert_eq!(Some(model.get(&h).copied().unwrap()), allowed.copied());
                        assert!(model.insert(new, expected.unwrap()).is_none());
                    }
                    Err(_) => assert_eq!(expected, None),
                }
            }
            Op::Replace(p, rights) => {
                let h = pick(p, &model);
                let expected = model.get(&h).and_then(|&r| derived(r, *rights));
                match table.replace(h, *rights) {
                    Ok(new) => {
                        model.remove(&h);
                        assert!(model.insert(new, expected.unwrap()).is_none());
                    }
                    Err(_) => assert_eq!(expected, None),
                }
            }
            Op::Check(p, required) => {
                let h = pick(p, &model);
                let expected = model.get(&h).is_some_and(|&r| r & required == *required);
                assert_eq!(table.check(h, &[], *required).is_ok(), expected);
            }
        }
        assert_eq!(table.len(), model.len());
        for (&h, &rights) in &model {
            assert_eq!(table.get(h).unwrap().rights, rights);
        }
    }
});
