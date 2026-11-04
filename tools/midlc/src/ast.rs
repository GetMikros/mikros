//! The syntax tree of a MIDL file, as parsed and before names are
//! resolved.

use crate::Span;

/// A dotted name, such as `mikros.base.Status`.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub parts: Vec<String>,
    pub span: Span,
}

impl Path {
    pub fn text(&self) -> String {
        self.parts.join(".")
    }
}

/// The `@available` attribute.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Available {
    pub added: Option<u64>,
    pub deprecated: Option<u64>,
    pub removed: Option<u64>,
}

/// What precedes a declaration or member: documentation lines and
/// availability.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attributes {
    pub doc: Vec<String>,
    pub available: Option<Available>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub attrs: Attributes,
    pub library: Path,
    pub usings: Vec<Path>,
    pub decls: Vec<Decl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    pub attrs: Attributes,
    pub name: String,
    pub span: Span,
    pub kind: DeclKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Strictness {
    Strict,
    Flexible,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeclKind {
    Const {
        ty: TypeRef,
        value: Constant,
    },
    Alias(TypeRef),
    Struct(Vec<Member>),
    Enum {
        strictness: Strictness,
        underlying: TypeRef,
        members: Vec<ValueMember>,
    },
    Bits {
        strictness: Strictness,
        underlying: TypeRef,
        members: Vec<ValueMember>,
    },
    Union {
        strictness: Strictness,
        members: Vec<OrdinalMember>,
    },
    Table(Vec<OrdinalMember>),
    Protocol(Vec<Method>),
}

/// A structure member, method argument or method result.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub attrs: Attributes,
    pub name: String,
    pub span: Span,
    pub ty: TypeRef,
}

/// An enumeration or bit set member.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueMember {
    pub attrs: Attributes,
    pub name: String,
    pub span: Span,
    pub value: Constant,
}

/// A union or table member.
#[derive(Clone, Debug, PartialEq)]
pub struct OrdinalMember {
    pub attrs: Attributes,
    pub ordinal: u64,
    pub name: String,
    pub span: Span,
    pub ty: TypeRef,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MethodKind {
    TwoWay {
        request: Vec<Member>,
        response: Vec<Member>,
        error: Option<TypeRef>,
    },
    OneWay(Vec<Member>),
    Event(Vec<Member>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Method {
    pub attrs: Attributes,
    pub strictness: Strictness,
    pub ordinal: u64,
    pub name: String,
    pub span: Span,
    pub kind: MethodKind,
}

/// A type as written: a name with optional parameters and `?`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeRef {
    pub name: Path,
    pub params: Vec<Param>,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Int(u64, Span),
    Type(TypeRef),
    /// Names joined by `|`, such as rights.
    Union(Vec<Path>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// An integer literal, negative when `negative` is set.
    Int {
        value: u64,
        negative: bool,
        span: Span,
    },
    Bool(bool, Span),
    Str(String, Span),
    /// Names of constants or members joined by `|`.
    Names(Vec<Path>),
}
