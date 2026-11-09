// SPDX-License-Identifier: GPL-2.0
//! The shim finds the initcalls by the linker's start and stop symbols of
//! their sections, which the linker must keep for those symbols alone,
//! as mikros-btrfs's build script says.

fn main() {
    println!("cargo:rustc-link-arg-bins=-znostart-stop-gc");
}
