//! Starting the initial process from the boot image.

use crate::aspace::{USER_END, USER_START};
use crate::memory_object::MemoryObject;
use crate::process::Process;
use crate::sched::Thread;
use crate::space;
use alloc::sync::{Arc, Weak};
use boot_protocol::BootInfo;
use kernel_core::elf::{self, EM_X86_64, PF_W, PF_X, Segment};
use kernel_core::object::Name;
use kernel_core::object::budget::Budget;
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::regions::{MapOptions, PERM_EXECUTE, PERM_READ, PERM_WRITE, SPECIFIC};
use kernel_core::sync::SpinLock;
use kernel_core::ustar;

/// The size of the stack of the initial thread.
const STACK_SIZE: u64 = 256 * 1024;

/// The initial process, once it exists.
static INIT: SpinLock<Weak<Process>> = SpinLock::new(Weak::new());

/// Panics when `process` is the initial process. Called as a process
/// terminates.
pub fn terminated(process: &Process, code: i64) {
    if core::ptr::eq(INIT.lock().as_ptr(), process) {
        panic!("init terminated with exit code {code}");
    }
}

/// The initial process, loaded and ready to start.
pub struct Loaded {
    pub process: Arc<Process>,
    pub thread: Arc<Thread>,
    pub entry: u64,
    pub stack: u64,
    pub boot_image: Arc<MemoryObject>,
}

/// The boot image as a memory object that owns its frames, with the bytes
/// after the image zeroed.
fn boot_image(info: &BootInfo, budget: &Arc<Budget>) -> Arc<MemoryObject> {
    let size = info.boot_image_size;
    let pages = size.div_ceil(PAGE_SIZE);
    let tail = pages * PAGE_SIZE - size;
    // SAFETY: the bootloader allocated whole pages for the image, which
    // lie in the direct map and which nothing else uses.
    let after = unsafe {
        core::slice::from_raw_parts_mut(
            space::direct(info.boot_image_phys + size) as *mut u8,
            tail as usize,
        )
    };
    after.fill(0);
    MemoryObject::from_frames(budget, info.boot_image_phys, pages)
        .unwrap_or_else(|e| panic!("no memory for the boot image object: {e:?}"))
}

/// Maps a new memory object of `len` bytes, a multiple of the page size,
/// at `addr` with `perms` into `process`, and returns it.
fn map(
    process: &Process,
    budget: &Arc<Budget>,
    addr: u64,
    len: u64,
    perms: u32,
) -> Arc<MemoryObject> {
    let mem =
        MemoryObject::new(budget, len, 0).unwrap_or_else(|e| panic!("no memory for init: {e:?}"));
    let options = MapOptions::parse(perms | SPECIFIC, addr).expect("valid map options");
    process
        .aspace()
        .add_region(options, addr, len, &mem, 0)
        .unwrap_or_else(|e| panic!("mapping init at {addr:#x}: {e:?}"));
    mem
}

/// Loads `segment` of `file` into `process`.
fn load_segment(process: &Process, budget: &Arc<Budget>, file: &[u8], segment: &Segment) {
    let pages = segment.pages();
    let mut perms = PERM_READ;
    if segment.flags & PF_W != 0 {
        perms |= PERM_WRITE;
    }
    if segment.flags & PF_X != 0 {
        perms |= PERM_EXECUTE;
    }
    let mem = map(process, budget, pages.start, pages.end - pages.start, perms);
    let data = &file[segment.offset as usize..][..segment.filesz as usize];
    let mut rest = data;
    mem.write(segment.vaddr - pages.start, segment.filesz, |chunk| {
        let (now, later) = rest.split_at(chunk.len());
        chunk.copy_from_slice(now);
        rest = later;
        Ok(())
    })
    .unwrap_or_else(|e| panic!("no memory for init: {e:?}"));
}

/// Creates the initial process and its thread from the `init` member of
/// the boot image, with its segments and stack mapped, charged to
/// `budget`.
pub fn load(info: &BootInfo, budget: &Arc<Budget>) -> Loaded {
    let boot_image = boot_image(info, budget);
    // SAFETY: the boot image object owns these frames and nothing writes
    // to them while the kernel reads the image.
    let image = unsafe {
        core::slice::from_raw_parts(
            space::direct(info.boot_image_phys) as *const u8,
            info.boot_image_size as usize,
        )
    };
    let file = ustar::find(image, b"init").unwrap_or_else(|e| panic!("{e}"));
    let elf =
        elf::parse(file, EM_X86_64, USER_START..USER_END).unwrap_or_else(|e| panic!("init: {e}"));

    let name = || Name::new(b"init").expect("a valid name");
    let process =
        Process::new(budget, name(), true).unwrap_or_else(|e| panic!("creating init: {e:?}"));
    *INIT.lock() = Arc::downgrade(&process);
    for segment in &elf.segments {
        load_segment(&process, budget, file, segment);
    }
    // The stack is placed with the page below it, which stays unmapped.
    let guard = process
        .aspace()
        .choose(STACK_SIZE + PAGE_SIZE)
        .unwrap_or_else(|e| panic!("no room for the init stack: {e:?}"));
    let stack = guard + PAGE_SIZE;
    map(&process, budget, stack, STACK_SIZE, PERM_READ | PERM_WRITE);
    let thread = Process::create_thread(&process, name(), true)
        .unwrap_or_else(|e| panic!("creating the init thread: {e:?}"));
    Loaded {
        process,
        thread,
        entry: elf.entry,
        stack: stack + STACK_SIZE,
        boot_image,
    }
}
