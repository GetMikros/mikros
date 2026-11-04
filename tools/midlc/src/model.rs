//! The checked form of a set of MIDL libraries: every name resolved,
//! aliases expanded and only the elements available at the chosen
//! version kept.

pub use crate::ast::Strictness;

/// The integer primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Int {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl Int {
    pub fn size(self) -> u64 {
        match self {
            Int::I8 | Int::U8 => 1,
            Int::I16 | Int::U16 => 2,
            Int::I32 | Int::U32 => 4,
            Int::I64 | Int::U64 => 8,
        }
    }

    pub fn signed(self) -> bool {
        matches!(self, Int::I8 | Int::I16 | Int::I32 | Int::I64)
    }

    /// The smallest and largest value of the type.
    pub fn range(self) -> (i128, i128) {
        let bits = 8 * self.size() as u32;
        if self.signed() {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (0, (1i128 << bits) - 1)
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Int::I8 => "i8",
            Int::I16 => "i16",
            Int::I32 => "i32",
            Int::I64 => "i64",
            Int::U8 => "u8",
            Int::U16 => "u16",
            Int::U32 => "u32",
            Int::U64 => "u64",
        }
    }

    pub fn from_name(name: &str) -> Option<Int> {
        Some(match name {
            "i8" => Int::I8,
            "i16" => Int::I16,
            "i32" => Int::I32,
            "i64" => Int::I64,
            "u8" => Int::U8,
            "u16" => Int::U16,
            "u32" => Int::U32,
            "u64" => Int::U64,
            _ => return None,
        })
    }
}

/// A resolved type.
#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Bool,
    Int(Int),
    F32,
    F64,
    String {
        max: u64,
        optional: bool,
    },
    Vector {
        element: Box<Type>,
        max: u64,
        optional: bool,
    },
    Array {
        element: Box<Type>,
        len: u64,
    },
    /// A handle with the object type and rights it must have, if any.
    Handle {
        object: Option<u32>,
        rights: Option<u32>,
        optional: bool,
    },
    Endpoint {
        client: bool,
        protocol: String,
        optional: bool,
    },
    /// A structure, enumeration, bit set, union or table, by its full
    /// name, such as `mikros.base.Status`.
    Named {
        name: String,
        optional: bool,
    },
}

/// The value of a constant or member.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i128),
    Bool(bool),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub doc: Vec<String>,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValueMember {
    pub name: String,
    pub doc: Vec<String>,
    pub value: i128,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrdinalField {
    pub ordinal: u64,
    pub name: String,
    pub doc: Vec<String>,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MethodKind {
    TwoWay {
        request: Vec<Field>,
        response: Vec<Field>,
        error: Option<Type>,
    },
    OneWay(Vec<Field>),
    Event(Vec<Field>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Method {
    pub name: String,
    pub doc: Vec<String>,
    pub ordinal: u64,
    pub strictness: Strictness,
    pub kind: MethodKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeclKind {
    Const {
        ty: Type,
        value: Value,
    },
    Alias(Type),
    Struct(Vec<Field>),
    Enum {
        strictness: Strictness,
        underlying: Int,
        members: Vec<ValueMember>,
    },
    Bits {
        strictness: Strictness,
        underlying: Int,
        members: Vec<ValueMember>,
    },
    Union {
        strictness: Strictness,
        members: Vec<OrdinalField>,
    },
    Table(Vec<OrdinalField>),
    Protocol(Vec<Method>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    pub name: String,
    pub doc: Vec<String>,
    pub kind: DeclKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    pub name: String,
    pub doc: Vec<String>,
    pub decls: Vec<Decl>,
}

/// Checked libraries, each after the libraries it uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub libraries: Vec<Library>,
}

impl Model {
    /// The declaration with full name `name`.
    pub fn decl(&self, name: &str) -> Option<&Decl> {
        let (library, decl) = name.rsplit_once('.')?;
        let library = self.libraries.iter().find(|l| l.name == library)?;
        library.decls.iter().find(|d| d.name == decl)
    }
}

/// The kernel object types that `handle<Type>` can name, with their
/// numbers.
pub const OBJECT_TYPES: [(&str, u32); 17] = [
    ("Process", 1),
    ("Thread", 2),
    ("MemoryObject", 3),
    ("Channel", 4),
    ("Event", 5),
    ("Port", 6),
    ("Timer", 7),
    ("Interrupt", 8),
    ("Resource", 9),
    ("Log", 10),
    ("Budget", 11),
    ("LogReader", 12),
    ("Pager", 13),
    ("Timeline", 14),
    ("Stream", 15),
    ("Bti", 16),
    ("Pmt", 17),
];

/// The handle rights that `handle<Type, RIGHTS>` can name, with their
/// bits.
pub const RIGHTS: [(&str, u32); 10] = [
    ("DUPLICATE", 1 << 0),
    ("TRANSFER", 1 << 1),
    ("READ", 1 << 2),
    ("WRITE", 1 << 3),
    ("EXECUTE", 1 << 4),
    ("MAP", 1 << 5),
    ("WAIT", 1 << 6),
    ("SIGNAL", 1 << 7),
    ("MANAGE", 1 << 8),
    ("INSPECT", 1 << 9),
];
