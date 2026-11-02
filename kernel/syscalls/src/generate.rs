//! Rust code generated from the system call table.

use crate::table::{Call, Dir, Table, Type};
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

fn fuzz_cfg(call: &Call) -> &'static str {
    if call.fuzz_only {
        "#[cfg(feature = \"fuzz\")]\n"
    } else {
        ""
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
            fuzz_cfg(call),
            constant(call),
            call.number
        );
    }
    out.push_str(
        "\n/// Runs system call `number` with the argument registers `args` and\n\
         /// returns its status.\n\
         pub fn dispatch(number: u64, args: &[u64; 8]) -> i64 {\n    \
         #[allow(unused_imports)]\n    \
         use kernel_core::object::{ObjectType, rights};\n    \
         use kernel_core::status::{Error, status};\n    \
         let _ = args;\n    \
         match number {\n",
    );
    for call in calls_for(table, arch) {
        let cfg = fuzz_cfg(call).replace('\n', "\n        ");
        let _ = writeln!(out, "        {cfg}{} => {{", constant(call));
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
                    let types = match &check.types {
                        None => "None".to_owned(),
                        Some(types) => format!(
                            "Some(&[{}])",
                            types
                                .iter()
                                .map(|t| format!("ObjectType::{t}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    let rights = if check.rights.is_empty() {
                        "0".to_owned()
                    } else {
                        check
                            .rights
                            .iter()
                            .map(|r| format!("rights::{r}"))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    };
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
            "            status(handlers::{}({}))\n        }}",
            call.name,
            names.join(", ")
        );
    }
    out.push_str("        _ => Error::BadSyscall as i64,\n    }\n}\n");
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
            cfg = fuzz_cfg(call),
            number = call.number,
        );
    }
    out
}
