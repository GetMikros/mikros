//! exFAT volumes, for the exFAT server: the layout that a volume's boot
//! region gives, after its checksum, from the main region or its backup.

#![no_std]

mod boot;

pub use boot::Layout;
pub use mikros_fat::Error;

#[cfg(test)]
mod tests;
