//! Deadlines on the monotonic clock.

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

/// The deadline that never expires.
pub const INFINITE: i64 = i64::MAX;

/// Items waiting for deadlines, earliest first.
pub struct Deadlines<T: PartialEq> {
    /// Sorted by deadline, latest first, so that expired items come off
    /// the end.
    items: Vec<(i64, T)>,
}

impl<T: PartialEq> Default for Deadlines<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: PartialEq> Deadlines<T> {
    /// The size of the entry of one item.
    pub const ENTRY_SIZE: usize = size_of::<(i64, T)>();

    pub const fn new() -> Self {
        Deadlines { items: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds `item` to wait for `deadline`, after items with the same
    /// deadline. An item waiting for `INFINITE` never expires.
    pub fn insert(&mut self, deadline: i64, item: T) -> Result<(), TryReserveError> {
        self.items.try_reserve(1)?;
        let at = self.items.partition_point(|&(d, _)| d > deadline);
        self.items.insert(at, (deadline, item));
        Ok(())
    }

    /// The earliest deadline.
    pub fn next(&self) -> Option<i64> {
        self.items.last().map(|&(d, _)| d)
    }

    /// The earliest deadline that can expire.
    pub fn next_finite(&self) -> Option<i64> {
        self.next().filter(|&d| d != INFINITE)
    }

    /// Takes an item whose deadline is at or before `now`.
    pub fn pop_expired(&mut self, now: i64) -> Option<T> {
        if self.next_finite()? <= now {
            self.items.pop().map(|(_, item)| item)
        } else {
            None
        }
    }

    /// Removes `item`, returning whether it was waiting.
    pub fn remove(&mut self, item: &T) -> bool {
        let Some(i) = self.items.iter().position(|(_, it)| it == item) else {
            return false;
        };
        self.items.remove(i);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_in_deadline_order() {
        let mut d = Deadlines::new();
        d.insert(30, 'c').unwrap();
        d.insert(10, 'a').unwrap();
        d.insert(20, 'b').unwrap();
        d.insert(10, 'x').unwrap();
        assert_eq!(d.next(), Some(10));
        assert_eq!(d.pop_expired(9), None);
        assert_eq!(d.pop_expired(10), Some('a'));
        assert_eq!(d.pop_expired(10), Some('x'));
        assert_eq!(d.pop_expired(10), None);
        assert!(d.remove(&'c'));
        assert!(!d.remove(&'c'));
        d.insert(INFINITE, 'z').unwrap();
        assert_eq!(d.pop_expired(i64::MAX - 1), Some('b'));
        assert_eq!(d.next_finite(), None);
        assert_eq!(d.pop_expired(INFINITE), None);
        assert_eq!(d.len(), 1);
    }
}
