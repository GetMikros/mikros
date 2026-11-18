//! The CMOS clock through its index and data ports: reading its time
//! registers until two reads in a row agree, each once an update is not
//! in progress, and setting them with updates stopped.

use crate::cmos::{
    DAY, Format, HOURS, MINUTES, MONTH, Registers, SECONDS, SET, STATUS_A, STATUS_B,
    UPDATE_IN_PROGRESS, WEEKDAY, YEAR,
};
use mikros_rt::hardware::IoPorts;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::objects::now;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// How long an update may stay in progress: the clock's take under 2 ms.
const UPDATE_LIMIT: i64 = 10_000_000;
/// How many reads may disagree with the one before them.
const READS_MAX: u32 = 100;

/// Port offsets: the index port, then the data port.
const INDEX: u16 = 0;
const DATA: u16 = 1;

/// The clock, whose index and data ports are `ports`, and the index of
/// its century register, when it has one.
pub struct Clock {
    ports: IoPorts,
    century: Option<u8>,
}

impl Clock {
    /// The clock at `ports`, its index port and then its data port.
    pub fn new(ports: IoPorts, century: Option<u8>) -> Clock {
        Clock { ports, century }
    }

    fn read(&self, register: u8) -> u8 {
        self.ports.write8(INDEX, register);
        self.ports.read8(DATA)
    }

    fn write(&self, register: u8, value: u8) {
        self.ports.write8(INDEX, register);
        self.ports.write8(DATA, value);
    }

    /// Waits until no update is in progress: false when one lasts too long.
    fn wait_for_update(&self) -> bool {
        let deadline = now() + UPDATE_LIMIT;
        while self.read(STATUS_A) & UPDATE_IN_PROGRESS != 0 {
            if now() >= deadline {
                return false;
            }
            core::hint::spin_loop();
        }
        true
    }

    /// The time registers, but not the day of the week, which is not
    /// part of the time read.
    fn registers(&self) -> Registers {
        Registers {
            seconds: self.read(SECONDS),
            minutes: self.read(MINUTES),
            hours: self.read(HOURS),
            weekday: 0,
            day: self.read(DAY),
            month: self.read(MONTH),
            year: self.read(YEAR),
            century: self.century.map(|c| self.read(c)),
        }
    }

    /// The clock's date and time, in nanoseconds, as it keeps them: `IO`
    /// when its registers hold no valid date and time, or keep changing.
    pub fn get(&self) -> Result<i64, Status> {
        let format = Format::of(self.read(STATUS_B));
        let mut last: Option<Registers> = None;
        for _ in 0..READS_MAX {
            if !self.wait_for_update() {
                return Err(Status::Io);
            }
            let read = self.registers();
            if last == Some(read) {
                let seconds = crate::cmos::decode(&read, format).ok_or(Status::Io)?;
                return seconds.checked_mul(NANOS_PER_SECOND).ok_or(Status::Io);
            }
            last = Some(read);
        }
        Err(Status::Io)
    }

    /// Sets the clock to `time`, in nanoseconds, in its format, with the
    /// day of the week: `OUT_OF_RANGE`, writing nothing, when it cannot
    /// hold the time.
    pub fn set(&self, time: i64) -> Result<(), Status> {
        let status_b = self.read(STATUS_B);
        let seconds = time.div_euclid(NANOS_PER_SECOND);
        let registers = crate::cmos::encode(seconds, Format::of(status_b), self.century.is_some())
            .ok_or(Status::OutOfRange)?;
        self.write(STATUS_B, status_b | SET);
        self.write(SECONDS, registers.seconds);
        self.write(MINUTES, registers.minutes);
        self.write(HOURS, registers.hours);
        self.write(WEEKDAY, registers.weekday);
        self.write(DAY, registers.day);
        self.write(MONTH, registers.month);
        self.write(YEAR, registers.year);
        if let (Some(index), Some(century)) = (self.century, registers.century) {
            self.write(index, century);
        }
        self.write(STATUS_B, status_b & !SET);
        Ok(())
    }
}
