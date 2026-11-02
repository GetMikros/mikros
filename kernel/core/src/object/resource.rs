//! Resources: the rights to ranges of device memory, I/O ports,
//! interrupts and other privileged facilities.

use super::budget::Budget;
use super::{Header, Object, ObjectType};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

pub const KIND_ROOT: u32 = 0;
pub const KIND_MMIO: u32 = 1;
pub const KIND_IOPORT: u32 = 2;
pub const KIND_IRQ: u32 = 3;
pub const KIND_DMA: u32 = 4;
pub const KIND_PRIORITY: u32 = 5;
pub const KIND_MSI: u32 = 6;
pub const KIND_POLICY: u32 = 7;
pub const KIND_BTI: u32 = 8;

/// The option that lets the range overlap other `SHARED` resources.
pub const SHARED: u32 = 1 << 0;

/// The number of I/O ports.
const IOPORT_COUNT: u64 = 0x1_0000;
/// The end of the range of `PRIORITY` resources, exclusive.
const PRIORITY_END: u64 = 32;

/// Every live resource except the root, for the overlap rules.
static LIVE: SpinLock<Vec<Weak<Resource>>> = SpinLock::new(Vec::new());

/// What `resource_create` asks for: a kind, the `size` units from
/// `base`, and options.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub kind: u32,
    pub base: u64,
    pub size: u64,
    pub options: u32,
}

pub struct Resource {
    header: Header,
    kind: u32,
    base: u64,
    size: u64,
    options: u32,
    parent: Option<Arc<Resource>>,
    /// The budget charged for the resource, and the amount.
    charged: Option<(Arc<Budget>, u64)>,
}

/// Whether the kinds have no range.
fn rangeless(kind: u32) -> bool {
    matches!(kind, KIND_DMA | KIND_MSI | KIND_POLICY)
}

impl Resource {
    /// The `ROOT` resource, whose range is every range of every other
    /// kind. The kernel creates exactly one.
    pub fn root() -> Arc<Resource> {
        Arc::new(Resource {
            header: Header::new(ObjectType::Resource),
            kind: KIND_ROOT,
            base: 0,
            size: 0,
            options: 0,
            parent: None,
            charged: None,
        })
    }

    /// A new resource as `request` asks for, made from `parent` and
    /// charged to `budget`. `has_ioports` says whether
    /// the architecture has I/O ports, and `denied` whether the kernel
    /// keeps a range of a kind, given by start and exclusive end, for
    /// itself.
    pub fn create(
        parent: &Arc<Resource>,
        request: Request,
        budget: &Arc<Budget>,
        has_ioports: bool,
        denied: impl Fn(u32, u64, u64) -> bool,
    ) -> Result<Arc<Resource>, Error> {
        let Request {
            kind,
            base,
            size,
            options,
        } = request;
        let known = (KIND_MMIO..=KIND_BTI).contains(&kind);
        let fits_kind = parent.kind == KIND_ROOT || parent.kind == kind;
        let size_ok = if rangeless(kind) {
            base == 0 && size == 0
        } else {
            size != 0
        };
        if !known || !fits_kind || !size_ok || options & !SHARED != 0 {
            return Err(Error::InvalidArgs);
        }
        let end = base.checked_add(size).ok_or(Error::OutOfRange)?;
        if kind == KIND_PRIORITY && end > PRIORITY_END {
            return Err(Error::OutOfRange);
        }
        if kind == KIND_IOPORT {
            if !has_ioports {
                return Err(Error::NotSupported);
            }
            if end > IOPORT_COUNT {
                return Err(Error::OutOfRange);
            }
        }
        if parent.kind != KIND_ROOT && (base < parent.base || end > parent.base + parent.size) {
            return Err(Error::OutOfRange);
        }
        if !rangeless(kind) && denied(kind, base, end) {
            return Err(Error::AccessDenied);
        }
        let cost = size_of::<Resource>() as u64;
        let mut live = LIVE.lock();
        live.retain(|r| r.strong_count() > 0);
        let shared = options & SHARED != 0;
        let conflict = kind != KIND_PRIORITY
            && live.iter().filter_map(Weak::upgrade).any(|r| {
                r.kind == kind
                    && r.base < end
                    && base < r.base + r.size
                    && (!shared || r.options & SHARED == 0)
                    && !r.is_ancestor_of(parent)
            });
        if conflict {
            return Err(Error::AlreadyExists);
        }
        live.try_reserve(1).map_err(|_| Error::NoMemory)?;
        budget.charge_kernel(cost)?;
        let resource = crate::fallible::try_arc(Resource {
            header: Header::new(ObjectType::Resource),
            kind,
            base,
            size,
            options,
            parent: Some(parent.clone()),
            charged: Some((budget.clone(), cost)),
        })?;
        live.push(Arc::downgrade(&resource));
        Ok(resource)
    }

    /// Whether this resource is `other` or one of its ancestors.
    fn is_ancestor_of(&self, other: &Resource) -> bool {
        let mut at = Some(other);
        while let Some(r) = at {
            if core::ptr::eq(r, self) {
                return true;
            }
            at = r.parent.as_deref();
        }
        false
    }

    pub fn kind(&self) -> u32 {
        self.kind
    }

    /// The range's base and size.
    pub fn range(&self) -> (u64, u64) {
        (self.base, self.size)
    }

    pub fn options(&self) -> u32 {
        self.options
    }

    /// Whether the range of this resource covers the `size` units from
    /// `base`.
    pub fn covers(&self, base: u64, size: u64) -> bool {
        base.checked_add(size)
            .is_some_and(|end| base >= self.base && end <= self.base + self.size)
    }
}

impl Drop for Resource {
    fn drop(&mut self) {
        if let Some((budget, cost)) = &self.charged {
            budget.release_kernel(*cost);
        }
    }
}

impl Object for Resource {
    fn header(&self) -> &Header {
        &self.header
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(
        parent: &Arc<Resource>,
        kind: u32,
        base: u64,
        size: u64,
        options: u32,
    ) -> Result<Arc<Resource>, Error> {
        let budget = Budget::root();
        let request = Request {
            kind,
            base,
            size,
            options,
        };
        Resource::create(parent, request, &budget, true, |_, _, _| false)
    }

    #[test]
    fn the_root_resource_has_the_root_kind() {
        let root = Resource::root();
        assert_eq!(root.kind(), KIND_ROOT);
        assert_eq!(root.header().kind(), ObjectType::Resource);
    }

    #[test]
    fn checks_kinds_and_ranges() {
        let root = Resource::root();
        let e = |r: Result<Arc<Resource>, Error>| r.err();
        assert_eq!(e(make(&root, KIND_ROOT, 0, 1, 0)), Some(Error::InvalidArgs));
        assert_eq!(e(make(&root, 9, 0, 1, 0)), Some(Error::InvalidArgs));
        assert_eq!(
            e(make(&root, KIND_IRQ, 0x1000, 0, 0)),
            Some(Error::InvalidArgs)
        );
        assert_eq!(e(make(&root, KIND_DMA, 0, 1, 0)), Some(Error::InvalidArgs));
        assert_eq!(e(make(&root, KIND_IRQ, 0, 1, 2)), Some(Error::InvalidArgs));
        assert_eq!(
            e(make(&root, KIND_PRIORITY, 30, 3, 0)),
            Some(Error::OutOfRange)
        );
        assert_eq!(
            e(make(&root, KIND_IOPORT, 0xFFFF, 2, 0)),
            Some(Error::OutOfRange)
        );
        assert_eq!(
            e(make(&root, KIND_MMIO, u64::MAX, 2, 0)),
            Some(Error::OutOfRange)
        );
        assert!(make(&root, KIND_DMA, 0, 0, 0).is_ok());
        assert!(make(&root, KIND_PRIORITY, 0, 32, 0).is_ok());
        let budget = Budget::root();
        let no_ports = Resource::create(
            &root,
            Request {
                kind: KIND_IOPORT,
                base: 0,
                size: 1,
                options: 0,
            },
            &budget,
            false,
            |_, _, _| false,
        );
        assert_eq!(no_ports.err(), Some(Error::NotSupported));
        let denied = Resource::create(
            &root,
            Request {
                kind: KIND_MMIO,
                base: 0xF000,
                size: 0x10,
                options: 0,
            },
            &budget,
            true,
            |k, b, e| k == KIND_MMIO && b < 0xF001 && e > 0xF000,
        );
        assert_eq!(denied.err(), Some(Error::AccessDenied));
    }

    #[test]
    fn children_stay_inside_their_parent() {
        let root = Resource::root();
        let parent = make(&root, KIND_IRQ, 100, 10, 0).unwrap();
        assert_eq!(
            make(&parent, KIND_MMIO, 100, 1, 0).err(),
            Some(Error::InvalidArgs)
        );
        assert_eq!(
            make(&parent, KIND_IRQ, 105, 6, 0).err(),
            Some(Error::OutOfRange)
        );
        let child = make(&parent, KIND_IRQ, 105, 5, 0).unwrap();
        assert!(child.covers(106, 4) && !child.covers(104, 2));
        // A grandchild may overlap its ancestors.
        assert!(make(&child, KIND_IRQ, 105, 1, 0).is_ok());
    }

    #[test]
    fn keeps_ranges_exclusive() {
        let root = Resource::root();
        let a = make(&root, KIND_IOPORT, 0x200, 0x10, 0).unwrap();
        assert_eq!(
            make(&root, KIND_IOPORT, 0x20F, 1, 0).err(),
            Some(Error::AlreadyExists)
        );
        assert_eq!(
            make(&root, KIND_IOPORT, 0x20F, 1, SHARED).err(),
            Some(Error::AlreadyExists)
        );
        assert!(make(&root, KIND_IOPORT, 0x210, 1, 0).is_ok());
        drop(a);
        let s = make(&root, KIND_IOPORT, 0x200, 0x10, SHARED).unwrap();
        assert!(make(&root, KIND_IOPORT, 0x208, 0x10, SHARED).is_ok());
        assert_eq!(
            make(&root, KIND_IOPORT, 0x208, 1, 0).err(),
            Some(Error::AlreadyExists)
        );
        // Priorities may always overlap.
        let _p = make(&root, KIND_PRIORITY, 0, 8, 0).unwrap();
        assert!(make(&root, KIND_PRIORITY, 4, 8, 0).is_ok());
        drop(s);
    }
}
