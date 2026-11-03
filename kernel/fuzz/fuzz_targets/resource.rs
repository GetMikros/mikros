//! Creates resources from the root and from each other, and drops them,
//! as `resource_create` and closing handles do. Compares every result,
//! including the overlap rules between live resources, and every
//! resource's kind, range, options and coverage with a model.

#![no_main]

use kernel_core::object::budget::{Budget, UNLIMITED};
use kernel_core::object::resource::{
    KIND_BTI, KIND_DMA, KIND_IOPORT, KIND_MMIO, KIND_MSI, KIND_POLICY, KIND_PRIORITY, KIND_ROOT,
    Request, Resource, SHARED,
};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::sync::{Arc, Weak};

const MAX: usize = 32;

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Value {
    /// Small values, so that ranges meet and overlap.
    Small(u8),
    Raw(u64),
}

impl Value {
    fn get(self) -> u64 {
        match self {
            Value::Small(v) => u64::from(v),
            Value::Raw(v) => v,
        }
    }
}

#[derive(Arbitrary, Debug)]
enum Op {
    Create {
        parent: u8,
        kind: u8,
        base: Value,
        size: Value,
        options: u32,
        /// Whether the budget charged has no room.
        full: bool,
    },
    DropHandle(u8),
    Covers {
        resource: u8,
        base: Value,
        size: Value,
    },
}

#[derive(Arbitrary, Debug)]
struct Input {
    has_ioports: bool,
    /// The range the kernel keeps for itself, of one kind.
    denied: (u8, u8, u8),
    ops: Vec<Op>,
}

struct Node {
    kind: u32,
    base: u64,
    size: u64,
    options: u32,
    parent: Option<usize>,
    handle: Option<Arc<Resource>>,
    weak: Weak<Resource>,
}

struct Model {
    nodes: Vec<Node>,
}

fn rangeless(kind: u32) -> bool {
    matches!(kind, KIND_DMA | KIND_MSI | KIND_POLICY)
}

impl Model {
    /// Resources are alive while they have a handle or a live child.
    fn alive(&self) -> Vec<bool> {
        let mut alive = vec![false; self.nodes.len()];
        for i in (0..self.nodes.len()).rev() {
            alive[i] |= self.nodes[i].handle.is_some();
            if let (true, Some(p)) = (alive[i], self.nodes[i].parent) {
                alive[p] = true;
            }
        }
        alive
    }

    fn ancestors(&self, mut i: usize) -> Vec<usize> {
        let mut out = vec![i];
        while let Some(p) = self.nodes[i].parent {
            out.push(p);
            i = p;
        }
        out
    }

    /// The result `create` must give.
    fn create(
        &self,
        p: usize,
        r: Request,
        full: bool,
        has_ioports: bool,
        denied: impl Fn(u32, u64, u64) -> bool,
    ) -> Result<(), Error> {
        let parent = &self.nodes[p];
        let size_ok = if rangeless(r.kind) {
            r.base == 0 && r.size == 0
        } else {
            r.size != 0
        };
        if !(KIND_MMIO..=KIND_BTI).contains(&r.kind)
            || parent.kind != KIND_ROOT && parent.kind != r.kind
            || !size_ok
            || r.options & !SHARED != 0
        {
            return Err(Error::InvalidArgs);
        }
        let end = r.base.checked_add(r.size).ok_or(Error::OutOfRange)?;
        if r.kind == KIND_PRIORITY && end > 32 {
            return Err(Error::OutOfRange);
        }
        if r.kind == KIND_IOPORT && !has_ioports {
            return Err(Error::NotSupported);
        }
        if r.kind == KIND_IOPORT && end > 0x1_0000 {
            return Err(Error::OutOfRange);
        }
        if parent.kind != KIND_ROOT && (r.base < parent.base || end > parent.base + parent.size) {
            return Err(Error::OutOfRange);
        }
        if !rangeless(r.kind) && denied(r.kind, r.base, end) {
            return Err(Error::AccessDenied);
        }
        let alive = self.alive();
        let ancestors = self.ancestors(p);
        let shared = r.options & SHARED != 0;
        let conflict = r.kind != KIND_PRIORITY
            && self.nodes.iter().enumerate().skip(1).any(|(i, n)| {
                alive[i]
                    && n.kind == r.kind
                    && n.base < end
                    && r.base < n.base + n.size
                    && (!shared || n.options & SHARED == 0)
                    && !ancestors.contains(&i)
            });
        if conflict {
            return Err(Error::AlreadyExists);
        }
        if full {
            return Err(Error::NoResources);
        }
        Ok(())
    }

    fn check(&self) {
        let alive = self.alive();
        for (i, n) in self.nodes.iter().enumerate() {
            let r = n.weak.upgrade();
            assert_eq!(r.is_some(), alive[i], "resource {i} alive");
            if let Some(r) = r {
                assert_eq!((r.kind(), r.range(), r.options()), (n.kind, (n.base, n.size), n.options));
            }
        }
    }
}

fuzz_target!(|input: Input| {
    let root = Resource::root();
    let budget = Budget::root();
    let full = Budget::child(&budget, 0, UNLIMITED, None).unwrap();
    let (dk, ds, de) = input.denied;
    let denied_kind = u32::from(dk) % 9;
    let (ds, de) = (u64::from(ds), u64::from(de));
    let denied = |kind: u32, start: u64, end: u64| kind == denied_kind && start < de && ds < end;
    let mut m = Model {
        nodes: vec![Node {
            kind: KIND_ROOT,
            base: 0,
            size: 0,
            options: 0,
            parent: None,
            weak: Arc::downgrade(&root),
            handle: Some(root),
        }],
    };
    for op in input.ops.iter().take(256) {
        match *op {
            Op::Create {
                parent,
                kind,
                base,
                size,
                options,
                full: no_room,
            } => {
                let p = usize::from(parent) % m.nodes.len();
                let Some(parent) = m.nodes[p].handle.clone() else { continue };
                if m.nodes.len() == MAX {
                    continue;
                }
                let request = Request {
                    kind: u32::from(kind) % 10,
                    base: base.get(),
                    size: size.get(),
                    options: options & 3,
                };
                let charged = if no_room { &full } else { &budget };
                let result = Resource::create(&parent, request, charged, input.has_ioports, denied);
                let want = m.create(p, request, no_room, input.has_ioports, denied);
                assert_eq!(result.as_ref().map(|_| ()).map_err(|e| *e), want);
                if let Ok(r) = result {
                    m.nodes.push(Node {
                        kind: request.kind,
                        base: request.base,
                        size: request.size,
                        options: request.options,
                        parent: Some(p),
                        weak: Arc::downgrade(&r),
                        handle: Some(r),
                    });
                }
            }
            Op::DropHandle(i) => {
                let i = usize::from(i) % m.nodes.len();
                if i > 0 {
                    m.nodes[i].handle = None;
                }
            }
            Op::Covers { resource, base, size } => {
                let n = &m.nodes[usize::from(resource) % m.nodes.len()];
                let Some(r) = n.weak.upgrade() else { continue };
                let (base, size) = (base.get(), size.get());
                let want = base
                    .checked_add(size)
                    .is_some_and(|end| base >= n.base && end <= n.base + n.size);
                assert_eq!(r.covers(base, size), want);
            }
        }
        m.check();
    }
    // Dropping every resource releases every charge.
    drop(m);
    assert_eq!(budget.info()[0x08..0x10], [0; 8]);
});
