//! Generates bindings for the test libraries in `midl/`, against the mock
//! kernel in `src/mock.rs`.

fn main() {
    let dir = std::path::Path::new("midl");
    println!("cargo::rerun-if-changed=midl");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("midl/ exists")
        .map(|e| e.expect("midl/ is readable").path())
        .collect();
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
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("midl.rs"), midlc::rust::generate(&model, &options))
        .expect("OUT_DIR is writable");
}
