//! The kernel random number generator.
//!
//! The key is replaced by a BLAKE2s-256 hash of the previous key and each
//! new input. Output comes from the ChaCha20 keystream under the key, and
//! the first 32 bytes of the keystream of every request become the next
//! key, so that a later key leaks nothing about earlier output.

use blake2::{Blake2s256, Digest};
use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};

/// The largest request `fill` serves in one call.
pub const FILL_MAX: usize = 4096;

/// Separates the generator's hashes from other uses of BLAKE2s.
const DOMAIN: &[u8] = b"Mikros kernel RNG v1";

pub struct Rng {
    key: [u8; 32],
    seeded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The generator has received no input yet.
    Unseeded,
    /// The request is longer than `FILL_MAX`.
    TooLong,
}

impl Rng {
    pub const fn new() -> Self {
        Rng {
            key: [0; 32],
            seeded: false,
        }
    }

    /// Mixes `inputs` into the key. Each input is hashed with its length,
    /// so that inputs cannot run into each other.
    pub fn reseed(&mut self, inputs: &[&[u8]]) {
        let mut h = Blake2s256::new();
        h.update(DOMAIN);
        h.update(self.key);
        for input in inputs {
            h.update((input.len() as u64).to_le_bytes());
            h.update(input);
        }
        self.key = h.finalize().into();
        self.seeded = true;
    }

    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Fills `out` with random bytes.
    pub fn fill(&mut self, out: &mut [u8]) -> Result<(), Error> {
        if !self.seeded {
            return Err(Error::Unseeded);
        }
        if out.len() > FILL_MAX {
            return Err(Error::TooLong);
        }
        // Each key is used for one request only, so the nonce can be
        // fixed.
        let mut cipher = ChaCha20::new(&self.key.into(), &[0u8; 12].into());
        let mut next = [0u8; 32];
        cipher.apply_keystream(&mut next);
        out.fill(0);
        cipher.apply_keystream(out);
        self.key = next;
        next.fill(0);
        Ok(())
    }
}

impl Default for Rng {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chacha20_matches_rfc_8439() {
        // RFC 8439 section 2.4.2: the keystream for counter 1 starts 64
        // bytes into the keystream that starts at counter 0.
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce = [0, 0, 0, 0, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let mut c = ChaCha20::new(&key.into(), &nonce.into());
        let mut block = [0u8; 128];
        c.apply_keystream(&mut block);
        assert_eq!(
            block[64..80],
            [
                0x22, 0x4f, 0x51, 0xf3, 0x40, 0x1b, 0xd9, 0xe1, 0x2f, 0xde, 0x27, 0x6f, 0xb8, 0x63,
                0x1d, 0xed
            ]
        );
    }

    #[test]
    fn blake2s_matches_rfc_7693() {
        let digest: [u8; 32] = Blake2s256::digest(b"abc").into();
        assert_eq!(
            digest[..8],
            [0x50, 0x8c, 0x5e, 0x8c, 0x32, 0x7c, 0x14, 0xe2]
        );
    }

    #[test]
    fn refuses_until_seeded_and_bounds_requests() {
        let mut rng = Rng::new();
        let mut out = [0u8; 16];
        assert_eq!(rng.fill(&mut out), Err(Error::Unseeded));
        rng.reseed(&[b"seed"]);
        assert!(rng.is_seeded());
        let mut long = std::vec![0u8; FILL_MAX + 1];
        assert_eq!(rng.fill(&mut long), Err(Error::TooLong));
        assert_eq!(rng.fill(&mut long[..FILL_MAX]), Ok(()));
    }

    #[test]
    fn output_depends_on_every_input_and_changes() {
        let output = |inputs: &[&[u8]]| {
            let mut rng = Rng::new();
            rng.reseed(inputs);
            let mut a = [0u8; 32];
            let mut b = [0u8; 32];
            rng.fill(&mut a).unwrap();
            rng.fill(&mut b).unwrap();
            assert_ne!(a, b);
            a
        };
        let base = output(&[b"ab", b"c"]);
        assert_eq!(base, output(&[b"ab", b"c"]));
        assert_ne!(base, output(&[b"a", b"bc"]));
        assert_ne!(base, output(&[b"ab", b"d"]));
    }
}
