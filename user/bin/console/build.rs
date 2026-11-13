//! Links FreeType and xkbcommon, which `cargo xtask` builds as ports
//! before the programs with std.

use std::path::PathBuf;

fn main() {
    // The host builds only the tests, which link neither.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("mikros") {
        return;
    }
    let root =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo")).join("../../..");
    for port in ["freetype", "xkbcommon"] {
        let lib = root
            .join("target/ported")
            .join(port)
            .join("install/usr/lib");
        println!("cargo:rustc-link-search=native={}", lib.display());
    }
    println!("cargo:rustc-link-lib=dylib=freetype");
    println!("cargo:rustc-link-lib=dylib=xkbcommon");
}
