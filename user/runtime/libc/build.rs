//! Generates the C library's system call functions from
//! `kernel/syscalls.yaml`.

use std::path::PathBuf;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let table_path = PathBuf::from(&dir).join("../../../kernel/syscalls.yaml");
    println!("cargo:rerun-if-changed={}", table_path.display());
    let shown = "kernel/syscalls.yaml";
    let bytes = std::fs::read(&table_path).unwrap_or_else(|e| panic!("{shown}: {e}"));
    let read = syscall_table::table::read(&bytes).unwrap_or_else(|e| panic!("{shown}:{e}"));
    let code = syscall_table::c::definitions(&read.table, "x86_64");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("syscalls.rs");
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
