//! Generates the system call bindings from `kernel/syscalls.yaml`, and
//! the bindings of the system libraries in `midl/`.

use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let table_path = PathBuf::from(&dir).join("../../../kernel/syscalls.yaml");
    println!("cargo:rerun-if-changed={}", table_path.display());
    let shown = "kernel/syscalls.yaml";
    let bytes = std::fs::read(&table_path).unwrap_or_else(|e| panic!("{shown}: {e}"));
    let read = syscall_table::table::read(&bytes).unwrap_or_else(|e| panic!("{shown}:{e}"));
    let code = syscall_table::generate::user_bindings_x86_64(&read.table);
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("syscalls.rs");
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    system_libraries(&PathBuf::from(&dir).join("../../../midl"));
}

/// Compiles every `.midl` file under `root` into `interfaces.rs`.
fn system_libraries(root: &Path) {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut sources = Vec::new();
    for library in std::fs::read_dir(root).unwrap_or_else(|e| panic!("midl: {e}")) {
        let library = library.unwrap_or_else(|e| panic!("midl: {e}")).path();
        println!("cargo:rerun-if-changed={}", library.display());
        for file in std::fs::read_dir(&library).unwrap_or_else(|e| panic!("midl: {e}")) {
            let path = file.unwrap_or_else(|e| panic!("midl: {e}")).path();
            if path.extension().is_some_and(|e| e == "midl") {
                println!("cargo:rerun-if-changed={}", path.display());
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                sources.push((path.display().to_string(), text));
            }
        }
    }
    sources.sort();
    let sources: Vec<_> = sources
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let model = midlc::compile(&sources, None).unwrap_or_else(|e| panic!("{e}"));
    let options = midlc::rust::Options {
        kernel: "crate::midl".into(),
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("interfaces.rs");
    std::fs::write(&out, midlc::rust::generate(&model, &options))
        .unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
