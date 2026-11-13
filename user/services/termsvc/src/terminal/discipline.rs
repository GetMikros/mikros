//! The line discipline: what the bytes typed on the master side do, as
//! the attributes say: queued input, in lines or as they come, echo,
//! erasing, signals and stopping output, and what written bytes become
//! on their way to the master side.

use crate::terminal::termios::*;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// The most bytes a line, or the queue of non-canonical input, holds;
/// more are dropped.
pub const INPUT_MAX: usize = 4096;
const NL: u8 = b'\n';
const CR: u8 = b'\r';
const TAB: u8 = b'\t';
const BACKSPACE: u8 = 0x08;

/// A signal for the foreground process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Quit,
    Suspend,
}

/// What a byte of input caused, besides what it queued.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Bytes to send to the master side, before output processing.
    pub echo: Vec<u8>,
    pub signal: Option<Signal>,
    /// Whether output was stopped or started again.
    pub output_stopped: Option<bool>,
    /// Whether the input and output queues were flushed.
    pub flushed: bool,
}

/// What `read` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    Data(Vec<u8>),
    /// The end of input: a line made of the end-of-file character alone.
    End,
    /// Nothing to read yet.
    Empty,
}

#[derive(Default)]
pub struct Discipline {
    pub attributes: Attributes,
    /// The line being edited in canonical mode.
    line: Vec<u8>,
    /// Completed lines, each with whether it ended with end-of-file
    /// rather than a delimiter, which stays out of the data.
    lines: VecDeque<(Vec<u8>, bool)>,
    /// Non-canonical input.
    raw: VecDeque<u8>,
    /// The next byte is taken literally.
    literal: bool,
    stopped: bool,
    /// The output column, for erasing tabs and for `ONOCR`.
    column: usize,
}

impl Discipline {
    pub fn new(attributes: Attributes) -> Discipline {
        Discipline {
            attributes,
            ..Discipline::default()
        }
    }

    fn canonical(&self) -> bool {
        self.attributes.local(ICANON)
    }

    /// Sets the attributes. Input moves between line and byte queues
    /// when canonical mode is turned off or on.
    pub fn set_attributes(&mut self, attributes: Attributes) {
        let was = self.canonical();
        self.attributes = attributes;
        match (was, self.canonical()) {
            (true, false) => {
                for (line, _) in self.lines.drain(..) {
                    self.raw.extend(line);
                }
                self.raw.extend(self.line.drain(..));
            }
            (false, true) if !self.raw.is_empty() => {
                let bytes: Vec<u8> = self.raw.drain(..).collect();
                self.lines.push_back((bytes, false));
            }
            _ => {}
        }
        if !self.attributes.input(IXON) {
            self.stopped = false;
        }
    }

    /// Whether output is stopped by the stop character.
    pub fn output_stopped(&self) -> bool {
        self.stopped
    }

    /// The bytes a `read` could take now: in canonical mode those of a
    /// completed line, otherwise all queued ones.
    pub fn available(&self) -> usize {
        match self.canonical() {
            true => self.lines.front().map_or(0, |(l, _)| l.len().max(1)),
            false => self.raw.len(),
        }
    }

    /// Whether a canonical read would complete now, or a non-canonical
    /// one has `MIN` bytes.
    pub fn readable(&self) -> bool {
        match self.canonical() {
            true => !self.lines.is_empty(),
            false => {
                let min = usize::from(self.attributes.control_chars[VMIN]);
                !self.raw.is_empty() && self.raw.len() >= min.max(1)
            }
        }
    }

    /// Takes at most `count` bytes of input. In canonical mode a read
    /// returns at most one line, and a line's rest stays for the next.
    pub fn read(&mut self, count: usize) -> Read {
        if self.canonical() {
            let Some((line, eof)) = self.lines.front_mut() else {
                return Read::Empty;
            };
            if line.is_empty() && *eof {
                self.lines.pop_front();
                return Read::End;
            }
            let n = count.min(line.len());
            let data: Vec<u8> = line.drain(..n).collect();
            if line.is_empty() {
                self.lines.pop_front();
            }
            return Read::Data(data);
        }
        if self.raw.is_empty() {
            return Read::Empty;
        }
        let n = count.min(self.raw.len());
        Read::Data(self.raw.drain(..n).collect())
    }

    /// Drops queued input.
    pub fn flush_input(&mut self) {
        self.line.clear();
        self.lines.clear();
        self.raw.clear();
    }

    /// Takes in `byte`, typed on the master side.
    pub fn input(&mut self, byte: u8) -> Effects {
        let mut e = Effects::default();
        let a = self.attributes;
        let mut b = byte;
        if a.input(ISTRIP) {
            b &= 0x7F;
        }
        if self.literal {
            self.literal = false;
            self.queue(b, &mut e);
            return e;
        }
        if a.input(IXON) {
            if a.is(VSTOP, b) {
                self.stopped = true;
                e.output_stopped = Some(true);
                return e;
            }
            if a.is(VSTART, b) || (self.stopped && a.input(IXANY)) {
                let was = self.stopped;
                self.stopped = false;
                if was {
                    e.output_stopped = Some(false);
                }
                if a.is(VSTART, b) {
                    return e;
                }
            }
        }
        if a.local(ISIG) {
            let signal = match () {
                _ if a.is(VINTR, b) => Some(Signal::Interrupt),
                _ if a.is(VQUIT, b) => Some(Signal::Quit),
                _ if a.is(VSUSP, b) => Some(Signal::Suspend),
                _ => None,
            };
            if let Some(signal) = signal {
                e.signal = Some(signal);
                if !a.local(NOFLSH) {
                    self.flush_input();
                    e.flushed = true;
                }
                self.echo_char(b, &mut e);
                return e;
            }
        }
        match b {
            CR if a.input(IGNCR) => return e,
            CR if a.input(ICRNL) => b = NL,
            NL if a.input(INLCR) => b = CR,
            _ => {}
        }
        if a.local(ICANON) {
            if a.local(IEXTEN) && a.is(VLNEXT, b) {
                self.literal = true;
                if a.local(ECHO) && a.local(ECHOCTL) {
                    e.echo.extend([b'^', BACKSPACE]);
                }
                return e;
            }
            if a.is(VERASE, b) {
                self.erase(&mut e);
                return e;
            }
            if a.local(IEXTEN) && a.is(VWERASE, b) {
                while self.line.last().is_some_and(|c| c.is_ascii_whitespace()) {
                    self.erase(&mut e);
                }
                while self.line.last().is_some_and(|c| !c.is_ascii_whitespace()) {
                    self.erase(&mut e);
                }
                return e;
            }
            if a.is(VKILL, b) {
                if a.local(ECHO) && a.local(ECHOKE) && a.local(ECHOE) {
                    while !self.line.is_empty() {
                        self.erase(&mut e);
                    }
                } else {
                    self.line.clear();
                    self.echo_char(b, &mut e);
                    if a.local(ECHOK) {
                        self.echo(NL, &mut e);
                    }
                }
                self.line.clear();
                return e;
            }
            if a.local(IEXTEN) && a.is(VREPRINT, b) {
                self.echo_char(b, &mut e);
                self.echo(NL, &mut e);
                for c in self.line.clone() {
                    self.echo_char(c, &mut e);
                }
                return e;
            }
            if a.is(VEOF, b) {
                let line = core::mem::take(&mut self.line);
                self.lines.push_back((line, true));
                return e;
            }
            let delimiter = b == NL || a.is(VEOL, b) || (a.local(IEXTEN) && a.is(VEOL2, b));
            if delimiter {
                if a.local(ECHO) || (b == NL && a.local(ECHONL)) {
                    self.echo(b, &mut e);
                }
                let mut line = core::mem::take(&mut self.line);
                line.push(b);
                self.lines.push_back((line, false));
                return e;
            }
        }
        self.queue(b, &mut e);
        e
    }

    /// Queues `b` as input and echoes it.
    fn queue(&mut self, b: u8, e: &mut Effects) {
        let (queue_len, canonical) = (self.line.len(), self.canonical());
        let full = match canonical {
            true => queue_len >= INPUT_MAX - 1,
            false => self.raw.len() >= INPUT_MAX,
        };
        if full {
            return;
        }
        match canonical {
            true => self.line.push(b),
            false => self.raw.push_back(b),
        }
        self.echo_char(b, e);
    }

    /// Echoes `b` when echo is on: control characters other than tab and
    /// newline as `^X` with `ECHOCTL`.
    fn echo_char(&mut self, b: u8, e: &mut Effects) {
        if !self.attributes.local(ECHO) {
            return;
        }
        if self.attributes.local(ECHOCTL) && is_control(b) {
            e.echo.extend([b'^', b ^ 0x40]);
            self.column += 2;
        } else {
            self.echo(b, e);
        }
    }

    fn echo(&mut self, b: u8, e: &mut Effects) {
        e.echo.push(b);
        self.column = match b {
            NL | CR => 0,
            TAB => (self.column | 7) + 1,
            BACKSPACE => self.column.saturating_sub(1),
            _ => self.column + 1,
        };
    }

    /// Erases the last character of the line, a whole UTF-8 sequence
    /// with `IUTF8`, and with `ECHOE` from the screen.
    fn erase(&mut self, e: &mut Effects) {
        let utf8 = self.attributes.input(IUTF8);
        let mut erased = Vec::new();
        while let Some(c) = self.line.pop() {
            erased.push(c);
            if !utf8 || c & 0xC0 != 0x80 {
                break;
            }
        }
        let Some(&first) = erased.last() else {
            return;
        };
        let a = self.attributes;
        if !a.local(ECHO) {
            return;
        }
        if !a.local(ECHOE) {
            let erase = a.control_chars[VERASE];
            self.echo_char(erase, e);
            return;
        }
        let width = match first {
            TAB => self.tab_width(),
            c if a.local(ECHOCTL) && is_control(c) => 2,
            _ => 1,
        };
        for _ in 0..width {
            e.echo.extend([BACKSPACE, b' ', BACKSPACE]);
            self.column = self.column.saturating_sub(1);
        }
    }

    /// How many columns the tab at the end of the line took, from where
    /// the rest of the line ends.
    fn tab_width(&self) -> usize {
        let mut column = 0;
        for &c in &self.line {
            column = match c {
                TAB => (column | 7) + 1,
                c if self.attributes.local(ECHOCTL) && is_control(c) => column + 2,
                _ => column + 1,
            };
        }
        ((column | 7) + 1) - column
    }

    /// `data`, written on the slave side, as it goes to the master side.
    pub fn output(&mut self, data: &[u8]) -> Vec<u8> {
        let a = self.attributes;
        if !a.output(OPOST) {
            self.track(data);
            return data.to_vec();
        }
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            match b {
                NL if a.output(ONLCR) => {
                    out.extend([CR, NL]);
                    self.column = 0;
                }
                NL => {
                    out.push(NL);
                    if a.output(ONLRET) {
                        self.column = 0;
                    }
                }
                CR if a.output(ONOCR) && self.column == 0 => {}
                CR if a.output(OCRNL) => {
                    out.push(NL);
                    if a.output(ONLRET) {
                        self.column = 0;
                    }
                }
                _ => {
                    out.push(b);
                    self.track(&[b]);
                }
            }
        }
        out
    }

    fn track(&mut self, data: &[u8]) {
        for &b in data {
            self.column = match b {
                NL | CR => 0,
                TAB => (self.column | 7) + 1,
                BACKSPACE => self.column.saturating_sub(1),
                c if c < 0x20 || c == 0x7F => self.column,
                // UTF-8 continuation bytes take no column.
                c if c & 0xC0 == 0x80 => self.column,
                _ => self.column + 1,
            };
        }
    }
}

fn is_control(b: u8) -> bool {
    (b < 0x20 && b != TAB && b != NL) || b == 0x7F
}
