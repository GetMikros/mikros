//! The client of each protocol: for protocol `P` with method `M`, the
//! ordinal `P_M_ORDINAL`; for a two-way method the function `P_M`,
//! which calls it and waits for the reply, and `P_M_reply`, which
//! decodes one; for a one-way method `P_M`, which sends it; and for the
//! events, the type `P_Event` with `P_DecodeEvent` and `P_ReadEvent`.

use super::{Gen, doc, ident, indented};
use crate::model::*;
use std::fmt::Write;

/// The names that the functions give their own parameters and
/// variables, which a parameter for a field must not take.
const TAKEN: [&str; 5] = ["channel", "deadline", "arena", "response", "error"];

impl Gen<'_> {
    pub(super) fn protocol(&self, d: &Decl, methods: &[Method], out: &mut String) {
        let p = self.own(&d.name);
        writeln!(
            out,
            "/* The client of protocol {}.{}. */",
            self.library.name, d.name
        )
        .unwrap();
        out.push_str(&doc(&d.doc, ""));
        for m in methods {
            writeln!(
                out,
                "#define {p}_{}_ORDINAL UINT32_C({})",
                m.name, m.ordinal
            )
            .unwrap();
        }
        out.push('\n');
        for m in methods {
            match &m.kind {
                MethodKind::TwoWay {
                    request,
                    response,
                    error,
                } => self.two_way(&p, m, request, response, error.as_ref(), out),
                MethodKind::OneWay(fields) => self.one_way(&p, m, fields, out),
                MethodKind::Event(_) => {}
            }
        }
        let events: Vec<&Method> = methods
            .iter()
            .filter(|m| matches!(m.kind, MethodKind::Event(_)))
            .collect();
        if !events.is_empty() {
            self.events(&p, &events, out);
        }
    }

    /// The parameters for `fields`, each followed by a comma, and the
    /// statements that fill `midl_request` from them.
    fn params(&self, fields: &[Field]) -> (String, String) {
        let (mut params, mut fill) = (String::new(), String::new());
        for f in fields {
            let field = ident(&f.name);
            let param = match TAKEN.contains(&f.name.as_str()) || f.name.starts_with("midl_") {
                true => format!("{field}_"),
                false => field.clone(),
            };
            let target = format!("midl_request.{field}");
            match &f.ty {
                Type::Array { .. } => {
                    write!(params, "const {}, ", self.cdecl(&f.ty, &param)).unwrap();
                    writeln!(fill, "memcpy({target}, {param}, sizeof {target});").unwrap();
                }
                Type::Named { name, optional } => {
                    let c = self.cname(name);
                    match self.decl_of(name) {
                        DeclKind::Struct(_) if *optional => {
                            write!(params, "const {c} *{param}, ").unwrap();
                            writeln!(fill, "{target} = ({c} *){param};").unwrap();
                        }
                        DeclKind::Union { .. } if *optional => {
                            write!(params, "const {c} *{param}, ").unwrap();
                            writeln!(fill, "if ({param} != NULL) {{\n{target} = *{param};\n}}")
                                .unwrap();
                        }
                        DeclKind::Struct(_) | DeclKind::Union { .. } | DeclKind::Table(_) => {
                            write!(params, "const {c} *{param}, ").unwrap();
                            writeln!(fill, "{target} = *{param};").unwrap();
                        }
                        _ => {
                            write!(params, "{}, ", self.cdecl(&f.ty, &param)).unwrap();
                            writeln!(fill, "{target} = {param};").unwrap();
                        }
                    }
                }
                _ => {
                    write!(params, "{}, ", self.cdecl(&f.ty, &param)).unwrap();
                    writeln!(fill, "{target} = {param};").unwrap();
                }
            }
        }
        (params, fill)
    }

    /// The statements that start the message of `m` in `midl_e`, with
    /// its arguments `fields` taken from the parameters.
    fn request(&self, base: &str, m: &Method, fields: &[Field]) -> String {
        let flags = match m.strictness {
            Strictness::Flexible => "MIKROS_MIDL_FLAG_FLEXIBLE",
            Strictness::Strict => "0",
        };
        let size = self.layout.struct_shape(fields).0.size;
        let mut s = format!(
            "mikros_midl_encoder_t midl_e;\nmikros_midl_encoder_init(&midl_e, arena, 0, {base}_ORDINAL, {flags}, {size}u);\n"
        );
        if !fields.is_empty() {
            let (_, fill) = self.params(fields);
            write!(
                s,
                "{base}_Request midl_request;\nmemset(&midl_request, 0, sizeof midl_request);\n{fill}{base}_Request_encode(&midl_e, MIKROS_MIDL_HEADER_SIZE, &midl_request);\n"
            )
            .unwrap();
        }
        s
    }

    fn two_way(
        &self,
        p: &str,
        m: &Method,
        request: &[Field],
        response: &[Field],
        error: Option<&Type>,
        out: &mut String,
    ) {
        let base = format!("{p}_{}", m.name);
        let (params, _) = self.params(request);
        let resp_size = self.layout.struct_shape(response).0.size;
        let (resp_param, resp_arg, resp_decode) = match response.is_empty() {
            true => (String::new(), "", String::new()),
            false => (
                format!(", {base}_Response *response"),
                ", response",
                format!("{base}_Response_decode(d, at, response);\n"),
            ),
        };
        let (err_param, err_arg) = match error {
            Some(e) => (format!(", {}", self.cdecl(e, "*error")), ", error"),
            None => (String::new(), ""),
        };
        let body = match error {
            None => format!(
                "uint32_t at = mikros_midl_claim(d, {resp_size}u);\n{resp_decode}(void)at;\nreturn mikros_midl_decoder_finish(d);\n"
            ),
            Some(e) => {
                let (size, decode) = (self.size(e), self.decode(e, "(*error)", "at"));
                format!(
                    "uint32_t result = mikros_midl_claim(d, 16u);\nuint64_t which = mikros_midl_get_u64(d, result);\nuint32_t bytes = mikros_midl_get_u32(d, result + 8u);\nuint32_t handles = mikros_midl_get_u32(d, result + 12u);\nmikros_midl_mark_t start = mikros_midl_decoder_mark(d);\nbool failed = false;\nif (which == 1) {{\nuint32_t at = mikros_midl_claim(d, {resp_size}u);\n{resp_decode}(void)at;\n}} else if (which == 2) {{\nuint32_t at = mikros_midl_claim(d, {size}u);\n{decode}failed = true;\n}} else {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\nmikros_midl_check_envelope(d, start, bytes, handles);\nmikros_midl_status_t s = mikros_midl_decoder_finish(d);\nreturn s == MIKROS_MIDL_OK && failed ? MIKROS_MIDL_ERROR : s;\n"
                )
            }
        };
        let reply = format!(
            "/* Decodes the reply to {base} that `d` holds into `response`; on a\n * failure, the values are not valid and their handles are closed. */\nstatic inline mikros_midl_status_t {base}_reply(mikros_midl_decoder_t *d{resp_param}{err_param})\n{{\nuint32_t txid;\nuint32_t ordinal;\nuint16_t flags;\nmikros_midl_header(d, &txid, &ordinal, &flags);\n(void)txid;\nif (ordinal != {base}_ORDINAL || (flags & MIKROS_MIDL_FLAG_FLEXIBLE) != 0) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\nif (d->status == MIKROS_MIDL_OK && (flags & MIKROS_MIDL_FLAG_UNKNOWN_METHOD) != 0) {{\nmikros_midl_status_t s = mikros_midl_decoder_finish(d);\nreturn s == MIKROS_MIDL_OK ? MIKROS_MIDL_UNKNOWN_METHOD : s;\n}}\n{body}}}\n"
        );
        out.push_str(&indented(&reply));
        out.push('\n');
        out.push_str(&doc(&m.doc, ""));
        let call = format!(
            "static inline mikros_midl_status_t {base}(mikros_handle_t channel, {params}{}{}int64_t deadline, mikros_midl_arena_t *arena)\n{{\n{}mikros_midl_decoder_t midl_d;\nmikros_midl_status_t midl_s = mikros_midl_call(channel, &midl_e, deadline, arena, &midl_d);\nif (midl_s != MIKROS_MIDL_OK) {{\nreturn midl_s;\n}}\nreturn {base}_reply(&midl_d{resp_arg}{err_arg});\n}}\n",
            resp_param.trim_start_matches(", ").to_string()
                + if resp_param.is_empty() { "" } else { ", " },
            err_param.trim_start_matches(", ").to_string()
                + if err_param.is_empty() { "" } else { ", " },
            self.request(&base, m, request)
        );
        out.push_str(&indented(&call));
        out.push('\n');
    }

    fn one_way(&self, p: &str, m: &Method, fields: &[Field], out: &mut String) {
        let base = format!("{p}_{}", m.name);
        let (params, _) = self.params(fields);
        out.push_str(&doc(&m.doc, ""));
        let send = format!(
            "static inline mikros_midl_status_t {base}(mikros_handle_t channel, {params}mikros_midl_arena_t *arena)\n{{\nsize_t midl_used = arena->used;\n{}mikros_midl_status_t midl_s = mikros_midl_send(channel, &midl_e);\narena->used = midl_used;\nreturn midl_s;\n}}\n",
            self.request(&base, m, fields)
        );
        out.push_str(&indented(&send));
        out.push('\n');
    }

    fn events(&self, p: &str, events: &[&Method], out: &mut String) {
        let mut members = String::new();
        let mut arms = String::new();
        for m in events {
            let MethodKind::Event(fields) = &m.kind else {
                unreachable!("only events are passed");
            };
            let base = format!("{p}_{}", m.name);
            let flexible = m.strictness == Strictness::Flexible;
            let size = self.layout.struct_shape(fields).0.size;
            let decode = match fields.is_empty() {
                true => String::new(),
                false => {
                    writeln!(members, "        {base}_Event {};", ident(&m.name)).unwrap();
                    format!("{base}_Event_decode(d, at, &event->{});\n", ident(&m.name))
                }
            };
            writeln!(
                arms,
                "case {base}_ORDINAL: {{\nif (flexible != {flexible}) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\nuint32_t at = mikros_midl_claim(d, {size}u);\n{decode}(void)at;\nbreak;\n}}"
            )
            .unwrap();
        }
        let union = match members.is_empty() {
            true => String::new(),
            false => format!("    union {{\n{members}    }};\n"),
        };
        writeln!(
            out,
            "/* An event of the protocol: `ordinal` is that of its method, and the\n * member of the method's name holds its arguments. */\ntypedef struct {{\n    uint32_t ordinal;\n{union}}} {p}_Event;\n"
        )
        .unwrap();
        let decode = format!(
            "/* Decodes the event in the message of `len` bytes and `count` handles,\n * taking the handles. MIKROS_MIDL_UNKNOWN_METHOD for a flexible event\n * this version does not declare, which it discards. */\nstatic inline mikros_midl_status_t {p}_DecodeEvent(const uint8_t *bytes, uint32_t len, mikros_handle_t *handles, uint32_t count, {p}_Event *event, mikros_midl_arena_t *arena)\n{{\nmikros_midl_decoder_t midl_d;\nmikros_midl_decoder_t *d = &midl_d;\nmikros_midl_decoder_init(d, bytes, len, handles, count, arena);\nuint32_t txid;\nuint32_t ordinal;\nuint16_t flags;\nmikros_midl_header(d, &txid, &ordinal, &flags);\nif (txid != 0 || (flags & MIKROS_MIDL_FLAG_UNKNOWN_METHOD) != 0) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\n}}\nmemset(event, 0, sizeof *event);\nevent->ordinal = ordinal;\nbool flexible = (flags & MIKROS_MIDL_FLAG_FLEXIBLE) != 0;\nif (d->status == MIKROS_MIDL_OK) {{\nswitch (ordinal) {{\n{arms}default: {{\nif (!flexible) {{\nmikros_midl_decoder_fail(d, MIKROS_MIDL_INVALID);\nbreak;\n}}\nfor (uint32_t i = 0; i < count; i++) {{\nmikros_handle_close(handles[i]);\n}}\nreturn MIKROS_MIDL_UNKNOWN_METHOD;\n}}\n}}\n}}\nreturn mikros_midl_decoder_finish(d);\n}}\n\n/* The next event on `channel`, waiting for one until `deadline`. Events\n * this version does not declare are discarded when they are flexible. */\nstatic inline mikros_midl_status_t {p}_ReadEvent(mikros_handle_t channel, {p}_Event *event, int64_t deadline, mikros_midl_arena_t *arena)\n{{\nfor (;;) {{\nsize_t used = arena->used;\nuint8_t *bytes = NULL;\nuint32_t len = 0;\nmikros_handle_t *handles = NULL;\nuint32_t count = 0;\nmikros_midl_status_t s = mikros_midl_read(channel, deadline, arena, &bytes, &len, &handles, &count);\nif (s != MIKROS_MIDL_OK) {{\nreturn s;\n}}\ns = {p}_DecodeEvent(bytes, len, handles, count, event, arena);\nif (s != MIKROS_MIDL_UNKNOWN_METHOD) {{\nreturn s;\n}}\narena->used = used;\n}}\n}}\n"
        );
        out.push_str(&indented(&decode));
        out.push('\n');
    }
}
