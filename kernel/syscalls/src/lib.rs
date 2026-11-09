//! The Mikros system call table: reading it, and generating code from it.

pub mod c;
pub mod generate;
pub mod syzkaller;
pub mod table;
pub mod yaml;

#[cfg(test)]
mod tests;
