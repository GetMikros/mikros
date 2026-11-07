//! What a file holds: a resizable anonymous memory object, made on the
//! first write, and the file's size, which the object covers rounded up
//! to a page.
//!
//! TODO: the object's pages are charged to the server's own budget, not
//! to the process that writes the data (MIKSPEC-0050.R9,
//! MIKSPEC-0021.R15).

use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const RESIZABLE: u32 = 1 << 0;
const PAGE: u64 = 4096;

#[derive(Default)]
pub struct File {
    pub mem: Option<OwnedHandle>,
    pub size: u64,
}

/// Why changing a file failed.
pub enum Failed {
    /// The kernel refused memory.
    NoSpace,
    Io,
}

impl File {
    /// The bytes from `offset`, at most `count`.
    pub fn read(&self, offset: u64, count: usize) -> Result<alloc::vec::Vec<u8>, Failed> {
        let end = offset.saturating_add(count as u64).min(self.size);
        let Some(mem) = &self.mem else {
            return Ok(alloc::vec::Vec::new());
        };
        if offset >= end {
            return Ok(alloc::vec::Vec::new());
        }
        let mut data = alloc::vec![0u8; (end - offset) as usize];
        // SAFETY: the pointer and length describe `data`.
        let status =
            unsafe { sys::mem_read(mem.raw(), offset, data.as_mut_ptr(), data.len() as u64) };
        match status {
            mikros_rt::OK => Ok(data),
            _ => Err(Failed::Io),
        }
    }

    /// Makes the object cover `size` bytes, creating it when the file has
    /// none.
    fn reserve(&mut self, size: u64) -> Result<(), Failed> {
        let capacity = size.next_multiple_of(PAGE).max(PAGE);
        match &self.mem {
            None => {
                let mut raw = 0;
                // SAFETY: the out pointer is valid for the call.
                if unsafe { sys::mem_create(capacity, RESIZABLE, &mut raw) } != mikros_rt::OK {
                    return Err(Failed::NoSpace);
                }
                // SAFETY: the kernel just created the handle for us.
                self.mem = Some(unsafe { OwnedHandle::from_raw(raw) });
                Ok(())
            }
            Some(mem) if capacity > self.size.next_multiple_of(PAGE).max(PAGE) => {
                // SAFETY: the call takes no pointers.
                match unsafe { sys::mem_set_size(mem.raw(), capacity) } {
                    mikros_rt::OK => Ok(()),
                    _ => Err(Failed::NoSpace),
                }
            }
            Some(_) => Ok(()),
        }
    }

    /// Writes `data` at `offset`, growing the file, with zeros between
    /// its old end and `offset`.
    pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Failed> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(Failed::NoSpace)?;
        if data.is_empty() {
            return Ok(());
        }
        self.reserve(end.max(self.size))?;
        let mem = self.mem.as_ref().ok_or(Failed::Io)?;
        // SAFETY: the pointer and length describe `data`.
        let status = unsafe { sys::mem_write(mem.raw(), offset, data.as_ptr(), data.len() as u64) };
        if status != mikros_rt::OK {
            return Err(Failed::Io);
        }
        self.size = self.size.max(end);
        Ok(())
    }

    /// Sets the size to `size`: the bytes past it are gone, and those it
    /// gains read as zero.
    pub fn truncate(&mut self, size: u64) -> Result<(), Failed> {
        if size > self.size {
            self.reserve(size)?;
        } else if let Some(mem) = &self.mem {
            let capacity = size.next_multiple_of(PAGE).max(PAGE);
            // SAFETY: the call takes no pointers.
            if unsafe { sys::mem_set_size(mem.raw(), capacity) } != mikros_rt::OK {
                return Err(Failed::Io);
            }
            // The rest of the last page reads as zero once the file grows.
            let tail = (capacity - size) as usize;
            if tail > 0 {
                let zeros = alloc::vec![0u8; tail];
                // SAFETY: the pointer and length describe `zeros`.
                unsafe { sys::mem_write(mem.raw(), size, zeros.as_ptr(), tail as u64) };
            }
        }
        self.size = size;
        Ok(())
    }

    /// The bytes the object takes.
    pub fn allocated(&self) -> u64 {
        match &self.mem {
            Some(_) => self.size.next_multiple_of(PAGE).max(PAGE),
            None => 0,
        }
    }
}
