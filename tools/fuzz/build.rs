//! Generates, from every MIDL file in `midl/`, the Rust bindings against
//! the mock kernel of midl-tests, and `serve`, which serves a channel as
//! the server of one of the protocols, chosen by its index in
//! `PROTOCOLS`.

use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let midl = root.join("../../midl");
    println!("cargo::rerun-if-changed={}", midl.display());
    let mut paths = Vec::new();
    for library in std::fs::read_dir(&midl).expect("midl/ exists") {
        let library = library.expect("midl/ is readable").path();
        if library.is_dir() {
            for file in std::fs::read_dir(&library).expect("readable") {
                let file = file.expect("readable").path();
                if file.extension().is_some_and(|e| e == "midl") {
                    paths.push(file);
                }
            }
        }
    }
    paths.sort();
    let texts: Vec<String> = paths
        .iter()
        .map(|p| std::fs::read_to_string(p).expect("readable"))
        .collect();
    let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let sources: Vec<(&str, &str)> = names
        .iter()
        .map(String::as_str)
        .zip(texts.iter().map(String::as_str))
        .collect();
    let model = midlc::compile(&sources, None).unwrap_or_else(|e| panic!("{e}"));
    let options = midlc::rust::Options {
        kernel: "crate::mock".into(),
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo"));
    std::fs::write(out.join("midl.rs"), midlc::rust::generate(&model, &options))
        .expect("OUT_DIR is writable");

    let (mut names, mut arms) = (String::new(), String::new());
    let mut count = 0;
    for library in &model.libraries {
        let module = library.name.replace('.', "::");
        for decl in &library.decls {
            if let midlc::model::DeclKind::Protocol(_) = decl.kind {
                let name = &decl.name;
                writeln!(names, "    \"{}.{name}\",", library.name).unwrap();
                writeln!(
                    arms,
                    "        {count} => {{\n            let server = crate::midl::{module}::{name}Server::new(ServerEnd::new(end));\n            while let Ok(Some(_)) = server.try_next() {{}}\n        }}"
                )
                .unwrap();
                count += 1;
            }
        }
    }
    let serve = format!(
        "/// The protocols that `serve` serves, by index.\npub const PROTOCOLS: &[&str] = &[\n{names}];\n\n\
         /// Serves `end` as the server of protocol `index` of `PROTOCOLS`,\n\
         /// decoding every request queued on it, until the first that\n\
         /// does not decode.\n\
         fn serve(index: usize, end: OwnedHandle) {{\n    match index {{\n{arms}        _ => drop(end),\n    }}\n}}\n"
    );
    std::fs::write(out.join("serve.rs"), serve).expect("OUT_DIR is writable");
}
