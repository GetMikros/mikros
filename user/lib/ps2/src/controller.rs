//! The i8042 controller: setting it up, and sending commands to the
//! devices on its two ports and waiting for their answers.

/// The status register's bits: the output buffer holds a byte, the input
/// buffer holds one the controller has not taken, and the output byte
/// came from the second port.
const OUTPUT_FULL: u8 = 1 << 0;
const INPUT_FULL: u8 = 1 << 1;
const FROM_SECOND: u8 = 1 << 5;
/// Controller commands.
const READ_CONFIG: u8 = 0x20;
const WRITE_CONFIG: u8 = 0x60;
const DISABLE_SECOND: u8 = 0xA7;
const ENABLE_SECOND: u8 = 0xA8;
const TEST_SECOND: u8 = 0xA9;
const SELF_TEST: u8 = 0xAA;
const TEST_FIRST: u8 = 0xAB;
const DISABLE_FIRST: u8 = 0xAD;
const ENABLE_FIRST: u8 = 0xAE;
const TO_SECOND: u8 = 0xD4;
/// The configuration byte's bits: the ports' interrupts, their clocks
/// being off, and translation to scan code set 1.
const FIRST_INTERRUPT: u8 = 1 << 0;
const SECOND_INTERRUPT: u8 = 1 << 1;
const FIRST_CLOCK_OFF: u8 = 1 << 4;
const SECOND_CLOCK_OFF: u8 = 1 << 5;
const TRANSLATION: u8 = 1 << 6;
/// What the self-test and the port tests answer when they pass.
const SELF_TEST_PASSED: u8 = 0x55;
const PORT_TEST_PASSED: u8 = 0x00;
/// Device commands and answers.
pub const RESET: u8 = 0xFF;
pub const ACK: u8 = 0xFA;
pub const RESEND: u8 = 0xFE;
const RESET_PASSED: u8 = 0xAA;
/// The most bytes flushed from the output buffer.
const FLUSH_MAX: usize = 64;
/// How long a device has to acknowledge a command, and the controller
/// and a reset device to answer, in nanoseconds.
pub const ACK_TIMEOUT: i64 = 100_000_000;
const CONTROLLER_TIMEOUT: i64 = 100_000_000;
const RESET_TIMEOUT: i64 = 1_000_000_000;

/// Access to the controller's data port and its status and command
/// port, and the monotonic clock in nanoseconds.
pub trait Ports {
    fn status(&mut self) -> u8;
    fn command(&mut self, command: u8);
    fn read(&mut self) -> u8;
    fn write(&mut self, data: u8);
    fn now(&mut self) -> i64;
}

/// One of the controller's two ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    First,
    Second,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The controller did not take or answer a command in time.
    Timeout,
    /// The self-test answered this.
    SelfTest(u8),
    /// The device did not acknowledge a command.
    NoAck,
}

/// The ports whose test passed and whose device answered the reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Present {
    pub first: bool,
    pub second: bool,
}

pub struct Controller<P> {
    ports: P,
    config: u8,
}

impl<P: Ports> Controller<P> {
    pub fn new(ports: P) -> Controller<P> {
        Controller { ports, config: 0 }
    }

    pub fn ports(&mut self) -> &mut P {
        &mut self.ports
    }

    /// Sets the controller up in this order: both ports disabled, the
    /// output buffer flushed, interrupts and translation off, the
    /// self-test, the port tests, the ports that passed enabled, and
    /// their devices reset. Interrupts stay off until `enable_interrupts`.
    pub fn init(&mut self, other: &mut impl FnMut(Port, u8)) -> Result<Present, Error> {
        self.controller(DISABLE_FIRST)?;
        self.controller(DISABLE_SECOND)?;
        for _ in 0..FLUSH_MAX {
            if self.ports.status() & OUTPUT_FULL == 0 {
                break;
            }
            self.ports.read();
        }
        let config = self.controller_reply(READ_CONFIG)?;
        // With the second port disabled, its clock is off if it exists.
        let has_second = config & SECOND_CLOCK_OFF != 0;
        self.config = config & !(FIRST_INTERRUPT | SECOND_INTERRUPT | TRANSLATION);
        self.write_config()?;
        match self.controller_reply(SELF_TEST)? {
            SELF_TEST_PASSED => {}
            answer => return Err(Error::SelfTest(answer)),
        }
        // Some controllers reset the configuration byte in the self-test.
        self.write_config()?;
        let mut present = Present {
            first: self.controller_reply(TEST_FIRST)? == PORT_TEST_PASSED,
            second: has_second && self.controller_reply(TEST_SECOND)? == PORT_TEST_PASSED,
        };
        if present.first {
            self.controller(ENABLE_FIRST)?;
        }
        if present.second {
            self.controller(ENABLE_SECOND)?;
        }
        present.first = present.first && self.reset(Port::First, other);
        present.second = present.second && self.reset(Port::Second, other);
        Ok(present)
    }

    /// Turns on the interrupts of the ports in `present`. Their clocks,
    /// which enabling the ports turned on, stay on: the configuration
    /// byte read while they were disabled says off.
    pub fn enable_interrupts(&mut self, present: Present) -> Result<(), Error> {
        if present.first {
            self.config = (self.config | FIRST_INTERRUPT) & !FIRST_CLOCK_OFF;
        }
        if present.second {
            self.config = (self.config | SECOND_INTERRUPT) & !SECOND_CLOCK_OFF;
        }
        self.write_config()
    }

    /// Resets the device on `port`: whether it answered that it passed.
    /// The ID byte that a mouse sends after that is read and dropped.
    fn reset(&mut self, port: Port, other: &mut impl FnMut(Port, u8)) -> bool {
        if self.send(port, RESET, other).is_err() {
            return false;
        }
        let passed = self.receive(port, RESET_TIMEOUT, other) == Some(RESET_PASSED);
        if passed && port == Port::Second {
            self.receive(port, ACK_TIMEOUT, other);
        }
        passed
    }

    /// Sends `byte` to the device on `port` and waits for it to be
    /// acknowledged, sending it once more when the device asks for that.
    /// Bytes that are not the answer go to `other`.
    pub fn send(
        &mut self,
        port: Port,
        byte: u8,
        other: &mut impl FnMut(Port, u8),
    ) -> Result<(), Error> {
        for _ in 0..2 {
            if port == Port::Second {
                self.controller(TO_SECOND)?;
            }
            self.write_data(byte)?;
            match self.acknowledgment(port, other) {
                Some(ACK) => return Ok(()),
                Some(_) => continue,
                None => return Err(Error::NoAck),
            }
        }
        Err(Error::NoAck)
    }

    /// `ACK` or `RESEND` from `port` within `ACK_TIMEOUT`. Other bytes,
    /// such as the scan codes of keys pressed meanwhile, go to `other`.
    fn acknowledgment(&mut self, port: Port, other: &mut impl FnMut(Port, u8)) -> Option<u8> {
        let deadline = self.ports.now() + ACK_TIMEOUT;
        while self.ports.now() < deadline {
            match self.poll() {
                Some((from, answer @ (ACK | RESEND))) if from == port => return Some(answer),
                Some((from, byte)) => other(from, byte),
                None => {}
            }
        }
        None
    }

    /// Sends each of `bytes` with `send`.
    pub fn send_all(
        &mut self,
        port: Port,
        bytes: &[u8],
        other: &mut impl FnMut(Port, u8),
    ) -> Result<(), Error> {
        bytes.iter().try_for_each(|&b| self.send(port, b, other))
    }

    /// The next byte from `port` within `timeout`. Bytes from the other
    /// port go to `other`.
    pub fn receive(
        &mut self,
        port: Port,
        timeout: i64,
        other: &mut impl FnMut(Port, u8),
    ) -> Option<u8> {
        let deadline = self.ports.now() + timeout;
        while self.ports.now() < deadline {
            if let Some((from, byte)) = self.poll() {
                if from == port {
                    return Some(byte);
                }
                other(from, byte);
            }
        }
        None
    }

    /// The byte in the output buffer and the port it came from, if
    /// there is one.
    pub fn poll(&mut self) -> Option<(Port, u8)> {
        let status = self.ports.status();
        if status & OUTPUT_FULL == 0 {
            return None;
        }
        let from = if status & FROM_SECOND != 0 {
            Port::Second
        } else {
            Port::First
        };
        Some((from, self.ports.read()))
    }

    fn write_config(&mut self) -> Result<(), Error> {
        self.controller(WRITE_CONFIG)?;
        self.write_data(self.config)
    }

    fn controller(&mut self, command: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.ports.command(command);
        Ok(())
    }

    fn controller_reply(&mut self, command: u8) -> Result<u8, Error> {
        self.controller(command)?;
        let deadline = self.ports.now() + CONTROLLER_TIMEOUT;
        while self.ports.now() < deadline {
            if self.ports.status() & OUTPUT_FULL != 0 {
                return Ok(self.ports.read());
            }
        }
        Err(Error::Timeout)
    }

    fn write_data(&mut self, data: u8) -> Result<(), Error> {
        self.wait_input_empty()?;
        self.ports.write(data);
        Ok(())
    }

    fn wait_input_empty(&mut self) -> Result<(), Error> {
        let deadline = self.ports.now() + CONTROLLER_TIMEOUT;
        while self.ports.status() & INPUT_FULL != 0 {
            if self.ports.now() >= deadline {
                return Err(Error::Timeout);
            }
        }
        Ok(())
    }
}
