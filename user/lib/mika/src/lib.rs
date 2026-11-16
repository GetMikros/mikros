//! `.mika` packages (MIKSPEC-0101): their IDs and versions and their file
//! format, which the install service, `pkg` and the process manager
//! share.

#![no_std]

extern crate alloc;

pub mod format;
pub mod id;
pub mod version;
