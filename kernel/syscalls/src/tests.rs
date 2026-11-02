use crate::generate::{kernel_dispatch, user_bindings_x86_64};
use crate::table::{Dir, HandleCheck, Type, read};

const ENTRY: &str = "  - name: obj_op\n    number: 7\n    arch: [x86_64]\n    fuzz_only: false\n    spec: [MIKSPEC-0007.R12]\n";

fn table(args: &str) -> String {
    format!("syscalls:\n{ENTRY}    args:\n{args}")
}

fn error(text: &str) -> String {
    read(text.as_bytes()).expect_err(text).message
}

#[test]
fn reads_the_kernel_table() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../syscalls.yaml")).unwrap();
    let read = read(&text).unwrap_or_else(|e| panic!("syscalls.yaml:{e}"));
    assert!(read.ignored.is_empty(), "{:?}", read.ignored);
    let close = &read.table.calls[0];
    assert_eq!((close.name.as_str(), close.number), ("handle_close", 1));
    assert_eq!(
        close.args[0].handle,
        Some(HandleCheck {
            types: None,
            rights: vec![],
            allow_invalid: true
        })
    );
    // The generated code is valid Rust syntax as far as balance goes.
    for code in [
        kernel_dispatch(&read.table, "x86_64"),
        user_bindings_x86_64(&read.table),
    ] {
        let depth = code.chars().fold(0i32, |d, c| match c {
            '{' | '(' | '[' => d + 1,
            '}' | ')' | ']' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}

#[test]
fn reads_arguments() {
    let text = table(
        "      - {name: h, type: Handle, dir: in, object: [Process, Thread], rights: [MANAGE, INSPECT]}\n\
         \x20     - {name: p, type: '*const u8', dir: in}\n\
         \x20     - {name: o, type: '*mut u64', dir: inout}\n\
         \x20     - {name: n, type: i32, dir: in, note: ignored}\n",
    );
    let read = read(text.as_bytes()).unwrap();
    let args = &read.table.calls[0].args;
    assert_eq!(
        args[0].handle.as_ref().unwrap().types,
        Some(vec!["Process".to_owned(), "Thread".to_owned()])
    );
    assert_eq!(args[1].ty, Type::Const("u8".to_owned()));
    assert_eq!(
        (&args[2].ty, args[2].dir),
        (&Type::Mut("u64".to_owned()), Dir::InOut)
    );
    assert_eq!(args[3].ty, Type::Int("i32".to_owned()));
    assert_eq!(read.ignored.len(), 1);
    assert_eq!(read.ignored[0].0, "note");
}

#[test]
fn refuses_bad_entries() {
    let nine: String = (0..9)
        .map(|i| format!("      - {{name: x{i}, type: u64, dir: in}}\n"))
        .collect();
    let cases = [
        (
            "      - {name: h, type: Handle, dir: in, rights: []}\n",
            "missing key `object`",
        ),
        (
            "      - {name: h, type: Handle, dir: in, object: Widget, rights: []}\n",
            "unknown object type `Widget`",
        ),
        (
            "      - {name: h, type: Handle, dir: in, object: any, rights: [FLY]}\n",
            "unknown right `FLY`",
        ),
        (
            "      - {name: x, type: u64, dir: in, rights: []}\n",
            "`rights` applies only to handles",
        ),
        (
            "      - {name: x, type: '*mut u8', dir: in}\n",
            "`x` cannot have this direction",
        ),
        (
            "      - {name: x, type: '*const u8', dir: out}\n",
            "`x` cannot have this direction",
        ),
        (
            "      - {name: x, type: f64, dir: in}\n",
            "unknown type `f64`",
        ),
        (
            "      - {name: x, type: '*const str', dir: in}\n",
            "unknown pointer target `str`",
        ),
        (
            "      - {name: X, type: u64, dir: in}\n",
            "bad argument name `X`",
        ),
        (
            "      - {name: x, type: u64, dir: in}\n      - {name: x, type: u64, dir: in}\n",
            "`obj_op` has two arguments named `x`",
        ),
        (nine.as_str(), "`obj_op` has more than 8 arguments"),
    ];
    for (args, message) in cases {
        assert_eq!(error(&table(args)), message, "{args}");
    }
    let twice = format!(
        "syscalls:\n{ENTRY}{}",
        ENTRY.replace("number: 7", "number: 8")
    );
    assert_eq!(error(&twice), "`obj_op` is defined twice");
    let clash = format!("syscalls:\n{ENTRY}{}", ENTRY.replace("obj_op", "other"));
    assert_eq!(error(&clash), "`other` has the number 7 of `obj_op`");
    let spec = format!(
        "syscalls:\n{}",
        ENTRY.replace("MIKSPEC-0007.R12", "MIKSPEC-7.R1")
    );
    assert_eq!(error(&spec), "`MIKSPEC-7.R1` is not a requirement ID");
    let arch = format!("syscalls:\n{}", ENTRY.replace("x86_64", "vax"));
    assert_eq!(error(&arch), "unknown architecture `vax`");
    let fuzz = format!("syscalls:\n{}", ENTRY.replace("false", "'false'"));
    assert_eq!(error(&fuzz), "expected `true` or `false`");
}

#[test]
fn generates_handle_checks_and_bindings() {
    let text = table(
        "      - {name: a, type: Handle, dir: in, object: any, rights: [], allow_invalid: true}\n\
         \x20     - {name: b, type: Handle, dir: in, object: Budget, rights: [WRITE, MANAGE]}\n\
         \x20     - {name: c, type: i32, dir: in}\n\
         \x20     - {name: d, type: '*mut Handle', dir: out}\n",
    );
    let mut table = read(text.as_bytes()).unwrap().table;
    let kernel = kernel_dispatch(&table, "x86_64");
    assert!(kernel.contains("pub const OBJ_OP: u64 = 7;"));
    assert!(kernel.contains("handle(args[0] as u32, None, 0)"));
    assert!(kernel.contains("HANDLE_INVALID => None"));
    assert!(kernel.contains(
        "handle(args[1] as u32, Some(&[ObjectType::Budget]), rights::WRITE | rights::MANAGE)"
    ));
    assert!(kernel.contains("let c = args[2] as u32 as i32;"));
    assert!(kernel.contains("UserOut::<kernel_core::object::handles::Handle>::new(args[3])"));
    assert!(kernel.contains("status(handlers::obj_op(a, b, c, d))"));
    let user = user_bindings_x86_64(&table);
    assert!(
        user.contains(
            "pub unsafe fn obj_op(a: Handle, b: Handle, c: i32, d: *mut Handle) -> Status"
        )
    );
    assert!(user.contains("inlateout(\"rax\") 7u64 => status"));
    assert!(user.contains("in(\"rdi\") a as u64"));
    assert!(user.contains("in(\"rdx\") c as u32 as u64"));
    assert!(user.contains("in(\"r10\") d as u64"));

    table.calls[0].fuzz_only = true;
    let kernel = kernel_dispatch(&table, "x86_64");
    assert_eq!(kernel.matches("#[cfg(feature = \"fuzz\")]").count(), 2);
    table.calls[0].arches.clear();
    assert!(!kernel_dispatch(&table, "x86_64").contains("OBJ_OP"));
    assert!(!user_bindings_x86_64(&table).contains("obj_op"));
}
