//! Resources: the rights to ranges of device memory, I/O ports,
//! interrupts and other privileged facilities.

use super::{Header, Object, ObjectType};
use alloc::sync::Arc;

pub const KIND_ROOT: u32 = 0;
pub const KIND_MMIO: u32 = 1;
pub const KIND_IOPORT: u32 = 2;
pub const KIND_IRQ: u32 = 3;
pub const KIND_DMA: u32 = 4;
pub const KIND_PRIORITY: u32 = 5;
pub const KIND_MSI: u32 = 6;
pub const KIND_POLICY: u32 = 7;
pub const KIND_BTI: u32 = 8;

pub struct Resource {
    header: Header,
    kind: u32,
    base: u64,
    size: u64,
    options: u32,
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
        })
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
}

impl Object for Resource {
    fn header(&self) -> &Header {
        &self.header
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_resource_has_the_root_kind() {
        let root = Resource::root();
        assert_eq!(root.kind(), KIND_ROOT);
        assert_eq!(root.header().kind(), ObjectType::Resource);
    }
}
