//! The debug serial port: the 16550 UART at I/O port 0x3F8.

use crate::cpu::{in8, out8};
use core::sync::atomic::{AtomicBool, Ordering};

const BASE: u16 = 0x3F8;
const DATA: u16 = BASE;
const INTERRUPT_ENABLE: u16 = BASE + 1;
const DIVISOR_LOW: u16 = BASE;
const DIVISOR_HIGH: u16 = BASE + 1;
const FIFO_CONTROL: u16 = BASE + 2;
const LINE_CONTROL: u16 = BASE + 3;
const MODEM_CONTROL: u16 = BASE + 4;
const LINE_STATUS: u16 = BASE + 5;
const SCRATCH: u16 = BASE + 7;

/// Line control: 8 data bits, no parity, 1 stop bit.
const EIGHT_N_ONE: u8 = 0x03;
/// Line control: divisor latch access.
const DIVISOR_LATCH: u8 = 0x80;
/// The divisor of the 115200 Hz base clock for 115200 baud.
const DIVISOR: u16 = 1;
/// Line status: the transmit holding register is empty.
const TRANSMIT_EMPTY: u8 = 0x20;

static PRESENT: AtomicBool = AtomicBool::new(false);

/// Finds and sets up the port. Until this has found it, writes do nothing.
pub fn init() {
    // SAFETY: the ports belong to the debug UART, which nothing else in the
    // kernel uses; if no UART is there, the accesses have no effect.
    unsafe {
        out8(SCRATCH, 0x5A);
        if in8(SCRATCH) != 0x5A {
            return;
        }
        out8(SCRATCH, 0xA5);
        if in8(SCRATCH) != 0xA5 {
            return;
        }
        out8(INTERRUPT_ENABLE, 0);
        out8(LINE_CONTROL, DIVISOR_LATCH);
        let [low, high] = DIVISOR.to_le_bytes();
        out8(DIVISOR_LOW, low);
        out8(DIVISOR_HIGH, high);
        out8(LINE_CONTROL, EIGHT_N_ONE);
        // Enable and clear the FIFOs.
        out8(FIFO_CONTROL, 0x07);
        // Data terminal ready and request to send, without the interrupt
        // output.
        out8(MODEM_CONTROL, 0x03);
    }
    PRESENT.store(true, Ordering::Relaxed);
}

/// Whether the port was found.
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// The most line status reads that wait for the transmitter, about 100 ms
/// of port reads, before the port counts as gone.
const TRANSMIT_POLLS: u32 = 100_000;

/// Writes `bytes`, waiting for the transmitter before each one, and
/// answering TLB shootdowns while it waits: the log is written with
/// interrupts disabled, and a slow port must not hold up the other
/// processors. A port whose transmitter does not empty within
/// `TRANSMIT_POLLS` reads is no longer written to.
pub fn write(bytes: &[u8]) {
    if !PRESENT.load(Ordering::Relaxed) {
        return;
    }
    'bytes: for &b in bytes {
        let mut polls = 0;
        // SAFETY: as in `init`; reading the line status and writing the data
        // register only transmit a byte.
        while unsafe { in8(LINE_STATUS) } & TRANSMIT_EMPTY == 0 {
            crate::tlb::service();
            polls += 1;
            if polls == TRANSMIT_POLLS {
                PRESENT.store(false, Ordering::Relaxed);
                break 'bytes;
            }
        }
        // SAFETY: as above.
        unsafe { out8(DATA, b) };
    }
}

/// The serial port as a formatting target.
pub struct Serial;

impl core::fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write(s.as_bytes());
        Ok(())
    }
}
