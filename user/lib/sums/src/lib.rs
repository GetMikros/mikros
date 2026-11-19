//! What sha256sum and md5sum share (MIKSPEC-0098.R51): writing the sums
//! of files as lines or JSON, reading lists of sums back and checking
//! them, by the algorithm that each command gives.

mod lines;
mod run;

pub use lines::{Form, Listed, Tally, escape, json, line, parse};
pub use run::run;

use mikros_cli::Spec;
use std::io::{self, Read};

/// A sum command's algorithm: its name in lines of the BSD form and in
/// messages, the hexadecimal digits of its sums, how it hashes, and the
/// command's tables.
pub struct Algorithm {
    pub name: &'static str,
    pub digits: usize,
    pub digest: fn(&mut dyn Read) -> io::Result<String>,
    pub command: &'static Spec,
}

/// Hands what `input` holds to `update`, piece by piece.
pub fn feed(input: &mut dyn Read, mut update: impl FnMut(&[u8])) -> io::Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => update(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// `bytes` in lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
