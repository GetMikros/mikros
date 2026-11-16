//! `.mika` packages (MIKSPEC-0101): their IDs and versions, which the
//! install service, `pkg` and the process manager share.

#![no_std]

extern crate alloc;

pub mod id;
pub mod version;
