//! Links FreeType, which `cargo xtask` builds as a port before the
//! programs with std.

use std::path::PathBuf;

fn main() {
    // The host builds only the tests, which link neither.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("mikros") {
        return;
    }
    let root =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo")).join("../../..");
    let lib = root.join("target/ported/freetype/install/system/libraries");
    println!("cargo:rustc-link-search=native={}", lib.display());
    // Relinked whenever the library is built again, whose name it needs.
    println!(
        "cargo:rerun-if-changed={}",
        lib.join("libfreetype.dlib").display()
    );
    println!("cargo:rustc-link-lib=dylib=freetype");
}
