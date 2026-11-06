//! The interrupt descriptor table and the common interrupt entry path.

#[cfg(target_os = "none")]
use crate::gdt::Pointer;
use crate::gdt::{IST_DEBUG, IST_DOUBLE_FAULT, IST_MACHINE_CHECK, IST_NMI, KERNEL_CODE};
#[cfg(target_os = "none")]
use core::arch::{asm, global_asm};
use core::fmt;
use kernel_core::exception::kind;

/// The distance between two entry stubs.
const STUB_SIZE: u64 = 16;

pub const DEBUG: u8 = 1;
pub const NMI: u8 = 2;
pub const BREAKPOINT: u8 = 3;
pub const DOUBLE_FAULT: u8 = 8;
pub const GENERAL_PROTECTION: u8 = 13;
pub const PAGE_FAULT: u8 = 14;
pub const MACHINE_CHECK: u8 = 18;

/// The vectors for which the processor pushes an error code, as a mask.
pub const ERROR_CODE_VECTORS: u32 = 1 << 8
    | 1 << 10
    | 1 << 11
    | 1 << 12
    | 1 << 13
    | 1 << 14
    | 1 << 17
    | 1 << 21
    | 1 << 29
    | 1 << 30;

/// Whether vector `vector` always stops the kernel, even from user mode.
pub fn fatal(vector: u8) -> bool {
    matches!(vector, NMI | DOUBLE_FAULT | MACHINE_CHECK)
}

/// The exception type of vector `vector` raised in user mode.
pub fn exception_type(vector: u8) -> u32 {
    match vector {
        0 => kind::ARITHMETIC,
        1 => kind::DEBUG,
        3 => kind::BREAKPOINT,
        6 => kind::UNDEFINED_INSTRUCTION,
        10..=13 | 21 => kind::PROTECTION,
        14 => kind::PAGE_FAULT,
        16 | 19 => kind::FLOATING_POINT,
        17 => kind::ALIGNMENT,
        _ => kind::GENERAL,
    }
}

/// The architecture code of an exception report for `frame`: the vector
/// and, where the processor pushes one, the error code.
pub fn arch_code(frame: &Frame) -> u64 {
    let vector = frame.vector & 0xFF;
    if ERROR_CODE_VECTORS >> vector & 1 != 0 {
        vector | (frame.error & 0xFFFF_FFFF) << 32
    } else {
        vector
    }
}

/// The registers saved on entry, lowest address first.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    /// The processor's error code, or zero for vectors without one.
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

// One 16-byte stub per vector: it pushes a zero in place of an error code
// for vectors without one, and the vector number, then saves the general
// registers and calls `interrupt` with the frame. The processor aligns
// the stack to 16 bytes before it pushes, so the 22 words of the frame
// leave it aligned for the call.
//
// Both paths first clear RFLAGS.AC when SMAP is reported: neither an
// interrupt gate nor the processor clears it, and user code or the user
// copy routine may have set it. IRETQ restores the interrupted value.
//
// Vectors with an IST entry go through `ist_common` instead. Such an
// entry can interrupt kernel code that has not yet swapped GS, such as
// the first instruction of the system call entry, so neither the saved
// CS nor the current GS base shows which base is loaded. `ist_common`
// keeps the GS base it finds in RBX, loads the boot processor's data by
// its address, and writes the kept base back before IRETQ. A debug
// exception from user mode then moves its frame to the thread's kernel
// stack and continues in `interrupt_common`, with the kept user base in
// IA32_KERNEL_GS_BASE as SWAPGS would have left it, so that the thread
// may block or switch there as on any other entry from user mode.
//
// `interrupt` neither blocks nor switches threads nor enables
// interrupts. When it leaves work to the interrupted thread, which may
// do all three, it returns true, and the handling ends with an IRETQ to
// `interrupt_resume` in kernel mode, on the same stack with the frame
// left above it and interrupts still disabled. That calls
// `after_interrupt` with the frame and returns through it as the
// handler would have.
#[cfg(target_os = "none")]
global_asm!(
    ".pushsection .text.interrupt_stubs, \"ax\"",
    ".balign 16",
    ".global interrupt_stubs",
    "interrupt_stubs:",
    ".set vector, 0",
    ".rept 256",
    ".balign 16",
    ".if vector >= 32 || ((({mask} >> vector) & 1) == 0)",
    "push 0",
    ".endif",
    "push vector",
    ".if vector == {debug} || vector == {nmi} || vector == {double_fault} || vector == {machine_check}",
    "jmp ist_common",
    ".else",
    "jmp interrupt_common",
    ".endif",
    ".set vector, vector + 1",
    ".endr",
    "interrupt_common:",
    "cmp byte ptr [rip + {smap}], 0",
    "je 7f",
    "clac",
    "7:",
    // Swap to the kernel GS base when the interrupted code ran in user
    // mode, as its saved CS shows, and back before returning to it.
    "test byte ptr [rsp + 24], 3",
    "jz 2f",
    "swapgs",
    "2:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi",
    "push rbp", "push r8", "push r9", "push r10", "push r11", "push r12",
    "push r13", "push r14", "push r15",
    "interrupt_saved:",
    "cld",
    // An entry from user mode restricts indirect branch predictions.
    "test byte ptr [rsp + 18*8], 3",
    "jz 11f",
    "cmp byte ptr [rip + {ibrs}], 0",
    "je 11f",
    "mov ecx, {spec_ctrl}",
    "mov eax, 1",
    "xor edx, edx",
    "wrmsr",
    "11:",
    // In the fuzz build, the depth of hardware interrupt handling is
    // counted, so that coverage leaves it out.
    ".if {fuzz}",
    "cmp qword ptr [rsp + 15*8], 32",
    "jb 4f",
    "inc qword ptr gs:[{interrupts}]",
    "4:",
    ".endif",
    "mov rdi, rsp",
    "call {interrupt}",
    ".if {fuzz}",
    "cmp qword ptr [rsp + 15*8], 32",
    "jb 5f",
    "dec qword ptr gs:[{interrupts}]",
    "5:",
    ".endif",
    "test al, al",
    "jnz interrupt_left",
    "interrupt_restore:",
    "test byte ptr [rsp + 18*8], 3",
    "jz 12f",
    "cmp byte ptr [rip + {ibrs}], 0",
    "je 12f",
    "mov ecx, {spec_ctrl}",
    "xor eax, eax",
    "xor edx, edx",
    "wrmsr",
    "12:",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10",
    "pop r9", "pop r8", "pop rbp", "pop rdi", "pop rsi", "pop rdx",
    "pop rcx", "pop rbx", "pop rax",
    "add rsp, 16",
    "test byte ptr [rsp + 8], 3",
    "jz 3f",
    "cmp byte ptr [rip + {verw}], 0",
    "je 9f",
    "verw word ptr [rip + {verw_selector}]",
    "9:",
    "swapgs",
    "3:",
    "iretq",
    "interrupt_left:",
    "mov rax, rsp",
    "push {kernel_data}",
    "push rax",
    "push {rflags}",
    "push {kernel_code}",
    "lea rax, [rip + interrupt_resume]",
    "push rax",
    "iretq",
    "interrupt_resume:",
    "mov rdi, rsp",
    "call {after_interrupt}",
    "jmp interrupt_restore",
    "ist_common:",
    "cmp byte ptr [rip + {smap}], 0",
    "je 8f",
    "clac",
    "8:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi",
    "push rbp", "push r8", "push r9", "push r10", "push r11", "push r12",
    "push r13", "push r14", "push r15",
    "cld",
    "cmp byte ptr [rip + {ibrs}], 0",
    "je 13f",
    "mov ecx, {spec_ctrl}",
    "mov eax, 1",
    "xor edx, edx",
    "wrmsr",
    "13:",
    "mov ecx, {gs_msr}",
    "rdmsr",
    "mov ebx, eax",
    "shl rdx, 32",
    "or rbx, rdx",
    // The stack's first word, above the 22 saved, holds this processor's
    // data.
    "mov rax, [rsp + 22*8]",
    "mov rdx, rax",
    "shr rdx, 32",
    "wrmsr",
    "cmp qword ptr [rsp + 15*8], {debug}",
    "jne 6f",
    "test byte ptr [rsp + 18*8], 3",
    "jz 6f",
    "mov ecx, {kernel_gs_msr}",
    "mov eax, ebx",
    "mov rdx, rbx",
    "shr rdx, 32",
    "wrmsr",
    "mov rsi, rsp",
    "mov rdi, gs:[{kernel_stack}]",
    "sub rdi, 22*8",
    "mov rsp, rdi",
    "mov ecx, 22",
    "rep movsq",
    "jmp interrupt_saved",
    "6:",
    "mov rdi, rsp",
    "call {interrupt}",
    "mov ecx, {gs_msr}",
    "mov eax, ebx",
    "mov rdx, rbx",
    "shr rdx, 32",
    "wrmsr",
    "test byte ptr [rsp + 18*8], 3",
    "jz 14f",
    "cmp byte ptr [rip + {ibrs}], 0",
    "je 14f",
    "mov ecx, {spec_ctrl}",
    "xor eax, eax",
    "xor edx, edx",
    "wrmsr",
    "14:",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10",
    "pop r9", "pop r8", "pop rbp", "pop rdi", "pop rsi", "pop rdx",
    "pop rcx", "pop rbx", "pop rax",
    "add rsp, 16",
    "test byte ptr [rsp + 8], 3",
    "jz 10f",
    "cmp byte ptr [rip + {verw}], 0",
    "je 10f",
    "verw word ptr [rip + {verw_selector}]",
    "10:",
    "iretq",
    ".popsection",
    debug = const DEBUG,
    nmi = const NMI,
    double_fault = const DOUBLE_FAULT,
    machine_check = const MACHINE_CHECK,
    gs_msr = const crate::percpu::IA32_GS_BASE,
    kernel_gs_msr = const crate::percpu::IA32_KERNEL_GS_BASE,
    kernel_stack = const crate::percpu::KERNEL_STACK_OFFSET,
    mask = const ERROR_CODE_VECTORS,
    interrupt = sym interrupt,
    after_interrupt = sym after_interrupt,
    kernel_code = const KERNEL_CODE,
    kernel_data = const crate::gdt::KERNEL_DATA,
    // Reserved bit 1 only: interrupts stay disabled.
    rflags = const 2,
    fuzz = const cfg!(feature = "fuzz") as u8,
    interrupts = const crate::percpu::INTERRUPTS_OFFSET,
    smap = sym crate::cpu::SMAP,
    verw = sym crate::mitigations::VERW,
    verw_selector = sym crate::mitigations::VERW_SELECTOR,
    ibrs = sym crate::mitigations::IBRS_ON_ENTRY,
    spec_ctrl = const 0x48,
);

#[cfg(target_os = "none")]
unsafe extern "C" {
    static interrupt_stubs: u8;
}

/// An interrupt gate descriptor.
#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl Gate {
    const EMPTY: Gate = Gate {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn new(handler: u64, ist: u8, dpl: u8) -> Gate {
        Gate {
            offset_low: handler as u16,
            selector: KERNEL_CODE,
            ist,
            // Present, 64-bit interrupt gate.
            attributes: 0x8E | dpl << 5,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

/// The IST entry vector `vector` uses, or 0 for none.
pub fn ist(vector: u8) -> u8 {
    match vector {
        DOUBLE_FAULT => IST_DOUBLE_FAULT,
        NMI => IST_NMI,
        MACHINE_CHECK => IST_MACHINE_CHECK,
        DEBUG => IST_DEBUG,
        _ => 0,
    }
}

/// The privilege level that may raise vector `vector` with INT.
pub fn dpl(vector: u8) -> u8 {
    if vector == BREAKPOINT { 3 } else { 0 }
}

#[cfg(target_os = "none")]
static mut IDT: [Gate; 256] = [Gate::EMPTY; 256];

/// Fills the IDT and loads it. The GDT and the TSS must be loaded.
#[cfg(target_os = "none")]
pub fn init() {
    fill();
    load();
}

/// Fills the IDT, which every processor shares.
#[cfg(target_os = "none")]
fn fill() {
    let stubs = (&raw const interrupt_stubs) as u64;
    let idt = (&raw mut IDT).cast::<Gate>();
    for vector in 0..=255u8 {
        let handler = stubs + u64::from(vector) * STUB_SIZE;
        // SAFETY: the IDT has 256 gates, and nothing reads it before it is
        // loaded here, on the only running processor.
        unsafe {
            idt.add(usize::from(vector))
                .write(Gate::new(handler, ist(vector), dpl(vector)))
        };
    }
}

/// Loads the IDT on this processor. The GDT and the TSS must be loaded.
#[cfg(target_os = "none")]
pub fn load() {
    let pointer = Pointer {
        limit: (size_of::<[Gate; 256]>() - 1) as u16,
        base: (&raw const IDT) as u64,
    };
    // SAFETY: every gate points at its entry stub in the kernel code
    // segment, and the IST entries they name are set in the TSS.
    unsafe { asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags)) };
}

/// The name of exception vector `vector`.
pub fn exception_name(vector: u8) -> &'static str {
    match vector {
        0 => "divide error",
        1 => "debug exception",
        2 => "NMI",
        3 => "breakpoint",
        4 => "overflow",
        5 => "bound range exceeded",
        6 => "invalid opcode",
        7 => "device not available",
        8 => "double fault",
        9 => "coprocessor segment overrun",
        10 => "invalid TSS",
        11 => "segment not present",
        12 => "stack fault",
        13 => "general protection fault",
        14 => "page fault",
        16 => "x87 floating-point exception",
        17 => "alignment check",
        18 => "machine check",
        19 => "SIMD floating-point exception",
        20 => "virtualization exception",
        21 => "control protection exception",
        28 => "hypervisor injection exception",
        29 => "VMM communication exception",
        30 => "security exception",
        _ => "reserved exception",
    }
}

/// The panic title for an exception.
pub struct Exception<'a>(pub &'a Frame);

impl fmt::Display for Exception<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let vector = self.0.vector as u8;
        write!(f, "{} (vector {vector})", exception_name(vector))?;
        if ERROR_CODE_VECTORS >> vector & 1 != 0 {
            write!(f, ", error code {:#x}", self.0.error)?;
        }
        Ok(())
    }
}

/// Handles the interrupt or exception in `frame`, and returns whether it
/// leaves work to the interrupted thread, for `after_interrupt`: an
/// exception from user mode or a page fault in the user copy routine,
/// whose handling may wait, and whatever a thread does on its way back to
/// user mode.
#[cfg(target_os = "none")]
extern "sysv64" fn interrupt(frame: &mut Frame) -> bool {
    use crate::apic::{PIC_BASE, SPURIOUS_VECTOR, TIMER_VECTOR};
    use core::sync::atomic::Ordering;
    let handling = &crate::percpu::this().handling;
    handling.fetch_add(1, Ordering::Relaxed);
    let vector = frame.vector as u8;
    let user = frame.cs & 3 == 3;
    match vector {
        PAGE_FAULT if user || crate::usercopy::in_copy(frame) => {
            let address = crate::cpu::read_cr2();
            crate::percpu::this()
                .fault_address
                .store(address, Ordering::Relaxed);
        }
        GENERAL_PROTECTION if crate::usercopy::recover(frame) => {}
        // A user thread's breakpoint hit while the kernel touched user
        // memory is reported when the thread returns to user mode.
        DEBUG if !user && defer_debug() => {}
        v @ 0..32 if user && !fatal(v) => {}
        0..32 => crate::panic::exception(frame),
        // The PICs are masked; what arrives from them is ignored, and the
        // local APIC expects no end of interrupt for them or for its
        // spurious interrupt.
        v if (PIC_BASE..PIC_BASE + 16).contains(&v) || v == SPURIOUS_VECTOR => {}
        TIMER_VECTOR => crate::timer::interrupt(),
        crate::apic::IOMMU_FAULT_VECTOR => crate::iommu::fault_interrupt(),
        // The interrupt itself is the request: the thread it interrupted
        // looks at the reschedule flag as the handling ends, and an idle
        // processor at its run queue.
        crate::apic::RESCHEDULE_VECTOR => crate::apic::eoi(),
        crate::tlb::SHOOTDOWN_VECTOR => {
            crate::tlb::service();
            crate::apic::eoi();
        }
        v if crate::apic::DEVICE_VECTORS.contains(&v) => crate::interrupt::deliver(v),
        // Nothing handles the vectors reserved for the kernel yet.
        _ => crate::apic::eoi(),
    }
    handling.fetch_sub(1, Ordering::Relaxed);
    user || vector == PAGE_FAULT
}

const FAULT_WRITE: u64 = 1 << 1;
const FAULT_RESERVED: u64 = 1 << 3;
const FAULT_FETCH: u64 = 1 << 4;

/// Resolves the page fault at `address` with its registers in `frame`:
/// one raised by user code, or by the user copy routine, that a region
/// allows.
#[cfg(target_os = "none")]
fn resolve_fault(frame: &Frame, address: u64) -> crate::aspace::Fault {
    if frame.error & FAULT_RESERVED != 0 || (frame.cs & 3 == 0 && !crate::usercopy::in_copy(frame))
    {
        return crate::aspace::Fault::Denied;
    }
    let write = frame.error & FAULT_WRITE != 0;
    let fetch = frame.error & FAULT_FETCH != 0;
    crate::aspace::resolve_fault(address, write, fetch)
}

/// Does the work that `interrupt` left to the thread it interrupted,
/// once the handling has ended. A page fault on a user page that a region
/// allows is resolved, waiting for the page if need be. Other exceptions
/// from user mode are reported, and a fault in the user copy routine that
/// cannot be resolved makes the copy fail. A thread going back to user
/// mode then gives way if it is to, makes its reports, takes its signals,
/// and ends if it is to.
#[cfg(target_os = "none")]
extern "sysv64" fn after_interrupt(frame: &mut Frame) {
    use crate::aspace::Fault;
    use core::sync::atomic::Ordering;
    let vector = frame.vector as u8;
    let user = frame.cs & 3 == 3;
    match vector {
        PAGE_FAULT => {
            let address = crate::percpu::this().fault_address.load(Ordering::Relaxed);
            match resolve_fault(frame, address) {
                Fault::Resolved => {}
                _ if !user && crate::usercopy::recover(frame) => {}
                _ if !user => crate::panic::exception(frame),
                // `resolve_fault` waits for absent and changing pages
                // itself.
                Fault::Denied | Fault::Absent(_) | Fault::Changed(_) | Fault::Busy(_) => {
                    crate::exception::raise(frame, kind::PAGE_FAULT, address)
                }
                Fault::OutOfMemory => {
                    crate::exception::raise(frame, kind::OUT_OF_MEMORY, frame.rip)
                }
            }
        }
        v @ 0..32 if user => crate::exception::raise(frame, exception_type(v), frame.rip),
        _ => {}
    }
    if user {
        crate::sched::preempt_user();
        crate::exception::before_user_return(frame);
        crate::sched::exit_if_dead();
    }
}

/// Whether the processor is handling an interrupt or an exception.
#[cfg(target_os = "none")]
pub fn handling() -> bool {
    crate::percpu::READY.load(core::sync::atomic::Ordering::Acquire)
        && crate::percpu::this()
            .handling
            .load(core::sync::atomic::Ordering::Relaxed)
            != 0
}

/// Records a debug exception raised in kernel mode by a breakpoint of the
/// running user thread. Returns whether it was one.
#[cfg(target_os = "none")]
fn defer_debug() -> bool {
    use crate::thread_arch::ThreadX86;
    let dr6 = crate::cpu::read_dr(6);
    match crate::sched::current() {
        Some(thread) if thread.x86().has_breakpoints() && dr6 & 0xF != 0 => {
            thread.x86().defer_debug(dr6);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_ist_stacks_for_fatal_vectors_and_debug() {
        let with_ist: std::vec::Vec<u8> = (0..=255).filter(|&v| ist(v) != 0).collect();
        assert_eq!(with_ist, [1, 2, 8, 18]);
        assert_eq!((ist(8), ist(2), ist(18), ist(1)), (1, 2, 3, 4));
        let user: std::vec::Vec<u8> = (0..=255).filter(|&v| dpl(v) != 0).collect();
        assert_eq!(user, [3]);
    }

    #[test]
    fn maps_vectors_to_exception_types() {
        let types: std::vec::Vec<u32> = (0..32).map(exception_type).collect();
        assert_eq!(&types[..8], &[4, 7, 1, 6, 1, 1, 3, 1]);
        assert_eq!(&types[10..22], &[8, 8, 8, 8, 2, 1, 5, 9, 1, 5, 1, 8]);
        assert_eq!(types[31], kind::GENERAL);
        assert!(fatal(2) && fatal(8) && fatal(18) && !fatal(14));
    }

    #[test]
    fn encodes_the_arch_code() {
        let mut frame = Frame {
            vector: 14,
            error: 0x7,
            ..Frame::default()
        };
        assert_eq!(arch_code(&frame), 0x7_0000_000E);
        frame.vector = 6;
        assert_eq!(arch_code(&frame), 6);
    }

    #[test]
    fn encodes_gates() {
        let g = Gate::new(0xFFFF_FFFF_8012_3456, 2, 3);
        assert_eq!(
            (g.offset_low, g.offset_mid, g.offset_high),
            (0x3456, 0x8012, 0xFFFF_FFFF)
        );
        assert_eq!((g.selector, g.ist, g.attributes), (0x08, 2, 0xEE));
        assert_eq!(size_of::<Gate>(), 16);
    }

    #[test]
    fn names_the_exception() {
        let mut frame = Frame {
            vector: 14,
            error: 2,
            cs: 0x08,
            ..Frame::default()
        };
        let title = std::format!("{}", Exception(&frame));
        assert_eq!(title, "page fault (vector 14), error code 0x2");
        frame.vector = 6;
        assert_eq!(
            std::format!("{}", Exception(&frame)),
            "invalid opcode (vector 6)"
        );
    }
}
