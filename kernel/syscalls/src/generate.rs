//! Rust code generated from the system call table.

use crate::table::{Arg, Call, Dir, HandleCheck, Table, Type};
use std::fmt::Write;

const HEADER: &str = "// @generated from kernel/syscalls.yaml by the syscall-table crate.\n\
                      // Do not edit.\n\n";

/// The x86_64 registers that carry the arguments, in order.
pub const X86_64_ARG_REGISTERS: [&str; 8] = ["rdi", "rsi", "rdx", "r10", "r8", "r9", "r12", "r13"];

fn calls_for<'t>(table: &'t Table, arch: &'t str) -> impl Iterator<Item = &'t Call> {
    table
        .calls
        .iter()
        .filter(move |c| c.arches.iter().any(|a| a == arch))
}

fn constant(call: &Call) -> String {
    call.name.to_ascii_uppercase()
}

/// The condition on a call of the kernel's dispatch: the kernel's fuzz
/// or diag feature for a call that exists only in that build.
fn kernel_cfg(call: &Call) -> &'static str {
    if call.fuzz_only {
        "#[cfg(feature = \"fuzz\")]\n"
    } else if call.diag_only {
        "#[cfg(feature = \"diag\")]\n"
    } else {
        ""
    }
}

/// The condition on a call of the user-space bindings: their crate's
/// fuzz feature, or the `mikros_diag` cfg of the diagnostic build.
fn user_cfg(call: &Call) -> &'static str {
    if call.diag_only {
        "#[cfg(mikros_diag)]\n"
    } else {
        kernel_cfg(call)
    }
}

/// The kernel's Rust type for a value type.
fn kernel_value(t: &str) -> &str {
    if t == "Handle" {
        "kernel_core::object::handles::Handle"
    } else {
        t
    }
}

/// The accepted object types of a handle check, as an expression of type
/// `Option<&[ObjectType]>`.
fn types_expr(check: &HandleCheck) -> String {
    match &check.types {
        None => "None".to_owned(),
        Some(types) => format!(
            "Some(&[{}])",
            types
                .iter()
                .map(|t| format!("ObjectType::{t}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The required rights of a handle check, as a `u32` expression.
fn rights_expr(check: &HandleCheck) -> String {
    if check.rights.is_empty() {
        return "0".to_owned();
    }
    check
        .rights
        .iter()
        .map(|r| format!("rights::{r}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The type a handler of the kernel's dispatch takes `arg` as.
fn kernel_param(arg: &Arg) -> String {
    match (&arg.ty, &arg.handle) {
        (Type::Int(t), _) => t.clone(),
        (Type::Handle, Some(check)) if check.allow_invalid => "Option<Checked>".to_owned(),
        (Type::Handle, Some(_)) => "Checked".to_owned(),
        (Type::Handle, None) => kernel_value("Handle").to_owned(),
        (Type::Const(t), _) => format!("UserIn<{}>", kernel_value(t)),
        (Type::Mut(t), _) if arg.dir == Dir::InOut => format!("UserInOut<{}>", kernel_value(t)),
        (Type::Mut(t), _) => format!("UserOut<{}>", kernel_value(t)),
    }
}

/// The model of an argument for the fuzz harness, an `ArgModel`.
fn arg_model(arg: &Arg) -> String {
    match (&arg.ty, &arg.handle) {
        (Type::Int(t), _) => format!("ArgModel::{}", t.to_ascii_uppercase()),
        (Type::Handle, Some(check)) => format!(
            "ArgModel::Checked {{ types: {}, rights: {}, allow_invalid: {} }}",
            types_expr(check),
            rights_expr(check),
            check.allow_invalid
        ),
        (Type::Handle, None) => "ArgModel::Handle".to_owned(),
        (Type::Const(_) | Type::Mut(_), _) => "ArgModel::Pointer".to_owned(),
    }
}

/// A harness for fuzzing the decoding of `kernel_dispatch` on the host,
/// included into the same module as the dispatch. Besides what the
/// dispatch needs, the module provides:
///
/// - a `Record` trait, implemented by every handler parameter type,
///   whose `record(&self)` returns a `Value`;
/// - `record(number: u64, args: &[Value])`, which the handlers call;
/// - `CallModel { number: u64, fuzz_only: bool, args: &'static
///   [ArgModel] }` and an `ArgModel` enum with the variants `U32`, `U64`,
///   `I32`, `I64`, `Handle`, `Pointer` and `Checked { types:
///   Option<&'static [ObjectType]>, rights: u32, allow_invalid: bool }`.
///
/// It defines `handlers`, whose functions record their call and succeed,
/// and `CALLS`, the model of every call.
pub fn fuzz_harness(table: &Table, arch: &str) -> String {
    let mut out = String::from(HEADER);
    out.push_str("pub mod handlers {\n    #[allow(unused_imports)]\n    use super::*;\n");
    // The fuzz build has no calls of the diagnostic build.
    let calls = || calls_for(table, arch).filter(|c| !c.diag_only);
    for call in calls() {
        let params = call
            .args
            .iter()
            .map(|a| format!("{}: {}", a.name, kernel_param(a)))
            .collect::<Vec<_>>()
            .join(", ");
        let values = call
            .args
            .iter()
            .map(|a| format!("{}.record()", a.name))
            .collect::<Vec<_>>()
            .join(", ");
        let cfg = kernel_cfg(call).replace('\n', "\n    ");
        let _ = writeln!(
            out,
            "\n    {cfg}pub fn {}({params}) -> Result<(), kernel_core::status::Error> {{\n        \
             record({}, &[{values}]);\n        \
             Ok(())\n    \
             }}",
            call.name, call.number
        );
    }
    out.push_str("}\n\n/// The model of every call.\npub static CALLS: &[CallModel] = &[\n");
    for call in calls() {
        let args = call
            .args
            .iter()
            .map(arg_model)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "    CallModel {{ number: {}, fuzz_only: {}, args: &[{args}] }},",
            call.number, call.fuzz_only
        );
    }
    out.push_str("];\n");
    out
}

/// The kernel's dispatch for `arch`. It is included into a module that
/// provides:
///
/// - `handle(raw: u32, types: Option<&[ObjectType]>, rights: u32) ->
///   Result<Checked, Error>`, which checks a handle of the calling process;
/// - `UserIn<T>`, `UserOut<T>` and `UserInOut<T>`, each made with
///   `new(addr: u64)`;
/// - `handlers`, a module with one function per call, taking the
///   arguments in order and returning `Result<(), Error>`.
///
/// A checked handle argument is passed as `Checked`, or as
/// `Option<Checked>` when `HANDLE_INVALID` is allowed.
pub fn kernel_dispatch(table: &Table, arch: &str) -> String {
    let mut out = String::from(HEADER);
    for call in calls_for(table, arch) {
        let _ = writeln!(
            out,
            "/// `{}`: {}.\n{}pub const {}: u64 = {};",
            call.name,
            call.spec.join(", "),
            kernel_cfg(call),
            constant(call),
            call.number
        );
    }
    // Each call decodes its arguments in a function of its own, so that
    // `dispatch` does not reserve the locals of every call at once, which
    // unoptimized builds do, on the stack below every handler.
    out.push_str(
        "\n/// Runs system call `number` with the argument registers `args` and\n\
         /// returns its status.\n\
         pub fn dispatch(number: u64, args: &[u64; 8]) -> i64 {\n    \
         let _ = args;\n    \
         match number {\n",
    );
    for call in calls_for(table, arch) {
        let cfg = kernel_cfg(call).replace('\n', "\n        ");
        let _ = writeln!(
            out,
            "        {cfg}{} => call_{}(args),",
            constant(call),
            call.name
        );
    }
    out.push_str("        _ => kernel_core::status::Error::BadSyscall as i64,\n    }\n}\n");
    for call in calls_for(table, arch) {
        let _ = writeln!(
            out,
            "\n/// Decodes the arguments of `{}` and runs it.\n\
             {}#[inline(never)]\n\
             fn call_{}(args: &[u64; 8]) -> i64 {{\n    \
             #[allow(unused_imports)]\n    \
             use kernel_core::object::{{ObjectType, rights}};\n    \
             use kernel_core::status::status;\n    \
             let _ = args;",
            call.name,
            kernel_cfg(call),
            call.name
        );
        let mut names = Vec::new();
        for (i, arg) in call.args.iter().enumerate() {
            let raw = format!("args[{i}]");
            let value = match (&arg.ty, &arg.handle) {
                (Type::Int(t), _) => match t.as_str() {
                    "u32" => format!("{raw} as u32"),
                    "i32" => format!("{raw} as u32 as i32"),
                    "i64" => format!("{raw} as i64"),
                    _ => raw,
                },
                (Type::Handle, Some(check)) => {
                    let (types, rights) = (types_expr(check), rights_expr(check));
                    let checked = format!("handle({raw} as u32, {types}, {rights})");
                    if check.allow_invalid {
                        format!(
                            "match {raw} as u32 {{\n                \
                             kernel_core::object::handles::HANDLE_INVALID => None,\n                \
                             _ => match {checked} {{\n                    \
                             Ok(h) => Some(h),\n                    \
                             Err(e) => return e as i64,\n                \
                             }},\n            \
                             }}"
                        )
                    } else {
                        format!(
                            "match {checked} {{\n                \
                             Ok(h) => h,\n                \
                             Err(e) => return e as i64,\n            \
                             }}"
                        )
                    }
                }
                (Type::Handle, None) => format!("{raw} as u32"),
                (Type::Const(t), _) => format!("UserIn::<{}>::new({raw})", kernel_value(t)),
                (Type::Mut(t), _) => {
                    let wrapper = if arg.dir == Dir::InOut {
                        "UserInOut"
                    } else {
                        "UserOut"
                    };
                    format!("{wrapper}::<{}>::new({raw})", kernel_value(t))
                }
            };
            let _ = writeln!(out, "            let {} = {value};", arg.name);
            names.push(arg.name.as_str());
        }
        let _ = writeln!(
            out,
            "            status(handlers::{}({}))\n}}",
            call.name,
            names.join(", ")
        );
    }
    out
}

/// The user-space Rust type for an argument.
fn user_type(ty: &Type) -> String {
    match ty {
        Type::Int(t) => t.clone(),
        Type::Handle => "Handle".to_owned(),
        Type::Const(t) => format!("*const {t}"),
        Type::Mut(t) => format!("*mut {t}"),
    }
}

/// The value of an argument in its register.
fn user_register(arg: &crate::table::Arg) -> String {
    match &arg.ty {
        Type::Int(t) if t == "i32" => format!("{} as u32 as u64", arg.name),
        Type::Int(t) if t == "u64" => arg.name.clone(),
        Type::Int(_) | Type::Handle => format!("{} as u64", arg.name),
        Type::Const(_) | Type::Mut(_) => format!("{} as u64", arg.name),
    }
}

/// The user-space bindings for x86_64: one function per call, each making
/// the call with `SYSCALL`.
pub fn user_bindings_x86_64(table: &Table) -> String {
    let mut out = String::from(HEADER);
    out.push_str(
        "/// A handle value.\npub type Handle = u32;\n\
         /// The status a system call returns.\npub type Status = i64;\n\n",
    );
    for call in calls_for(table, "x86_64") {
        let params = call
            .args
            .iter()
            .map(|a| format!("{}: {}", a.name, user_type(&a.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut registers = String::new();
        for (arg, reg) in call.args.iter().zip(X86_64_ARG_REGISTERS) {
            let _ = writeln!(
                registers,
                "            in(\"{reg}\") {},",
                user_register(arg)
            );
        }
        let _ = writeln!(
            out,
            "/// `{name}`: {spec}.\n///\n/// # Safety\n///\n\
             /// Pointer arguments must be valid as the call requires.\n\
             {cfg}#[inline]\n\
             pub unsafe fn {name}({params}) -> Status {{\n    \
             let status: i64;\n    \
             // SAFETY: SYSCALL changes only RAX, RCX and R11.\n    \
             unsafe {{\n        \
             core::arch::asm!(\n            \
             \"syscall\",\n            \
             inlateout(\"rax\") {number}u64 => status,\n\
             {registers}            \
             out(\"rcx\") _,\n            \
             out(\"r11\") _,\n            \
             options(nostack),\n        \
             )\n    \
             }};\n    \
             status\n}}\n",
            name = call.name,
            spec = call.spec.join(", "),
            cfg = user_cfg(call),
            number = call.number,
        );
    }
    out
}
