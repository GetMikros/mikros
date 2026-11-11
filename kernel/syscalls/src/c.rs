//! The C library's system call functions, generated from the table: the
//! header `<mikros/syscalls.h>` that declares them, and the Rust code of
//! the C library that defines them on the user-space bindings.
//!
//! Only the calls of production builds get a function, so calls that
//! exist only for fuzzing or diagnosis are left out.

use crate::table::{Call, Table, Type};
use std::fmt::Write;

fn production_calls<'t>(table: &'t Table, arch: &'t str) -> impl Iterator<Item = &'t Call> {
    table
        .calls
        .iter()
        .filter(move |c| !c.fuzz_only && !c.diag_only && c.arches.iter().any(|a| a == arch))
}

/// The C type of a value type of the table.
fn c_value(t: &str) -> &'static str {
    match t {
        "u8" => "uint8_t",
        "u32" => "uint32_t",
        "u64" => "uint64_t",
        "i32" => "int32_t",
        "i64" => "int64_t",
        "Handle" => "mikros_handle_t",
        other => panic!("no C type for {other}"),
    }
}

fn c_type(ty: &Type) -> String {
    match ty {
        Type::Int(t) => c_value(t).to_owned(),
        Type::Handle => "mikros_handle_t".to_owned(),
        Type::Const(t) => format!("const {} *", c_value(t)),
        Type::Mut(t) => format!("{} *", c_value(t)),
    }
}

/// The parameters of `call` as C declares them.
fn c_params(call: &Call) -> String {
    if call.args.is_empty() {
        return "void".to_owned();
    }
    call.args
        .iter()
        .map(|a| {
            let ty = c_type(&a.ty);
            if ty.ends_with('*') {
                format!("{ty}{}", a.name)
            } else {
                format!("{ty} {}", a.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The header `<mikros/syscalls.h>` for `arch`.
pub fn header(table: &Table, arch: &str) -> String {
    let mut out = String::from(
        "/* @generated from kernel/syscalls.yaml by the syscall-table crate.\n \
         * Do not edit. */\n\n\
         #ifndef MIKROS_SYSCALLS_H\n#define MIKROS_SYSCALLS_H\n\n\
         #include <stdint.h>\n\n\
         #ifdef __cplusplus\nextern \"C\" {\n#endif\n\n\
         typedef uint32_t mikros_handle_t;\ntypedef int64_t mikros_status_t;\n\n",
    );
    for call in production_calls(table, arch) {
        let _ = writeln!(
            out,
            "/* {} */\nmikros_status_t mikros_{}({});\n",
            call.spec.join(", "),
            call.name,
            c_params(call)
        );
    }
    out.push_str("#ifdef __cplusplus\n}\n#endif\n\n#endif\n");
    out
}

/// The Rust type of an argument in the definitions.
fn rust_type(ty: &Type) -> String {
    match ty {
        Type::Int(t) => t.clone(),
        Type::Handle => "sys::Handle".to_owned(),
        Type::Const(t) if t == "Handle" => "*const sys::Handle".to_owned(),
        Type::Mut(t) if t == "Handle" => "*mut sys::Handle".to_owned(),
        Type::Const(t) => format!("*const {t}"),
        Type::Mut(t) => format!("*mut {t}"),
    }
}

/// The definitions of the functions for `arch`, each making its call
/// through `sys`, the bindings of mikros-rt.
pub fn definitions(table: &Table, arch: &str) -> String {
    let mut out = String::from(
        "// @generated from kernel/syscalls.yaml by the syscall-table crate.\n\
         // Do not edit.\n\n",
    );
    for call in production_calls(table, arch) {
        let params = call
            .args
            .iter()
            .map(|a| format!("{}: {}", a.name, rust_type(&a.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let names = call
            .args
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "/// `{name}`: {spec}.\n///\n/// # Safety\n///\n\
             /// Pointer arguments must be valid as the call requires.\n\
             #[unsafe(no_mangle)]\n\
             pub unsafe extern \"C\" fn mikros_{name}({params}) -> sys::Status {{\n    \
             // SAFETY: the caller passes arguments as the call requires.\n    \
             unsafe {{ sys::{name}({names}) }}\n}}\n",
            name = call.name,
            spec = call.spec.join(", "),
        );
    }
    out
}
