//! The terminal attributes, with the flag bits, character indexes and
//! speeds of the C library's `termios` definitions, and the defaults a
//! new terminal gets.

/// Indexes of the control characters.
pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;
pub const VSTART: usize = 8;
pub const VSTOP: usize = 9;
pub const VSUSP: usize = 10;
pub const VEOL: usize = 11;
pub const VREPRINT: usize = 12;
pub const VDISCARD: usize = 13;
pub const VWERASE: usize = 14;
pub const VLNEXT: usize = 15;
pub const VEOL2: usize = 16;
pub const NCCS: usize = 32;

/// Input flags.
pub const IGNBRK: u32 = 0o1;
pub const BRKINT: u32 = 0o2;
pub const ISTRIP: u32 = 0o40;
pub const INLCR: u32 = 0o100;
pub const IGNCR: u32 = 0o200;
pub const ICRNL: u32 = 0o400;
pub const IXON: u32 = 0o2000;
pub const IXANY: u32 = 0o4000;
pub const IXOFF: u32 = 0o10000;
pub const IUTF8: u32 = 0o40000;

/// Output flags.
pub const OPOST: u32 = 0o1;
pub const ONLCR: u32 = 0o4;
pub const OCRNL: u32 = 0o10;
pub const ONOCR: u32 = 0o20;
pub const ONLRET: u32 = 0o40;

/// Control flags.
pub const CS8: u32 = 0o60;
pub const CREAD: u32 = 0o200;
pub const HUPCL: u32 = 0o2000;

/// Local flags.
pub const ISIG: u32 = 0o1;
pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ECHOE: u32 = 0o20;
pub const ECHOK: u32 = 0o40;
pub const ECHONL: u32 = 0o100;
pub const NOFLSH: u32 = 0o200;
pub const TOSTOP: u32 = 0o400;
pub const ECHOCTL: u32 = 0o1000;
pub const ECHOKE: u32 = 0o4000;
pub const IEXTEN: u32 = 0o100000;

/// The speed of 38400 baud, which new terminals report.
pub const B38400: u32 = 0o17;

/// A terminal's attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attributes {
    pub input_flags: u32,
    pub output_flags: u32,
    pub control_flags: u32,
    pub local_flags: u32,
    pub control_chars: [u8; NCCS],
    pub input_speed: u32,
    pub output_speed: u32,
}

/// Control-`c`, the byte that Control and the letter `c` make.
const fn ctrl(c: u8) -> u8 {
    c & 0x1F
}

impl Default for Attributes {
    /// Canonical input with echo and signals, CR read as NL, NL written
    /// as CR NL, and the usual control characters.
    fn default() -> Attributes {
        let mut cc = [0u8; NCCS];
        cc[VINTR] = ctrl(b'c');
        cc[VQUIT] = ctrl(b'\\');
        cc[VERASE] = 0x7F;
        cc[VKILL] = ctrl(b'u');
        cc[VEOF] = ctrl(b'd');
        cc[VTIME] = 0;
        cc[VMIN] = 1;
        cc[VSTART] = ctrl(b'q');
        cc[VSTOP] = ctrl(b's');
        cc[VSUSP] = ctrl(b'z');
        cc[VREPRINT] = ctrl(b'r');
        cc[VDISCARD] = ctrl(b'o');
        cc[VWERASE] = ctrl(b'w');
        cc[VLNEXT] = ctrl(b'v');
        Attributes {
            input_flags: ICRNL | IXON,
            output_flags: OPOST | ONLCR,
            control_flags: B38400 | CS8 | CREAD | HUPCL,
            local_flags: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
            control_chars: cc,
            input_speed: B38400,
            output_speed: B38400,
        }
    }
}

impl Attributes {
    pub fn local(&self, flag: u32) -> bool {
        self.local_flags & flag != 0
    }

    pub fn input(&self, flag: u32) -> bool {
        self.input_flags & flag != 0
    }

    pub fn output(&self, flag: u32) -> bool {
        self.output_flags & flag != 0
    }

    /// Whether `byte` is the control character at `index`. A control
    /// character of 0 is disabled.
    pub fn is(&self, index: usize, byte: u8) -> bool {
        let c = self.control_chars[index];
        c != 0 && c == byte
    }
}
