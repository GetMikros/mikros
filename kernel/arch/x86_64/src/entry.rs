//! The kernel entry point.

use crate::features::{Leaves, Missing};
use crate::gdt;
use crate::log::log;
use crate::serial;
use crate::space;
use boot_protocol::{BootInfo, NOTE_TYPE, VERSION_MAJOR, VERSION_MINOR, flags};
use kernel_core::bootinfo::{self, HEADER_SIZE};

/// An ELF note: a header, a name padded to 4 bytes, and a descriptor.
#[repr(C, align(4))]
struct Note {
    name_size: u32,
    desc_size: u32,
    kind: u32,
    name: [u8; 8],
    desc: [u16; 2],
}

/// The boot protocol version this kernel is built for.
#[used]
#[unsafe(link_section = ".note.mikros")]
static PROTOCOL_NOTE: Note = Note {
    name_size: 7,
    desc_size: 4,
    kind: NOTE_TYPE,
    name: *b"Mikros\0\0",
    desc: [VERSION_MAJOR, VERSION_MINOR],
};

/// The name of the build kind this kernel was built as.
const BUILD_KIND: &str = if cfg!(feature = "fuzz") {
    "fuzz"
} else if cfg!(debug_assertions) {
    "debug"
} else {
    "production"
};

/// Reads the `BootInfo` at `info`.
fn boot_info(info: *const u8) -> BootInfo {
    // SAFETY: the bootloader passes the address of BootInfo in the direct
    // map, which is readable for at least the header.
    let header = unsafe { core::slice::from_raw_parts(info, HEADER_SIZE) };
    let size = bootinfo::size(header).unwrap_or_else(|e| panic!("{e}"));
    // SAFETY: the bootloader maps the whole structure, whose size the
    // header gives.
    let data = unsafe { core::slice::from_raw_parts(info, size) };
    bootinfo::decode(data).unwrap_or_else(|e| panic!("{e}"))
}

/// Called by the bootloader with the direct-map address of `BootInfo`.
#[unsafe(no_mangle)]
extern "sysv64" fn _start(info: *const u8) -> ! {
    serial::init();
    log!(
        Info,
        "Mikros kernel {} ({} build)",
        env!("CARGO_PKG_VERSION"),
        BUILD_KIND
    );
    let leaves = Leaves::read(|leaf, subleaf| {
        let r = core::arch::x86_64::__cpuid_count(leaf, subleaf);
        [r.eax, r.ebx, r.ecx, r.edx]
    });
    if leaves.missing().next().is_some() {
        panic!("missing CPU features: {}", Missing(&leaves));
    }
    crate::cpu::init();
    crate::percpu::init();
    crate::context::init();
    let info = boot_info(info);
    log!(
        Info,
        "boot protocol {}.{}, {} memory map entries",
        info.version_major,
        info.version_minor,
        info.memory_map_count
    );
    crate::memory::init(&info);
    space::enter(info, on_kernel_tables)
}

/// Continues boot on the kernel's page tables and stack.
extern "sysv64" fn on_kernel_tables() -> ! {
    let Some(mut info) = *space::BOOT_INFO.lock() else {
        panic!("BootInfo was not kept");
    };
    let ist = || space::new_stack(gdt::IST_STACK_SIZE);
    gdt::init(space::stack_top(), ist(), ist(), ist(), ist());
    crate::idt::init();
    // A framebuffer that fails the checks is treated as absent from here
    // on: no console, no panic screen and nothing for init.
    if info.flags & flags::FRAMEBUFFER_PRESENT != 0
        && let Err(e) =
            bootinfo::check_framebuffer(&info.framebuffer, crate::memory::memory_map(&info))
    {
        log!(Warning, "not using the framebuffer: {e}");
        info.flags &= !flags::FRAMEBUFFER_PRESENT;
    }
    crate::console::init(&info);
    crate::reserved::init(&info);
    crate::syscall::init();
    crate::wait::init();
    kernel_core::sync::set_preempt_hook(crate::sched::preempt_point);
    crate::apic::init(&info);
    crate::time::init(&info);
    crate::timer::init();
    crate::random::init(&info);
    crate::sched::start_maintenance();
    crate::iommu::init(&info);
    crate::init::start(&info);
    let free = space::reclaim_boot_memory(&info);
    log!(
        Info,
        "kernel page tables active, {} MiB of memory free",
        (free * 4096) >> 20
    );
    crate::sched::run()
}
