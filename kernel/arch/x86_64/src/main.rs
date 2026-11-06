//! The Mikros kernel for x86_64: the architecture layer and the entry point.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(any(target_os = "none", test))]
extern crate alloc;

#[cfg(target_os = "none")]
mod apic;
#[cfg(target_os = "none")]
use kernel_core::aspace;
#[cfg(any(target_os = "none", test))]
mod backtrace;
#[cfg(target_os = "none")]
mod bti;
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
use kernel_core::coverage;
#[cfg(all(target_os = "none", feature = "fuzz"))]
mod coverage_hook;
#[cfg(target_os = "none")]
mod cpu;
#[cfg(target_os = "none")]
mod entropy;
#[cfg(target_os = "none")]
mod entry;
#[cfg(target_os = "none")]
mod exception;
#[cfg(any(target_os = "none", test))]
mod features;
#[cfg(all(target_os = "none", feature = "fuzz"))]
use kernel_core::fuzz;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod gdt;
#[cfg(target_os = "none")]
mod heap;
#[cfg(any(target_os = "none", test))]
#[cfg_attr(test, allow(dead_code))]
mod idt;
#[cfg(target_os = "none")]
mod interface;
#[cfg(target_os = "none")]
mod interrupt;
#[cfg(target_os = "none")]
mod iommu;
#[cfg(target_os = "none")]
use kernel_core::ktimer;
#[cfg(target_os = "none")]
use kernel_core::log;
#[cfg(target_os = "none")]
mod memory;
#[cfg(target_os = "none")]
use kernel_core::memory_object;
#[cfg(target_os = "none")]
use kernel_core::output;
#[cfg(target_os = "none")]
mod page_tables;
#[cfg(target_os = "none")]
use kernel_core::pager;
#[cfg(any(target_os = "none", test))]
mod mitigations;
#[cfg(any(target_os = "none", test))]
mod paging;
#[cfg(target_os = "none")]
mod panic;
#[cfg(target_os = "none")]
mod percpu;
#[cfg(target_os = "none")]
use kernel_core::port;
#[cfg(target_os = "none")]
use kernel_core::process;
#[cfg(target_os = "none")]
use kernel_core::publish;
#[cfg(target_os = "none")]
use kernel_core::random;
#[cfg(target_os = "none")]
mod reserved;
#[cfg(target_os = "none")]
use kernel_core::sched;
#[cfg(target_os = "none")]
mod serial;
#[cfg(target_os = "none")]
use kernel_core::signal;
#[cfg(target_os = "none")]
mod signal_frame;
#[cfg(target_os = "none")]
mod smp;
#[cfg(target_os = "none")]
mod space;
#[cfg(target_os = "none")]
mod syscall;
#[cfg(target_os = "none")]
mod thread_arch;
#[cfg(any(target_os = "none", test))]
mod thread_state;
#[cfg(target_os = "none")]
mod time;
#[cfg(target_os = "none")]
use kernel_core::timeline;
#[cfg(target_os = "none")]
mod timer;
#[cfg(target_os = "none")]
mod tlb;
#[cfg(target_os = "none")]
use kernel_core::user;
#[cfg(target_os = "none")]
mod usercopy;
#[cfg(target_os = "none")]
use kernel_core::wait;

/// The kernel only runs on its own target. This lets the workspace build
/// on the host.
#[cfg(not(target_os = "none"))]
fn main() {}
