//! Decodes a message as a request of one of the protocols in `midl/`:
//! the first byte picks the protocol, the second how many handles, up to
//! 3, come with the bytes that follow. No message may panic, and every
//! handle must be closed after it.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let [pick, handles, bytes @ ..] = data else {
        return;
    };
    let index = usize::from(*pick) % user_fuzz::PROTOCOLS.len();
    user_fuzz::decode(index, bytes, usize::from(handles % 4));
});
