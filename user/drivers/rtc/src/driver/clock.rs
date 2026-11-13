//! The CMOS clock through its index and data ports: reading its time
//! registers until two reads in a row agree, each once an update is not
//! in progress, and setting them with updates stopped.

use crate::cmos::{
    DAY, Format, HOURS, MINUTES, MONTH, Registers, SECONDS, SET, STATUS_A, STATUS_B,
    UPDATE_IN_PROGRESS, WEEKDAY, YEAR,
};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// How long an update may stay in progress: the clock's take under 2 ms.
const UPDATE_LIMIT: i64 = 10_000_000;
/// How many reads may disagree with the one before them.
const READS_MAX: u32 = 100;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// The clock, whose index port is `index` and data port the next, and
/// the index of its century register, when it has one.
pub struct Clock {
    index: u16,
    century: Option<u8>,
}

impl Clock {
    /// The clock at the ports from `index`, which the process may use.
    pub fn new(index: u16, century: Option<u8>) -> Clock {
        Clock { index, century }
    }

    fn read(&self, register: u8) -> u8 {
        let value: u8;
        // SAFETY: the process may use the clock's two ports, which only
        // the hardware reads.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") self.index, in("al") register, options(nomem, nostack, preserves_flags));
            core::arch::asm!("in al, dx", out("al") value, in("dx") self.index + 1, options(nomem, nostack, preserves_flags));
        }
        value
    }

    fn write(&self, register: u8, value: u8) {
        // SAFETY: as for `read`.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") self.index, in("al") register, options(nomem, nostack, preserves_flags));
            core::arch::asm!("out dx, al", in("dx") self.index + 1, in("al") value, options(nomem, nostack, preserves_flags));
        }
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
