//! Anonymous memory objects: their frames and the copies to and from them.

use crate::aspace::{self, Access, AddressSpace, Fault, USER_END, USER_START};
use crate::memory::FRAMES;
use crate::paging::Policy;
use crate::publish::{ITEM_STEP, Work};
use crate::space;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use kernel_core::object::budget::Budget;
use kernel_core::object::memory::{
    INFO_MEMORY_SIZE, KIND_ANONYMOUS, KIND_PHYSICAL, PAGE_ENTRY_COST, PAGE_SIZE, Pages,
};
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::regions::{PERM_EXECUTE, PERM_WRITE};
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
    /// For a physical object, the start of its range and the cache policy
    /// of its mappings. Its pages then never hold frames.
    physical: Option<(u64, Policy)>,
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

/// The frame that pages of anonymous objects read before they are
/// written map. Nothing ever writes to it.
static ZERO_FRAME: SpinLock<Option<u64>> = SpinLock::new(None);

/// The shared zero page, allocated on first use.
fn zero_frame() -> Option<u64> {
    let mut zero = ZERO_FRAME.lock();
    if zero.is_none() {
        let phys = FRAMES.lock().as_mut().and_then(|f| f.alloc())?;
        // SAFETY: the frame was just allocated, so nothing references it.
        unsafe { frame_bytes(phys) }.fill(0);
        *zero = Some(phys);
    }
    *zero
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
            physical: None,
            spaces: SpinLock::new(Vec::new()),
        }))
    }

    /// A physical object of the `size` bytes from `base`, both multiples
    /// of the page size, mapped with `policy`, charged to `budget`.
    pub fn new_physical(
        budget: &Arc<Budget>,
        base: u64,
        size: u64,
        policy: Policy,
    ) -> Result<Arc<MemoryObject>, Error> {
        debug_assert!(base.is_multiple_of(PAGE_SIZE) && size.is_multiple_of(PAGE_SIZE));
        let pages = Pages::new(size, 0, MAX_SIZE)?;
        let charged = size_of::<MemoryObject>() as u64;
        budget.charge_kernel(charged)?;
        Ok(Arc::new(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.clone(),
            charged,
            pages: SpinLock::new(pages),
            physical: Some((base, policy)),
            spaces: SpinLock::new(Vec::new()),
        }))
    }

    pub fn is_physical(&self) -> bool {
        self.physical.is_some()
    }

    /// An anonymous object of `count` pages that takes over the `count`
    /// frames from `phys`, charging them to `budget`. The frames return
    /// to the frame allocator when the object is destroyed.
    pub fn from_frames(
        budget: &Arc<Budget>,
        phys: u64,
        count: u64,
    ) -> Result<Arc<MemoryObject>, Error> {
        budget.charge_pages(count)?;
        // The frames come back to the allocator with the object.
        if let Some(frames) = FRAMES.lock().as_mut() {
            frames.adopt(count);
        }
        if let Err(e) = budget.charge_kernel(count * PAGE_ENTRY_COST) {
            budget.release_pages(count);
            return Err(e);
        }
        let object = MemoryObject::new(budget, count * PAGE_SIZE, 0).inspect_err(|_| {
            budget.release_pages(count);
            budget.release_kernel(count * PAGE_ENTRY_COST);
        })?;
        let mut pages = object.pages.lock();
        for index in 0..count {
            pages.insert(index, phys + index * PAGE_SIZE);
        }
        drop(pages);
        Ok(object)
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

    /// Gives page `index` a frame and removes the translations of the
    /// shared zero page that stood in for it.
    fn alloc_mapped(&self, pages: &mut Pages, index: u64) -> Result<u64, Error> {
        let phys = self.alloc(pages, index)?;
        self.unmap_pages(index, index + 1);
        Ok(phys)
    }

    /// Enters the page at `virt` of `space` into its tables for a fault
    /// that was a write or a fetch as given. Pages read before they were
    /// written map the shared zero page read-only. Fails with `Denied`
    /// unless the region at `virt`, which maps this object, allows the
    /// access.
    pub fn fault(&self, space: &AddressSpace, virt: u64, write: bool, fetch: bool) -> Fault {
        let mut pages = self.pages.lock();
        let mine = |r: Option<&aspace::RegionOf>| {
            r.filter(|r| core::ptr::eq(&*r.mem, self))
                .map(|r| (r.page(virt), r.perms))
        };
        let Some((index, perms)) = space.with_region(virt, mine) else {
            return Fault::Denied;
        };
        if !aspace::allows(perms, write, fetch) || index >= pages.count() {
            return Fault::Denied;
        }
        let (phys, writable) = match (self.physical, pages.frame(index)) {
            (Some((base, _)), _) => (base + index * PAGE_SIZE, perms & PERM_WRITE != 0),
            (None, Some(phys)) => (phys, perms & PERM_WRITE != 0),
            (None, None) if write => match self.alloc_mapped(&mut pages, index) {
                Ok(phys) => (phys, true),
                Err(_) => return Fault::OutOfMemory,
            },
            (None, None) => match zero_frame() {
                Some(phys) => (phys, false),
                None => return Fault::OutOfMemory,
            },
        };
        let access = Access {
            writable,
            executable: perms & PERM_EXECUTE != 0,
            policy: self.physical.map_or(Policy::Cached, |(_, policy)| policy),
        };
        // The region may have gone while the zero page was being removed.
        if !space.with_region(virt, |r| mine(r) == Some((index, perms))) {
            return Fault::Denied;
        }
        match space.replace(virt, phys, access) {
            Ok(()) => Fault::Resolved,
            Err(_) => Fault::OutOfMemory,
        }
    }

    /// Holds the page at `virt` of `space`, which this object backs, as
    /// the destination of a kernel write that cannot fail later: gives it
    /// a frame if it has none and keeps that frame until the result is
    /// dropped, or maps a physical page into the kernel with the object's
    /// policy. Fails with `InvalidArgs` unless the region at `virt` maps
    /// this object writable.
    pub fn hold_for_write(
        self: &Arc<Self>,
        space: &AddressSpace,
        virt: u64,
    ) -> Result<Held, Error> {
        let mut pages = self.pages.lock();
        let mine = |r: Option<&aspace::RegionOf>| {
            r.filter(|r| core::ptr::eq(&*r.mem, &**self))
                .map(|r| (r.page(virt), r.perms))
        };
        let Some((index, perms)) = space.with_region(virt, mine) else {
            return Err(Error::InvalidArgs);
        };
        if !aspace::allows(perms, true, false) || index >= pages.count() {
            return Err(Error::InvalidArgs);
        }
        if let Some((base, policy)) = self.physical {
            drop(pages);
            let virt = space::map_physical_page(base + index * PAGE_SIZE, policy)
                .ok_or(Error::NoMemory)?;
            return Ok(Held {
                mem: self.clone(),
                backing: Some(Backing::Mapped(virt)),
            });
        }
        let phys = match pages.frame(index) {
            Some(phys) => phys,
            None => self.alloc_mapped(&mut pages, index)?,
        };
        pages.pin();
        Ok(Held {
            mem: self.clone(),
            backing: Some(Backing::Frame(phys)),
        })
    }

    /// Gives every page a frame and keeps the frames until `unpin_all`,
    /// even when the object shrinks meanwhile, and returns them in page
    /// order with the object's size. Fails with `WrongType` for a
    /// physical object.
    #[cfg(feature = "fuzz")]
    pub fn pin_all(&self) -> Result<(Vec<u64>, u64), Error> {
        if self.is_physical() {
            return Err(Error::WrongType);
        }
        let mut pages = self.pages.lock();
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(pages.count() as usize)
            .map_err(|_| Error::NoMemory)?;
        for index in 0..pages.count() {
            let phys = match pages.frame(index) {
                Some(phys) => phys,
                None => self.alloc_mapped(&mut pages, index)?,
            };
            frames.push(phys);
        }
        pages.pin();
        Ok((frames, pages.size()))
    }

    /// Ends a pin of `pin_all`.
    #[cfg(feature = "fuzz")]
    pub fn unpin_all(&self) {
        self.unpin(&mut Work::new(|| {}));
    }

    /// Records the frame of every page of the `len` bytes from `offset`,
    /// keeping each until the result is released, so that they can be read
    /// without the pages locked. Pages without a frame read as zeros and
    /// get none. The record is charged to `budget`. Fails with `OutOfRange`
    /// when the range stops lying inside the object meanwhile.
    pub fn hold_for_read<F: FnMut()>(
        self: &Arc<Self>,
        offset: u64,
        len: u64,
        budget: Option<&Arc<Budget>>,
        work: &mut Work<F>,
    ) -> Result<Source, Error> {
        debug_assert!(!self.is_physical());
        let first = offset / PAGE_SIZE;
        let count = match len {
            0 => 0,
            _ => (offset + len).div_ceil(PAGE_SIZE) - first,
        };
        let bytes = count * size_of::<Option<u64>>() as u64;
        if let Some(budget) = budget {
            budget.charge_kernel(bytes)?;
        }
        let mut source = Source {
            mem: self.clone(),
            offset,
            frames: Vec::new(),
            pinned: false,
            charged: budget.map(|budget| (budget.clone(), bytes)),
        };
        if source.frames.try_reserve_exact(count as usize).is_err() {
            return Err(Error::NoMemory);
        }
        self.pages.lock().pin();
        source.pinned = true;
        for index in first..first + count {
            work.item();
            let pages = self.pages.lock();
            let found = pages.check_range(offset, len).map(|()| pages.frame(index));
            drop(pages);
            match found {
                Ok(frame) => source.frames.push(frame),
                Err(e) => {
                    source.release(work);
                    return Err(e);
                }
            }
        }
        Ok(source)
    }

    /// Ends one pin of the pages, and frees the frames that were taken
    /// from them while pinned once none is left.
    fn unpin<F: FnMut()>(&self, work: &mut Work<F>) {
        let orphans = self.pages.lock().unpin();
        for batch in orphans.chunks(ITEM_STEP) {
            for _ in batch {
                work.item();
            }
            self.free(batch.iter().copied());
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

    /// Fills the `len` bytes from `offset` from `source`, in chunks.
    pub fn write(
        &self,
        offset: u64,
        len: u64,
        mut source: impl FnMut(&mut [u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.is_physical() {
            return Err(Error::NotSupported);
        }
        self.check_range(offset, len)?;
        let mut done = 0;
        while done < len {
            let (index, inside, n) = chunk(offset + done, len - done);
            let mut buf = [0u8; CHUNK];
            source(&mut buf[..n])?;
            let mut pages = self.pages.lock();
            let phys = match pages.frame(index) {
                Some(phys) => phys,
                None => self.alloc_mapped(&mut pages, index)?,
            };
            // SAFETY: the pages are locked.
            let frame = unsafe { frame_bytes(phys) };
            frame[inside..inside + n].copy_from_slice(&buf[..n]);
            done += n as u64;
        }
        Ok(())
    }

    /// Gives every page of the `len` bytes from `offset` a frame, as
    /// `mem_op` `COMMIT` does.
    pub fn commit(&self, offset: u64, len: u64) -> Result<(), Error> {
        if self.is_physical() {
            return Err(Error::NotSupported);
        }
        let mut pages = self.pages.lock();
        pages.check_range(offset, len)?;
        let end = (offset + len).div_ceil(PAGE_SIZE);
        for index in offset / PAGE_SIZE..end {
            if pages.frame(index).is_none() {
                self.alloc_mapped(&mut pages, index)?;
            }
        }
        Ok(())
    }

    /// Frees the frames of every page of the `len` bytes from `offset`,
    /// which then read as zero, as `mem_op` `DECOMMIT` does.
    pub fn decommit(&self, offset: u64, len: u64) -> Result<(), Error> {
        if self.is_physical() {
            return Err(Error::NotSupported);
        }
        let mut pages = self.pages.lock();
        pages.check_range(offset, len)?;
        let (first, end) = (offset / PAGE_SIZE, (offset + len).div_ceil(PAGE_SIZE));
        self.unmap_pages(first, end);
        let freed = pages.decommit(first, end);
        drop(pages);
        self.free(freed);
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
        let kind = if self.is_physical() {
            KIND_PHYSICAL
        } else {
            KIND_ANONYMOUS
        };
        self.pages.lock().info(kind)
    }
}

/// Where a held page's bytes are reached.
enum Backing {
    /// A frame of the object, through the direct map.
    Frame(u64),
    /// A physical page, mapped into the kernel at this address.
    Mapped(u64),
}

/// A page of a memory object held for writing by `hold_for_write`.
pub struct Held {
    mem: Arc<MemoryObject>,
    /// `None` once released.
    backing: Option<Backing>,
}

impl Held {
    fn end<F: FnMut()>(&mut self, work: &mut Work<F>) {
        match self.backing.take() {
            Some(Backing::Frame(_)) => self.mem.unpin(work),
            Some(Backing::Mapped(virt)) => space::unmap_physical_page(virt),
            None => {}
        }
    }
}

impl crate::publish::Page for Held {
    fn base(&self) -> *mut u8 {
        match self.backing {
            Some(Backing::Frame(phys)) => space::direct(phys) as *mut u8,
            Some(Backing::Mapped(virt)) => virt as *mut u8,
            None => unreachable!("a released page is never written"),
        }
    }

    fn release<F: FnMut()>(mut self, work: &mut Work<F>) {
        work.item();
        self.end(work);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.end(&mut Work::new(|| {}));
    }
}

/// The frames of a range of an anonymous object recorded by
/// `hold_for_read`, by page from the range's first page.
pub struct Source {
    mem: Arc<MemoryObject>,
    offset: u64,
    frames: Vec<Option<u64>>,
    pinned: bool,
    charged: Option<(Arc<Budget>, u64)>,
}

impl Source {
    /// Copies the bytes from offset `at` of the object into `buf`, all of
    /// which must come from one recorded page.
    pub fn read(&self, at: u64, buf: &mut [u8]) {
        let index = (at / PAGE_SIZE - self.offset / PAGE_SIZE) as usize;
        let inside = (at % PAGE_SIZE) as usize;
        assert!(inside + buf.len() <= PAGE_SIZE as usize);
        match self.frames[index] {
            // SAFETY: the frame is pinned, so it stays allocated, and the
            // direct map covers it. Writers may change it meanwhile, which
            // only changes the bytes read.
            Some(phys) => unsafe {
                let from = (space::direct(phys) as *const u8).add(inside);
                core::ptr::copy_nonoverlapping(from, buf.as_mut_ptr(), buf.len());
            },
            None => buf.fill(0),
        }
    }

    /// Releases the frames, accounting for the work that takes.
    pub fn release<F: FnMut()>(mut self, work: &mut Work<F>) {
        if core::mem::take(&mut self.pinned) {
            self.mem.unpin(work);
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if core::mem::take(&mut self.pinned) {
            self.mem.unpin(&mut Work::new(|| {}));
        }
        if let Some((budget, bytes)) = self.charged.take() {
            budget.release_kernel(bytes);
        }
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
