//! Tests of the Rust bindings that midlc generates, run on the host
//! against a mock kernel.

extern crate alloc;

pub mod mock;

#[allow(clippy::all, unused)]
pub mod midl {
    include!(concat!(env!("OUT_DIR"), "/midl.rs"));
}

#[cfg(test)]
mod tests;
