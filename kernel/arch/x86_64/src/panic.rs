//! Stopping the kernel on a panic.

use crate::backtrace;
use crate::idt::{Exception, Frame};
use crate::serial::Serial;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use kernel_core::panic::{self, Report};

unsafe extern "C" {
    static __kernel_start: u8;
    static __build_id_note: u8;
    static __build_id_note_end: u8;
}

/// The size of the boot stack directly below the kernel image; the
/// bootloader provides at least this much.
const BOOT_STACK_SIZE: u64 = 64 << 10;

/// The descriptor of the GNU build ID note.
fn build_id() -> &'static [u8] {
    let start = &raw const __build_id_note;
    let end = &raw const __build_id_note_end;
    // SAFETY: the linker script places the build ID note, and nothing
    // else, between the two symbols in the read-only segment.
    let note = unsafe { core::slice::from_raw_parts(start, end as usize - start as usize) };
    let word = |at: usize| {
        note.get(at..at + 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let (name_size, desc_size) = (word(0), word(4));
    let desc = 12 + name_size.next_multiple_of(4);
    note.get(desc..desc + desc_size).unwrap_or(&[])
}

/// The registers of the panicking code, as seen in the panic handler.
#[inline(always)]
fn registers() -> [(&'static str, u64); 22] {
    let mut gp = [0u64; 16];
    let (rip, rflags): (u64, u64);
    // SAFETY: the stores go to `gp`, which has room for 16 values; reading
    // the instruction pointer and the flags has no side effects. RAX holds
    // the address of `gp`, so its stored value is that.
    unsafe {
        asm!(
            "mov [rax], rax", "mov [rax+8], rbx", "mov [rax+16], rcx",
            "mov [rax+24], rdx", "mov [rax+32], rsi", "mov [rax+40], rdi",
            "mov [rax+48], rbp", "mov [rax+56], rsp", "mov [rax+64], r8",
            "mov [rax+72], r9", "mov [rax+80], r10", "mov [rax+88], r11",
            "mov [rax+96], r12", "mov [rax+104], r13", "mov [rax+112], r14",
            "mov [rax+120], r15",
            in("rax") gp.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
        asm!("lea {}, [rip]", out(reg) rip, options(nomem, nostack, preserves_flags));
        asm!("pushfq", "pop {}", out(reg) rflags, options(preserves_flags));
    }
    let [cr0, cr2, cr3, cr4] = control_registers();
    [
        ("rip", rip),
        ("rsp", gp[7]),
        ("rbp", gp[6]),
        ("rflags", rflags),
        ("rax", gp[0]),
        ("rbx", gp[1]),
        ("rcx", gp[2]),
        ("rdx", gp[3]),
        ("rsi", gp[4]),
        ("rdi", gp[5]),
        ("r8", gp[8]),
        ("r9", gp[9]),
        ("r10", gp[10]),
        ("r11", gp[11]),
        ("r12", gp[12]),
        ("r13", gp[13]),
        ("r14", gp[14]),
        ("r15", gp[15]),
        ("cr0", cr0),
        ("cr2", cr2),
        ("cr3", cr3),
        ("cr4", cr4),
    ]
}

/// CR0, CR2, CR3 and CR4.
fn control_registers() -> [u64; 4] {
    let (cr0, cr2, cr3, cr4);
    // SAFETY: reading control registers has no side effects.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
    }
    [cr0, cr2, cr3, cr4]
}

static PANICKING: AtomicBool = AtomicBool::new(false);

/// The bounds of the stack the kernel runs on, once it has left the boot
/// stack.
static STACK_START: AtomicU64 = AtomicU64::new(0);
static STACK_END: AtomicU64 = AtomicU64::new(0);

/// Records the stack the kernel moves to, for backtraces.
pub fn set_stack(start: u64, end: u64) {
    STACK_START.store(start, Ordering::Relaxed);
    STACK_END.store(end, Ordering::Relaxed);
}

/// Writes the panic report and stops. `rbp` starts the backtrace.
fn stop(title: &dyn core::fmt::Display, registers: &[(&str, u64)], rbp: u64) -> ! {
    // SAFETY: disabling interrupts touches no memory.
    unsafe { asm!("cli", options(nomem, nostack)) };
    // A panic while writing the report stops without a second one.
    if PANICKING.swap(true, Ordering::Relaxed) {
        crate::cpu::halt();
    }
    // The chain is followed on the stack that holds its first frame: the
    // running thread's, the kernel's own, or the boot stack.
    let boot = (&raw const __kernel_start) as u64;
    // A panic before the per-CPU data is set up, such as the one for
    // missing CPU features, has no thread stack and must not reach it.
    let thread = if crate::percpu::READY.load(Ordering::Acquire) {
        crate::percpu::this().thread_stack.load(Ordering::Relaxed)
    } else {
        0
    };
    let stacks = [
        (
            thread.saturating_sub(crate::sched::KERNEL_STACK_SIZE),
            thread,
        ),
        (
            STACK_START.load(Ordering::Relaxed),
            STACK_END.load(Ordering::Relaxed),
        ),
        (boot - BOOT_STACK_SIZE, boot),
    ];
    let stack = backtrace::stack_of(rbp, &stacks).unwrap_or((0, 0));
    // SAFETY: `frames` only reads 8-byte aligned words inside the current
    // stack, which is mapped.
    let read = |address: u64| unsafe { (address as *const u64).read() };
    let _ = panic::write(
        &mut Serial,
        "\r\n",
        Report {
            title,
            build_id: build_id(),
            cpu: crate::percpu::index(),
            registers,
            frames: backtrace::frames(rbp, stack, read),
        },
    );
    crate::console::panic_screen(|mut screen| {
        let _ = panic::write(
            &mut screen,
            "\n",
            Report {
                title,
                build_id: build_id(),
                cpu: crate::percpu::index(),
                registers,
                frames: backtrace::frames(rbp, stack, read),
            },
        );
    });
    crate::cpu::halt()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let registers = registers();
    stop(&info.message(), &registers, registers[2].1)
}

/// Stops the kernel on an exception, reporting the interrupted registers.
pub fn exception(frame: &Frame) -> ! {
    let [cr0, cr2, cr3, cr4] = control_registers();
    let f = frame;
    let registers = [
        ("rip", f.rip),
        ("rsp", f.rsp),
        ("rbp", f.rbp),
        ("rflags", f.rflags),
        ("rax", f.rax),
        ("rbx", f.rbx),
        ("rcx", f.rcx),
        ("rdx", f.rdx),
        ("rsi", f.rsi),
        ("rdi", f.rdi),
        ("r8", f.r8),
        ("r9", f.r9),
        ("r10", f.r10),
        ("r11", f.r11),
        ("r12", f.r12),
        ("r13", f.r13),
        ("r14", f.r14),
        ("r15", f.r15),
        ("cr0", cr0),
        ("cr2", cr2),
        ("cr3", cr3),
        ("cr4", cr4),
        ("cs", f.cs),
        ("ss", f.ss),
    ];
    stop(&Exception(frame), &registers, f.rbp)
}
