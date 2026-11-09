//! The JSON description of checked libraries, for generators other than
//! midlc's own.

use crate::layout::Layout;
use crate::model::*;
use serde_json::{Value as Json, json};

/// Describes every library of `model` with its declarations, types and
/// layout.
pub fn describe(model: &Model) -> Json {
    let layout = Layout { model };
    let libraries: Vec<Json> = model
        .libraries
        .iter()
        .map(|l| {
            let decls: Vec<Json> = l.decls.iter().map(|d| decl(&layout, d)).collect();
            json!({ "name": l.name, "doc": l.doc, "declarations": decls })
        })
        .collect();
    json!({ "version": 1, "libraries": libraries })
}

fn strictness(s: Strictness) -> &'static str {
    match s {
        Strictness::Strict => "strict",
        Strictness::Flexible => "flexible",
    }
}

fn decl(layout: &Layout, d: &Decl) -> Json {
    let mut out = match &d.kind {
        DeclKind::Const { ty: t, value } => json!({
            "kind": "const",
            "type": ty(t),
            "value": match value {
                Value::Int(v) => json!(v.to_string()),
                Value::Bool(b) => json!(b),
                Value::Str(s) => json!(s),
            },
        }),
        DeclKind::Alias(t) => json!({ "kind": "alias", "type": ty(t) }),
        DeclKind::Struct(fields) => {
            let (shape, offsets) = layout.struct_shape(fields);
            let members: Vec<Json> = fields
                .iter()
                .zip(offsets)
                .map(|(f, offset)| json!({ "name": f.name, "doc": f.doc, "type": ty(&f.ty), "offset": offset }))
                .collect();
            json!({ "kind": "struct", "size": shape.size, "alignment": shape.align, "members": members })
        }
        DeclKind::Enum {
            strictness: s,
            underlying,
            members,
        }
        | DeclKind::Bits {
            strictness: s,
            underlying,
            members,
        } => {
            let kind = if matches!(d.kind, DeclKind::Enum { .. }) {
                "enum"
            } else {
                "bits"
            };
            let members: Vec<Json> = members
                .iter()
                .map(|m| json!({ "name": m.name, "doc": m.doc, "value": m.value.to_string() }))
                .collect();
            json!({ "kind": kind, "strictness": strictness(*s), "underlying": underlying.name(), "members": members })
        }
        DeclKind::Union {
            strictness: s,
            members,
        } => {
            json!({ "kind": "union", "strictness": strictness(*s), "members": ordinal_fields(members) })
        }
        DeclKind::Table(members) => json!({ "kind": "table", "members": ordinal_fields(members) }),
        DeclKind::Protocol(methods) => {
            let methods: Vec<Json> = methods.iter().map(method).collect();
            json!({ "kind": "protocol", "methods": methods })
        }
    };
    out["name"] = json!(d.name);
    out["doc"] = json!(d.doc);
    out
}

fn fields(fields: &[Field]) -> Json {
    fields
        .iter()
        .map(|f| json!({ "name": f.name, "doc": f.doc, "type": ty(&f.ty) }))
        .collect()
}

fn ordinal_fields(members: &[OrdinalField]) -> Json {
    members
        .iter()
        .map(|m| json!({ "ordinal": m.ordinal, "name": m.name, "doc": m.doc, "type": ty(&m.ty) }))
        .collect()
}

fn method(m: &Method) -> Json {
    let mut out = match &m.kind {
        MethodKind::TwoWay {
            request,
            response,
            error,
        } => json!({
            "kind": "two_way",
            "request": fields(request),
            "response": fields(response),
            "error": error.as_ref().map(ty),
        }),
        MethodKind::OneWay(args) => json!({ "kind": "one_way", "request": fields(args) }),
        MethodKind::Event(args) => json!({ "kind": "event", "request": fields(args) }),
    };
    out["name"] = json!(m.name);
    out["doc"] = json!(m.doc);
    out["ordinal"] = json!(m.ordinal);
    out["strictness"] = json!(strictness(m.strictness));
    out
}

fn ty(t: &Type) -> Json {
    match t {
        Type::Bool => json!({ "kind": "bool" }),
        Type::Int(int) => json!({ "kind": int.name() }),
        Type::F32 => json!({ "kind": "f32" }),
        Type::F64 => json!({ "kind": "f64" }),
        Type::String { max, optional } => {
            json!({ "kind": "string", "max": max, "optional": optional })
        }
        Type::Vector {
            element,
            max,
            optional,
        } => {
            json!({ "kind": "vector", "element": ty(element), "max": max, "optional": optional })
        }
        Type::Array { element, len } => {
            json!({ "kind": "array", "element": ty(element), "len": len })
        }
        Type::Handle {
            object,
            rights,
            optional,
        } => {
            json!({ "kind": "handle", "object_type": object, "rights": rights, "optional": optional })
        }
        Type::Endpoint {
            client,
            protocol,
            optional,
        } => json!({
            "kind": if *client { "client_end" } else { "server_end" },
            "protocol": protocol,
            "optional": optional,
        }),
        Type::Named { name, optional } => {
            json!({ "kind": "named", "name": name, "optional": optional })
        }
    }
}
