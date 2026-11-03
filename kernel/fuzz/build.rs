//! Generates, from `kernel/syscalls.yaml`, the kernel's system call
//! dispatch and the harness that the `syscall_args` target runs it with.

use std::path::PathBuf;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let table_path = PathBuf::from(&dir).join("../syscalls.yaml");
    println!("cargo:rerun-if-changed={}", table_path.display());
    let shown = "kernel/syscalls.yaml";
    let bytes = std::fs::read(&table_path).unwrap_or_else(|e| panic!("{shown}: {e}"));
    let read = syscall_table::table::read(&bytes).unwrap_or_else(|e| panic!("{shown}:{e}"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo"));
    for (file, code) in [
        (
            "syscalls.rs",
            syscall_table::generate::kernel_dispatch(&read.table, "x86_64"),
        ),
        (
            "harness.rs",
            syscall_table::generate::fuzz_harness(&read.table, "x86_64"),
        ),
    ] {
        let path = out.join(file);
        std::fs::write(&path, code).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}
