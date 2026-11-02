//! Links the kernel with its linker script when building for the kernel
//! target.

fn main() {
    println!("cargo:rerun-if-changed=link.ld");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "none" {
        let dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
        println!("cargo:rustc-link-arg-bins=-T{dir}/link.ld");
        println!("cargo:rustc-link-arg-bins=--build-id=sha1");
    }
}
