//! The system libraries in `midl/` at the repository root compile.

use std::fs;
use std::path::Path;

#[test]
fn system_libraries_compile() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../midl");
    let mut sources = Vec::new();
    for library in fs::read_dir(&root).unwrap() {
        for file in fs::read_dir(library.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|e| e == "midl") {
                let text = fs::read_to_string(&path).unwrap();
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
    let names: Vec<_> = model.libraries.iter().map(|l| l.name.as_str()).collect();
    for name in [
        "mikros.base",
        "mikros.fs",
        "mikros.service",
        "mikros.process",
    ] {
        assert!(names.contains(&name), "{name} is missing from {names:?}");
    }
}
