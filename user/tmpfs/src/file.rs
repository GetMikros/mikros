//! What a file holds: a resizable anonymous memory object, made on the
//! first write, and the file's size, which the object covers rounded up
//! to a page. Writing comes later; files are empty for now.
//!
//! TODO: the object's pages are charged to the server's own budget, not
//! to the process that writes the data (MIKSPEC-0050.R9,
//! MIKSPEC-0021.R15).

use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const PAGE: u64 = 4096;

#[derive(Default)]
pub struct File {
    pub mem: Option<OwnedHandle>,
    pub size: u64,
}

impl File {
    /// The bytes from `offset`, at most `count`; none that the kernel
    /// could not read.
    pub fn read(&self, offset: u64, count: usize) -> alloc::vec::Vec<u8> {
        let end = offset.saturating_add(count as u64).min(self.size);
        let Some(mem) = &self.mem else {
            return alloc::vec::Vec::new();
        };
        if offset >= end {
            return alloc::vec::Vec::new();
        }
        let mut data = alloc::vec![0u8; (end - offset) as usize];
        // SAFETY: the pointer and length describe `data`.
        let status =
            unsafe { sys::mem_read(mem.raw(), offset, data.as_mut_ptr(), data.len() as u64) };
        match status {
            mikros_rt::OK => data,
            _ => alloc::vec::Vec::new(),
        }
    }

    /// The bytes the object takes.
    pub fn allocated(&self) -> u64 {
        match &self.mem {
            Some(_) => self.size.next_multiple_of(PAGE).max(PAGE),
            None => 0,
        }
    }
}
