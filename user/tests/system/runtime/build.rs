//! Generates the bindings of the test library in `midl/`.

fn main() {
    println!("cargo::rerun-if-changed=midl");
    let path = "midl/test.midl";
    let text = std::fs::read_to_string(path).expect("midl/test.midl is readable");
    let model = midlc::compile(&[(path, &text)], None).unwrap_or_else(|e| panic!("{e}"));
    let options = midlc::rust::Options {
        kernel: "mikros_rt::midl".into(),
    };
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("midl.rs"), midlc::rust::generate(&model, &options))
        .expect("OUT_DIR is writable");
}
