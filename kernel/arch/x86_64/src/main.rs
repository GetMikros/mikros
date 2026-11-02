//! The Mikros kernel for x86_64: the architecture layer and the entry point.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(any(target_os = "none", test))]
extern crate alloc;

#[cfg(target_os = "none")]
mod apic;
#[cfg(target_os = "none")]
#[expect(dead_code, reason = "nothing creates processes yet")]
mod aspace;
#[cfg(any(target_os = "none", test))]
mod backtrace;
#[cfg(any(target_os = "none", test))]
mod clock;
#[cfg(target_os = "none")]
mod console;
#[cfg(target_os = "none")]
mod context;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod control;
#[cfg(all(target_os = "none", feature = "fuzz"))]
mod coverage;
#[cfg(target_os = "none")]
mod cpu;
#[cfg(target_os = "none")]
mod entry;
#[cfg(target_os = "none")]
mod exception;
#[cfg(any(target_os = "none", test))]
mod features;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod gdt;
#[cfg(target_os = "none")]
mod heap;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod idt;
#[cfg(target_os = "none")]
mod init;
#[cfg(target_os = "none")]
mod ktimer;
#[cfg(target_os = "none")]
mod log;
#[cfg(target_os = "none")]
mod memory;
#[cfg(target_os = "none")]
mod memory_object;
#[cfg(any(target_os = "none", test))]
mod output;
#[cfg(any(target_os = "none", test))]
mod paging;
#[cfg(target_os = "none")]
mod panic;
#[cfg(target_os = "none")]
mod percpu;
#[cfg(target_os = "none")]
mod port;
#[cfg(target_os = "none")]
mod process;
#[cfg(any(target_os = "none", test))]
mod publish;
#[cfg(target_os = "none")]
mod random;
#[cfg(target_os = "none")]
mod reserved;
#[cfg(target_os = "none")]
mod sched;
#[cfg(target_os = "none")]
mod serial;
#[cfg(target_os = "none")]
mod signal;
#[cfg(target_os = "none")]
mod space;
#[cfg(target_os = "none")]
mod syscall;
#[cfg(any(target_os = "none", test))]
mod thread_state;
#[cfg(target_os = "none")]
mod time;
#[cfg(target_os = "none")]
mod timeline;
#[cfg(target_os = "none")]
mod timer;
#[cfg(target_os = "none")]
mod usercopy;
#[cfg(target_os = "none")]
mod wait;

/// The kernel only runs on its own target. This lets the workspace build
/// on the host.
#[cfg(not(target_os = "none"))]
fn main() {}
