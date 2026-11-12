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
pub fn build_id() -> &'static [u8] {
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

/// Whether the kernel is stopping on a panic.
pub fn panicking() -> bool {
    PANICKING.load(Ordering::Acquire)
}

/// The processors that stopped for another's panic.
static STOPPED: crate::tlb::CpuMask = crate::tlb::CpuMask::new();

/// How many times the panicking processor checks for the others to stop
/// before it writes its report anyway.
const STOP_WAIT_TURNS: u32 = 50_000_000;

/// Stops every other running processor with an NMI, and waits until they
/// have stopped or the wait runs out.
fn stop_others() {
    if !crate::percpu::READY.load(Ordering::Acquire) {
        return;
    }
    let me = crate::percpu::index() as usize;
    let others = || crate::tlb::RUNNING.iter().filter(move |&cpu| cpu != me);
    for cpu in others() {
        if let Some(data) = crate::percpu::of(cpu) {
            crate::apic::send_ipi(data.apic_id.load(Ordering::Relaxed), crate::apic::ICR_NMI);
        }
    }
    for _ in 0..STOP_WAIT_TURNS {
        if others().all(|cpu| STOPPED.iter().any(|s| s == cpu)) {
            return;
        }
        core::hint::spin_loop();
    }
}

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
/// The lowest address of the image's loadable segments as linked, where
/// link.ld places it.
const LINKED_BASE: u64 = boot_protocol::x86_64::KERNEL_RANGE_START;

/// The linked address of the kernel image address `address`, wherever the
/// bootloader placed the image: `__kernel_start` is the start of the
/// image's lowest segment, `kernel_virt_base`, where it runs.
fn linked(address: u64) -> u64 {
    let base = (&raw const __kernel_start) as u64;
    address.wrapping_sub(base).wrapping_add(LINKED_BASE)
}

fn stop(title: &dyn core::fmt::Display, registers: &[(&str, u64)], rbp: u64) -> ! {
    // SAFETY: disabling interrupts touches no memory.
    unsafe { asm!("cli", options(nomem, nostack)) };
    // Only the first processor to panic writes a report. The others,
    // stopped by its NMI or panicking themselves, stop without one, as
    // does a panic while the report is written.
    if PANICKING.swap(true, Ordering::AcqRel) {
        if crate::percpu::READY.load(Ordering::Acquire) {
            #[cfg(feature = "diag")]
            others::stopped(registers);
            STOPPED.insert(crate::percpu::index() as usize);
        }
        crate::cpu::halt();
    }
    stop_others();
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
        #[cfg(feature = "diag")]
        others::own_stack(),
    ];
    let stack = backtrace::stack_of(rbp, &stacks).unwrap_or((0, 0));
    // SAFETY: `frames` only reads 8-byte aligned words inside the current
    // stack, which is mapped.
    let read = |address: u64| unsafe { (address as *const u64).read() };
    // The records the log writer had not written out yet come first.
    kernel_core::log::flush_for_panic(&mut crate::serial::write);
    let _ = panic::write(
        &mut Serial,
        "\r\n",
        Report {
            title,
            build_id: build_id(),
            cpu: crate::percpu::index(),
            registers,
            frames: backtrace::frames(rbp, stack, read).map(linked),
        },
    );
    #[cfg(feature = "diag")]
    let _ = others::write(&mut Serial, "\r\n");
    crate::console::panic_screen(|mut screen| {
        let _ = panic::write(
            &mut screen,
            "\n",
            Report {
                title,
                build_id: build_id(),
                cpu: crate::percpu::index(),
                registers,
                frames: backtrace::frames(rbp, stack, read).map(linked),
            },
        );
        #[cfg(feature = "diag")]
        let _ = others::write(&mut screen, "\n");
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
        ("dr6", crate::cpu::read_dr(6)),
        ("dr7", crate::cpu::read_dr(7)),
    ];
    stop(&Exception(frame), &registers, f.rbp)
}

/// In the diagnostic build, the state of the processors that stopped for
/// another's panic, for its report.
#[cfg(feature = "diag")]
pub mod others {
    use super::{STOPPED, linked};
    use core::fmt::{self, Write};
    use core::sync::atomic::{AtomicU64, Ordering};
    use kernel_core::smp::MAX_CPUS;

    /// The most frames written for each processor.
    const FRAMES: usize = 16;

    /// For each processor: where it was interrupted as it stopped, its
    /// frame pointer and its code segment, and the bounds of its own
    /// stack, which it idles on.
    struct State {
        rip: AtomicU64,
        rbp: AtomicU64,
        cs: AtomicU64,
        stack: (AtomicU64, AtomicU64),
    }

    static STATES: [State; MAX_CPUS] = [const {
        State {
            rip: AtomicU64::new(0),
            rbp: AtomicU64::new(0),
            cs: AtomicU64::new(0),
            stack: (AtomicU64::new(0), AtomicU64::new(0)),
        }
    }; MAX_CPUS];

    /// Records the stack of processor `index`, from `start` to `end`.
    pub fn set_stack(index: usize, start: u64, end: u64) {
        if let Some(s) = STATES.get(index) {
            s.stack.0.store(start, Ordering::Relaxed);
            s.stack.1.store(end, Ordering::Relaxed);
        }
    }

    /// The bounds of this processor's own stack, once it has one.
    pub fn own_stack() -> (u64, u64) {
        if !crate::percpu::READY.load(Ordering::Acquire) {
            return (0, 0);
        }
        let s = &STATES[crate::percpu::index() as usize];
        (
            s.stack.0.load(Ordering::Relaxed),
            s.stack.1.load(Ordering::Relaxed),
        )
    }

    /// Records the registers this processor stopped with.
    pub fn stopped(registers: &[(&str, u64)]) {
        let get = |name| registers.iter().find(|r| r.0 == name).map_or(0, |r| r.1);
        let s = &STATES[crate::percpu::index() as usize];
        s.rip.store(get("rip"), Ordering::Relaxed);
        s.rbp.store(get("rbp"), Ordering::Relaxed);
        s.cs.store(get("cs"), Ordering::Relaxed);
    }

    /// Writes, for each other running processor, the thread it ran, where
    /// it was interrupted and a backtrace from there.
    pub fn write(out: &mut dyn Write, newline: &str) -> fmt::Result {
        let me = crate::percpu::index() as usize;
        for cpu in crate::tlb::RUNNING.iter().filter(|&c| c != me) {
            let thread = kernel_core::cpu::of(cpu).map_or(0, |c| c.running.load(Ordering::Relaxed));
            write!(out, "cpu {cpu} thread {thread}")?;
            if !STOPPED.iter().any(|s| s == cpu) {
                write!(out, " did not stop{newline}")?;
                continue;
            }
            let s = &STATES[cpu];
            let (rip, rbp) = (s.rip.load(Ordering::Relaxed), s.rbp.load(Ordering::Relaxed));
            if s.cs.load(Ordering::Relaxed) & 3 != 0 {
                write!(out, " in user mode at {rip:#x}{newline}")?;
                continue;
            }
            write!(out, " rip {:#x}", linked(rip))?;
            let top = crate::percpu::of(cpu).map_or(0, |d| d.thread_stack.load(Ordering::Relaxed));
            let stacks = [
                (top.saturating_sub(crate::sched::KERNEL_STACK_SIZE), top),
                (
                    s.stack.0.load(Ordering::Relaxed),
                    s.stack.1.load(Ordering::Relaxed),
                ),
                (
                    super::STACK_START.load(Ordering::Relaxed),
                    super::STACK_END.load(Ordering::Relaxed),
                ),
            ];
            let stack = crate::backtrace::stack_of(rbp, &stacks).unwrap_or((0, 0));
            // SAFETY: `frames` only reads 8-byte aligned words inside a
            // processor's stack, which is mapped.
            let read = |address: u64| unsafe { (address as *const u64).read() };
            for address in crate::backtrace::frames(rbp, stack, read).take(FRAMES) {
                write!(out, " < {:#x}", linked(address))?;
            }
            out.write_str(newline)?;
        }
        // The account of the kernel's state that needs no lock that a
        // stopped processor may hold, as one paragraph, so that the
        // report fits on the screen.
        out.write_str("state: ")?;
        let mut records = Records(out);
        kernel_core::diag::summary(&mut records)?;
        crate::diag::records(&mut records, false)?;
        out.write_str(newline)
    }

    /// Writes records to the report with spaces between their fields and
    /// a semicolon between records.
    struct Records<'a>(&'a mut dyn Write);

    impl Write for Records<'_> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            for c in s.chars() {
                match c {
                    '\n' => self.0.write_char(';')?,
                    '\t' => self.0.write_char(' ')?,
                    c => self.0.write_char(c)?,
                }
            }
            Ok(())
        }
    }
}
