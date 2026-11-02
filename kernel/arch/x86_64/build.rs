//! Links the kernel with its linker script when building for the kernel
//! target, and generates the system call dispatch from
//! `kernel/syscalls.yaml`.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=link.ld");
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "none" {
        println!("cargo:rustc-link-arg-bins=-T{dir}/link.ld");
        println!("cargo:rustc-link-arg-bins=--build-id=sha1");
    }

    let table_path = PathBuf::from(&dir).join("../../syscalls.yaml");
    println!("cargo:rerun-if-changed={}", table_path.display());
    let shown = "kernel/syscalls.yaml";
    let bytes = std::fs::read(&table_path).unwrap_or_else(|e| panic!("{shown}: {e}"));
    let read = syscall_table::table::read(&bytes).unwrap_or_else(|e| panic!("{shown}:{e}"));
    for (key, at) in read.ignored {
        println!(
            "cargo:warning={shown}:{}:{}: ignored the unknown key `{key}`",
            at.line, at.column
        );
    }
    let code = syscall_table::generate::kernel_dispatch(&read.table, "x86_64");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("syscalls.rs");
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
