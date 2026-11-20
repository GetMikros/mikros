//! The C statements that encode and decode a value of each type.

use super::{Gen, wire_int};
use crate::model::*;

impl Gen<'_> {
    /// Statements that encode `v`, a C lvalue of `ty`, with its inline
    /// part at the offset `at`, through the encoder `e`. A handle in `v`
    /// is taken by the encoder, also when encoding fails.
    pub(super) fn encode(&self, ty: &Type, v: &str, at: &str) -> String {
        match ty {
            Type::Bool => format!("mikros_midl_put_u8(e, {at}, (uint8_t)(({v}) != 0));\n"),
            Type::Int(int) => {
                let (t, n) = wire_int(*int);
                format!("mikros_midl_put_{n}(e, {at}, ({t})({v}));\n")
            }
            Type::F32 => format!("mikros_midl_put_f32(e, {at}, {v});\n"),
            Type::F64 => format!("mikros_midl_put_f64(e, {at}, {v});\n"),
            Type::String { max, optional } => {
                let put = format!(
                    "if (({v}).size > {max}u || !mikros_midl_utf8((const uint8_t *)({v}).data, ({v}).size)) {{\nmikros_midl_encoder_fail(e, MIKROS_MIDL_TOO_LONG);\n}} else {{\nmikros_midl_put_bytes(e, {at}, ({v}).data, ({v}).size, {max}u);\n}}\n"
                );
                self.present_or_absent(v, *optional, "size", &put)
            }
            Type::Vector {
                element,
                max,
                optional,
            } if is_byte(element) => {
                let put =
                    format!("mikros_midl_put_bytes(e, {at}, ({v}).data, ({v}).count, {max}u);\n");
                self.present_or_absent(v, *optional, "count", &put)
            }
            Type::Vector {
                element,
                max,
                optional,
            } => {
                let (base, i) = (self.fresh("base"), self.fresh("i"));
                let size = self.size(element);
                let item = format!("(({})({v}).data)[{i}]", self.pointer(element, true));
                let inner = self.encode(
                    element,
                    &item,
                    &format!("{base} + (uint32_t)({i} * {size}u)"),
                );
                // A vector whose elements carry no handles is not walked
                // once the encoder has failed.
                let more = match self.layout.carries_handles(element) {
                    true => String::new(),
                    false => " && e->status == MIKROS_MIDL_OK".into(),
                };
                let put = format!(
                    "mikros_midl_put_vector(e, {at}, ({v}).count, {max}u);\nuint32_t {base} = mikros_midl_append(e, ({v}).count * {size}u);\nfor (uint64_t {i} = 0; {i} < ({v}).count{more}; {i}++) {{\n{inner}}}\n"
                );
                self.present_or_absent(v, *optional, "count", &format!("{{\n{put}}}\n"))
            }
            Type::Array { element, len } => {
                let i = self.fresh("i");
                let size = self.size(element);
                let inner = self.encode(
                    element,
                    &format!("({v})[{i}]"),
                    &format!("{at} + {i} * {size}u"),
                );
                format!("for (uint32_t {i} = 0; {i} < {len}u; {i}++) {{\n{inner}}}\n")
            }
            Type::Handle { optional, .. } | Type::Endpoint { optional, .. } => {
                let absent = match optional {
                    true => String::new(),
                    false => {
                        " else {\nmikros_midl_encoder_fail(e, MIKROS_MIDL_TOO_LONG);\n}".into()
                    }
                };
                format!("if (({v}) != 0) {{\nmikros_midl_put_handle(e, {at}, {v});\n}}{absent}\n")
            }
            Type::Named { name, optional } => {
                let c = self.cname(name);
                match self.decl_of(name) {
                    DeclKind::Struct(_) if *optional => {
                        let o = self.fresh("o");
                        let size = self.size(&Type::Named {
                            name: name.clone(),
                            optional: false,
                        });
                        format!(
                            "if (({v}) != NULL) {{\nmikros_midl_put_present(e, {at});\nuint32_t {o} = mikros_midl_append(e, {size}u);\n{c}_encode(e, {o}, {v});\n}}\n"
                        )
                    }
                    DeclKind::Union { .. } => format!("{c}_encode(e, {at}, &({v}), {optional});\n"),
                    DeclKind::Struct(_) | DeclKind::Table(_) => {
                        format!("{c}_encode(e, {at}, &({v}));\n")
                    }
                    DeclKind::Enum {
                        strictness,
                        underlying,
                        ..
                    } => {
                        let (t, n) = wire_int(*underlying);
                        let check = match strictness {
                            Strictness::Strict => format!(
                                "if (!{c}_known({v})) {{\nmikros_midl_encoder_fail(e, MIKROS_MIDL_TOO_LONG);\n}}\n"
                            ),
                            Strictness::Flexible => String::new(),
                        };
                        format!("{check}mikros_midl_put_{n}(e, {at}, ({t})({v}));\n")
                    }
                    DeclKind::Bits {
                        strictness,
                        underlying,
                        ..
                    } => {
                        let (t, n) = wire_int(*underlying);
                        let check = match strictness {
                            Strictness::Strict => format!(
                                "if ((({v}) & ~{c}_mask()) != 0) {{\nmikros_midl_encoder_fail(e, MIKROS_MIDL_TOO_LONG);\n}}\n"
                            ),
                            Strictness::Flexible => String::new(),
                        };
                        format!("{check}mikros_midl_put_{n}(e, {at}, ({t})({v}));\n")
                    }
                    _ => unreachable!("only data types are named as types"),
                }
            }
        }
    }

    /// `put` for a string or vector `v` that is present; an absent
    /// optional one, whose data is NULL, leaves its zeroed marker, and
    /// a required one without data must be empty.
    fn present_or_absent(&self, v: &str, optional: bool, len: &str, put: &str) -> String {
        match optional {
            true => format!("if (({v}).data != NULL) {{\n{put}}}\n"),
            false => format!(
                "if (({v}).data == NULL && ({v}).{len} != 0) {{\nmikros_midl_encoder_fail(e, MIKROS_MIDL_TOO_LONG);\n}} else {{\n{put}}}\n"
            ),
        }
    }

    /// Statements that decode the value of `ty` whose inline part is at
    /// `at`, through the decoder `d`, into `out`, a C lvalue.
    pub(super) fn decode(&self, ty: &Type, out: &str, at: &str) -> String {
        match ty {
            Type::Bool => format!("{out} = mikros_midl_get_bool(d, {at});\n"),
            Type::Int(int) => {
                let n = wire_int(*int).1;
                format!(
                    "{out} = ({})mikros_midl_get_{n}(d, {at});\n",
                    super::int_type(*int)
                )
            }
            Type::F32 => format!("{out} = mikros_midl_get_f32(d, {at});\n"),
            Type::F64 => format!("{out} = mikros_midl_get_f64(d, {at});\n"),
            Type::String { max, optional } => {
                format!("{out} = mikros_midl_get_string(d, {at}, {max}u, {optional});\n")
            }
            Type::Vector {
                element,
                max,
                optional,
            } if is_byte(element) => {
                format!("{out} = mikros_midl_get_bytes(d, {at}, {max}u, {optional});\n")
            }
            Type::Vector {
                element,
                max,
                optional,
            } => {
                let (present, n, base, items, i) = (
                    self.fresh("present"),
                    self.fresh("n"),
                    self.fresh("base"),
                    self.fresh("items"),
                    self.fresh("i"),
                );
                let size = self.size(element);
                let inner = self.decode(
                    element,
                    &format!("{items}[{i}]"),
                    &format!("{base} + (uint32_t)({i} * {size}u)"),
                );
                let required = match optional {
                    true => String::new(),
                    false => format!(
                        "if (!{present}) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\n"
                    ),
                };
                format!(
                    "{{\nbool {present};\nuint64_t {n} = mikros_midl_get_vector(d, {at}, {max}u, &{present});\n({out}).data = NULL;\n({out}).count = 0;\n{required}if ({present}) {{\nuint32_t {base} = mikros_midl_claim(d, {n} * {size}u);\n{} = ({})mikros_midl_room(d, {n} ? {n} : 1, sizeof({}));\nfor (uint64_t {i} = 0; {i} < {n} && d->status == MIKROS_MIDL_OK; {i}++) {{\n{inner}}}\nif (d->status == MIKROS_MIDL_OK) {{\n({out}).data = {items};\n({out}).count = {n};\n}}\n}}\n}}\n",
                    self.cdecl(element, &format!("(*{items})")),
                    self.pointer(element, false),
                    self.cdecl(element, ""),
                )
            }
            Type::Array { element, len } => {
                let i = self.fresh("i");
                let size = self.size(element);
                let inner = self.decode(
                    element,
                    &format!("({out})[{i}]"),
                    &format!("{at} + {i} * {size}u"),
                );
                format!("for (uint32_t {i} = 0; {i} < {len}u; {i}++) {{\n{inner}}}\n")
            }
            Type::Handle {
                object,
                rights,
                optional,
            } => {
                let any =
                    |v: &Option<u32>| v.map_or("MIKROS_MIDL_ANY".into(), |v| format!("{v:#x}u"));
                format!(
                    "{out} = mikros_midl_get_handle(d, {at}, {}, {}, {optional});\n",
                    any(object),
                    any(rights)
                )
            }
            // Endpoints are channels.
            Type::Endpoint { optional, .. } => {
                format!(
                    "{out} = mikros_midl_get_handle(d, {at}, 4u, MIKROS_MIDL_ANY, {optional});\n"
                )
            }
            Type::Named { name, optional } => {
                let c = self.cname(name);
                match self.decl_of(name) {
                    DeclKind::Struct(_) if *optional => {
                        let (o, s) = (self.fresh("o"), self.fresh("s"));
                        let size = self.size(&Type::Named {
                            name: name.clone(),
                            optional: false,
                        });
                        format!(
                            "{out} = NULL;\nif (mikros_midl_get_presence(d, {at})) {{\nuint32_t {o} = mikros_midl_claim(d, {size}u);\n{c} *{s} = ({c} *)mikros_midl_room(d, 1, sizeof({c}));\nif ({s} != NULL) {{\n{c}_decode(d, {o}, {s});\n{out} = {s};\n}}\n}}\n"
                        )
                    }
                    DeclKind::Union { .. } => {
                        format!("{c}_decode(d, {at}, &({out}), {optional});\n")
                    }
                    DeclKind::Struct(_) | DeclKind::Table(_) => {
                        format!("{c}_decode(d, {at}, &({out}));\n")
                    }
                    DeclKind::Enum {
                        strictness,
                        underlying,
                        ..
                    } => {
                        let n = wire_int(*underlying).1;
                        let check = match strictness {
                            Strictness::Strict => format!(
                                "if (!{c}_known({out})) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\n"
                            ),
                            Strictness::Flexible => String::new(),
                        };
                        format!("{out} = ({c})mikros_midl_get_{n}(d, {at});\n{check}")
                    }
                    DeclKind::Bits {
                        strictness,
                        underlying,
                        ..
                    } => {
                        let n = wire_int(*underlying).1;
                        let check = match strictness {
                            Strictness::Strict => format!(
                                "if ((({out}) & ~{c}_mask()) != 0) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\n"
                            ),
                            Strictness::Flexible => String::new(),
                        };
                        format!("{out} = ({c})mikros_midl_get_{n}(d, {at});\n{check}")
                    }
                    _ => unreachable!("only data types are named as types"),
                }
            }
        }
    }

    /// The C type of a pointer to `ty`, to its const form when `constant`.
    fn pointer(&self, ty: &Type, constant: bool) -> String {
        let t = self.cdecl(ty, "(*)");
        match constant {
            true => format!("const {t}"),
            false => t,
        }
    }
}

/// Whether vectors of `element` are vectors of bytes, which are copied
/// at once rather than an element at a time.
fn is_byte(element: &Type) -> bool {
    matches!(element, Type::Int(Int::U8))
}
