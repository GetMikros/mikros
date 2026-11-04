//! The x86_64 coverage hook for the fuzz build, which appends to the
//! buffer described by the per-CPU data.

use crate::percpu::{self, COV_AREA_OFFSET, COV_CAPACITY_OFFSET, INTERRUPTS_OFFSET, READY};
use core::sync::atomic::Ordering;

// The fuzz build calls this at the start of every instrumented basic
// block. It is written in assembly so that it is not instrumented
// itself. Until the GS base points to the per-CPU data, and during
// hardware interrupt handling, it does nothing. Otherwise it appends its
// return address to the running thread's buffer while the count is
// below the capacity. It changes only RAX, RCX, RDX and the flags, which
// a call may change.
core::arch::global_asm!(
    ".pushsection .text.sanitizer_cov_trace_pc, \"ax\"",
    ".global __sanitizer_cov_trace_pc",
    "__sanitizer_cov_trace_pc:",
    "cmp byte ptr [rip + {ready}], 0",
    "je 1f",
    "cmp qword ptr gs:[{interrupts}], 0",
    "jne 1f",
    "mov rax, gs:[{area}]",
    "test rax, rax",
    "jz 1f",
    "mov rcx, [rax]",
    "cmp rcx, gs:[{capacity}]",
    "jae 1f",
    "mov rdx, [rsp]",
    "mov [rax + rcx * 8 + 8], rdx",
    "inc rcx",
    "mov [rax], rcx",
    "1:",
    "ret",
    ".popsection",
    ready = sym READY,
    interrupts = const INTERRUPTS_OFFSET,
    area = const COV_AREA_OFFSET,
    capacity = const COV_CAPACITY_OFFSET,
);

/// Points the hook on this processor at the buffer at `area` holding
/// `capacity` addresses, or at none when `area` is zero.
pub fn point(area: u64, capacity: u64) {
    let cpu = percpu::this();
    cpu.cov_area.store(0, Ordering::Relaxed);
    cpu.cov_capacity.store(capacity, Ordering::Relaxed);
    cpu.cov_area.store(area, Ordering::Relaxed);
}
