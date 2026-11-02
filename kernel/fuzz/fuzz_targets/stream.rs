//! Writes, reads and closes stream endpoints and compares the queues
//! with lists of the bytes written.

#![no_main]

use kernel_core::object::Object;
use kernel_core::object::signals::{PEER_CLOSED, READABLE, WRITABLE};
use kernel_core::object::stream::{ATOMIC_WRITE, QUEUE_BYTES, StreamEnd};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::VecDeque;

#[derive(Arbitrary, Debug)]
enum Op {
    Write { from: bool, len: u16, repeat: u8 },
    Read { at: bool, max: u16, consume: bool },
    Close { which: bool },
}

fuzz_target!(|ops: Vec<Op>| {
    let (a, b) = StreamEnd::pair(None).unwrap();
    let mut ends = [Some(a), Some(b)];
    let mut queued: [VecDeque<u8>; 2] = Default::default();
    let mut next = 0u8;
    for op in ops.iter().take(256) {
        match *op {
            Op::Write { from, len, repeat } => {
                let (from, to) = (usize::from(from), usize::from(!from));
                let Some(end) = &ends[from] else { continue };
                for _ in 0..=repeat % 8 {
                    let bytes: Vec<u8> = (0..usize::from(len) * 2)
                        .map(|_| {
                            next = next.wrapping_add(1);
                            next
                        })
                        .collect();
                    let room = QUEUE_BYTES - queued[to].len();
                    let expected = if ends[to].is_none() {
                        Err(Error::PeerClosed)
                    } else if room == 0 || (bytes.len() <= ATOMIC_WRITE && bytes.len() > room) {
                        Err(Error::ShouldWait)
                    } else {
                        Ok(bytes.len().min(room))
                    };
                    assert_eq!(end.write(&bytes), expected);
                    if let Ok(n) = expected {
                        queued[to].extend(&bytes[..n]);
                    }
                }
            }
            Op::Read { at, max, consume } => {
                let at = usize::from(at);
                let Some(end) = &ends[at] else { continue };
                let mut buf = vec![0u8; usize::from(max)];
                // A consuming read copies and removes under one lock.
                let result = if consume { end.read(&mut buf) } else { end.peek(&mut buf) };
                if queued[at].is_empty() {
                    let closed = ends[1 - at].is_none();
                    assert_eq!(result, Err(if closed { Error::PeerClosed } else { Error::ShouldWait }));
                    continue;
                }
                let n = result.unwrap();
                assert_eq!(n, buf.len().min(queued[at].len()));
                assert!(buf[..n].iter().eq(queued[at].iter().take(n)));
                if consume {
                    queued[at].drain(..n);
                }
            }
            Op::Close { which } => {
                ends[usize::from(which)] = None;
            }
        }
        for side in 0..2 {
            let Some(end) = &ends[side] else { continue };
            let s = end.header().signals();
            assert_eq!(s & READABLE != 0, !queued[side].is_empty());
            let peer_open = ends[1 - side].is_some();
            assert_eq!(s & PEER_CLOSED != 0, !peer_open);
            assert_eq!(s & WRITABLE != 0, peer_open && queued[1 - side].len() < QUEUE_BYTES);
        }
    }
});
