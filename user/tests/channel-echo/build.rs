//! Links the program as a static executable at a fixed address, which is
//! the kind of executable that the test init's loader, mikros-test's
//! `elf`, loads.

fn main() {
    println!("cargo:rustc-link-arg-bins=--no-pie");
}
