//! Parsing what a program writes to a terminal: UTF-8 text, control
//! characters, and escape, control and operating system command
//! sequences, after the DEC VT500 parser state machine.

use alloc::vec::Vec;

/// The most parameters, intermediate bytes and OSC bytes kept; more
/// are dropped.
pub const PARAMS_MAX: usize = 32;
pub const INTERMEDIATES_MAX: usize = 2;
pub const OSC_MAX: usize = 512;
const ESC: u8 = 0x1B;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1A;
const BEL: u8 = 0x07;
const DEL: u8 = 0x7F;
/// What a byte that is not valid UTF-8 shows as.
const REPLACEMENT: char = '\u{FFFD}';

/// What a sequence of bytes means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A character to show.
    Print(char),
    /// A C0 control character, such as LF or BS.
    Execute(u8),
    /// A control sequence: its private marker or intermediate bytes,
    /// its parameters, each with its subparameters after `:`, missing
    /// ones as `None`, and its final byte.
    Csi {
        intermediates: Vec<u8>,
        params: Vec<Option<u32>>,
        sub: Vec<Vec<Option<u32>>>,
        last: u8,
    },
    /// An escape sequence: its intermediate bytes and final byte.
    Esc { intermediates: Vec<u8>, last: u8 },
    /// An operating system command's text.
    Osc(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    Osc,
    /// After ESC within an OSC or a device control string, which ESC \
    /// ends.
    StringEscape,
    /// A device control, privacy or application program string, which
    /// is ignored.
    Ignored,
}

pub struct Parser {
    state: State,
    /// The state that a `StringEscape` returns to when the ESC began no
    /// string terminator.
    string: State,
    intermediates: Vec<u8>,
    params: Vec<Option<u32>>,
    sub: Vec<Vec<Option<u32>>>,
    /// The parameter being read, and whether it is a subparameter.
    current: Option<u32>,
    in_sub: bool,
    osc: Vec<u8>,
    /// A UTF-8 sequence being decoded: its value so far and how many
    /// continuation bytes are still to come.
    utf8: u32,
    pending: u8,
    /// The smallest value of the sequence, to refuse overlong forms.
    minimum: u32,
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

impl Parser {
    pub const fn new() -> Parser {
        Parser {
            state: State::Ground,
            string: State::Ground,
            intermediates: Vec::new(),
            params: Vec::new(),
            sub: Vec::new(),
            current: None,
            in_sub: false,
            osc: Vec::new(),
            utf8: 0,
            pending: 0,
            minimum: 0,
        }
    }

    /// Takes in `bytes` and calls `act` for each action they complete.
    pub fn feed(&mut self, bytes: &[u8], mut act: impl FnMut(Action)) {
        for &b in bytes {
            self.byte(b, &mut act);
        }
    }

    fn clear(&mut self) {
        self.intermediates.clear();
        self.params.clear();
        self.sub.clear();
        self.current = None;
        self.in_sub = false;
    }

    fn byte(&mut self, b: u8, act: &mut impl FnMut(Action)) {
        // A UTF-8 sequence in text ends at any byte that cannot continue
        // it, which shows as a replacement character.
        if self.pending > 0 {
            if b & 0xC0 == 0x80 {
                self.utf8 = self.utf8 << 6 | u32::from(b & 0x3F);
                self.pending -= 1;
                if self.pending == 0 {
                    let c = Some(self.utf8)
                        .filter(|&v| v >= self.minimum)
                        .and_then(char::from_u32)
                        .unwrap_or(REPLACEMENT);
                    act(Action::Print(c));
                }
                return;
            }
            self.pending = 0;
            act(Action::Print(REPLACEMENT));
        }
        // These act the same in every state.
        match b {
            CAN | SUB => {
                self.state = State::Ground;
                return;
            }
            ESC if !matches!(self.state, State::Osc | State::Ignored) => {
                self.clear();
                self.state = State::Escape;
                return;
            }
            _ => {}
        }
        match self.state {
            State::Ground => self.ground(b, act),
            State::Escape => self.escape(b, act),
            State::EscapeIntermediate => match b {
                0x00..=0x1F => act(Action::Execute(b)),
                0x20..=0x2F => self.intermediate(b),
                0x30..=0x7E => {
                    self.state = State::Ground;
                    act(Action::Esc {
                        intermediates: core::mem::take(&mut self.intermediates),
                        last: b,
                    });
                }
                _ => {}
            },
            State::CsiEntry | State::CsiParam => self.csi(b, act),
            State::CsiIntermediate => match b {
                0x00..=0x1F => act(Action::Execute(b)),
                0x20..=0x2F => self.intermediate(b),
                0x30..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => self.dispatch(b, act),
                _ => {}
            },
            State::CsiIgnore => match b {
                0x00..=0x1F => act(Action::Execute(b)),
                0x40..=0x7E => self.state = State::Ground,
                _ => {}
            },
            State::Osc => match b {
                BEL => {
                    self.state = State::Ground;
                    act(Action::Osc(core::mem::take(&mut self.osc)));
                }
                ESC => {
                    self.string = State::Osc;
                    self.state = State::StringEscape;
                }
                0x00..=0x1F => {}
                _ if self.osc.len() < OSC_MAX => self.osc.push(b),
                _ => {}
            },
            State::Ignored => match b {
                ESC => {
                    self.string = State::Ignored;
                    self.state = State::StringEscape;
                }
                BEL => self.state = State::Ground,
                _ => {}
            },
            State::StringEscape => {
                // ESC \ ends the string; any other ESC sequence ends it
                // too and starts anew.
                let was = self.string;
                self.state = State::Ground;
                if was == State::Osc {
                    act(Action::Osc(core::mem::take(&mut self.osc)));
                }
                if b != b'\\' {
                    self.clear();
                    self.state = State::Escape;
                    self.byte(b, act);
                }
            }
        }
    }

    fn ground(&mut self, b: u8, act: &mut impl FnMut(Action)) {
        match b {
            0x00..=0x1F => act(Action::Execute(b)),
            DEL => {}
            0x20..=0x7E => act(Action::Print(char::from(b))),
            0xC2..=0xDF => self.start_utf8(b & 0x1F, 1, 0x80),
            0xE0..=0xEF => self.start_utf8(b & 0x0F, 2, 0x800),
            0xF0..=0xF4 => self.start_utf8(b & 0x07, 3, 0x1_0000),
            _ => act(Action::Print(REPLACEMENT)),
        }
    }

    fn start_utf8(&mut self, bits: u8, pending: u8, minimum: u32) {
        self.utf8 = u32::from(bits);
        self.pending = pending;
        self.minimum = minimum;
    }

    fn escape(&mut self, b: u8, act: &mut impl FnMut(Action)) {
        match b {
            0x00..=0x1F => act(Action::Execute(b)),
            b'[' => {
                self.clear();
                self.state = State::CsiEntry;
            }
            b']' => {
                self.osc.clear();
                self.state = State::Osc;
            }
            b'P' | b'X' | b'^' | b'_' => self.state = State::Ignored,
            0x20..=0x2F => {
                self.intermediate(b);
                self.state = State::EscapeIntermediate;
            }
            0x30..=0x7E => {
                self.state = State::Ground;
                act(Action::Esc {
                    intermediates: Vec::new(),
                    last: b,
                });
            }
            _ => {}
        }
    }

    fn intermediate(&mut self, b: u8) {
        if self.intermediates.len() < INTERMEDIATES_MAX {
            self.intermediates.push(b);
        }
    }

    fn csi(&mut self, b: u8, act: &mut impl FnMut(Action)) {
        match b {
            0x00..=0x1F => act(Action::Execute(b)),
            b'0'..=b'9' => {
                self.state = State::CsiParam;
                let digit = u32::from(b - b'0');
                let value = self
                    .current
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit);
                self.current = Some(value.min(65535));
            }
            b';' => {
                self.state = State::CsiParam;
                self.end_param();
                self.in_sub = false;
            }
            b':' => {
                self.state = State::CsiParam;
                self.end_param();
                self.in_sub = true;
            }
            // A private marker comes only first.
            b'<'..=b'?' if self.state == State::CsiEntry => {
                self.intermediate(b);
                self.state = State::CsiParam;
            }
            b'<'..=b'?' => self.state = State::CsiIgnore,
            0x20..=0x2F => {
                self.end_param();
                self.intermediate(b);
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7E => {
                self.end_param();
                self.dispatch(b, act);
            }
            _ => {}
        }
    }

    /// Ends the parameter or subparameter being read.
    fn end_param(&mut self) {
        let value = self.current.take();
        if self.in_sub {
            if let Some(last) = self.sub.last_mut()
                && last.len() < PARAMS_MAX
            {
                last.push(value);
            }
        } else if self.params.len() < PARAMS_MAX {
            self.params.push(value);
            self.sub.push(Vec::new());
        }
    }

    fn dispatch(&mut self, last: u8, act: &mut impl FnMut(Action)) {
        self.state = State::Ground;
        // A sequence without parameters has one missing parameter.
        if self.params == [None] && self.sub.iter().all(Vec::is_empty) {
            self.params.clear();
            self.sub.clear();
        }
        act(Action::Csi {
            intermediates: core::mem::take(&mut self.intermediates),
            params: core::mem::take(&mut self.params),
            sub: core::mem::take(&mut self.sub),
            last,
        });
        self.clear();
    }
}
