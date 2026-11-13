//! The permission broker: `service` answers whether a process holds a
//! permission, from the grants it keeps and the manifests it reads, with
//! `broker`; `permissions` names the permissions and their kinds. Only
//! `permissions`, with its tests, is built for the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod broker;
mod permissions;
#[cfg(target_os = "mikros")]
mod service;

#[cfg(not(target_os = "mikros"))]
fn main() {}
