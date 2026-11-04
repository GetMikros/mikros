//! The Rust items of each declaration and their encoding and decoding.

use super::{Gen, camel, doc, ident};
use crate::model::*;
use std::fmt::Write;

impl Gen<'_> {
    pub(super) fn decl(&self, decl: &Decl, out: &mut String) {
        out.push_str(&doc(&decl.doc, ""));
        let name = &decl.name;
        match &decl.kind {
            DeclKind::Const { ty, value } => {
                let (t, v) = match value {
                    Value::Int(v) => (self.rust_type(ty), v.to_string()),
                    Value::Bool(b) => ("bool".to_string(), b.to_string()),
                    Value::Str(s) => ("&str".to_string(), format!("{s:?}")),
                };
                writeln!(out, "pub const {name}: {t} = {v};\n").unwrap();
            }
            DeclKind::Alias(ty) => {
                writeln!(out, "pub type {name} = {};\n", self.rust_type(ty)).unwrap()
            }
            DeclKind::Struct(fields) => self.structure(name, fields, out),
            DeclKind::Enum {
                strictness,
                underlying,
                members,
            } => self.enumeration(name, *strictness, *underlying, members, out),
            DeclKind::Bits {
                strictness,
                underlying,
                members,
            } => self.bits(name, *strictness, *underlying, members, out),
            DeclKind::Union {
                strictness,
                members,
            } => self.union(name, *strictness, members, out),
            DeclKind::Table(members) => self.table(name, members, out),
            DeclKind::Protocol(_) => {}
        }
    }

    /// A structure with `fields`, also used for method arguments and
    /// results.
    pub(super) fn structure(&self, name: &str, fields: &[Field], out: &mut String) {
        let rt = self.rt();
        let (shape, offsets) = self.layout.struct_shape(fields);
        writeln!(out, "#[derive(Debug, PartialEq)]\npub struct {name} {{").unwrap();
        for f in fields {
            out.push_str(&doc(&f.doc, "    "));
            writeln!(
                out,
                "    pub {}: {},",
                ident(&f.name),
                self.rust_type(&f.ty)
            )
            .unwrap();
        }
        let names: Vec<String> = fields.iter().map(|f| ident(&f.name)).collect();
        let mut encode = String::new();
        let mut checks = String::new();
        let mut decode = String::new();
        let mut end = 0;
        for (f, offset) in fields.iter().zip(&offsets) {
            if *offset > end {
                writeln!(checks, "d.zeros(at + {end}, {})?;", offset - end).unwrap();
            }
            end = offset + self.size(&f.ty);
            encode.push_str(&self.encode(&f.ty, &ident(&f.name), &format!("at + {offset}")));
            writeln!(
                decode,
                "{}: {},",
                ident(&f.name),
                self.decode(&f.ty, &format!("at + {offset}"))
            )
            .unwrap();
        }
        if shape.size > end {
            writeln!(checks, "d.zeros(at + {end}, {})?;", shape.size - end).unwrap();
        }
        let size = shape.size;
        let names = names.join(", ");
        writeln!(
            out,
            "}}\n\nimpl {name} {{\n#[doc(hidden)]\npub const __SIZE: usize = {size};\n\n#[doc(hidden)]\npub fn __encode(self, e: &mut {rt}::Encoder, at: usize) -> Result<(), {rt}::Error> {{\nlet {name} {{ {names} }} = self;\n{encode}let _ = (e, at);\nOk(())\n}}\n\n#[doc(hidden)]\npub fn __decode(d: &mut {rt}::Decoder<'_>, at: usize) -> Result<{name}, {rt}::Error> {{\n{checks}let _ = (&d, at);\nOk({name} {{\n{decode}}})\n}}\n}}\n"
        )
        .unwrap();
    }

    fn enumeration(
        &self,
        name: &str,
        strictness: Strictness,
        int: Int,
        members: &[ValueMember],
        out: &mut String,
    ) {
        let rt = self.rt();
        let raw = int.name();
        let flexible = strictness == Strictness::Flexible;
        let unknown = if members.iter().any(|m| camel(&m.name) == "Unknown") {
            "UnknownValue"
        } else {
            "Unknown"
        };
        writeln!(
            out,
            "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum {name} {{"
        )
        .unwrap();
        let (mut from, mut into) = (String::new(), String::new());
        for m in members {
            out.push_str(&doc(&m.doc, "    "));
            let variant = camel(&m.name);
            writeln!(out, "    {variant},").unwrap();
            let value = m.value;
            let wrap = if flexible {
                format!("{name}::{variant}")
            } else {
                format!("Some({name}::{variant})")
            };
            writeln!(from, "{value} => {wrap},").unwrap();
            writeln!(into, "{name}::{variant} => {value},").unwrap();
        }
        if flexible {
            writeln!(
                out,
                "    /// A value this version does not declare.\n    {unknown}({raw}),"
            )
            .unwrap();
            writeln!(from, "other => {name}::{unknown}(other),").unwrap();
            writeln!(into, "{name}::{unknown}(raw) => raw,").unwrap();
        } else {
            from.push_str("_ => None,\n");
        }
        let (ret, wire) = match flexible {
            true => (name.to_string(), format!("Ok({name}::from_raw(raw))")),
            false => (
                format!("Option<{name}>"),
                format!("{name}::from_raw(raw).ok_or({rt}::Error::Invalid)"),
            ),
        };
        writeln!(
            out,
            "}}\n\nimpl {name} {{\npub fn from_raw(raw: {raw}) -> {ret} {{\nmatch raw {{\n{from}}}\n}}\n\npub fn into_raw(self) -> {raw} {{\nmatch self {{\n{into}}}\n}}\n\n#[doc(hidden)]\npub fn __from_wire(raw: {raw}) -> Result<{name}, {rt}::Error> {{\n{wire}\n}}\n}}\n"
        )
        .unwrap();
    }

    fn bits(
        &self,
        name: &str,
        strictness: Strictness,
        int: Int,
        members: &[ValueMember],
        out: &mut String,
    ) {
        let rt = self.rt();
        let raw = int.name();
        let all = members.iter().fold(0, |a, m| a | m.value);
        writeln!(out, "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]\npub struct {name}(pub {raw});\n\nimpl {name} {{").unwrap();
        for m in members {
            out.push_str(&doc(&m.doc, ""));
            writeln!(out, "pub const {}: {name} = {name}({});", m.name, m.value).unwrap();
        }
        let check = match strictness {
            Strictness::Strict => {
                format!("if raw & !{all} != 0 {{\nreturn Err({rt}::Error::Invalid);\n}}\n")
            }
            Strictness::Flexible => String::new(),
        };
        writeln!(
            out,
            "\n/// Whether every bit of `other` is set.\npub fn contains(self, other: {name}) -> bool {{\nself.0 & other.0 == other.0\n}}\n\n#[doc(hidden)]\npub fn __from_wire(raw: {raw}) -> Result<{name}, {rt}::Error> {{\n{check}Ok({name}(raw))\n}}\n}}\n\nimpl core::ops::BitOr for {name} {{\ntype Output = {name};\nfn bitor(self, other: {name}) -> {name} {{\n{name}(self.0 | other.0)\n}}\n}}\n"
        )
        .unwrap();
    }

    fn union(
        &self,
        name: &str,
        strictness: Strictness,
        members: &[OrdinalField],
        out: &mut String,
    ) {
        let rt = self.rt();
        let flexible = strictness == Strictness::Flexible;
        writeln!(out, "#[derive(Debug, PartialEq)]\npub enum {name} {{").unwrap();
        let (mut encode, mut decode) = (String::new(), String::new());
        for m in members {
            out.push_str(&doc(&m.doc, "    "));
            let variant = camel(&m.name);
            writeln!(out, "    {variant}({}),", self.rust_type(&m.ty)).unwrap();
            let size = self.size(&m.ty);
            let ordinal = m.ordinal;
            let enc = self.encode(&m.ty, "v", "o");
            let dec = self.decode(&m.ty, "o");
            writeln!(
                encode,
                "{name}::{variant}(v) => {{\ne.put(at, &{ordinal}u64.to_le_bytes());\nlet o = e.alloc({size});\n{enc}}}"
            )
            .unwrap();
            writeln!(
                decode,
                "{ordinal} => {{\nlet o = d.claim({size})?;\n{name}::{variant}({dec})\n}}"
            )
            .unwrap();
        }
        let unknown = if flexible {
            writeln!(
                out,
                "    /// A member this version does not declare, by its ordinal.\n    Unknown(u64),"
            )
            .unwrap();
            writeln!(
                encode,
                "{name}::Unknown(_) => return Err({rt}::Error::UnknownMember),"
            )
            .unwrap();
            format!(
                "other => {{\nd.skip(bytes, handles)?;\nreturn Ok(Some({name}::Unknown(other)));\n}}"
            )
        } else {
            format!("_ => return Err({rt}::Error::Invalid),")
        };
        writeln!(
            out,
            "}}\n\nimpl {name} {{\n#[doc(hidden)]\npub fn __encode(self, e: &mut {rt}::Encoder, at: usize) -> Result<(), {rt}::Error> {{\nlet start = e.begin_envelope();\nmatch self {{\n{encode}}}\ne.end_envelope(at + 8, start);\nOk(())\n}}\n\n#[doc(hidden)]\npub fn __decode(d: &mut {rt}::Decoder<'_>, at: usize) -> Result<Option<{name}>, {rt}::Error> {{\nlet ordinal = d.u64(at)?;\nlet bytes = d.u32(at + 8)?;\nlet handles = d.u32(at + 12)?;\nif ordinal == 0 {{\nif bytes != 0 || handles != 0 {{\nreturn Err({rt}::Error::Invalid);\n}}\nreturn Ok(None);\n}}\nlet start = d.begin_envelope();\nlet value = match ordinal {{\n{decode}{unknown}\n}};\nd.end_envelope(start, bytes, handles)?;\nOk(Some(value))\n}}\n}}\n"
        )
        .unwrap();
    }

    fn table(&self, name: &str, members: &[OrdinalField], out: &mut String) {
        let rt = self.rt();
        writeln!(
            out,
            "#[derive(Debug, Default, PartialEq)]\npub struct {name} {{"
        )
        .unwrap();
        let (mut count, mut encode, mut decode) = (String::new(), String::new(), String::new());
        for m in members {
            out.push_str(&doc(&m.doc, "    "));
            let field = ident(&m.name);
            writeln!(out, "    pub {field}: Option<{}>,", self.rust_type(&m.ty)).unwrap();
            let (ordinal, size) = (m.ordinal, self.size(&m.ty));
            writeln!(count, "if {field}.is_some() {{\ncount = {ordinal};\n}}").unwrap();
            let enc = self.encode(&m.ty, "v", "o");
            writeln!(
                encode,
                "if let Some(v) = {field} {{\nlet slot = envelopes + {};\nlet start = e.begin_envelope();\nlet o = e.alloc({size});\n{enc}e.end_envelope(slot, start);\ne.present(slot + 8);\n}}",
                (ordinal - 1) * 16
            )
            .unwrap();
            let dec = self.decode(&m.ty, "o");
            writeln!(
                decode,
                "{ordinal} => {{\nlet o = d.claim({size})?;\nout.{field} = Some({dec});\n}}"
            )
            .unwrap();
        }
        let fields: Vec<String> = members.iter().map(|m| ident(&m.name)).collect();
        let fields = fields.join(", ");
        writeln!(
            out,
            "}}\n\nimpl {name} {{\n#[doc(hidden)]\npub fn __encode(self, e: &mut {rt}::Encoder, at: usize) -> Result<(), {rt}::Error> {{\nlet {name} {{ {fields} }} = self;\nlet mut count = 0usize;\n{count}e.vector(at, count, u64::MAX)?;\nlet envelopes = e.alloc(count * 16);\nlet _ = envelopes;\n{encode}Ok(())\n}}\n\n#[doc(hidden)]\npub fn __decode(d: &mut {rt}::Decoder<'_>, at: usize) -> Result<{name}, {rt}::Error> {{\nlet count = d.vector(at, u64::MAX)?.ok_or({rt}::Error::Invalid)?;\nlet envelopes = d.claim(count.checked_mul(16).ok_or({rt}::Error::Invalid)?)?;\nlet mut out = {name}::default();\nfor i in 0..count {{\nlet slot = envelopes + i * 16;\nlet bytes = d.u32(slot)?;\nlet handles = d.u32(slot + 4)?;\nif !d.presence(slot + 8)? {{\nif bytes != 0 || handles != 0 || i + 1 == count {{\nreturn Err({rt}::Error::Invalid);\n}}\ncontinue;\n}}\nlet start = d.begin_envelope();\nmatch i + 1 {{\n{decode}_ => {{\nd.skip(bytes, handles)?;\ncontinue;\n}}\n}}\nd.end_envelope(start, bytes, handles)?;\n}}\nOk(out)\n}}\n}}\n"
        )
        .unwrap();
    }
}
