//! The Wayland wire format. A message is a header of two 32-bit words in
//! the host's byte order, the ID of the object it is sent to or from and
//! then its size in bytes in the high half and its opcode in the low half,
//! followed by its arguments, each a whole number of words. On Mikros a
//! channel message holds one or more whole Wayland messages and the
//! handles of their file descriptor arguments, in order.

use std::collections::VecDeque;

/// The most bytes and handles that one channel message of a client
/// holds, as `libwayland.dlib` packs them.
pub const CHANNEL_BYTES: usize = 65536;
pub const CHANNEL_HANDLES: usize = 64;

/// The size of a message's header.
pub const HEADER: usize = 8;

/// The ID of an object of a client.
pub type ObjectId = u32;

/// The ID of an object that a message creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewId(pub u32);

/// A signed 24.8 fixed-point number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fixed(pub i32);

impl Fixed {
    pub fn from_f64(value: f64) -> Fixed {
        Fixed((value * 256.0).round() as i32)
    }

    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / 256.0
    }
}

/// A file descriptor argument, which a channel carries as a kernel
/// handle.
#[cfg(target_os = "mikros")]
pub type Fd = std::os::mikros::io::OwnedHandle;

/// A file descriptor argument: in the host's tests, a number that stands
/// for a handle.
#[cfg(not(target_os = "mikros"))]
#[derive(Debug, PartialEq, Eq)]
pub struct Fd(pub u32);

/// The type of an argument in a message's signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgType {
    Int,
    Uint,
    Fixed,
    String {
        nullable: bool,
    },
    Object {
        nullable: bool,
    },
    /// The ID of a new object of `interface`, or, without one, of the
    /// interface and version that the message names before the ID.
    NewId {
        interface: Option<&'static str>,
    },
    Array,
    Fd,
}

/// A request or an event of an interface.
#[derive(Debug)]
pub struct Message {
    pub name: &'static str,
    /// The version of the interface that has it first.
    pub since: u32,
    /// Whether it destroys the object.
    pub destructor: bool,
    pub args: &'static [ArgType],
}

/// An interface, with its requests and events by opcode.
#[derive(Debug)]
pub struct Interface {
    pub name: &'static str,
    pub version: u32,
    pub requests: &'static [Message],
    pub events: &'static [Message],
}

/// An argument of a message.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum Arg {
    Int(i32),
    Uint(u32),
    Fixed(Fixed),
    String(Option<String>),
    Object(Option<ObjectId>),
    NewId(NewId),
    /// A new object of the interface and version that the message names.
    Untyped {
        interface: String,
        version: u32,
        id: NewId,
    },
    Array(Vec<u8>),
    Fd(Fd),
}

/// What is wrong with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Its size is less than its header or not a whole number of words,
    /// or more than the bytes that hold it.
    Size,
    /// Its arguments end before its signature does, or go on after it.
    Length,
    /// A string without its terminating zero, or not UTF-8.
    String,
    /// A null object or string where the signature allows none, or a new
    /// ID of 0.
    Null,
    /// A file descriptor argument without a handle left for it.
    MissingFd,
    /// An opcode that the interface does not have.
    Opcode,
}

/// The header of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub sender: ObjectId,
    pub opcode: u16,
    pub size: usize,
}

fn word_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

/// Splits the message at the start of `bytes`, which must hold it whole,
/// into its header and its arguments, and returns them with the bytes
/// after it.
pub fn split(bytes: &[u8]) -> Result<(Header, &[u8], &[u8]), DecodeError> {
    if bytes.len() < HEADER {
        return Err(DecodeError::Size);
    }
    let second = word_at(bytes, 4);
    let header = Header {
        sender: word_at(bytes, 0),
        opcode: second as u16,
        size: (second >> 16) as usize,
    };
    if header.size < HEADER || header.size % 4 != 0 || header.size > bytes.len() {
        return Err(DecodeError::Size);
    }
    let (message, rest) = bytes.split_at(header.size);
    Ok((header, &message[HEADER..], rest))
}

/// The arguments that a message's bytes hold.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn word(&mut self) -> Result<u32, DecodeError> {
        if self.bytes.len() < 4 {
            return Err(DecodeError::Length);
        }
        let word = word_at(self.bytes, 0);
        self.bytes = &self.bytes[4..];
        Ok(word)
    }

    /// The `len` bytes after a length word, which take a whole number of
    /// words.
    fn padded(&mut self, len: usize) -> Result<&[u8], DecodeError> {
        let words = len.div_ceil(4) * 4;
        if self.bytes.len() < words {
            return Err(DecodeError::Length);
        }
        let (taken, rest) = self.bytes.split_at(words);
        self.bytes = rest;
        Ok(&taken[..len])
    }

    fn string(&mut self) -> Result<Option<String>, DecodeError> {
        let len = self.word()? as usize;
        if len == 0 {
            return Ok(None);
        }
        let bytes = self.padded(len)?;
        let (text, zero) = bytes.split_at(len - 1);
        if zero != [0] {
            return Err(DecodeError::String);
        }
        let text = std::str::from_utf8(text).map_err(|_| DecodeError::String)?;
        Ok(Some(text.to_owned()))
    }

    fn new_id(&mut self) -> Result<NewId, DecodeError> {
        match self.word()? {
            0 => Err(DecodeError::Null),
            id => Ok(NewId(id)),
        }
    }
}

/// The arguments of `message` in `body`, the bytes after its header,
/// taking the handles of its file descriptor arguments from `fds`.
pub fn decode(
    message: &Message,
    body: &[u8],
    fds: &mut VecDeque<Fd>,
) -> Result<Vec<Arg>, DecodeError> {
    let mut reader = Reader { bytes: body };
    let mut args = Vec::with_capacity(message.args.len());
    for &ty in message.args {
        let arg = match ty {
            ArgType::Int => Arg::Int(reader.word()? as i32),
            ArgType::Uint => Arg::Uint(reader.word()?),
            ArgType::Fixed => Arg::Fixed(Fixed(reader.word()? as i32)),
            ArgType::String { nullable } => match reader.string()? {
                None if !nullable => return Err(DecodeError::Null),
                text => Arg::String(text),
            },
            ArgType::Object { nullable } => match reader.word()? {
                0 if !nullable => return Err(DecodeError::Null),
                0 => Arg::Object(None),
                id => Arg::Object(Some(id)),
            },
            ArgType::NewId { interface: Some(_) } => Arg::NewId(reader.new_id()?),
            ArgType::NewId { interface: None } => Arg::Untyped {
                interface: reader.string()?.ok_or(DecodeError::Null)?,
                version: reader.word()?,
                id: reader.new_id()?,
            },
            ArgType::Array => {
                let len = reader.word()? as usize;
                Arg::Array(reader.padded(len)?.to_vec())
            }
            ArgType::Fd => Arg::Fd(fds.pop_front().ok_or(DecodeError::MissingFd)?),
        };
        args.push(arg);
    }
    if !reader.bytes.is_empty() {
        return Err(DecodeError::Length);
    }
    Ok(args)
}

/// Messages to send, with where each ends, so that they can be sent in
/// channel messages that each hold whole ones.
#[derive(Debug, Default)]
pub struct Outgoing {
    bytes: Vec<u8>,
    fds: Vec<Fd>,
    /// The end of each message in `bytes` and in `fds`.
    ends: VecDeque<(usize, usize)>,
}

impl Outgoing {
    fn word(&mut self, word: u32) {
        self.bytes.extend_from_slice(&word.to_ne_bytes());
    }

    /// `bytes` after their length `len`, padded to a whole number of words.
    fn padded(&mut self, len: usize, bytes: &[u8]) {
        self.word(len as u32);
        self.bytes.extend_from_slice(bytes);
        let pad = len.div_ceil(4) * 4 - bytes.len();
        self.bytes.extend(std::iter::repeat_n(0, pad));
    }

    fn string(&mut self, text: Option<&str>) {
        match text {
            None => self.word(0),
            Some(text) => {
                let mut bytes = text.as_bytes().to_vec();
                bytes.push(0);
                self.padded(bytes.len(), &bytes);
            }
        }
    }

    /// Adds the message `opcode` of the object `sender` with `args`.
    pub fn send(&mut self, sender: ObjectId, opcode: u16, args: Vec<Arg>) {
        let start = self.bytes.len();
        self.word(sender);
        self.word(0);
        for arg in args {
            match arg {
                Arg::Int(value) => self.word(value as u32),
                Arg::Uint(value) => self.word(value),
                Arg::Fixed(value) => self.word(value.0 as u32),
                Arg::String(text) => self.string(text.as_deref()),
                Arg::Object(id) => self.word(id.unwrap_or(0)),
                Arg::NewId(id) => self.word(id.0),
                Arg::Untyped {
                    interface,
                    version,
                    id,
                } => {
                    self.string(Some(&interface));
                    self.word(version);
                    self.word(id.0);
                }
                Arg::Array(bytes) => self.padded(bytes.len(), &bytes),
                Arg::Fd(fd) => self.fds.push(fd),
            }
        }
        let size = self.bytes.len() - start;
        assert!(
            size <= usize::from(u16::MAX),
            "a message too large for its header"
        );
        let second = (size as u32) << 16 | u32::from(opcode);
        self.bytes[start + 4..start + 8].copy_from_slice(&second.to_ne_bytes());
        self.ends.push_back((self.bytes.len(), self.fds.len()));
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Takes the oldest messages, as many as fit in one channel message,
    /// but always at least one, with the handles of their file
    /// descriptors.
    pub fn take(&mut self) -> Option<(Vec<u8>, Vec<Fd>)> {
        let mut end = *self.ends.front()?;
        let mut count = 1;
        for &(bytes, fds) in self.ends.iter().skip(1) {
            if bytes > CHANNEL_BYTES || fds > CHANNEL_HANDLES {
                break;
            }
            (end, count) = ((bytes, fds), count + 1);
        }
        self.ends.drain(..count);
        for at in self.ends.iter_mut() {
            *at = (at.0 - end.0, at.1 - end.1);
        }
        let bytes = self.bytes.drain(..end.0).collect();
        let fds = self.fds.drain(..end.1).collect();
        Some((bytes, fds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: [ArgType; 10] = [
        ArgType::Int,
        ArgType::Uint,
        ArgType::Fixed,
        ArgType::String { nullable: false },
        ArgType::String { nullable: true },
        ArgType::Object { nullable: true },
        ArgType::NewId {
            interface: Some("wl_callback"),
        },
        ArgType::NewId { interface: None },
        ArgType::Array,
        ArgType::Fd,
    ];
    const MESSAGE: Message = Message {
        name: "every",
        since: 1,
        destructor: false,
        args: &EVERY,
    };

    fn every() -> Vec<Arg> {
        vec![
            Arg::Int(-5),
            Arg::Uint(7),
            Arg::Fixed(Fixed::from_f64(-1.5)),
            Arg::String(Some("wl_seat".into())),
            Arg::String(None),
            Arg::Object(Some(12)),
            Arg::NewId(NewId(13)),
            Arg::Untyped {
                interface: "wl_output".into(),
                version: 4,
                id: NewId(14),
            },
            Arg::Array(vec![1, 2, 3, 4, 5]),
            Arg::Fd(Fd(99)),
        ]
    }

    #[test]
    fn decodes_what_it_encodes() {
        let mut out = Outgoing::default();
        out.send(3, 2, every());
        let (bytes, fds) = out.take().expect("one message");
        assert!(out.is_empty());
        let (header, body, rest) = split(&bytes).expect("a whole message");
        assert_eq!(
            header,
            Header {
                sender: 3,
                opcode: 2,
                size: bytes.len()
            }
        );
        assert!(rest.is_empty());
        let mut fds = VecDeque::from(fds);
        assert_eq!(decode(&MESSAGE, body, &mut fds), Ok(every()));
        assert!(fds.is_empty());
    }

    #[test]
    fn pads_strings_and_arrays_to_words() {
        let mut out = Outgoing::default();
        out.send(
            1,
            0,
            vec![Arg::String(Some("abc".into())), Arg::Array(vec![9])],
        );
        let (bytes, _) = out.take().unwrap();
        // The header, the length 4 and "abc\0", the length 1 and one
        // padded byte.
        assert_eq!(bytes.len(), 8 + 4 + 4 + 4 + 4);
        assert_eq!(&bytes[8..16], &[4, 0, 0, 0, b'a', b'b', b'c', 0]);
        assert_eq!(&bytes[16..24], &[1, 0, 0, 0, 9, 0, 0, 0]);
        assert_eq!(Fixed(384).to_f64(), 1.5);
    }

    fn encoded(args: Vec<Arg>) -> Vec<u8> {
        let mut out = Outgoing::default();
        out.send(1, 0, args);
        out.take().unwrap().0
    }

    fn decoded(types: &[ArgType], bytes: &[u8]) -> Result<Vec<Arg>, DecodeError> {
        let message = Message {
            name: "test",
            since: 1,
            destructor: false,
            args: Box::leak(types.into()),
        };
        let (_, body, _) = split(bytes)?;
        decode(&message, body, &mut VecDeque::new())
    }

    #[test]
    fn refuses_nulls_where_the_signature_allows_none() {
        let null = encoded(vec![Arg::Object(None)]);
        assert_eq!(
            decoded(&[ArgType::Object { nullable: false }], &null),
            Err(DecodeError::Null)
        );
        let id = ArgType::NewId {
            interface: Some("wl_callback"),
        };
        assert_eq!(decoded(&[id], &null), Err(DecodeError::Null));
        let null = encoded(vec![Arg::String(None)]);
        assert_eq!(
            decoded(&[ArgType::String { nullable: false }], &null),
            Err(DecodeError::Null)
        );
    }

    #[test]
    fn refuses_messages_that_do_not_match_their_signature() {
        let one = encoded(vec![Arg::Uint(1)]);
        assert_eq!(
            decoded(&[ArgType::Uint, ArgType::Uint], &one),
            Err(DecodeError::Length)
        );
        assert_eq!(decoded(&[], &one), Err(DecodeError::Length));
        assert_eq!(
            decoded(&[ArgType::Fd], &encoded(vec![])),
            Err(DecodeError::MissingFd)
        );
        // A string whose last byte is not zero, and one that is not UTF-8.
        let mut bad = encoded(vec![Arg::String(Some("abc".into()))]);
        bad[15] = b'd';
        let string = [ArgType::String { nullable: false }];
        assert_eq!(decoded(&string, &bad), Err(DecodeError::String));
        let invalid = encoded(vec![Arg::String(Some("\u{e9}".into()))]);
        let mut invalid = invalid.clone();
        invalid[12] = 0xff;
        assert_eq!(decoded(&string, &invalid), Err(DecodeError::String));
    }

    #[test]
    fn refuses_sizes_that_do_not_fit() {
        let mut bytes = encoded(vec![Arg::Uint(1)]);
        assert!(split(&bytes[..10]).is_err());
        bytes[6] = 13;
        assert_eq!(split(&bytes).map(|_| ()), Err(DecodeError::Size));
        bytes[6] = 4;
        assert_eq!(split(&bytes).map(|_| ()), Err(DecodeError::Size));
        bytes[6] = 16;
        assert_eq!(split(&bytes).map(|_| ()), Err(DecodeError::Size));
    }

    #[test]
    fn packs_whole_messages_into_channel_messages() {
        let mut out = Outgoing::default();
        // Messages of 4096 bytes: 16 fit in one channel message.
        for i in 0..20 {
            out.send(i, 0, vec![Arg::Array(vec![0; 4084])]);
        }
        let (first, _) = out.take().unwrap();
        assert_eq!(first.len(), 16 * 4096);
        let (second, _) = out.take().unwrap();
        assert_eq!(second.len(), 4 * 4096);
        assert_eq!(split(&second).unwrap().0.sender, 16);
        assert!(out.take().is_none());
        // And no more handles than one holds.
        for i in 0..70 {
            out.send(i, 0, vec![Arg::Fd(Fd(i))]);
        }
        let (bytes, fds) = out.take().unwrap();
        assert_eq!((bytes.len(), fds.len()), (64 * 8, 64));
        let (_, fds) = out.take().unwrap();
        assert_eq!(fds, (64..70).map(Fd).collect::<Vec<_>>());
    }
}
