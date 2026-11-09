//! The initial process. It starts the process manager, registers with
//! it as process ID 1, asks it to start the service manager, and keeps
//! running.

#![no_std]
#![no_main]

extern crate alloc;

mod manager;

use mikros_rt::{Startup, log, sys};

mikros_rt::main!(main);

fn main(startup: &Startup) -> i64 {
    let cmdline = core::str::from_utf8(startup.cmdline).unwrap_or("<not UTF-8>");
    log!(
        Info,
        "init started: {} memory map ranges, boot image of {} bytes, command line \"{cmdline}\"",
        startup.memory_map().count(),
        startup.boot_image_size,
    );
    if let Some(fb) = &startup.framebuffer {
        log!(
            Info,
            "framebuffer {}x{} at {:#x}",
            fb.width,
            fb.height,
            fb.phys_base
        );
    }
    if let Err(failure) = manager::start(startup) {
        log!(Error, "{failure}");
    }
    // init must not end, so it sleeps.
    loop {
        // SAFETY: thread_sleep takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}
