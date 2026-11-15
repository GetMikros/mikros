//! Who gets the keyboard: the terminal of the console shown, or an
//! authorization request, and the key that repeats while it is held.
//!
//! Every key goes through the router, which hands its bytes to the owner
//! of the keyboard at the time the key was pressed, as the device's
//! timestamp says, not at the time the console gets to the event. A key
//! pressed under an owner that is gone by then is dropped, never handed
//! to the next owner, and changing the owner stops the repeating key. The
//! routes are typed, so that the bytes of a key meant for an
//! authorization request cannot be written to a terminal: only the router
//! makes `TerminalInput` and `SecureKey`.

use alloc::vec::Vec;

/// How long a key is held before it repeats, and how often it then does,
/// in nanoseconds of the monotonic clock.
pub const REPEAT_DELAY: i64 = 500_000_000;
pub const REPEAT_INTERVAL: i64 = 33_000_000;

/// Who the keyboard belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The terminal, or the login prompt, of the console shown.
    Terminal,
    /// The authorization request with this number.
    Secure(u32),
}

/// The bytes of a key for the console shown.
#[derive(Debug, PartialEq, Eq)]
pub struct TerminalInput(Vec<u8>);

impl TerminalInput {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// The bytes of a key for an authorization request, which are wiped when
/// they are dropped.
#[derive(Debug, PartialEq, Eq)]
pub struct SecureKey(Vec<u8>);

impl SecureKey {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// A key with `bytes`, for the tests of what takes keys.
    #[cfg(test)]
    pub fn for_test(bytes: &[u8]) -> SecureKey {
        SecureKey(bytes.to_vec())
    }
}

impl Drop for SecureKey {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // SAFETY: the pointer refers to a byte of the vector.
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
    }
}

/// Where the bytes of a key go.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Terminal(TerminalInput),
    Secure(u32, SecureKey),
    /// Nowhere: the key was pressed under an owner that is gone, or has
    /// no bytes.
    Dropped,
}

pub struct Router {
    owner: Owner,
    /// When the keyboard got its owner.
    since: i64,
    /// The key that repeats while held, and when it next does.
    repeat: Option<(u16, i64)>,
}

impl Default for Router {
    fn default() -> Router {
        Router {
            owner: Owner::Terminal,
            since: i64::MIN,
            repeat: None,
        }
    }
}

impl Router {
    pub fn owner(&self) -> Owner {
        self.owner
    }

    /// Gives the keyboard to `owner` from `now` on. A key pressed before
    /// stays with the owner it was pressed under, and stops repeating.
    pub fn set_owner(&mut self, owner: Owner, now: i64) {
        if owner != self.owner {
            self.owner = owner;
            self.since = now;
            self.repeat = None;
        }
    }

    /// Routes `bytes`, those of key `code` that was pressed at `at`, and
    /// starts its repeat at `now` when the key `repeats`.
    pub fn press(
        &mut self,
        code: u16,
        at: i64,
        bytes: Option<Vec<u8>>,
        repeats: bool,
        now: i64,
    ) -> Route {
        let Some(bytes) = bytes else {
            return Route::Dropped;
        };
        if at < self.since {
            return Route::Dropped;
        }
        self.repeat = repeats.then_some((code, now + REPEAT_DELAY));
        self.deliver(bytes)
    }

    /// Takes in the release of key `code`.
    pub fn release(&mut self, code: u16) {
        if self.repeat.is_some_and(|(c, _)| c == code) {
            self.repeat = None;
        }
    }

    /// When the held key next repeats.
    pub fn next_repeat(&self) -> Option<i64> {
        self.repeat.map(|(_, at)| at)
    }

    /// The repeat of the held key once it is due at `now`, with the bytes
    /// that `bytes_of` gives for its code then.
    pub fn repeat(&mut self, now: i64, bytes_of: impl FnOnce(u16) -> Option<Vec<u8>>) -> Route {
        let Some((code, at)) = self.repeat else {
            return Route::Dropped;
        };
        if now < at {
            return Route::Dropped;
        }
        self.repeat = Some((code, now + REPEAT_INTERVAL));
        match bytes_of(code) {
            Some(bytes) => self.deliver(bytes),
            None => Route::Dropped,
        }
    }

    fn deliver(&self, bytes: Vec<u8>) -> Route {
        match self.owner {
            Owner::Terminal => Route::Terminal(TerminalInput(bytes)),
            Owner::Secure(request) => Route::Secure(request, SecureKey(bytes)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const A: u16 = 30;
    const B: u16 = 48;

    fn press(r: &mut Router, code: u16, at: i64) -> Route {
        r.press(code, at, Some(vec![code as u8]), true, at)
    }

    #[test]
    fn hands_keys_to_the_owner_they_were_pressed_under() {
        let mut r = Router::default();
        assert_eq!(
            press(&mut r, A, 10),
            Route::Terminal(TerminalInput(vec![A as u8]))
        );
        r.set_owner(Owner::Secure(7), 20);
        // A key pressed before the request, handled after it came, goes
        // nowhere, and neither does one pressed during it, handled after.
        assert_eq!(press(&mut r, A, 15), Route::Dropped);
        assert_eq!(
            press(&mut r, B, 25),
            Route::Secure(7, SecureKey(vec![B as u8]))
        );
        r.set_owner(Owner::Terminal, 30);
        assert_eq!(press(&mut r, B, 28), Route::Dropped);
        assert_eq!(
            press(&mut r, B, 31),
            Route::Terminal(TerminalInput(vec![B as u8]))
        );
        assert_eq!(r.press(A, 40, None, true, 40), Route::Dropped);
    }

    #[test]
    fn stops_the_repeat_when_the_owner_changes() {
        let mut r = Router::default();
        r.set_owner(Owner::Secure(1), 0);
        press(&mut r, A, 10);
        let bytes = |code: u16| Some(vec![code as u8]);
        assert_eq!(r.repeat(10 + REPEAT_DELAY - 1, bytes), Route::Dropped);
        assert_eq!(
            r.repeat(10 + REPEAT_DELAY, bytes),
            Route::Secure(1, SecureKey(vec![A as u8]))
        );
        r.set_owner(Owner::Terminal, 1_000_000_000);
        assert_eq!(r.next_repeat(), None);
        assert_eq!(r.repeat(i64::MAX, bytes), Route::Dropped);
        // Releasing another key leaves the repeat; releasing it ends it.
        press(&mut r, B, 2_000_000_000);
        r.release(A);
        assert!(r.next_repeat().is_some());
        r.release(B);
        assert_eq!(r.next_repeat(), None);
    }

    /// A small generator of pseudo-random numbers, for the sequences of
    /// the invariant test.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Over many random sequences of presses, releases, repeats, late
    /// events and changes of owner: no key reaches a terminal unless the
    /// terminal owned the keyboard when it was pressed, or a request
    /// other than the one it was pressed under, and a repeat never
    /// outlives its owner.
    #[test]
    fn never_crosses_owners() {
        let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
        for _ in 0..200 {
            let mut r = Router::default();
            // The owner at each moment, to judge each route by.
            let mut history: Vec<(i64, Owner)> = vec![(i64::MIN, Owner::Terminal)];
            let owner_at = |history: &[(i64, Owner)], at: i64| {
                history
                    .iter()
                    .rev()
                    .find(|&&(t, _)| t <= at)
                    .map(|&(_, o)| o)
            };
            let mut now = 0i64;
            let mut request = 0u32;
            // The owner that the repeating key was pressed under.
            let mut repeating: Option<Owner> = None;
            for _ in 0..500 {
                now += (random.next() % 50_000_000) as i64;
                match random.next() % 6 {
                    0 => {
                        request += 1;
                        r.set_owner(Owner::Secure(request), now);
                        history.push((now, Owner::Secure(request)));
                        repeating = None;
                    }
                    1 => {
                        if r.owner() != Owner::Terminal {
                            repeating = None;
                        }
                        r.set_owner(Owner::Terminal, now);
                        history.push((now, Owner::Terminal));
                    }
                    2 | 3 => {
                        // Events may be handled up to a second late.
                        let late = (random.next() % 1_000_000_000) as i64;
                        let at = now - late;
                        let code = (random.next() % 4) as u16;
                        let route = r.press(code, at, Some(vec![1]), true, now);
                        let pressed_under = owner_at(&history, at);
                        if route != Route::Dropped {
                            repeating = Some(r.owner());
                        }
                        match route {
                            Route::Terminal(_) => {
                                assert_eq!(pressed_under, Some(Owner::Terminal));
                                assert_eq!(r.owner(), Owner::Terminal);
                            }
                            Route::Secure(id, _) => {
                                assert_eq!(pressed_under, Some(Owner::Secure(id)));
                                assert_eq!(r.owner(), Owner::Secure(id));
                            }
                            Route::Dropped => {}
                        }
                    }
                    4 => r.release((random.next() % 4) as u16),
                    _ => {
                        let route = r.repeat(now, |_| Some(vec![2]));
                        // A repeat only carries a key pressed under the
                        // owner it goes to.
                        match route {
                            Route::Terminal(_) => {
                                assert_eq!(repeating, Some(Owner::Terminal));
                                assert_eq!(r.owner(), Owner::Terminal);
                            }
                            Route::Secure(id, _) => {
                                assert_eq!(repeating, Some(Owner::Secure(id)));
                                assert_eq!(r.owner(), Owner::Secure(id));
                            }
                            Route::Dropped => {}
                        }
                    }
                }
            }
        }
    }
}
