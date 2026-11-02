//! Anonymous memory objects: their frames and the copies to and from them.

use crate::aspace::{AddressSpace, USER_END, USER_START};
use crate::memory::FRAMES;
use crate::space;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use kernel_core::object::budget::Budget;
use kernel_core::object::memory::{INFO_MEMORY_SIZE, PAGE_ENTRY_COST, PAGE_SIZE, Pages};
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The largest memory object: the size of the user address range.
const MAX_SIZE: u64 = USER_END - USER_START;

/// The most bytes copied while the pages are locked. Copies to and from
/// user memory happen in between, without the lock, as they can fault.
const CHUNK: usize = 256;

pub struct MemoryObject {
    header: Header,
    /// The budget of the creating process, which its frames are charged
    /// to.
    budget: Arc<Budget>,
    /// Kernel memory charged to the budget for the object itself.
    charged: u64,
    pages: SpinLock<Pages>,
    /// The address spaces with regions that map the object, so that
    /// pages can be unmapped when the object changes.
    spaces: SpinLock<Vec<Weak<AddressSpace>>>,
}

/// The bytes of the frame at `phys`.
///
/// # Safety
///
/// The frame must belong to a memory object whose pages are locked, and
/// nothing else may reference its bytes while the result is used.
unsafe fn frame_bytes<'a>(phys: u64) -> &'a mut [u8; PAGE_SIZE as usize] {
    // SAFETY: the direct map covers every frame, and the caller rules out
    // other references.
    unsafe { &mut *(space::direct(phys) as *mut [u8; PAGE_SIZE as usize]) }
}

/// The page index and the offset inside the page of byte `at`, and the
/// length of the chunk from there, which ends at the page's end, after
/// `CHUNK` bytes or after `left` bytes.
fn chunk(at: u64, left: u64) -> (u64, usize, usize) {
    let inside = (at % PAGE_SIZE) as usize;
    let len = (PAGE_SIZE as usize - inside).min(CHUNK);
    (at / PAGE_SIZE, inside, len.min(left as usize))
}

impl MemoryObject {
    /// An anonymous memory object of `size` bytes created with `options`,
    /// charged to `budget`.
    pub fn new(budget: &Arc<Budget>, size: u64, options: u32) -> Result<Arc<MemoryObject>, Error> {
        let pages = Pages::new(size, options, MAX_SIZE)?;
        let charged = size_of::<MemoryObject>() as u64;
        budget.charge_kernel(charged)?;
        Ok(Arc::new(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.clone(),
            charged,
            pages: SpinLock::new(pages),
            spaces: SpinLock::new(Vec::new()),
        }))
    }

    /// Fails with `OutOfRange` unless the `len` bytes from `offset` lie
    /// inside the object.
    pub fn check_range(&self, offset: u64, len: u64) -> Result<(), Error> {
        self.pages.lock().check_range(offset, len)
    }

    /// Records that `space` has regions that map the object.
    pub fn add_space(&self, space: &Arc<AddressSpace>) {
        let mut spaces = self.spaces.lock();
        spaces.retain(|s| s.strong_count() > 0);
        if !spaces.iter().any(|s| core::ptr::eq(s.as_ptr(), &**space)) {
            spaces.push(Arc::downgrade(space));
        }
    }

    /// Records that `space` has no regions left that map the object.
    pub fn remove_space(&self, space: &AddressSpace) {
        self.spaces
            .lock()
            .retain(|s| !core::ptr::eq(s.as_ptr(), space));
    }

    /// Removes the pages that map pages `first` to `end`, exclusive, of
    /// the object from every address space. The pages must be locked.
    fn unmap_pages(&self, first: u64, end: u64) {
        let spaces: Vec<_> = self
            .spaces
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for space in spaces {
            space.unmap_object(self, first, end);
        }
    }

    /// Gives page `index` a zeroed frame, charging the frame to the
    /// budget, and returns it.
    fn alloc(&self, pages: &mut Pages, index: u64) -> Result<u64, Error> {
        self.budget.charge_pages(1)?;
        if let Err(e) = self.budget.charge_kernel(PAGE_ENTRY_COST) {
            self.budget.release_pages(1);
            return Err(e);
        }
        let Some(phys) = FRAMES.lock().as_mut().and_then(|f| f.alloc()) else {
            self.release(1);
            return Err(Error::NoMemory);
        };
        // SAFETY: the frame was just allocated, so nothing references it.
        unsafe { frame_bytes(phys) }.fill(0);
        pages.insert(index, phys);
        Ok(phys)
    }

    /// Releases the charges of `count` frames.
    fn release(&self, count: u64) {
        self.budget.release_pages(count);
        self.budget.release_kernel(count * PAGE_ENTRY_COST);
    }

    /// Frees frames taken from the pages and releases their charges.
    fn free(&self, frames: impl IntoIterator<Item = u64>) {
        let mut count = 0;
        if let Some(allocator) = FRAMES.lock().as_mut() {
            for phys in frames {
                allocator.free(phys, 1);
                count += 1;
            }
        }
        self.release(count);
    }

    /// Passes the `len` bytes from `offset` to `sink`, in chunks. Pages
    /// without a frame read as zeros and get none.
    pub fn read(
        &self,
        offset: u64,
        len: u64,
        mut sink: impl FnMut(&[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.check_range(offset, len)?;
        let mut done = 0;
        while done < len {
            let (index, inside, n) = chunk(offset + done, len - done);
            let mut buf = [0u8; CHUNK];
            {
                let pages = self.pages.lock();
                if let Some(phys) = pages.frame(index) {
                    // SAFETY: the pages are locked.
                    let frame = unsafe { frame_bytes(phys) };
                    buf[..n].copy_from_slice(&frame[inside..inside + n]);
                }
            }
            sink(&buf[..n])?;
            done += n as u64;
        }
        Ok(())
    }

    /// Fills the `len` bytes from `offset` from `source`, in chunks.
    pub fn write(
        &self,
        offset: u64,
        len: u64,
        mut source: impl FnMut(&mut [u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.check_range(offset, len)?;
        let mut done = 0;
        while done < len {
            let (index, inside, n) = chunk(offset + done, len - done);
            let mut buf = [0u8; CHUNK];
            source(&mut buf[..n])?;
            let mut pages = self.pages.lock();
            let phys = match pages.frame(index) {
                Some(phys) => phys,
                None => self.alloc(&mut pages, index)?,
            };
            // SAFETY: the pages are locked.
            let frame = unsafe { frame_bytes(phys) };
            frame[inside..inside + n].copy_from_slice(&buf[..n]);
            done += n as u64;
        }
        Ok(())
    }

    /// Changes the size to `size` bytes, freeing the frames past it.
    pub fn set_size(&self, size: u64) -> Result<(), Error> {
        let mut pages = self.pages.lock();
        let resized = pages.set_size(size, MAX_SIZE)?;
        if let Some((phys, from)) = resized.zero {
            // SAFETY: the pages are locked.
            let frame = unsafe { frame_bytes(phys) };
            frame[from..].fill(0);
        }
        self.unmap_pages(pages.count(), u64::MAX);
        drop(pages);
        self.free(resized.freed.into_values());
        Ok(())
    }

    /// The memory information record.
    pub fn info(&self) -> [u8; INFO_MEMORY_SIZE] {
        self.pages.lock().info()
    }
}

impl Object for MemoryObject {
    fn header(&self) -> &Header {
        &self.header
    }
}

impl Drop for MemoryObject {
    fn drop(&mut self) {
        let frames = self.pages.lock().take_all();
        self.free(frames.into_values());
        self.budget.release_kernel(self.charged);
    }
}
