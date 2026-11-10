// SPDX-License-Identifier: GPL-2.0
//! Links the library of Linux's btrfs code on the shim, which
//! `cargo xtask btrfs` builds into target/btrfs/<flavor>: the host's for
//! the build host, Mikros's for Mikros. The shim finds the initcalls by
//! the linker's start and stop symbols of their sections, which the
//! linker must keep for those symbols alone: a program that links the
//! crate needs `-z nostart-stop-gc` too, as its tests do.

use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").expect("cargo sets TARGET");
    let flavor = if target.contains("mikros") {
        "mikros"
    } else {
        "host"
    };
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let dir = manifest.join("../../../target/btrfs").join(flavor);
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=btrfs");
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("libbtrfs.a").display()
    );
    let keep = if flavor == "mikros" {
        "-znostart-stop-gc"
    } else {
        "-Wl,-z,nostart-stop-gc"
    };
    println!("cargo:rustc-link-arg={keep}");
}
