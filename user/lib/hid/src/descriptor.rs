//! Parsing report descriptors: their main, global and local items, into
//! the fields of each report and the application collections they
//! belong to.

use alloc::vec::Vec;

/// The usage pages that the mapping looks at.
pub const GENERIC_DESKTOP: u16 = 0x01;
pub const KEYBOARD: u16 = 0x07;
pub const LED: u16 = 0x08;
pub const BUTTON: u16 = 0x09;
pub const CONSUMER: u16 = 0x0C;

/// A usage with its page in the upper 16 bits.
pub const fn usage(page: u16, id: u16) -> u32 {
    (page as u32) << 16 | id as u32
}

/// The bits of a main item's data: constant rather than data, variable
/// rather than array, relative rather than absolute.
const CONSTANT: u32 = 1 << 0;
const VARIABLE: u32 = 1 << 1;
const RELATIVE: u32 = 1 << 2;
/// The collection type of applications.
const APPLICATION: u32 = 0x01;
/// The long item prefix.
const LONG_ITEM: u8 = 0xFE;
/// The deepest push stack and collection nesting accepted.
const STACK_MAX: usize = 16;
const NESTING_MAX: usize = 64;
/// The most bits a report may have: a report fills at most one
/// 1024-byte interrupt transfer.
const REPORT_BITS_MAX: u32 = 1024 * 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// An item runs past the end of the descriptor.
    Truncated,
    /// A pop without a push, or pushes or collections nested too deep.
    Stack,
    /// An end of collection without a collection.
    Collection,
    /// Report ID 0, which is reserved.
    ReportId,
    /// A report longer than `REPORT_BITS_MAX`.
    TooLong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Input,
    Output,
    Feature,
}

/// A run of usages, from `first` to `last`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub first: u32,
    pub last: u32,
}

impl Span {
    fn len(&self) -> u32 {
        self.last.saturating_sub(self.first).saturating_add(1)
    }
}

/// The values of one main item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub kind: Kind,
    /// Its report's ID, 0 when the descriptor has no report IDs.
    pub report_id: u8,
    /// Where its first value starts, in bits after the report ID.
    pub offset: u32,
    /// The bits of each value, and the number of values.
    pub size: u32,
    pub count: u32,
    pub flags: u32,
    pub logical_minimum: i32,
    pub logical_maximum: i32,
    /// For a variable field, the usage of value `n` is the `n`th, or the
    /// last when there are fewer; for an array, value `v` stands for
    /// the usage at `v - logical_minimum`.
    pub usages: Vec<Span>,
    /// The application collection it is in, as an index into
    /// `Descriptor::applications`.
    pub application: Option<usize>,
}

impl Field {
    pub fn constant(&self) -> bool {
        self.flags & CONSTANT != 0
    }

    pub fn variable(&self) -> bool {
        self.flags & VARIABLE != 0
    }

    pub fn relative(&self) -> bool {
        self.flags & RELATIVE != 0
    }

    /// The `n`th of the usages, counting through the spans.
    pub fn nth_usage(&self, mut n: u32) -> Option<u32> {
        for span in &self.usages {
            if n < span.len() {
                return Some(span.first + n);
            }
            n -= span.len();
        }
        None
    }

    /// The usage of value `n` of a variable field.
    pub fn variable_usage(&self, n: u32) -> Option<u32> {
        self.nth_usage(n)
            .or_else(|| self.usages.last().map(|s| s.last))
    }

    /// Value `n` from `report`, which starts after the report ID, sign
    /// extended when the logical minimum is negative; values beyond 32
    /// bits keep their low 32 bits.
    pub fn value(&self, report: &[u8], n: u32) -> Option<i32> {
        let start = self.offset.checked_add(self.size.checked_mul(n)?)?;
        let bits = self.size.min(32);
        let mut raw: u32 = 0;
        for bit in 0..bits {
            let at = start + bit;
            let byte = *report.get(usize::try_from(at / 8).ok()?)?;
            if byte & (1 << (at % 8)) != 0 {
                raw |= 1 << bit;
            }
        }
        if self.logical_minimum < 0 && (1..32).contains(&bits) && raw & (1 << (bits - 1)) != 0 {
            raw |= u32::MAX << bits;
        }
        Some(raw as i32)
    }
}

/// What a report descriptor describes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Descriptor {
    pub fields: Vec<Field>,
    /// The usage of each top-level application collection.
    pub applications: Vec<u32>,
    /// Whether reports start with a report ID byte.
    pub report_ids: bool,
}

impl Descriptor {
    /// The length in bytes of the largest report of `kind`, with its
    /// report ID byte.
    pub fn report_len(&self, kind: Kind) -> usize {
        let bits = self
            .fields
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.offset + f.size * f.count)
            .max()
            .unwrap_or(0);
        bits.div_ceil(8) as usize + usize::from(self.report_ids)
    }

    /// The length in bytes of report `id` of `kind`, with its report ID
    /// byte, or None when there is no such report.
    pub fn report_len_of(&self, kind: Kind, id: u8) -> Option<usize> {
        let bits = self
            .fields
            .iter()
            .filter(|f| f.kind == kind && f.report_id == id)
            .map(|f| f.offset + f.size * f.count)
            .max()?;
        Some(bits.div_ceil(8) as usize + usize::from(self.report_ids))
    }
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_minimum: i32,
    logical_maximum: i32,
    size: u32,
    report_id: u8,
    count: u32,
}

/// Parses `bytes`, a report descriptor.
pub fn parse(bytes: &[u8]) -> Result<Descriptor, Error> {
    let mut descriptor = Descriptor::default();
    let mut globals = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<Span> = Vec::new();
    let mut minimum: Option<u32> = None;
    let mut depth = 0usize;
    let mut application = None;
    // The bits used so far in each report, by kind and report ID.
    let mut used: Vec<(Kind, u8, u32)> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let prefix = bytes[at];
        if prefix == LONG_ITEM {
            let len = *bytes.get(at + 1).ok_or(Error::Truncated)? as usize;
            at += 3 + len;
            if at > bytes.len() {
                return Err(Error::Truncated);
            }
            continue;
        }
        let len = [0, 1, 2, 4][usize::from(prefix & 0b11)];
        let data = bytes.get(at + 1..at + 1 + len).ok_or(Error::Truncated)?;
        at += 1 + len;
        let mut raw = [0u8; 4];
        raw[..len].copy_from_slice(data);
        let unsigned = u32::from_le_bytes(raw);
        let signed = match len {
            1 => i32::from(raw[0] as i8),
            2 => i32::from(i16::from_le_bytes([raw[0], raw[1]])),
            _ => unsigned as i32,
        };
        // A usage given in fewer than four bytes takes the usage page.
        let full = |page: u16| {
            if len == 4 {
                unsigned
            } else {
                usage(page, unsigned as u16)
            }
        };
        match (prefix >> 2 & 0b11, prefix >> 4) {
            // Main items.
            (0, tag @ (8 | 9 | 11)) => {
                let kind = match tag {
                    8 => Kind::Input,
                    9 => Kind::Output,
                    _ => Kind::Feature,
                };
                let entry = match used
                    .iter_mut()
                    .find(|(k, id, _)| *k == kind && *id == globals.report_id)
                {
                    Some(entry) => entry,
                    None => {
                        used.push((kind, globals.report_id, 0));
                        used.last_mut().unwrap()
                    }
                };
                let offset = entry.2;
                let bits = globals
                    .size
                    .checked_mul(globals.count)
                    .ok_or(Error::TooLong)?;
                entry.2 = offset
                    .checked_add(bits)
                    .filter(|&b| b <= REPORT_BITS_MAX)
                    .ok_or(Error::TooLong)?;
                descriptor.fields.push(Field {
                    kind,
                    report_id: globals.report_id,
                    offset,
                    size: globals.size,
                    count: globals.count,
                    flags: unsigned,
                    logical_minimum: globals.logical_minimum,
                    logical_maximum: globals.logical_maximum,
                    usages: core::mem::take(&mut usages),
                    application,
                });
                minimum = None;
            }
            (0, 10) => {
                if depth == NESTING_MAX {
                    return Err(Error::Stack);
                }
                if depth == 0 && unsigned == APPLICATION {
                    let usage = usages.first().map_or(0, |s| s.first);
                    descriptor.applications.push(usage);
                    application = Some(descriptor.applications.len() - 1);
                }
                depth += 1;
                usages.clear();
                minimum = None;
            }
            (0, 12) => {
                depth = depth.checked_sub(1).ok_or(Error::Collection)?;
                if depth == 0 {
                    application = None;
                }
                usages.clear();
                minimum = None;
            }
            // Global items.
            (1, 0) => globals.page = unsigned as u16,
            (1, 1) => globals.logical_minimum = signed,
            (1, 2) => globals.logical_maximum = signed,
            (1, 7) => globals.size = unsigned,
            (1, 8) => {
                globals.report_id = u8::try_from(unsigned)
                    .ok()
                    .filter(|&id| id != 0)
                    .ok_or(Error::ReportId)?;
                descriptor.report_ids = true;
            }
            (1, 9) => globals.count = unsigned,
            (1, 10) => {
                if stack.len() == STACK_MAX {
                    return Err(Error::Stack);
                }
                stack.push(globals);
            }
            (1, 11) => globals = stack.pop().ok_or(Error::Stack)?,
            // Local items.
            (2, 0) => {
                let u = full(globals.page);
                usages.push(Span { first: u, last: u });
            }
            (2, 1) => minimum = Some(full(globals.page)),
            (2, 2) => {
                if let Some(first) = minimum.take() {
                    let last = full(globals.page);
                    if last >= first {
                        usages.push(Span { first, last });
                    }
                }
            }
            _ => {}
        }
        // A logical maximum written as an unsigned value in its bytes,
        // as many devices do with a non-negative minimum.
        if prefix >> 2 & 0b11 == 1 && prefix >> 4 == 2 && globals.logical_minimum >= 0 && signed < 0
        {
            globals.logical_maximum = unsigned as i32;
        }
    }
    Ok(descriptor)
}
