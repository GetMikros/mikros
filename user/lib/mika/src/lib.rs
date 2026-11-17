//! `.mika` packages (MIKSPEC-0101): their IDs and versions, their file
//! format and their manifests, which the install service, `pkg` and the process manager
//! share, and the library names of the ELF images in them.

#![no_std]

extern crate alloc;

pub mod elf;
pub mod format;
pub mod id;
pub mod manifest;
pub mod version;
