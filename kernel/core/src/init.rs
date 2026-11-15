//! Starting the initial process from the boot modules.

use crate::arch::CachePolicy as Policy;
use crate::elf::{self, PF_W, PF_X, Segment};
use crate::memory_object::MemoryObject;
use crate::modules::{self, Module};
use crate::object::budget::Budget;
use crate::object::channel::{BYTES_MAX, Endpoint, HANDLES_MAX, Message};
use crate::object::handles::{Entry, HandleTable};
use crate::object::memory::PAGE_SIZE;
use crate::object::resource::Resource;
use crate::object::rights::{DUPLICATE, INSPECT, MANAGE, MAP, READ, TRANSFER, WRITE};
use crate::object::{Name, Object, ObjectType, default_rights};
use crate::process::Process;
use crate::regions::{MapOptions, PERM_EXECUTE, PERM_READ, PERM_WRITE, SPECIFIC};
use crate::sched::Thread;
use crate::startup;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use boot_protocol::{BOOT_INFO_SIZE, BootInfo, flags};

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
    boot_image: BootImage,
}

/// The boot image object, the size that the startup message gives for it,
/// and the `init` program in it.
struct BootImage {
    object: Arc<MemoryObject>,
    size: u64,
    init: &'static [u8],
}

/// `len` bytes at the physical address `phys`, through the direct map.
///
/// # Safety
///
/// The bytes must lie in memory that the direct map covers, and nothing
/// may write them while the slice is used.
unsafe fn physical(phys: u64, len: u64) -> &'static [u8] {
    // SAFETY: as the caller guarantees.
    unsafe {
        core::slice::from_raw_parts(crate::arch::frame_address(phys) as *const u8, len as usize)
    }
}

/// The boot modules as the boot image object: its index, followed by
/// each module's contents in the module's own frames, with the bytes
/// after each module zeroed.
fn modules(info: &BootInfo) -> BootImage {
    let map = || crate::boot::memory_map(info);
    let count = info.modules_count;
    modules::check_array(info.modules_phys, count, map()).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: the array lies in BootloaderReclaimable memory, which the
    // direct map covers and which nothing frees until the kernel reclaims
    // boot memory, after init has started.
    let entries =
        modules::decode(unsafe { physical(info.modules_phys, count * modules::ENTRY_SIZE as u64) });
    // SAFETY: `check` reads a path only once it lies in
    // BootloaderReclaimable memory, as the array does, and the others are
    // read after it.
    let path = |m: &Module| unsafe { physical(m.path_phys, u64::from(m.path_len)) };
    modules::check(&entries, map(), path).unwrap_or_else(|e| panic!("{e}"));
    let sizes: Vec<u64> = entries.iter().map(|m| m.size).collect();
    let paths: Vec<&[u8]> = entries.iter().map(path).collect();
    let layout = modules::layout(&sizes, &paths);
    let init = entries
        .iter()
        .find(|m| path(m) == b"init")
        .unwrap_or_else(|| panic!("there is no boot module init"));
    let mut runs = Vec::new();
    for (m, &offset) in entries.iter().zip(&layout.offsets) {
        if m.size == 0 {
            continue;
        }
        let end = m.size.next_multiple_of(PAGE_SIZE);
        // SAFETY: the module's pages lie in BootImage memory, which the
        // direct map covers and which holds no other module.
        let tail = unsafe {
            core::slice::from_raw_parts_mut(
                crate::arch::frame_address(m.phys + m.size) as *mut u8,
                (end - m.size) as usize,
            )
        };
        tail.fill(0);
        runs.push((offset / PAGE_SIZE, m.phys, end / PAGE_SIZE));
    }
    let object = MemoryObject::from_frame_runs(layout.size, &runs)
        .unwrap_or_else(|e| panic!("no memory for the boot image object: {e:?}"));
    let mut rest = &layout.index[..];
    // This runs during boot, where no other thread could run.
    let mut work = crate::publish::Work::new(|| {});
    object
        .write(0, layout.index.len() as u64, &mut work, |chunk| {
            let (now, later) = rest.split_at(chunk.len());
            chunk.copy_from_slice(now);
            rest = later;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("no memory for the boot image index: {e:?}"));
    BootImage {
        object,
        size: layout.size,
        // SAFETY: the boot image object owns these frames and nothing
        // writes to them while the kernel reads init.
        init: unsafe { physical(init.phys, init.size) },
    }
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
/// from the `init` module, with its segments and stack mapped. Nothing
/// made here is charged to a budget. Panics for a `BootInfo` older than
/// 1.4, which passes no modules.
fn load(info: &BootInfo, budget: &Arc<Budget>) -> Loaded {
    if info.version_minor < 4 || info.size < BOOT_INFO_SIZE {
        panic!(
            "boot protocol {}.{} passes no boot modules",
            info.version_major, info.version_minor
        );
    }
    let boot_image = modules(info);
    let file = boot_image.init;
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
    #[cfg(feature = "diag")]
    crate::diag::set_root_budget(&budget);
    let loaded = load(info, &budget);
    // SAFETY: the bootloader leaves the command line in the direct map
    // until the kernel reclaims boot memory, which happens later.
    let cmdline = unsafe {
        core::slice::from_raw_parts(
            crate::arch::frame_address(info.cmdline_phys) as *const u8,
            info.cmdline_len as usize,
        )
    };
    let bytes = startup::message(
        info,
        loaded.boot_image.size,
        cmdline,
        crate::boot::memory_map(info),
    );
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
            object: loaded.boot_image.object,
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
    let rights = default_rights(ObjectType::Channel);
    let mut bootstrap = |table: &mut HandleTable| table.insert(theirs.clone(), rights, None);
    Process::start(
        &loaded.process,
        &loaded.thread,
        loaded.entry,
        loaded.stack,
        Some(&mut bootstrap),
        0,
    )
    .unwrap_or_else(|e| panic!("starting init: {e:?}"));
}
