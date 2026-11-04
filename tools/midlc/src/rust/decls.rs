//! The Rust items of each declaration and their encoding and decoding.

use super::{Gen, doc, ident};
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
            DeclKind::Enum { .. }
            | DeclKind::Bits { .. }
            | DeclKind::Union { .. }
            | DeclKind::Table(_)
            | DeclKind::Protocol(_) => {}
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
}
