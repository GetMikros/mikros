//! A test program for exception handling and thread events. With a
//! second argument of 0 it executes an undefined instruction at once.
//! With 2 it stops at a breakpoint with the address of a user data
//! selector in R12, then loads SS from that address and executes
//! `SYSCALL` as the next instruction. With 3 it reads I/O port 0x80 and
//! then executes an undefined instruction. With 4 it reads the unmapped
//! address `UNMAPPED`, with 5 it divides by zero, and with 6 it calls
//! `thread_exit`. Otherwise it waits on its bootstrap channel until it is
//! killed.

#![no_std]
#![no_main]

use mikros_rt::sys::{self, Handle};

mikros_rt::raw_main!(main);

/// An address in the low 64 KiB, which nothing is ever mapped at.
const UNMAPPED: u64 = 0x1000;

fn main(bootstrap: Handle, arg2: u64) -> i64 {
    if arg2 == 0 {
        // SAFETY: the instruction only raises an exception.
        unsafe { core::arch::asm!("ud2") };
    }
    if arg2 == 2 {
        static SELECTOR: u16 = 0x23;
        // SAFETY: SS is reloaded with the user data selector it already
        // holds, and the system call number is not assigned, so the call
        // only fails.
        unsafe {
            core::arch::asm!(
                "int3",
                "mov ss, [r12]",
                "syscall",
                in("r12") &raw const SELECTOR,
                inout("rax") u64::MAX => _,
                out("rcx") _,
                out("r11") _,
            )
        };
    }
    if arg2 == 3 {
        // SAFETY: reading the port has no effect on the machine; without
        // access to it, the read only raises an exception, as the
        // undefined instruction does.
        unsafe { core::arch::asm!("in al, 0x80", "ud2", out("al") _) };
    }
    if arg2 == 4 {
        // SAFETY: the read only raises an exception, as nothing is mapped
        // at the address.
        unsafe { core::arch::asm!("mov al, [{}]", in(reg) UNMAPPED, out("al") _) };
    }
    if arg2 == 5 {
        // SAFETY: the division only raises an exception.
        unsafe {
            core::arch::asm!("div ecx", inout("eax") 1u32 => _, inout("edx") 0u32 => _, in("ecx") 0u32)
        };
    }
    if arg2 == 6 {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_exit() };
    }
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::object_wait_one(bootstrap, 1 << 0, i64::MAX, &mut observed) };
    0
}
