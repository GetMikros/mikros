//! The Mikros kernel for x86_64: the architecture layer and the entry point.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
mod apic;
#[cfg(any(target_os = "none", test))]
mod backtrace;
#[cfg(any(target_os = "none", test))]
mod clock;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod control;
#[cfg(target_os = "none")]
mod cpu;
#[cfg(target_os = "none")]
mod entry;
#[cfg(any(target_os = "none", test))]
mod features;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod gdt;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod idt;
#[cfg(target_os = "none")]
mod log;
#[cfg(target_os = "none")]
mod memory;
#[cfg(any(target_os = "none", test))]
mod paging;
#[cfg(target_os = "none")]
mod panic;
#[cfg(target_os = "none")]
mod percpu;
#[cfg(target_os = "none")]
mod serial;
#[cfg(target_os = "none")]
mod space;
#[cfg(target_os = "none")]
mod time;
#[cfg(target_os = "none")]
mod timer;

/// The kernel only runs on its own target. This lets the workspace build
/// on the host.
#[cfg(not(target_os = "none"))]
fn main() {}
