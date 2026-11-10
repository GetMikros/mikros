//! Password hashes: Argon2id (version 0x13) with a 16-byte salt, 65536
//! KiB of memory, 3 iterations, 4 lanes and a 32-byte output, as PHC
//! strings.

use alloc::string::{String, ToString};
use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};

pub const SALT_LEN: usize = 16;
const MEMORY_KIB: u32 = 65536;
const ITERATIONS: u32 = 3;
const LANES: u32 = 4;
const OUTPUT_LEN: usize = 32;

fn argon2() -> Option<Argon2<'static>> {
    let params = Params::new(MEMORY_KIB, ITERATIONS, LANES, Some(OUTPUT_LEN)).ok()?;
    Some(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// The hash of `password` with `salt`, or `None` when the memory for it
/// could not be had.
pub fn hash(password: &[u8], salt: &[u8; SALT_LEN]) -> Option<String> {
    let hash = argon2()?.hash_password_with_salt(password, salt).ok()?;
    Some(hash.to_string())
}

/// Whether `phc` is the hash of `password` with this module's
/// parameters. A hash with other parameters never matches.
pub fn verify(password: &[u8], phc: &str) -> bool {
    let Some(argon2) = argon2() else {
        return false;
    };
    phc.starts_with(PREFIX) && argon2.verify_password(password, phc).is_ok()
}

/// How every hash that `hash` makes starts.
const PREFIX: &str = "$argon2id$v=19$m=65536,t=3,p=4$";
