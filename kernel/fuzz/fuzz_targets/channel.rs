//! Writes and reads messages on a channel and compares the queues with
//! lists of the messages written.

#![no_main]

use kernel_core::object::channel::{CALL_ID, Endpoint, Message};
use kernel_core::object::handles::Entry;
use kernel_core::object::rights::ALL;
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::VecDeque;
use std::sync::Arc;

struct Plain(Header);

impl Object for Plain {
    fn header(&self) -> &Header {
        &self.0
    }
}

#[derive(Arbitrary, Debug)]
enum Op {
    /// Writes from end 0 or 1 a message of `len` bytes, repeated
    /// `repeat` times, with `handles` handles.
    Write { from: bool, len: u16, repeat: u8, handles: u8 },
    Read { at: bool, refuse: bool },
    Close { which: bool },
    /// Starts a call through end 0.
    Call,
    /// Writes from end 1 a reply to an earlier call, picked by index,
    /// or a message with any ID.
    Reply { call: u8, raw: Option<u32> },
    TakeReply(u8),
    Abandon(u8),
}

fuzz_target!(|ops: Vec<Op>| {
    let (a, b) = Endpoint::pair(None).unwrap();
    let mut ends = [Some(a), Some(b)];
    // The messages queued for each end to read: their bytes and handle
    // counts.
    let mut queued: [VecDeque<(Vec<u8>, usize)>; 2] = Default::default();
    let object: Arc<dyn Object> = Arc::new(Plain(Header::new(ObjectType::Event)));
    let mut sequence = 0u8;
    // Calls of end 0: their ID and whether their reply arrived.
    let mut calls: Vec<(u32, bool)> = Vec::new();
    let mut abandoned: Vec<u32> = Vec::new();
    for op in ops.iter().take(512) {
        match *op {
            Op::Write { from, len, repeat, handles } => {
                let (from, to) = (usize::from(from), usize::from(!from));
                let Some(end) = &ends[from] else { continue };
                for _ in 0..=repeat % 4 {
                    sequence = sequence.wrapping_add(1);
                    let bytes = vec![sequence; usize::from(len) % 70_000];
                    let handles: Vec<Entry> = (0..handles % 4)
                        .map(|_| Entry { object: object.clone(), rights: ALL })
                        .collect();
                    let count = handles.len();
                    let message = Message::new(bytes.clone(), handles, None).unwrap();
                    match end.write(message) {
                        Ok(_) => queued[to].push_back((bytes, count)),
                        Err((e, _)) => assert!(matches!(e, Error::ShouldWait | Error::PeerClosed | Error::OutOfRange)),
                    }
                }
            }
            Op::Read { at, refuse } => {
                let at = usize::from(at);
                let Some(end) = &ends[at] else { continue };
                let expected = queued[at].front().cloned();
                let result = end.read(|bytes, handles| {
                    assert_eq!(Some((bytes.to_vec(), handles.len())), expected);
                    if refuse { Err(Error::BufferTooSmall) } else { Ok(()) }
                });
                match (result, expected) {
                    (Ok(()), Some(_)) => {
                        assert!(!refuse);
                        queued[at].pop_front();
                    }
                    (Err(Error::BufferTooSmall), Some(_)) => assert!(refuse),
                    (Err(Error::ShouldWait), None) => assert!(ends[1 - at].is_some()),
                    (Err(Error::PeerClosed), None) => assert!(ends[1 - at].is_none()),
                    (r, e) => panic!("read gave {r:?} with {e:?} queued"),
                }
            }
            Op::Call => {
                let Some(end) = &ends[0] else { continue };
                let id = end.begin_call(None).unwrap();
                assert!(id & CALL_ID != 0);
                assert!(!calls.iter().any(|c| c.0 == id) && !abandoned.contains(&id));
                calls.push((id, false));
            }
            Op::Reply { call, raw } => {
                let Some(end) = &ends[1] else { continue };
                let id = match raw {
                    Some(id) => id,
                    None if calls.is_empty() => continue,
                    None => calls[usize::from(call) % calls.len()].0,
                };
                let message = Message::new(id.to_le_bytes().to_vec(), Vec::new(), None).unwrap();
                let result = end.write(message);
                let open = calls.iter().position(|c| c.0 == id && !c.1);
                if ends[0].is_none() {
                    assert!(result.is_err());
                } else if let Some(i) = open {
                    assert_eq!(result.ok().flatten().map(|r| r.1), Some(id));
                    calls[i].1 = true;
                } else if let Some(i) = abandoned.iter().position(|&a| a == id) {
                    assert!(matches!(result, Ok(None)));
                    abandoned.swap_remove(i);
                } else if let Ok(None) = result {
                    queued[0].push_back((id.to_le_bytes().to_vec(), 0));
                }
            }
            Op::TakeReply(i) => {
                let Some(end) = &ends[0] else { continue };
                if calls.is_empty() {
                    continue;
                }
                let i = usize::from(i) % calls.len();
                let (id, arrived) = calls[i];
                let reply = end.take_reply(id).expect("replies carry no endpoints");
                assert_eq!(reply.is_some(), arrived);
                if let Some(reply) = reply {
                    assert_eq!(reply.bytes, id.to_le_bytes());
                    calls.swap_remove(i);
                }
            }
            Op::Abandon(i) => {
                let Some(end) = &ends[0] else { continue };
                if calls.is_empty() {
                    continue;
                }
                let (id, arrived) = calls.swap_remove(usize::from(i) % calls.len());
                assert_eq!(end.abandon_call(id).is_some(), arrived);
                if !arrived {
                    abandoned.push(id);
                }
            }
            Op::Close { which } => {
                let which = usize::from(which);
                ends[which] = None;
                // A closed end's queue is discarded.
                queued[which].clear();
            }
        }
    }
});
