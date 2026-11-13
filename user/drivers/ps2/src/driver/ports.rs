//! The controller's I/O ports, reached with `in` and `out`.

use crate::ps2::controller::Ports;
use mikros_rt::sys;

pub struct IoPorts {
    pub data: u16,
    pub command: u16,
}

impl Ports for IoPorts {
    fn status(&mut self) -> u8 {
        inb(self.command)
    }

    fn command(&mut self, command: u8) {
        outb(self.command, command);
    }

    fn read(&mut self) -> u8 {
        inb(self.data)
    }

    fn write(&mut self, data: u8) {
        outb(self.data, data);
    }

    fn now(&mut self) -> i64 {
        now()
    }
}

/// The monotonic clock, in nanoseconds.
pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: `Start` enabled the controller's ports for this process.
    unsafe {
        core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

fn outb(port: u16, value: u8) {
    // SAFETY: as for `inb`.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}
