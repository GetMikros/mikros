//! The global descriptor table and the task state segment.

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const USER_DATA: u16 = 0x20;
pub const USER_CODE: u16 = 0x28;
pub const TSS_SELECTOR: u16 = 0x30;

/// The IST entries, numbered from 1 as in gate descriptors.
pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

/// The size of each IST stack.
pub const IST_STACK_SIZE: u64 = 16 << 10;

/// The I/O permission bitmap size: one bit per port.
const IO_BITMAP_SIZE: usize = 65536 / 8;
/// The offset of the I/O permission bitmap from the start of the TSS.
const IO_BITMAP_OFFSET: usize = 104;

#[repr(C, packed(4))]
struct Tss {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    io_bitmap_offset: u16,
    /// A set bit denies its port. The processor reads one byte past the
    /// bitmap, which must have every bit set.
    io_bitmap: [u8; IO_BITMAP_SIZE + 1],
}

const _: () = assert!(core::mem::offset_of!(Tss, io_bitmap) == IO_BITMAP_OFFSET);

static mut TSS: Tss = Tss {
    reserved0: 0,
    rsp: [0; 3],
    reserved1: 0,
    ist: [0; 7],
    reserved2: 0,
    reserved3: 0,
    io_bitmap_offset: IO_BITMAP_OFFSET as u16,
    io_bitmap: [0xFF; IO_BITMAP_SIZE + 1],
};

/// The descriptors before the TSS, indexed by selector / 8.
const SEGMENTS: [u64; 6] = [
    0,
    // Kernel code: present, DPL 0, execute/read, accessed, 64-bit.
    0x00AF_9B00_0000_FFFF,
    // Kernel data: present, DPL 0, read/write, accessed.
    0x00CF_9300_0000_FFFF,
    // 32-bit user code: DPL 3, execute/read, accessed, not present.
    0x00CF_7B00_0000_FFFF,
    // User data: present, DPL 3, read/write, accessed.
    0x00CF_F300_0000_FFFF,
    // User code: present, DPL 3, execute/read, accessed, 64-bit.
    0x00AF_FB00_0000_FFFF,
];

static mut GDT: [u64; 8] = [0; 8];

/// The two descriptor words of an available 64-bit TSS at `base` whose
/// last byte is at `base + limit`.
pub fn tss_descriptor(base: u64, limit: u32) -> [u64; 2] {
    let limit = u64::from(limit);
    let low = (limit & 0xFFFF)
        | (base & 0xFF_FFFF) << 16
        | 0x89 << 40
        | (limit >> 16 & 0xF) << 48
        | (base >> 24 & 0xFF) << 56;
    [low, base >> 32]
}

/// The operand of LGDT and LIDT.
#[repr(C, packed)]
pub struct Pointer {
    pub limit: u16,
    pub base: u64,
}

/// Sets the stack the processor switches to on an interrupt from user
/// mode.
pub fn set_rsp0(top: u64) {
    // SAFETY: only this processor uses its TSS, and an interrupt that
    // reads RSP0 cannot arrive in the middle of an aligned 8-byte store.
    unsafe { (&raw mut TSS.rsp[0]).write_unaligned(top) };
}

/// Allows or denies user-mode access to the I/O ports from `start` to
/// `end`, exclusive, which must not exceed 0x10000.
pub fn set_ports(start: u64, end: u64, allow: bool) {
    for port in start..end {
        let (byte, bit) = ((port / 8) as usize, 1u8 << (port % 8));
        // SAFETY: only this processor uses its TSS, `byte` lies inside
        // the bitmap, and the processor reads the bitmap only on I/O
        // instructions in user mode, which cannot run while this does.
        unsafe {
            let at = &raw mut TSS.io_bitmap[byte];
            *at = if allow { *at & !bit } else { *at | bit };
        }
    }
}

/// Builds the GDT and the TSS, with the IST stacks whose tops are given,
/// and loads them. Every segment register is reloaded.
#[cfg(target_os = "none")]
pub fn init(rsp0: u64, double_fault: u64, nmi: u64, machine_check: u64) {
    use crate::cpu::{read_msr, write_msr};
    use crate::percpu::{IA32_GS_BASE, IA32_KERNEL_GS_BASE};
    use core::arch::asm;
    let tss = &raw mut TSS;
    // SAFETY: nothing else uses the TSS or the GDT before they are loaded
    // here, on the only running processor.
    unsafe {
        let ist = [
            (IST_DOUBLE_FAULT, double_fault),
            (IST_NMI, nmi),
            (IST_MACHINE_CHECK, machine_check),
        ];
        for (entry, top) in ist {
            (&raw mut (*tss).ist[usize::from(entry) - 1]).write_unaligned(top);
        }
        set_rsp0(rsp0);
        let gdt = (&raw mut GDT).cast::<u64>();
        let limit = (IO_BITMAP_OFFSET + IO_BITMAP_SIZE) as u32;
        let tss_words = tss_descriptor(tss as u64, limit);
        for (i, &word) in SEGMENTS.iter().chain(&tss_words).enumerate() {
            gdt.add(i).write(word);
        }
        let pointer = Pointer {
            limit: (size_of::<[u64; 8]>() - 1) as u16,
            base: gdt as u64,
        };
        // Loading a null selector into GS may clear its base, so both GS
        // bases are kept across the reload.
        let gs = read_msr(IA32_GS_BASE);
        let kernel_gs = read_msr(IA32_KERNEL_GS_BASE);
        // The far return reloads CS; the other data segment registers
        // take the kernel data or the null selector.
        asm!(
            "lgdt [{pointer}]",
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ss, {data:x}",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov fs, {null:x}",
            "mov gs, {null:x}",
            "ltr {tss:x}",
            pointer = in(reg) &pointer,
            code = in(reg) u64::from(KERNEL_CODE),
            data = in(reg) u64::from(KERNEL_DATA),
            null = in(reg) 0u64,
            tss = in(reg) u64::from(TSS_SELECTOR),
            tmp = out(reg) _,
        );
        write_msr(IA32_GS_BASE, gs);
        write_msr(IA32_KERNEL_GS_BASE, kernel_gs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_tss_descriptor() {
        let [low, high] = tss_descriptor(0xFFFF_FFFF_8012_3456, 0x2068);
        assert_eq!(low, 0x8000_8912_3456_2068);
        assert_eq!(high, 0xFFFF_FFFF);
        let [low, _] = tss_descriptor(0, 0x1_2345);
        assert_eq!(low >> 48 & 0xF, 1);
    }
}
