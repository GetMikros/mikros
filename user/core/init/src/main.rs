//! The initial process. It starts the process manager, registers with
//! it as process ID 1, asks it to start the service manager, and keeps
//! running until either of them terminates, which it logs before it
//! exits, so that the kernel panics (MIKSPEC-0012.R21).

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
    match manager::start(startup) {
        Ok(managers) => watch(&managers),
        Err(failure) => log!(Error, "{failure}"),
    }
    // init must not end, so it sleeps.
    loop {
        // SAFETY: thread_sleep takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}

/// The `TERMINATED` signal of a process.
const TERMINATED: u32 = 1 << 4;
const PACKET_SIZE: usize = 48;

/// Waits until the process manager or the service manager terminates,
/// which nothing restarts, and exits naming it.
fn watch(managers: &manager::Managers) {
    let mut port = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::port_create(0, &mut port) } != mikros_rt::OK {
        log!(Error, "init: no port to watch the managers on");
        return;
    }
    let watched = [
        (1, managers.process_manager.raw(), "process manager"),
        (2, managers.service_manager.raw(), "service manager"),
    ];
    for (key, process, name) in watched {
        // SAFETY: the call takes no pointers.
        let waiting = unsafe { sys::object_wait_async(process, port, key, TERMINATED, 0) };
        if waiting != mikros_rt::OK {
            log!(Error, "init: cannot watch the {name}: {waiting}");
            return;
        }
    }
    loop {
        let mut packet = [0u8; PACKET_SIZE];
        // SAFETY: the buffer holds a packet.
        if unsafe { sys::port_wait(port, i64::MAX, packet.as_mut_ptr()) } != mikros_rt::OK {
            continue;
        }
        let key = u64::from_le_bytes(packet[..8].try_into().expect("8 bytes"));
        if let Some((_, _, name)) = watched.iter().find(|(k, _, _)| *k == key) {
            log!(Error, "init: the {name} terminated");
            // SAFETY: the call takes no pointers.
            unsafe { sys::process_exit(1) };
        }
    }
}
