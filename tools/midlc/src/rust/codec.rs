//! The Rust type of each MIDL type, and the code that encodes and
//! decodes one value of it at an offset.

use super::Gen;
use crate::model::*;

impl Gen<'_> {
    /// The Rust type that holds a value of `ty`.
    pub(super) fn rust_type(&self, ty: &Type) -> String {
        let rt = self.rt();
        let opt = |t: String, optional: bool| if optional { format!("Option<{t}>") } else { t };
        match ty {
            Type::Bool => "bool".into(),
            Type::Int(int) => int.name().into(),
            Type::F32 => "f32".into(),
            Type::F64 => "f64".into(),
            Type::String { optional, .. } => opt("::alloc::string::String".into(), *optional),
            Type::Vector {
                element, optional, ..
            } => opt(
                format!("::alloc::vec::Vec<{}>", self.rust_type(element)),
                *optional,
            ),
            Type::Array { element, len } => format!("[{}; {len}]", self.rust_type(element)),
            Type::Handle { optional, .. } => opt(format!("{rt}::OwnedHandle"), *optional),
            Type::Endpoint {
                client,
                protocol,
                optional,
            } => {
                let end = if *client { "ClientEnd" } else { "ServerEnd" };
                opt(format!("{rt}::{end}<{}>", self.path(protocol)), *optional)
            }
            Type::Named { name, optional } => {
                let path = self.path(name);
                match self.decl_of(name) {
                    DeclKind::Struct(_) if *optional => {
                        format!("Option<::alloc::boxed::Box<{path}>>")
                    }
                    _ => opt(path, *optional),
                }
            }
        }
    }

    /// Statements that encode `value`, an expression of `ty` that they
    /// move, with its inline part at `at`, through encoder `e`.
    pub(super) fn encode(&self, ty: &Type, value: &str, at: &str) -> String {
        let le = |v: &str| format!("e.put({at}, &({v}).to_le_bytes());\n");
        match ty {
            Type::Bool => format!("e.put({at}, &[u8::from({value})]);\n"),
            Type::Int(_) | Type::F32 | Type::F64 => le(value),
            Type::String {
                max,
                optional: false,
            } => format!("e.string({at}, &{value}, {max})?;\n"),
            Type::Vector {
                element,
                max,
                optional: false,
            } => {
                let (v, base, i, x) = (
                    self.fresh("v"),
                    self.fresh("base"),
                    self.fresh("i"),
                    self.fresh("x"),
                );
                let size = self.size(element);
                let inner = self.encode(element, &x, &format!("{base} + {i} * {size}"));
                format!(
                    "let {v} = {value};\ne.vector({at}, {v}.len(), {max})?;\nlet {base} = e.alloc({v}.len() * {size});\nfor ({i}, {x}) in {v}.into_iter().enumerate() {{\n{inner}}}\n"
                )
            }
            Type::Array { element, .. } => {
                let (i, x) = (self.fresh("i"), self.fresh("x"));
                let size = self.size(element);
                let inner = self.encode(element, &x, &format!("{at} + {i} * {size}"));
                format!("for ({i}, {x}) in {value}.into_iter().enumerate() {{\n{inner}}}\n")
            }
            Type::Handle {
                optional: false, ..
            } => format!("e.handle({at}, {value});\n"),
            Type::Endpoint {
                optional: false, ..
            } => format!("e.handle({at}, {value}.into_handle());\n"),
            Type::Named { name, optional } => match self.decl_of(name) {
                DeclKind::Struct(_) if *optional => {
                    let (s, o) = (self.fresh("s"), self.fresh("o"));
                    let size = self.size(&Type::Named {
                        name: name.clone(),
                        optional: false,
                    });
                    format!(
                        "if let Some({s}) = {value} {{\ne.present({at});\nlet {o} = e.alloc({size});\n(*{s}).__encode(e, {o})?;\n}}\n"
                    )
                }
                DeclKind::Union { .. } if *optional => {
                    let s = self.fresh("s");
                    format!("if let Some({s}) = {value} {{\n{s}.__encode(e, {at})?;\n}}\n")
                }
                DeclKind::Enum { .. } => le(&format!("{value}.into_raw()")),
                DeclKind::Bits { .. } => le(&format!("{value}.0")),
                _ => format!("{value}.__encode(e, {at})?;\n"),
            },
            // Optional strings, vectors, handles and endpoints: an absent
            // value leaves its zeroed marker.
            _ => {
                let s = self.fresh("s");
                let inner = self.encode(&required(ty), &s, at);
                format!("if let Some({s}) = {value} {{\n{inner}}}\n")
            }
        }
    }

    /// An expression that decodes a value of `ty` whose inline part is
    /// at `at`, through decoder `d`, in a function that returns
    /// `Result<_, Error>`.
    pub(super) fn decode(&self, ty: &Type, at: &str) -> String {
        let rt = self.rt();
        let get = |t: &str, n: u64| format!("{t}::from_le_bytes(d.get::<{n}>({at})?)");
        let need = |e: String, optional: bool| {
            if optional {
                e
            } else {
                format!("{e}.ok_or({rt}::Error::Invalid)?")
            }
        };
        match ty {
            Type::Bool => format!("d.bool({at})?"),
            Type::Int(int) => get(int.name(), int.size()),
            Type::F32 => get("f32", 4),
            Type::F64 => get("f64", 8),
            Type::String { max, optional } => need(format!("d.string({at}, {max})?"), *optional),
            Type::Vector {
                element,
                max,
                optional,
            } => {
                let (n, base, v, i) = (
                    self.fresh("n"),
                    self.fresh("base"),
                    self.fresh("v"),
                    self.fresh("i"),
                );
                let size = self.size(element);
                let inner = self.decode(element, &format!("{base} + {i} * {size}"));
                let body = format!(
                    "{{\nlet {base} = d.claim({n}.checked_mul({size}).ok_or({rt}::Error::Invalid)?)?;\nlet mut {v} = ::alloc::vec::Vec::new();\n{v}.try_reserve_exact({n}).map_err(|_| {rt}::Error::Invalid)?;\nfor {i} in 0..{n} {{\n{v}.push({inner});\n}}\n{v}\n}}"
                );
                match optional {
                    true => format!(
                        "match d.vector({at}, {max})? {{\nNone => None,\nSome({n}) => Some({body}),\n}}"
                    ),
                    false => format!(
                        "{{\nlet {n} = d.vector({at}, {max})?.ok_or({rt}::Error::Invalid)?;\n{body}\n}}"
                    ),
                }
            }
            Type::Array { element, len } => {
                let (v, i) = (self.fresh("v"), self.fresh("i"));
                let size = self.size(element);
                let inner = self.decode(element, &format!("{at} + {i} * {size}"));
                format!(
                    "{{\nlet mut {v} = ::alloc::vec::Vec::with_capacity({len});\nfor {i} in 0..{len} {{\n{v}.push({inner});\n}}\nmatch {v}.try_into() {{\nOk(a) => a,\nErr(_) => unreachable!(\"the array has its length\"),\n}}\n}}"
                )
            }
            Type::Handle {
                object,
                rights,
                optional,
            } => {
                let opt = |v: &Option<u32>| v.map_or("None".into(), |v| format!("Some({v:#x})"));
                need(
                    format!("d.handle({at}, {}, {})?", opt(object), opt(rights)),
                    *optional,
                )
            }
            Type::Endpoint {
                client, optional, ..
            } => {
                let end = if *client { "ClientEnd" } else { "ServerEnd" };
                // Endpoints are channels.
                let handle = format!("d.handle({at}, Some(4), None)?.map({rt}::{end}::new)");
                need(handle, *optional)
            }
            Type::Named { name, optional } => {
                let path = self.path(name);
                match self.decl_of(name) {
                    DeclKind::Struct(_) if *optional => {
                        let size = self.size(&Type::Named {
                            name: name.clone(),
                            optional: false,
                        });
                        format!(
                            "match d.presence({at})? {{\nfalse => None,\ntrue => {{\nlet o = d.claim({size})?;\nSome(::alloc::boxed::Box::new({path}::__decode(d, o)?))\n}}\n}}"
                        )
                    }
                    DeclKind::Union { .. } => {
                        need(format!("{path}::__decode(d, {at})?"), *optional)
                    }
                    DeclKind::Enum { underlying, .. } | DeclKind::Bits { underlying, .. } => {
                        format!(
                            "{path}::__from_wire({})?",
                            get(underlying.name(), underlying.size())
                        )
                    }
                    _ => format!("{path}::__decode(d, {at})?"),
                }
            }
        }
    }
}

/// `ty` without its `?`.
fn required(ty: &Type) -> Type {
    let mut ty = ty.clone();
    match &mut ty {
        Type::String { optional, .. }
        | Type::Vector { optional, .. }
        | Type::Handle { optional, .. }
        | Type::Endpoint { optional, .. }
        | Type::Named { optional, .. } => *optional = false,
        _ => {}
    }
    ty
}
