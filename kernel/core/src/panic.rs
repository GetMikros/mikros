//! Writing the panic report.

use core::fmt::{self, Display, Write};

/// The longest panic title, in bytes.
pub const TITLE_MAX: usize = 120;

/// Passes at most `TITLE_MAX` bytes on to `out`, with line breaks and
/// other control characters replaced by `?`, so that the title stays on
/// one line.
struct TitleWriter<'a, W: Write> {
    out: &'a mut W,
    written: usize,
}

impl<W: Write> Write for TitleWriter<'_, W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let c = if c.is_control() { '?' } else { c };
            let len = c.len_utf8();
            if self.written + len > TITLE_MAX {
                return Ok(());
            }
            self.written += len;
            self.out.write_char(c)?;
        }
        Ok(())
    }
}

/// What a panic report shows.
pub struct Report<'a, F: Iterator<Item = u64>> {
    pub title: &'a dyn Display,
    pub build_id: &'a [u8],
    pub cpu: u32,
    /// Register names and values, shown four to a line.
    pub registers: &'a [(&'a str, u64)],
    /// Return addresses, innermost first.
    pub frames: F,
}

/// Writes `report` to `out`, with `newline` after each line.
pub fn write<W: Write, F: Iterator<Item = u64>>(
    out: &mut W,
    newline: &str,
    report: Report<'_, F>,
) -> fmt::Result {
    out.write_str("KERNEL PANIC: ")?;
    let mut title = TitleWriter { out, written: 0 };
    // A failing Display implementation leaves the title short.
    let _ = write!(title, "{}", report.title);
    out.write_str(newline)?;

    out.write_str("build ")?;
    for b in report.build_id {
        write!(out, "{b:02x}")?;
    }
    out.write_str(newline)?;
    write!(out, "cpu {}{newline}", report.cpu)?;

    for line in report.registers.chunks(4) {
        for (i, (name, value)) in line.iter().enumerate() {
            let sep = if i == 0 { "" } else { " " };
            write!(out, "{sep}{name}={value:016x}")?;
        }
        out.write_str(newline)?;
    }
    for (n, address) in report.frames.enumerate() {
        write!(out, "#{n} 0x{address:016x}{newline}")?;
    }
    write!(out, "END KERNEL PANIC{newline}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;

    fn report(title: &dyn Display, frames: &[u64]) -> String {
        let mut out = String::new();
        let registers = [
            ("rip", 0xffff_ffff_8000_1234),
            ("rsp", 0x10),
            ("rbp", 0),
            ("rax", 1),
            ("cr2", 2),
        ];
        write(
            &mut out,
            "\r\n",
            Report {
                title,
                build_id: &[0xde, 0xad, 0x01],
                cpu: 0,
                registers: &registers,
                frames: frames.iter().copied(),
            },
        )
        .unwrap();
        out
    }

    #[test]
    fn writes_every_part() {
        assert_eq!(
            report(&"out of memory", &[0xffff_ffff_8000_0042, 0x1000]),
            "KERNEL PANIC: out of memory\r\n\
             build dead01\r\n\
             cpu 0\r\n\
             rip=ffffffff80001234 rsp=0000000000000010 rbp=0000000000000000 rax=0000000000000001\r\n\
             cr2=0000000000000002\r\n\
             #0 0xffffffff80000042\r\n\
             #1 0x0000000000001000\r\n\
             END KERNEL PANIC\r\n"
        );
    }

    #[test]
    fn keeps_the_title_on_one_short_line() {
        let long = "é".repeat(100);
        let out = report(&long, &[]);
        let title = out
            .lines()
            .next()
            .unwrap()
            .strip_prefix("KERNEL PANIC: ")
            .unwrap();
        assert_eq!(title.len(), TITLE_MAX);
        let out = report(&"two\nlines\r", &[]);
        assert!(out.starts_with("KERNEL PANIC: two?lines?\r\nbuild "));
    }
}
