//! The C types of each declaration, and the functions that encode and
//! decode them.

use super::{Gen, doc, ident, int_literal, string_literal};
use crate::model::*;
use std::collections::BTreeSet;
use std::fmt::Write;

/// A structure of the bindings: a declared one, or the arguments or
/// results of a method.
pub(super) struct Structure<'a> {
    pub name: String,
    pub doc: &'a [String],
    pub fields: &'a [Field],
}

impl Gen<'_> {
    pub(super) fn declarations(&self, out: &mut String) {
        let decls = &self.library.decls;
        for d in decls {
            match &d.kind {
                DeclKind::Const { ty, value } => self.constant(d, ty, value, out),
                DeclKind::Enum {
                    underlying,
                    members,
                    ..
                } => self.enumeration(d, *underlying, members, out),
                DeclKind::Bits {
                    underlying,
                    members,
                    ..
                } => self.bits(d, *underlying, members, out),
                _ => {}
            }
        }
        let methods = self.method_structures();
        // Every structure, union and table by name, so that pointers to
        // them may come before their definitions.
        let compound: Vec<&Decl> = decls
            .iter()
            .filter(|d| {
                matches!(
                    d.kind,
                    DeclKind::Struct(_) | DeclKind::Union { .. } | DeclKind::Table(_)
                )
            })
            .collect();
        for d in &compound {
            let c = self.own(&d.name);
            writeln!(out, "typedef struct {c} {c};").unwrap();
        }
        for s in &methods {
            writeln!(out, "typedef struct {0} {0};", s.name).unwrap();
        }
        if !compound.is_empty() || !methods.is_empty() {
            out.push('\n');
        }
        for d in self.ordered(&compound) {
            out.push_str(&doc(&d.doc, ""));
            match &d.kind {
                DeclKind::Struct(fields) => self.struct_type(&self.own(&d.name), fields, out),
                DeclKind::Union { members, .. } => self.union_type(&d.name, members, out),
                DeclKind::Table(members) => self.table_type(&d.name, members, out),
                _ => unreachable!("only compound declarations are ordered"),
            }
        }
        for s in &methods {
            out.push_str(&doc(s.doc, ""));
            self.struct_type(&s.name, s.fields, out);
        }
        for d in decls {
            if let DeclKind::Alias(ty) = &d.kind {
                out.push_str(&doc(&d.doc, ""));
                writeln!(out, "typedef {};\n", self.cdecl(ty, &self.own(&d.name))).unwrap();
            }
        }
    }

    fn constant(&self, d: &Decl, ty: &Type, value: &Value, out: &mut String) {
        out.push_str(&doc(&d.doc, ""));
        let text = match (ty, value) {
            (_, Value::Bool(b)) => b.to_string(),
            (_, Value::Str(s)) => string_literal(s),
            (Type::Int(int), Value::Int(v)) => int_literal(*int, *v),
            (Type::Named { name, .. }, Value::Int(v)) => match self.decl_of(name) {
                DeclKind::Enum { underlying, .. } | DeclKind::Bits { underlying, .. } => {
                    format!("(({}){})", self.cname(name), int_literal(*underlying, *v))
                }
                _ => unreachable!("integer constants are of integer types"),
            },
            _ => unreachable!("integer constants are of integer types"),
        };
        writeln!(out, "#define {} {text}\n", self.own(&d.name)).unwrap();
    }

    fn enumeration(&self, d: &Decl, int: Int, members: &[ValueMember], out: &mut String) {
        let c = self.own(&d.name);
        out.push_str(&doc(&d.doc, ""));
        writeln!(out, "typedef {} {c};", super::int_type(int)).unwrap();
        let mut cases = String::new();
        for m in members {
            out.push_str(&doc(&m.doc, ""));
            let value = int_literal(int, m.value);
            writeln!(out, "#define {c}_{} (({c}){value})", m.name).unwrap();
            writeln!(cases, "    case {value}:").unwrap();
        }
        // A switch without cases would warn.
        let body = match members.is_empty() {
            true => "    (void)v;\n    return false;\n".to_string(),
            false => format!(
                "    switch (v) {{\n{cases}        return true;\n    default:\n        return false;\n    }}\n"
            ),
        };
        writeln!(
            out,
            "\n/* Whether `v` is a value that this version declares. */\nstatic inline bool {c}_known({c} v)\n{{\n{body}}}\n"
        )
        .unwrap();
    }

    fn bits(&self, d: &Decl, int: Int, members: &[ValueMember], out: &mut String) {
        let c = self.own(&d.name);
        out.push_str(&doc(&d.doc, ""));
        writeln!(out, "typedef {} {c};", super::int_type(int)).unwrap();
        let mut all = 0i128;
        for m in members {
            out.push_str(&doc(&m.doc, ""));
            writeln!(
                out,
                "#define {c}_{} (({c}){})",
                m.name,
                int_literal(int, m.value)
            )
            .unwrap();
            all |= m.value;
        }
        writeln!(
            out,
            "\n/* Every bit that this version declares. */\nstatic inline {c} {c}_mask(void)\n{{\n    return ({c}){};\n}}\n",
            int_literal(int, all)
        )
        .unwrap();
    }

    /// The structures of the methods' arguments and results that have
    /// fields.
    fn method_structures(&self) -> Vec<Structure<'_>> {
        let mut out = Vec::new();
        for d in &self.library.decls {
            let DeclKind::Protocol(methods) = &d.kind else {
                continue;
            };
            for m in methods {
                let base = format!("{}_{}", self.own(&d.name), m.name);
                let parts: Vec<(&str, &[Field])> = match &m.kind {
                    MethodKind::TwoWay {
                        request, response, ..
                    } => vec![("Request", request), ("Response", response)],
                    MethodKind::OneWay(fields) => vec![("Request", fields)],
                    MethodKind::Event(fields) => vec![("Event", fields)],
                };
                for (suffix, fields) in parts {
                    if !fields.is_empty() {
                        out.push(Structure {
                            name: format!("{base}_{suffix}"),
                            doc: &[],
                            fields,
                        });
                    }
                }
            }
        }
        out
    }

    /// `decls` in an order in which each comes after the declarations of
    /// this library that it holds by value.
    fn ordered<'d>(&self, decls: &[&'d Decl]) -> Vec<&'d Decl> {
        let mut done = BTreeSet::new();
        let mut order = Vec::new();
        for d in decls {
            self.visit_decl(d, decls, &mut done, &mut order);
        }
        order
    }

    fn visit_decl<'d>(
        &self,
        d: &'d Decl,
        decls: &[&'d Decl],
        done: &mut BTreeSet<String>,
        order: &mut Vec<&'d Decl>,
    ) {
        if !done.insert(d.name.clone()) {
            return;
        }
        let types: Vec<&Type> = match &d.kind {
            DeclKind::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
            DeclKind::Union { members, .. } | DeclKind::Table(members) => {
                members.iter().map(|m| &m.ty).collect()
            }
            _ => Vec::new(),
        };
        for ty in types {
            for full in self.held(ty) {
                let (library, name) = full.rsplit_once('.').expect("full names have a library");
                if library != self.library.name {
                    continue;
                }
                if let Some(dep) = decls.iter().find(|x| x.name == name) {
                    self.visit_decl(dep, decls, done, order);
                }
            }
        }
        order.push(d);
    }

    /// The compound declarations that a value of `ty` holds by value.
    fn held(&self, ty: &Type) -> Vec<String> {
        match ty {
            Type::Array { element, .. } => self.held(element),
            Type::Named { name, optional } => match self.decl_of(name) {
                DeclKind::Struct(_) if *optional => Vec::new(),
                DeclKind::Struct(_) | DeclKind::Union { .. } | DeclKind::Table(_) => {
                    vec![name.clone()]
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn struct_type(&self, c: &str, fields: &[Field], out: &mut String) {
        writeln!(out, "struct {c} {{").unwrap();
        for f in fields {
            out.push_str(&doc(&f.doc, "    "));
            writeln!(
                out,
                "    {};{}",
                self.cdecl(&f.ty, &ident(&f.name)),
                self.element_note(&f.ty)
            )
            .unwrap();
        }
        if fields.is_empty() {
            out.push_str(
                "    /* The structure has no fields; C needs a member. */\n    uint8_t unused_;\n",
            );
        }
        out.push_str("};\n\n");
    }

    fn union_type(&self, name: &str, members: &[OrdinalField], out: &mut String) {
        let c = self.own(name);
        writeln!(
            out,
            "struct {c} {{\n    /* The ordinal of the member that is set, one of the {c}_ constants;\n     * once decoded, that of a member this version does not declare; 0 in\n     * an absent optional union. */\n    uint64_t ordinal;\n    union {{"
        )
        .unwrap();
        for m in members {
            out.push_str(&doc(&m.doc, "        "));
            writeln!(
                out,
                "        {};{}",
                self.cdecl(&m.ty, &member(&m.name)),
                self.element_note(&m.ty)
            )
            .unwrap();
        }
        out.push_str("    };\n};\n\n");
        for m in members {
            writeln!(out, "#define {c}_{} UINT64_C({})", m.name, m.ordinal).unwrap();
        }
        out.push('\n');
    }

    fn table_type(&self, name: &str, members: &[OrdinalField], out: &mut String) {
        let c = self.own(name);
        writeln!(out, "struct {c} {{").unwrap();
        for m in members {
            out.push_str(&doc(&m.doc, "    "));
            writeln!(
                out,
                "    bool has_{};\n    {};{}",
                m.name,
                self.cdecl(&m.ty, &ident(&m.name)),
                self.element_note(&m.ty)
            )
            .unwrap();
        }
        if members.is_empty() {
            out.push_str(
                "    /* The table has no fields; C needs a member. */\n    uint8_t unused_;\n",
            );
        }
        out.push_str("};\n\n");
    }
}

/// The C name of a union member, which must not be `ordinal`.
fn member(name: &str) -> String {
    match name {
        "ordinal" => "ordinal_".into(),
        _ => ident(name),
    }
}
