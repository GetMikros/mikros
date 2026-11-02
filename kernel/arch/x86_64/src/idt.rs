//! The interrupt descriptor table and the common interrupt entry path.

#[cfg(target_os = "none")]
use crate::gdt::Pointer;
use crate::gdt::{IST_DOUBLE_FAULT, IST_MACHINE_CHECK, IST_NMI, KERNEL_CODE};
#[cfg(target_os = "none")]
use core::arch::{asm, global_asm};
use core::fmt;
use kernel_core::exception::kind;

/// The distance between two entry stubs.
const STUB_SIZE: u64 = 16;

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
    "jmp interrupt_common",
    ".set vector, vector + 1",
    ".endr",
    "interrupt_common:",
    // Swap to the kernel GS base when the interrupted code ran in user
    // mode, as its saved CS shows, and back before returning to it.
    "test byte ptr [rsp + 24], 3",
    "jz 2f",
    "swapgs",
    "2:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi",
    "push rbp", "push r8", "push r9", "push r10", "push r11", "push r12",
    "push r13", "push r14", "push r15",
    "cld",
    "mov rdi, rsp",
    "call {interrupt}",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10",
    "pop r9", "pop r8", "pop rbp", "pop rdi", "pop rsi", "pop rdx",
    "pop rcx", "pop rbx", "pop rax",
    "add rsp, 16",
    "test byte ptr [rsp + 8], 3",
    "jz 3f",
    "swapgs",
    "3:",
    "iretq",
    ".popsection",
    mask = const ERROR_CODE_VECTORS,
    interrupt = sym interrupt,
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
    let pointer = Pointer {
        limit: (size_of::<[Gate; 256]>() - 1) as u16,
        base: idt as u64,
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

#[cfg(target_os = "none")]
extern "sysv64" fn interrupt(frame: &mut Frame) {
    use crate::aspace::Fault;
    let vector = frame.vector as u8;
    use crate::apic::{PIC_BASE, SPURIOUS_VECTOR, TIMER_VECTOR};
    match vector {
        // A page fault on a user page that a region allows is resolved.
        // Other exceptions from user mode are reported; a fault in the
        // user copy routine makes the copy fail, and anything else stops
        // the kernel.
        PAGE_FAULT => {
            let addr = crate::cpu::read_cr2();
            match crate::aspace::resolve_fault(frame, addr) {
                Fault::Resolved => {}
                _ if frame.cs & 3 == 0 && crate::usercopy::recover(frame) => {}
                _ if frame.cs & 3 == 0 => crate::panic::exception(frame),
                Fault::Denied => crate::exception::raise(frame, kind::PAGE_FAULT, addr),
                Fault::OutOfMemory => {
                    crate::exception::raise(frame, kind::OUT_OF_MEMORY, frame.rip)
                }
            }
        }
        GENERAL_PROTECTION if crate::usercopy::recover(frame) => {}
        v @ 0..32 if frame.cs & 3 == 3 && !fatal(v) => {
            crate::exception::raise(frame, exception_type(v), frame.rip)
        }
        0..32 => crate::panic::exception(frame),
        // The PICs are masked; what arrives from them is ignored, and the
        // local APIC expects no end of interrupt for them or for its
        // spurious interrupt.
        v if (PIC_BASE..PIC_BASE + 16).contains(&v) || v == SPURIOUS_VECTOR => {}
        TIMER_VECTOR => crate::timer::interrupt(),
        // Nothing handles other vectors yet.
        _ => crate::apic::eoi(),
    }
    if frame.cs & 3 == 3 {
        crate::sched::preempt_user();
        crate::sched::exit_if_dead();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_ist_stacks_only_for_fatal_vectors() {
        let with_ist: std::vec::Vec<u8> = (0..=255).filter(|&v| ist(v) != 0).collect();
        assert_eq!(with_ist, [2, 8, 18]);
        assert_eq!((ist(8), ist(2), ist(18)), (1, 2, 3));
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
