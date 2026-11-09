//! Links ACPICA with the ACPI server's glue, which `cargo xtask acpica`
//! builds into target/acpica/mikros.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let dir = manifest.join("../../target/acpica/mikros");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=acpica");
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("libacpica.a").display()
    );
}
