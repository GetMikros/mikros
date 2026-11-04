//! The architecture-independent part of the Mikros kernel. It builds for
//! the development host, so that its code can be tested there.

#![cfg_attr(not(test), no_std)]
#![feature(allocator_ext)]
#![feature(btreemap_alloc)]
#![feature(macro_metavar_expr)]
#![cfg_attr(test, feature(variant_count))]

extern crate alloc;
// The architecture interface names its types by paths that also hold in
// the crates that implement it.
extern crate self as kernel_core;

pub mod acpi;
pub mod arch;
pub mod aspace;
pub mod boot;
pub mod bootinfo;
pub mod clock;
pub mod cmdline;
pub mod console;
#[cfg(feature = "fuzz")]
pub mod coverage;
pub mod cpu;
pub mod deadline;
pub mod elf;
pub mod exception;
pub mod fallible;
pub mod frames;
#[cfg(feature = "fuzz")]
pub mod fuzz;
pub mod gaps;
pub mod heap;
pub mod init;
pub mod ktimer;
pub mod log;
pub mod memory_object;
pub mod nodes;
pub mod object;
pub mod output;
pub mod pager;
pub mod paging;
pub mod panic;
pub mod port;
pub mod process;
pub mod publish;
pub mod random;
pub mod ranges;
pub mod regions;
pub mod sched;
pub mod signal;
pub mod smp;
pub mod startup;
pub mod status;
pub mod sync;
pub mod timeline;
pub mod user;
pub mod ustar;
pub mod wait;
