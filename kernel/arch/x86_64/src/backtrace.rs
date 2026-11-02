//! Walking the frame pointer chain.

/// The most frames a backtrace shows.
pub const MAX_FRAMES: usize = 64;

/// The return addresses of the frames whose chain starts at `rbp`, read
/// with `read`. Only frame records that lie wholly in `stack` (start and
/// end addresses) are followed, and each must be above the one before, so
/// the walk ends at the outermost frame, whose saved RBP is zero, without
/// reading outside the stack. A zero return address, which the entry
/// point's frame holds, also ends it.
pub fn frames(rbp: u64, stack: (u64, u64), read: impl Fn(u64) -> u64) -> impl Iterator<Item = u64> {
    let mut next = rbp;
    core::iter::from_fn(move || {
        let rbp = next;
        let inside = rbp % 8 == 0 && rbp >= stack.0 && rbp.checked_add(16)? <= stack.1;
        if !inside {
            return None;
        }
        let saved = read(rbp);
        // A frame further out is at a higher address; anything else ends
        // the walk after this frame.
        next = if saved > rbp { saved } else { 0 };
        let address = read(rbp + 8);
        (address != 0).then_some(address)
    })
    .take(MAX_FRAMES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::vec::Vec;

    fn walk(memory: &[(u64, u64)], rbp: u64) -> Vec<u64> {
        let memory: HashMap<u64, u64> = memory.iter().copied().collect();
        frames(rbp, (0x1000, 0x2000), |a| memory[&a]).collect()
    }

    #[test]
    fn follows_the_chain_to_the_outermost_frame() {
        let memory = [(0x1100, 0x1200), (0x1108, 0xa), (0x1200, 0), (0x1208, 0xb)];
        assert_eq!(walk(&memory, 0x1100), [0xa, 0xb]);
        let memory = [(0x1100, 0x1200), (0x1108, 0xa), (0x1200, 0), (0x1208, 0)];
        assert_eq!(walk(&memory, 0x1100), [0xa]);
    }

    #[test]
    fn stops_at_records_outside_the_stack_or_out_of_order() {
        let memory = [(0x1100, 0x5000), (0x1108, 0xa)];
        assert_eq!(walk(&memory, 0x1100), [0xa]);
        let memory = [(0x1100, 0x1080), (0x1108, 0xa)];
        assert_eq!(walk(&memory, 0x1100), [0xa]);
        assert_eq!(walk(&[], 0x1ff8), Vec::<u64>::new());
        assert_eq!(walk(&[], 0x1104), Vec::<u64>::new());
        assert_eq!(walk(&[], 0), Vec::<u64>::new());
    }

    #[test]
    fn limits_the_number_of_frames() {
        let memory: Vec<_> = (0..100u64)
            .flat_map(|i| {
                let at = 0x1000 + i * 16;
                [(at, at + 16), (at + 8, i + 1)]
            })
            .collect();
        let memory: HashMap<u64, u64> = memory.into_iter().collect();
        let n = frames(0x1000, (0x1000, 0x2000), |a| memory[&a]).count();
        assert_eq!(n, MAX_FRAMES);
    }
}
