//! Starting the initial process from the boot image.

use crate::arch::CachePolicy as Policy;
use crate::elf::{self, PF_W, PF_X, Segment};
use crate::memory_object::MemoryObject;
use crate::object::budget::Budget;
use crate::object::channel::{BYTES_MAX, Endpoint, HANDLES_MAX, Message};
use crate::object::handles::Entry;
use crate::object::memory::PAGE_SIZE;
use crate::object::resource::Resource;
use crate::object::rights::{DUPLICATE, INSPECT, MANAGE, MAP, READ, TRANSFER, WRITE};
use crate::object::{Name, Object, ObjectType, default_rights};
use crate::process::Process;
use crate::regions::{MapOptions, PERM_EXECUTE, PERM_READ, PERM_WRITE, SPECIFIC};
use crate::sched::Thread;
use crate::startup;
use crate::ustar;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use boot_protocol::{BootInfo, flags};

/// The size of the stack of the initial thread.
const STACK_SIZE: u64 = 256 * 1024;

/// The rights of the startup handles to the boot image and the clock
/// page.
const READ_ONLY: u32 = DUPLICATE | TRANSFER | READ | MAP | INSPECT;
/// The default rights of physical memory objects.
const PHYSICAL: u32 = DUPLICATE | TRANSFER | READ | WRITE | MAP | MANAGE | INSPECT;

/// The initial process, loaded and ready to start.
struct Loaded {
    process: Arc<Process>,
    thread: Arc<Thread>,
    entry: u64,
    stack: u64,
    boot_image: Arc<MemoryObject>,
}

/// The boot image as a memory object that owns its frames, with the bytes
/// after the image zeroed.
fn boot_image(info: &BootInfo) -> Arc<MemoryObject> {
    let size = info.boot_image_size;
    let pages = size.div_ceil(PAGE_SIZE);
    let tail = pages * PAGE_SIZE - size;
    // SAFETY: the bootloader allocated whole pages for the image, which
    // lie in the direct map and which nothing else uses.
    let after = unsafe {
        core::slice::from_raw_parts_mut(
            crate::arch::frame_address(info.boot_image_phys + size) as *mut u8,
            tail as usize,
        )
    };
    after.fill(0);
    MemoryObject::from_frames(info.boot_image_phys, pages)
        .unwrap_or_else(|e| panic!("no memory for the boot image object: {e:?}"))
}

/// Maps a new memory object of `len` bytes, a multiple of the page size,
/// at `addr` with `perms` into `process`, and returns it. Neither is
/// charged to a budget.
fn map(process: &Process, addr: u64, len: u64, perms: u32) -> Arc<MemoryObject> {
    map_up_to(
        process,
        addr,
        len,
        perms,
        PERM_READ | PERM_WRITE | PERM_EXECUTE,
    )
}

/// Like `map`, with `max` as the most the region's permissions can be
/// raised to.
fn map_up_to(process: &Process, addr: u64, len: u64, perms: u32, max: u32) -> Arc<MemoryObject> {
    let mem =
        MemoryObject::new(None, len, 0).unwrap_or_else(|e| panic!("no memory for init: {e:?}"));
    let options = MapOptions::parse(perms | SPECIFIC, addr, false).expect("valid map options");
    process
        .aspace()
        .add_region(options, addr, len, &mem, 0, max, None)
        .unwrap_or_else(|e| panic!("mapping init at {addr:#x}: {e:?}"));
    mem
}

/// Loads `segment` of `file` into `process`.
fn load_segment(process: &Process, file: &[u8], segment: &Segment) {
    let pages = segment.pages();
    let mut perms = PERM_READ;
    if segment.flags & PF_W != 0 {
        perms |= PERM_WRITE;
    }
    if segment.flags & PF_X != 0 {
        perms |= PERM_EXECUTE;
    }
    let mem = map(process, pages.start, pages.end - pages.start, perms);
    let data = &file[segment.offset as usize..][..segment.filesz as usize];
    let mut rest = data;
    // This runs during boot, where no other thread could run.
    let mut work = crate::publish::Work::new(|| {});
    mem.write(
        segment.vaddr - pages.start,
        segment.filesz,
        &mut work,
        |chunk| {
            let (now, later) = rest.split_at(chunk.len());
            chunk.copy_from_slice(now);
            rest = later;
            Ok(())
        },
    )
    .unwrap_or_else(|e| panic!("no memory for init: {e:?}"));
}

/// Creates the initial process, belonging to `budget`, and its thread
/// from the `init` member of the boot image, with its segments and stack
/// mapped. Nothing made here is charged to a budget.
fn load(info: &BootInfo, budget: &Arc<Budget>) -> Loaded {
    let boot_image = boot_image(info);
    // SAFETY: the boot image object owns these frames and nothing writes
    // to them while the kernel reads the image.
    let image = unsafe {
        core::slice::from_raw_parts(
            crate::arch::frame_address(info.boot_image_phys) as *const u8,
            info.boot_image_size as usize,
        )
    };
    let file = ustar::find(image, b"init").unwrap_or_else(|e| panic!("{e}"));
    let elf = elf::parse(file, crate::arch::elf_machine(), {
        let (start, end) = crate::aspace::user_range();
        start..end
    })
    .unwrap_or_else(|e| panic!("init: {e}"));

    let name = || Name::new(b"init").expect("a valid name");
    let process =
        Process::new(budget, name(), false).unwrap_or_else(|e| panic!("creating init: {e:?}"));
    crate::process::set_init(&process);
    for segment in &elf.segments {
        load_segment(&process, file, segment);
    }
    // The stack is placed with the page below it, which is mapped without
    // access, and without a way to grant any, so that nothing else is
    // placed there.
    let guard = process
        .aspace()
        .choose(STACK_SIZE + PAGE_SIZE)
        .unwrap_or_else(|e| panic!("no room for the init stack: {e:?}"));
    map_up_to(&process, guard, PAGE_SIZE, 0, 0);
    let stack = guard + PAGE_SIZE;
    map(&process, stack, STACK_SIZE, PERM_READ | PERM_WRITE);
    let thread = Process::create_thread(&process, name(), false)
        .unwrap_or_else(|e| panic!("creating the init thread: {e:?}"));
    Loaded {
        process,
        thread,
        entry: elf.entry,
        stack: stack + STACK_SIZE,
        boot_image,
    }
}

/// Creates the clock page, charged to no budget, with the contents the
/// architecture gives.
fn clock_page() -> Result<Arc<MemoryObject>, crate::status::Error> {
    let mut header = vec![0; PAGE_SIZE as usize];
    let len = crate::arch::clock_page_header(&mut header);
    header.truncate(len);
    let page = MemoryObject::new(None, PAGE_SIZE, 0)?;
    // This runs during boot, where no other thread could run.
    let mut work = crate::publish::Work::new(|| {});
    page.write(0, header.len() as u64, &mut work, |chunk| {
        chunk.copy_from_slice(&header[..chunk.len()]);
        Ok(())
    })?;
    Ok(page)
}

/// The framebuffer as a physical memory object covering its pages.
fn framebuffer(info: &BootInfo) -> Option<Arc<MemoryObject>> {
    if info.flags & flags::FRAMEBUFFER_PRESENT == 0 {
        return None;
    }
    let fb = &info.framebuffer;
    let base = fb.phys_base & !(PAGE_SIZE - 1);
    let end = (fb.phys_base + fb.size).next_multiple_of(PAGE_SIZE);
    let object = MemoryObject::new_physical(None, base, end - base, Policy::WriteCombining)
        .unwrap_or_else(|e| panic!("no memory for the framebuffer object: {e:?}"));
    Some(object)
}

/// Starts the initial process with the startup message on its bootstrap
/// channel. Must run while the memory map and the command line are still
/// in place. The process belongs to the root budget, which its own
/// system calls and faults are charged to from then on, but nothing made
/// here is charged to it.
pub fn start(info: &BootInfo) {
    let budget = Budget::root();
    let loaded = load(info, &budget);
    // SAFETY: the bootloader leaves the command line in the direct map
    // until the kernel reclaims boot memory, which happens later.
    let cmdline = unsafe {
        core::slice::from_raw_parts(
            crate::arch::frame_address(info.cmdline_phys) as *const u8,
            info.cmdline_len as usize,
        )
    };
    let bytes = startup::message(info, cmdline, crate::boot::memory_map(info));
    let clock_page = clock_page().unwrap_or_else(|e| panic!("no memory for the clock page: {e:?}"));
    let entry = |object: Arc<dyn Object>, kind| Entry {
        object,
        rights: default_rights(kind),
    };
    let mut handles: Vec<Entry> = vec![
        entry(loaded.process.clone(), ObjectType::Process),
        entry(loaded.thread.clone(), ObjectType::Thread),
        entry(Resource::root(), ObjectType::Resource),
        Entry {
            object: loaded.boot_image,
            rights: READ_ONLY,
        },
        entry(crate::log::object(), ObjectType::Log),
        entry(budget.clone(), ObjectType::Budget),
        Entry {
            object: clock_page,
            rights: READ_ONLY,
        },
    ];
    if let Some(fb) = framebuffer(info) {
        handles.push(Entry {
            object: fb,
            rights: PHYSICAL,
        });
    }
    if bytes.len() > BYTES_MAX || handles.len() > HANDLES_MAX {
        panic!(
            "the startup message of {} bytes does not fit a message",
            bytes.len()
        );
    }
    let (theirs, ours) = Endpoint::pair(None)
        .unwrap_or_else(|e| panic!("no memory for the bootstrap channel: {e:?}"));
    let message = Message::new(bytes, handles, None)
        .unwrap_or_else(|e| panic!("no memory for the startup message: {e:?}"));
    if let Err((e, _)) = ours.write(message) {
        panic!("writing the startup message: {e:?}");
    }
    drop(ours);
    let bootstrap = Entry {
        object: theirs,
        rights: default_rights(ObjectType::Channel),
    };
    Process::start(
        &loaded.process,
        &loaded.thread,
        loaded.entry,
        loaded.stack,
        Some(bootstrap),
        0,
        None,
    )
    .unwrap_or_else(|e| panic!("starting init: {e:?}"));
}
