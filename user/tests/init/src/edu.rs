//! The tests that drive QEMU's `edu` device: DMA through the IOMMU and
//! message-signaled interrupts.

use crate::{
    BELOW_4G, DEVICE_READ, DEVICE_WRITE, KIND_DMA, SIGNALED, TIMED_OUT, bti_create, bti_pin, close,
    grant, in_time, mem_read_at, mem_write_at, now, wait,
};
use mikros_rt::Startup;
use mikros_rt::sys::{self, Handle};
use mikros_test::{KIND_IOPORT, PAGE_SIZE, PERM_READ, PERM_WRITE, Result, check, ensure, sleep_ms};

/// The PCI configuration ports.
const CONFIG_PORTS: u64 = 0xCF8;
/// The vendor and device IDs of `edu`.
const EDU_ID: u32 = 0x11E8_1234;
/// `edu` registers: DMA source, destination, length and command, and the
/// interrupt raise and acknowledge registers.
const DMA_SRC: usize = 0x80;
const DMA_DST: usize = 0x88;
const DMA_LEN: usize = 0x90;
const DMA_CMD: usize = 0x98;
const RAISE: usize = 0x60;
const ACK: usize = 0x64;
/// DMA command bits: start, and copy from the device to memory.
const START: u64 = 1 << 0;
const TO_MEMORY: u64 = 1 << 1;
/// The device address of `edu`'s DMA buffer.
const BUFFER: u64 = 0x40000;

fn config_address(dev: u8, offset: u8) -> u32 {
    0x8000_0000 | u32::from(dev) << 11 | u32::from(offset & 0xFC)
}

/// Reads the configuration register at `offset` of function 0 of `dev`
/// on bus 0.
fn config_read(dev: u8, offset: u8) -> u32 {
    let value: u32;
    // SAFETY: the process was granted the configuration ports, which
    // only select and access configuration registers.
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0xCF8u16, in("eax") config_address(dev, offset), options(nomem, nostack));
        core::arch::asm!("in eax, dx", in("dx") 0xCFCu16, out("eax") value, options(nomem, nostack));
    }
    value
}

/// Writes the configuration register at `offset` of function 0 of `dev`
/// on bus 0.
fn config_write(dev: u8, offset: u8, value: u32) {
    // SAFETY: as in `config_read`.
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0xCF8u16, in("eax") config_address(dev, offset), options(nomem, nostack));
        core::arch::asm!("out dx, eax", in("dx") 0xCFCu16, in("eax") value, options(nomem, nostack));
    }
}

/// The device, with its requester ID and registers mapped into the test,
/// and the resources and objects to close.
struct Edu {
    dev: u8,
    rid: u64,
    regs: u64,
    handles: [Handle; 3],
}

impl Edu {
    /// Finds `edu` on bus 0, turns on its memory space and bus mastering,
    /// and maps its registers.
    fn open(s: &Startup) -> Result<Edu> {
        let ports = grant(s, KIND_IOPORT, CONFIG_PORTS, 8)?;
        // SAFETY: the call takes no pointers.
        check("granting the configuration ports", unsafe {
            sys::process_ioports_enable(s.process, ports, CONFIG_PORTS, 8)
        })?;
        let dev = (0..32).find(|&d| config_read(d, 0) == EDU_ID);
        ensure("finding the edu device", dev.is_some())?;
        let dev = dev.unwrap_or(0);
        let bar = u64::from(config_read(dev, 0x10) & !0xF);
        config_write(dev, 0x04, config_read(dev, 0x04) | 0b110);
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        check("creating the register object", unsafe {
            sys::mem_create_physical(s.root_resource, bar, PAGE_SIZE, 2, &mut mem)
        })?;
        let mut regs = 0;
        // SAFETY: the out pointer is valid for the call.
        check("mapping the registers", unsafe {
            sys::vm_map(
                s.process,
                PERM_READ | PERM_WRITE,
                0,
                mem,
                0,
                PAGE_SIZE,
                &mut regs,
            )
        })?;
        Ok(Edu {
            dev,
            rid: u64::from(dev) << 3,
            regs,
            handles: [ports, mem, 0],
        })
    }

    fn read(&self, reg: usize) -> u64 {
        // SAFETY: the registers are mapped, and reading them has no
        // effect beyond the device.
        unsafe { ((self.regs + reg as u64) as *const u64).read_volatile() }
    }

    fn write(&self, reg: usize, value: u64) {
        // SAFETY: the registers are mapped; the test chooses the values.
        unsafe { ((self.regs + reg as u64) as *mut u64).write_volatile(value) };
    }

    fn write32(&self, reg: usize, value: u32) {
        // SAFETY: as in `write`.
        unsafe { ((self.regs + reg as u64) as *mut u32).write_volatile(value) };
    }

    /// Copies `len` bytes from `src` to `dst` by DMA, from memory to the
    /// device's buffer or back, and waits until the device is done.
    fn dma(&self, src: u64, dst: u64, len: u64, to_memory: bool) -> Result {
        self.write(DMA_SRC, src);
        self.write(DMA_DST, dst);
        self.write(DMA_LEN, len);
        self.write(DMA_CMD, START | if to_memory { TO_MEMORY } else { 0 });
        let deadline = in_time();
        while self.read(DMA_CMD) & START != 0 {
            ensure("finishing the DMA", now() < deadline)?;
            sleep_ms(5);
        }
        Ok(())
    }
}

impl Drop for Edu {
    fn drop(&mut self) {
        config_write(self.dev, 0x04, config_read(self.dev, 0x04) & !0b100);
        for handle in self.handles {
            if handle != 0 {
                let _ = close(handle);
            }
        }
    }
}

/// A zeroed page below 4 GiB for DMA, with its physical address.
fn dma_page(dma: Handle) -> Result<(Handle, u64)> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a DMA page", unsafe {
        sys::mem_create_contiguous(dma, PAGE_SIZE, 12, BELOW_4G, &mut mem)
    })?;
    let mut phys = 0;
    // SAFETY: the out pointer is valid for the call.
    check("reading its address", unsafe {
        sys::mem_get_physical(mem, &mut phys)
    })?;
    Ok((mem, phys))
}

/// The device reaches pinned pages with the access they were pinned
/// with, and none once they are unpinned: a copy into an unpinned page
/// leaves it as it was.
pub fn dma_through_pins(s: &Startup) -> Result {
    let edu = Edu::open(s)?;
    let dma = grant(s, KIND_DMA, 0, 0)?;
    let (status, bti) = bti_create(s.root_resource, edu.rid, 0);
    check("creating the device's Bti", status)?;
    let (src, src_phys) = dma_page(dma)?;
    let (dst, dst_phys) = dma_page(dma)?;
    mem_write_at(src, 0, &[0xA5; 8])?;
    let (status, read) = bti_pin(bti, src, 0, PAGE_SIZE, DEVICE_READ);
    check("pinning the source", status)?;
    let (status, write) = bti_pin(bti, dst, 0, PAGE_SIZE, DEVICE_WRITE);
    check("pinning the destination", status)?;
    edu.dma(src_phys, BUFFER, 8, false)?;
    edu.dma(BUFFER, dst_phys, 8, true)?;
    ensure(
        "copying through the pins",
        mem_read_at::<8>(dst, 0)? == [0xA5; 8],
    )?;

    // SAFETY: the call takes no pointers.
    check("unpinning the destination", unsafe {
        sys::pmt_unpin(write)
    })?;
    mem_write_at(dst, 0, &[0; 8])?;
    edu.dma(BUFFER, dst_phys, 8, true)?;
    let isolated = crate::kernel_logged(s, b"DMA isolated")?;
    let copied = mem_read_at::<8>(dst, 0)? == [0xA5; 8];
    ensure("blocking an unpinned page", copied != isolated)?;
    // SAFETY: the call takes no pointers.
    check("unpinning the source", unsafe { sys::pmt_unpin(read) })?;
    for handle in [src, dst, bti, dma] {
        close(handle)?;
    }
    Ok(())
}

/// Finds the MSI capability of `dev` and points it at `address` and
/// `data`, enabled.
fn enable_msi(dev: u8, address: u64, data: u32) -> Result {
    let mut at = (config_read(dev, 0x34) & 0xFC) as u8;
    while at != 0 && config_read(dev, at) & 0xFF != 0x05 {
        at = (config_read(dev, at) >> 8 & 0xFC) as u8;
    }
    ensure("finding the MSI capability", at != 0)?;
    let control = config_read(dev, at) >> 16;
    config_write(dev, at + 4, address as u32);
    let data_at = if control & 1 << 7 != 0 {
        config_write(dev, at + 8, (address >> 32) as u32);
        at + 12
    } else {
        at + 8
    };
    config_write(dev, data_at, data);
    config_write(dev, at, config_read(dev, at) | 1 << 16);
    Ok(())
}

/// A message that `interrupt_create_msi` returns, written by the device,
/// signals its object, which stays signaled until it is acknowledged.
pub fn msi_from_device(s: &Startup) -> Result {
    let edu = Edu::open(s)?;
    let (status, bti) = bti_create(s.root_resource, edu.rid, 0);
    check("creating the device's Bti", status)?;
    let mut interrupt = 0;
    let mut message = [0u8; 16];
    // SAFETY: the pointers describe one handle and one message.
    check("creating a message-signaled interrupt", unsafe {
        sys::interrupt_create_msi(
            s.root_resource,
            bti,
            1,
            0,
            &mut interrupt,
            message.as_mut_ptr(),
        )
    })?;
    let address = u64::from_le_bytes(message[0..8].try_into().unwrap());
    let data = u32::from_le_bytes(message[8..12].try_into().unwrap());
    ensure(
        "giving an address the local APICs take",
        address >> 20 == 0xFEE,
    )?;
    enable_msi(edu.dev, address, data)?;
    ensure(
        "starting clear",
        wait(interrupt, SIGNALED, 0).0 == TIMED_OUT,
    )?;
    for _ in 0..2 {
        edu.write32(RAISE, 1);
        ensure(
            "signaling the interrupt",
            wait(interrupt, SIGNALED, in_time()).0 == 0,
        )?;
        edu.write32(ACK, 1);
        // SAFETY: the call takes no pointers.
        check("acknowledging", unsafe { sys::interrupt_ack(interrupt) })?;
        ensure(
            "clearing SIGNALED",
            wait(interrupt, SIGNALED, 0).0 == TIMED_OUT,
        )?;
    }
    close(interrupt)?;
    close(bti)
}
