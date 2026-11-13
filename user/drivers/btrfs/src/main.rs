// SPDX-License-Identifier: GPL-2.0
//! The btrfs driver: `driver` serves a btrfs volume's files, folders and
//! subvolumes with Linux's btrfs code, which `linux` runs on the btrfs
//! shim, the C code in `shim/` on the Mikros architecture of `linux/`.
//! Only `linux`, with its tests on the host's build of the shim, is built
//! for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod driver;
mod linux;

#[cfg(not(target_os = "mikros"))]
fn main() {}
