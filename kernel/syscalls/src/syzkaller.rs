//! syzkaller descriptions generated from the system call table.

use crate::table::{Arg, Call, Dir, OBJECT_TYPES, Table, Type};
use std::fmt::Write;

const HEADER: &str = "# @generated from kernel/syscalls.yaml by the syscall-table crate.\n\
                      # Do not edit.\n\n";

/// The syzkaller resource of handles to objects of `object`.
fn resource(object: &str) -> String {
    let mut name = String::from("h_");
    for (i, c) in object.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_lowercase());
    }
    name
}

/// The syzkaller integer type of the Rust integer type `ty`.
fn int(ty: &str) -> &'static str {
    match ty {
        "u8" => "int8",
        "u32" | "i32" => "int32",
        _ => "int64",
    }
}

/// Whether `arg`, an integer, gives the length of the pointer argument
/// before it, by its name.
fn is_length(arg: &Arg) -> bool {
    let n = arg.name.as_str();
    matches!(n, "len" | "count") || n.ends_with("_len") || n.ends_with("_cap")
}

/// The syzkaller type of argument `i` of `call`.
fn arg_type(call: &Call, i: usize) -> String {
    let arg = &call.args[i];
    let direction = match arg.dir {
        Dir::In => "in",
        Dir::Out => "out",
        Dir::InOut => "inout",
    };
    match &arg.ty {
        Type::Handle => match arg.handle.as_ref().and_then(|h| h.types.as_deref()) {
            Some([object]) => resource(object),
            _ => "handle".into(),
        },
        Type::Int(ty) => {
            let before = i.checked_sub(1).map(|j| &call.args[j]);
            match before {
                Some(p) if is_length(arg) && matches!(p.ty, Type::Const(_) | Type::Mut(_)) => {
                    format!("len[{}, {}]", p.name, int(ty))
                }
                _ => int(ty).into(),
            }
        }
        Type::Const(pointee) | Type::Mut(pointee) => {
            let followed_by_length = call.args.get(i + 1).is_some_and(is_length);
            let element = match pointee.as_str() {
                "Handle" => arg.returns.as_deref().map_or("handle".into(), resource),
                other => int(other).into(),
            };
            if followed_by_length {
                format!("ptr[{direction}, array[{element}]]")
            } else {
                format!("ptr[{direction}, {element}]")
            }
        }
    }
}

/// The syzkaller description of the calls of `arch`: one resource for
/// handles, one derived from it for each object type, and one call
/// description per call.
pub fn descriptions(table: &Table, arch: &str) -> String {
    let mut out = String::from(HEADER);
    out.push_str("resource handle[int32]: 0\n");
    for object in OBJECT_TYPES {
        let _ = writeln!(out, "resource {}[handle]", resource(object));
    }
    out.push('\n');
    for call in table
        .calls
        .iter()
        .filter(|c| c.arches.iter().any(|a| a == arch))
    {
        let args: Vec<String> = (0..call.args.len())
            .map(|i| format!("{} {}", call.args[i].name, arg_type(call, i)))
            .collect();
        let _ = writeln!(out, "{}({})", call.name, args.join(", "));
    }
    out
}

/// The syzkaller constants file of `arch`: the number of each call.
pub fn constants(table: &Table, arch: &str) -> String {
    let mut out = String::from(HEADER);
    let syz_arch = if arch == "x86_64" { "amd64" } else { arch };
    let _ = writeln!(out, "arches = {syz_arch}");
    for call in table
        .calls
        .iter()
        .filter(|c| c.arches.iter().any(|a| a == arch))
    {
        let _ = writeln!(out, "__NR_{} = {}", call.name, call.number);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::read;

    fn table(args: &str) -> Table {
        let text = format!(
            "syscalls:\n  - name: obj_op\n    number: 7\n    arch: [x86_64]\n    \
             fuzz_only: false\n    spec: [MIKSPEC-0010.R10]\n    args:\n{args}"
        );
        read(text.as_bytes()).unwrap().table
    }

    #[test]
    fn names_resources_by_object_type() {
        assert_eq!(resource("Process"), "h_process");
        assert_eq!(resource("MemoryObject"), "h_memory_object");
        assert_eq!(resource("LogReader"), "h_log_reader");
    }

    #[test]
    fn describes_calls_and_numbers() {
        let t = table(
            "      - {name: a, type: Handle, dir: in, object: Budget, rights: [WRITE]}\n\
             \x20     - {name: b, type: Handle, dir: in, object: any, rights: []}\n\
             \x20     - {name: name, type: '*const u8', dir: in}\n\
             \x20     - {name: name_len, type: u64, dir: in}\n\
             \x20     - {name: value, type: i32, dir: in}\n\
             \x20     - {name: out, type: '*mut Handle', dir: out, object: Process}\n\
             \x20     - {name: any, type: '*mut Handle', dir: out}\n",
        );
        let d = descriptions(&t, "x86_64");
        assert!(d.contains("resource handle[int32]: 0\n"));
        assert!(d.contains("resource h_memory_object[handle]\n"));
        assert!(d.contains(
            "obj_op(a h_budget, b handle, name ptr[in, array[int8]], name_len len[name, int64], \
             value int32, out ptr[out, h_process], any ptr[out, handle])\n"
        ));
        assert!(!descriptions(&t, "aarch64").contains("obj_op"));
        let c = constants(&t, "x86_64");
        assert!(c.contains("arches = amd64\n"));
        assert!(c.contains("__NR_obj_op = 7\n"));
    }
}
