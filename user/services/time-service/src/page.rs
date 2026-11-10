//! The time page: a memory object of one page that holds the offset of
//! UTC from the monotonic clock, which readers map and read without
//! asking the service, retrying while an update is in progress.

use core::sync::atomic::{AtomicI64, AtomicU32, Ordering, fence};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const SIZE: u64 = 4096;
const VERSION: u32 = 1;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The rights of the handles given out: no `WRITE`, but `DUPLICATE`, so
/// that the process manager can give the page to every process.
const DUPLICATE: u32 = 1 << 0;
const TRANSFER: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const MAP: u32 = 1 << 5;
const INSPECT: u32 = 1 << 9;
const GIVEN: u32 = DUPLICATE | TRANSFER | READ | MAP | INSPECT;

/// The fields of the page, at offsets 0x00, 0x04 and 0x08.
#[repr(C)]
struct Fields {
    sequence: AtomicU32,
    version: AtomicU32,
    offset: AtomicI64,
}

pub struct Page {
    memory: OwnedHandle,
    fields: &'static Fields,
}

impl Page {
    /// A page mapped into `process` that holds `offset`. None when there
    /// is no memory for it.
    pub fn new(process: sys::Handle, offset: i64) -> Option<Page> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::mem_create(SIZE, 0, &mut raw) } != mikros_rt::OK {
            return None;
        }
        // SAFETY: the kernel just created the handle for us.
        let memory = unsafe { OwnedHandle::from_raw(raw) };
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let mapped =
            unsafe { sys::vm_map(process, PERM_READ | PERM_WRITE, 0, raw, 0, SIZE, &mut at) };
        if mapped != mikros_rt::OK {
            return None;
        }
        // SAFETY: the page stays mapped for the rest of the program, and
        // its fields are atomics, which other processes only read.
        let fields = unsafe { &*(at as *const Fields) };
        fields.version.store(VERSION, Ordering::Relaxed);
        fields.offset.store(offset, Ordering::Relaxed);
        fence(Ordering::Release);
        Some(Page { memory, fields })
    }

    /// The offset of UTC from the monotonic clock, in nanoseconds.
    pub fn offset(&self) -> i64 {
        self.fields.offset.load(Ordering::Relaxed)
    }

    /// Changes the offset: `sequence` is odd while it changes.
    pub fn set(&self, offset: i64) {
        self.fields.sequence.fetch_add(1, Ordering::AcqRel);
        fence(Ordering::Release);
        self.fields.offset.store(offset, Ordering::Relaxed);
        fence(Ordering::Release);
        self.fields.sequence.fetch_add(1, Ordering::AcqRel);
    }

    /// A read-only handle to the page.
    pub fn handle(&self) -> Option<OwnedHandle> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::handle_duplicate(self.memory.raw(), GIVEN, &mut raw) };
        // SAFETY: the kernel just created the handle for us.
        (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(raw) })
    }
}
