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
        // The prototypes first, since types may hold each other through
        // pointers and vectors.
        let optional = |d: &Decl| match d.kind {
            DeclKind::Union { .. } => ", bool optional",
            _ => "",
        };
        let names = compound
            .iter()
            .map(|d| (self.own(&d.name), optional(d)))
            .chain(methods.iter().map(|s| (s.name.clone(), "")));
        for (c, optional) in names.clone() {
            writeln!(out, "static inline void {c}_encode(mikros_midl_encoder_t *e, uint32_t at, const {c} *v{optional});").unwrap();
        }
        for (c, optional) in names {
            writeln!(out, "static inline void {c}_decode(mikros_midl_decoder_t *d, uint32_t at, {c} *v{optional});").unwrap();
        }
        if !compound.is_empty() || !methods.is_empty() {
            out.push('\n');
        }
        for d in &compound {
            match &d.kind {
                DeclKind::Struct(fields) => self.struct_encoder(&self.own(&d.name), fields, out),
                DeclKind::Union { members, .. } => self.union_encoder(&d.name, members, out),
                DeclKind::Table(members) => self.table_encoder(&d.name, members, out),
                _ => unreachable!("only compound declarations have encoders"),
            }
        }
        for s in &methods {
            self.struct_encoder(&s.name, s.fields, out);
        }
        for d in &compound {
            match &d.kind {
                DeclKind::Struct(fields) => self.struct_decoder(&self.own(&d.name), fields, out),
                DeclKind::Union {
                    strictness,
                    members,
                } => self.union_decoder(&d.name, *strictness, members, out),
                DeclKind::Table(members) => self.table_decoder(&d.name, members, out),
                _ => unreachable!("only compound declarations have decoders"),
            }
        }
        for s in &methods {
            self.struct_decoder(&s.name, s.fields, out);
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

    fn struct_encoder(&self, c: &str, fields: &[Field], out: &mut String) {
        let offsets = self.layout.struct_shape(fields).1;
        let mut encode = String::new();
        for (f, offset) in fields.iter().zip(&offsets) {
            let field = format!("v->{}", ident(&f.name));
            encode.push_str(&self.encode(&f.ty, &field, &format!("at + {offset}u")));
        }
        let body = format!(
            "static inline void {c}_encode(mikros_midl_encoder_t *e, uint32_t at, const {c} *v)\n{{\n{encode}(void)e;\n(void)at;\n(void)v;\n}}\n"
        );
        out.push_str(&super::indented(&body));
        out.push('\n');
    }

    /// The decoder of a structure, which checks that its padding is zero.
    fn struct_decoder(&self, c: &str, fields: &[Field], out: &mut String) {
        let (shape, offsets) = self.layout.struct_shape(fields);
        let mut decode = String::new();
        let mut end = 0;
        for (f, offset) in fields.iter().zip(&offsets) {
            if *offset > end {
                writeln!(
                    decode,
                    "mikros_midl_zeros(d, at + {end}u, {});",
                    offset - end
                )
                .unwrap();
            }
            end = offset + self.size(&f.ty);
            let field = format!("v->{}", ident(&f.name));
            decode.push_str(&self.decode(&f.ty, &field, &format!("at + {offset}u")));
        }
        if shape.size > end {
            writeln!(
                decode,
                "mikros_midl_zeros(d, at + {end}u, {});",
                shape.size - end
            )
            .unwrap();
        }
        let body = format!(
            "static inline void {c}_decode(mikros_midl_decoder_t *d, uint32_t at, {c} *v)\n{{\nmemset(v, 0, sizeof *v);\n{decode}(void)d;\n(void)at;\n}}\n"
        );
        out.push_str(&super::indented(&body));
        out.push('\n');
    }

    /// The encoder of a union: its ordinal and envelope inline, its
    /// member out of line. An absent optional union stays zero.
    fn union_encoder(&self, name: &str, members: &[OrdinalField], out: &mut String) {
        let c = self.own(name);
        let mut encode = String::new();
        for m in members {
            let (size, ordinal) = (self.size(&m.ty), m.ordinal);
            let o = self.fresh("o");
            writeln!(
                encode,
                "case UINT64_C({ordinal}): {{\nmikros_midl_put_u64(e, at, UINT64_C({ordinal}));\nuint32_t {o} = mikros_midl_append(e, {size}u);\n{}break;\n}}",
                self.encode(&m.ty, &format!("v->{}", member(&m.name)), &o)
            )
            .unwrap();
        }
        let body = format!(
            "static inline void {c}_encode(mikros_midl_encoder_t *e, uint32_t at, const {c} *v, bool optional)\n{{\nmikros_midl_mark_t start = mikros_midl_begin_envelope(e);\nswitch (v->ordinal) {{\ncase 0: {{\nif (!optional) {{\nmikros_midl_encoder_fail(e, MIKROS_MIDL_TOO_LONG);\n}}\nreturn;\n}}\n{encode}default: {{\nmikros_midl_encoder_fail(e, MIKROS_MIDL_UNKNOWN_MEMBER);\nreturn;\n}}\n}}\nmikros_midl_end_envelope(e, at + 8u, start);\n}}\n"
        );
        out.push_str(&super::indented(&body));
        out.push('\n');
    }

    /// The decoder of a union. A member of a flexible union that this
    /// version does not declare is skipped, keeping its ordinal.
    fn union_decoder(
        &self,
        name: &str,
        strictness: Strictness,
        members: &[OrdinalField],
        out: &mut String,
    ) {
        let c = self.own(name);
        let mut decode = String::new();
        for m in members {
            let (size, ordinal) = (self.size(&m.ty), m.ordinal);
            let o = self.fresh("o");
            writeln!(
                decode,
                "case UINT64_C({ordinal}): {{\nuint32_t {o} = mikros_midl_claim(d, {size}u);\n{}break;\n}}",
                self.decode(&m.ty, &format!("v->{}", member(&m.name)), &o)
            )
            .unwrap();
        }
        let unknown = match strictness {
            Strictness::Flexible => "mikros_midl_skip(d, bytes, handles);\nreturn;",
            Strictness::Strict => "mikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\nreturn;",
        };
        let body = format!(
            "static inline void {c}_decode(mikros_midl_decoder_t *d, uint32_t at, {c} *v, bool optional)\n{{\nuint64_t ordinal = mikros_midl_get_u64(d, at);\nuint32_t bytes = mikros_midl_get_u32(d, at + 8u);\nuint32_t handles = mikros_midl_get_u32(d, at + 12u);\nmemset(v, 0, sizeof *v);\nif (ordinal == 0) {{\nif (bytes != 0 || handles != 0 || !optional) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\nreturn;\n}}\nmikros_midl_mark_t start = mikros_midl_decoder_mark(d);\nv->ordinal = ordinal;\nswitch (ordinal) {{\n{decode}default: {{\n{unknown}\n}}\n}}\nmikros_midl_check_envelope(d, start, bytes, handles);\n}}\n"
        );
        out.push_str(&super::indented(&body));
        out.push('\n');
    }

    /// The members of a table by their ordinals.
    fn by_ordinal(members: &[OrdinalField]) -> Vec<&OrdinalField> {
        let mut members: Vec<&OrdinalField> = members.iter().collect();
        members.sort_by_key(|m| m.ordinal);
        members
    }

    /// The encoder of a table: a vector of envelopes up to the highest
    /// field that is set, and the fields out of line in their order.
    fn table_encoder(&self, name: &str, members: &[OrdinalField], out: &mut String) {
        let c = self.own(name);
        let (mut count, mut encode) = (String::new(), String::new());
        for m in Self::by_ordinal(members) {
            let (size, ordinal) = (self.size(&m.ty), m.ordinal);
            let field = format!("v->{}", ident(&m.name));
            writeln!(count, "if (v->has_{}) {{\ncount = {ordinal}u;\n}}", m.name).unwrap();
            let (slot, start, o) = (self.fresh("slot"), self.fresh("start"), self.fresh("o"));
            writeln!(
                encode,
                "if (v->has_{}) {{\nuint32_t {slot} = envelopes + {}u;\nmikros_midl_mark_t {start} = mikros_midl_begin_envelope(e);\nuint32_t {o} = mikros_midl_append(e, {size}u);\n{}mikros_midl_end_envelope(e, {slot}, {start});\nmikros_midl_put_present(e, {slot} + 8u);\n}}",
                m.name,
                (ordinal - 1) * 16,
                self.encode(&m.ty, &field, &o)
            )
            .unwrap();
        }
        let body = format!(
            "static inline void {c}_encode(mikros_midl_encoder_t *e, uint32_t at, const {c} *v)\n{{\nuint64_t count = 0;\n{count}mikros_midl_put_vector(e, at, count, UINT64_MAX);\nuint32_t envelopes = mikros_midl_append(e, count * 16u);\n(void)envelopes;\n{encode}(void)v;\n}}\n"
        );
        out.push_str(&super::indented(&body));
        out.push('\n');
    }

    /// The decoder of a table. Fields that this version does not declare
    /// are skipped.
    fn table_decoder(&self, name: &str, members: &[OrdinalField], out: &mut String) {
        let c = self.own(name);
        let mut decode = String::new();
        for m in Self::by_ordinal(members) {
            let (size, ordinal) = (self.size(&m.ty), m.ordinal);
            let field = format!("v->{}", ident(&m.name));
            let o = self.fresh("o");
            writeln!(
                decode,
                "case {ordinal}u: {{\nuint32_t {o} = mikros_midl_claim(d, {size}u);\n{}v->has_{} = true;\nbreak;\n}}",
                self.decode(&m.ty, &field, &o),
                m.name
            )
            .unwrap();
        }
        let body = format!(
            "static inline void {c}_decode(mikros_midl_decoder_t *d, uint32_t at, {c} *v)\n{{\nmemset(v, 0, sizeof *v);\nbool present;\nuint64_t count = mikros_midl_get_vector(d, at, UINT64_MAX, &present);\nif (!present) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\nuint32_t envelopes = mikros_midl_claim(d, count * 16u);\nfor (uint64_t i = 0; i < count && d->status == MIKROS_MIDL_OK; i++) {{\nuint32_t slot = envelopes + (uint32_t)(i * 16u);\nuint32_t bytes = mikros_midl_get_u32(d, slot);\nuint32_t handles = mikros_midl_get_u32(d, slot + 4u);\nif (!mikros_midl_get_presence(d, slot + 8u)) {{\nif (bytes != 0 || handles != 0 || i + 1 == count) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\ncontinue;\n}}\nmikros_midl_mark_t start = mikros_midl_decoder_mark(d);\nswitch (i + 1) {{\n{decode}default: {{\nmikros_midl_skip(d, bytes, handles);\ncontinue;\n}}\n}}\nmikros_midl_check_envelope(d, start, bytes, handles);\n}}\n}}\n"
        );
        out.push_str(&super::indented(&body));
        out.push('\n');
    }
}

/// The C name of a union member, which must not be `ordinal`.
fn member(name: &str) -> String {
    match name {
        "ordinal" => "ordinal_".into(),
        _ => ident(name),
    }
}
