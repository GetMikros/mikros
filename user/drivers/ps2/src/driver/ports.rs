//! The controller's I/O ports, reached through those that `Start`
//! enabled.

use crate::ps2::controller::Ports;
use alloc::vec::Vec;
use mikros_rt::hardware::IoPorts;
use mikros_rt::objects::now;

/// The ports enabled from the device's `IOPORT` resources, and where the
/// data and command ports are among them: the index in `enabled` and the
/// offset there.
pub struct ControllerPorts {
    pub enabled: Vec<IoPorts>,
    pub data: (usize, u16),
    pub command: (usize, u16),
}

impl ControllerPorts {
    fn inb(&self, (at, offset): (usize, u16)) -> u8 {
        self.enabled[at].read8(offset)
    }

    fn outb(&self, (at, offset): (usize, u16), value: u8) {
        self.enabled[at].write8(offset, value);
    }
}

impl Ports for ControllerPorts {
    fn status(&mut self) -> u8 {
        self.inb(self.command)
    }

    fn command(&mut self, command: u8) {
        self.outb(self.command, command);
    }

    fn read(&mut self) -> u8 {
        self.inb(self.data)
    }

    fn write(&mut self, data: u8) {
        self.outb(self.data, data);
    }

    fn now(&mut self) -> i64 {
        now()
    }
}
