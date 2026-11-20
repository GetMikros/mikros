//! Tests of the C bindings that midlc generates, built from c/tests.c on
//! the host against the mock kernel and compared with the Rust bindings.

use crate::midl::__midl::{self as rt, Decoder, Encoder, Error, Header};
use crate::midl::test::wire::*;
use crate::mock;
use crate::tests::{everything, owned, serial, serve};

const OK: i32 = 0;
const INVALID: i32 = -1000;
const TOO_LONG: i32 = -1001;
const UNKNOWN_MEMBER: i32 = -1002;
const ERROR: i32 = 1;

const HEADER: Header = Header {
    txid: 0,
    ordinal: 9,
    flags: 0,
};

unsafe extern "C" {
    fn c_encode_everything(size: u32, out: *mut u8) -> u32;
    fn c_encode_wrong(size: u32, change: i32) -> i32;
    fn c_reencode_everything(
        bytes: *const u8,
        len: u32,
        size: u32,
        out: *mut u8,
        out_len: *mut u32,
    ) -> i32;
    fn c_decode_holder(
        bytes: *const u8,
        len: u32,
        size: u32,
        handles: *mut u32,
        count: u32,
        out: *mut u32,
    ) -> i32;
    fn c_add(channel: u32, a: u32, b: u32, sum: *mut u32) -> i32;
    fn c_divide(channel: u32, a: u32, b: u32, quotient: *mut u32, error: *mut i32) -> i32;
    fn c_pass(channel: u32, h: u32, back: *mut u32) -> i32;
    fn c_note(channel: u32, text: *const u8, out: *mut u8) -> i32;
}

fn size() -> u32 {
    Everything::__SIZE as u32
}

fn rust_bytes() -> Vec<u8> {
    let mut e = Encoder::new(HEADER, Everything::__SIZE);
    everything().__encode(&mut e, rt::HEADER_SIZE).unwrap();
    e.finish().0
}

fn rust_decode(bytes: &[u8]) -> Result<Everything, Error> {
    let mut d = Decoder::new(bytes, Vec::new());
    d.header()?;
    let at = d.claim(Everything::__SIZE)?;
    let value = Everything::__decode(&mut d, at)?;
    d.finish()?;
    Ok(value)
}

/// The C bindings' status for `bytes`, and what they encode again.
fn c_reencode(bytes: &[u8]) -> (i32, Vec<u8>) {
    let mut out = vec![0u8; 65536];
    let mut len = 0;
    // SAFETY: the buffers have the lengths given.
    let s = unsafe {
        c_reencode_everything(
            bytes.as_ptr(),
            bytes.len() as u32,
            size(),
            out.as_mut_ptr(),
            &mut len,
        )
    };
    out.truncate(len as usize);
    (s, out)
}

#[test]
fn encodes_as_the_rust_bindings_do() {
    let _s = serial();
    let mut out = vec![0u8; 65536];
    // SAFETY: the buffer holds a channel message.
    let len = unsafe { c_encode_everything(size(), out.as_mut_ptr()) };
    out.truncate(len as usize);
    assert_eq!(out, rust_bytes());
}

#[test]
fn refuses_values_that_do_not_fit_their_types() {
    let _s = serial();
    // SAFETY: plain values.
    let status = |change| unsafe { c_encode_wrong(size(), change) };
    assert_eq!(status(0), OK);
    assert_eq!(status(1), TOO_LONG, "a string over its bound");
    assert_eq!(status(2), TOO_LONG, "a strict enumeration value");
    assert_eq!(status(3), TOO_LONG, "a strict bit");
    assert_eq!(status(4), UNKNOWN_MEMBER, "an unknown union member");
}

#[test]
fn decodes_what_the_rust_bindings_encode() {
    let _s = serial();
    let bytes = rust_bytes();
    assert_eq!(c_reencode(&bytes), (OK, bytes));
}

/// Every message that differs from a valid one in one byte is accepted
/// by the C bindings exactly when the Rust bindings accept it, and then
/// decodes to the same value.
#[test]
fn checks_messages_as_the_rust_bindings_do() {
    let _s = serial();
    let good = rust_bytes();
    let mut rejected = 0;
    for at in 0..good.len() {
        for value in [0, 1, 2, 0x80, 0xff, good[at] ^ 1] {
            let mut bytes = good.clone();
            bytes[at] = value;
            let (status, again) = c_reencode(&bytes);
            match rust_decode(&bytes) {
                Ok(v) => {
                    assert_eq!(status, OK, "byte {at} set to {value}");
                    // A member of a flexible union that this version does
                    // not declare is decoded, but not encoded again.
                    let mut e = Encoder::new(HEADER, Everything::__SIZE);
                    let expected = match v.__encode(&mut e, rt::HEADER_SIZE) {
                        Ok(()) => e.finish().0,
                        Err(_) => Vec::new(),
                    };
                    assert_eq!(again, expected, "byte {at} set to {value}");
                }
                Err(_) => {
                    assert_eq!(status, INVALID, "byte {at} set to {value}");
                    rejected += 1;
                }
            }
        }
    }
    assert!(rejected > good.len(), "most changes are rejected");
    let mut longer = good.clone();
    longer.extend([0; 8]);
    assert_eq!(c_reencode(&longer).0, INVALID, "trailing bytes");
    assert_eq!(
        c_reencode(&good[..good.len() - 8]).0,
        INVALID,
        "a short message"
    );
}

fn holder(event_rights: u32) -> (Vec<u8>, Vec<u32>) {
    let value = Holder {
        any: owned(mock::event(mock::READ)),
        event: owned(mock::event(event_rights)),
        maybe: None,
        events: vec![owned(mock::event(mock::READ))],
    };
    let mut e = Encoder::new(HEADER, Holder::__SIZE);
    value.__encode(&mut e, rt::HEADER_SIZE).unwrap();
    e.finish()
}

fn c_holder((bytes, mut handles): (Vec<u8>, Vec<u32>)) -> (i32, [u32; 5]) {
    let mut out = [0u32; 5];
    // SAFETY: the buffers have the lengths given.
    let s = unsafe {
        c_decode_holder(
            bytes.as_ptr(),
            bytes.len() as u32,
            Holder::__SIZE as u32,
            handles.as_mut_ptr(),
            handles.len() as u32,
            out.as_mut_ptr(),
        )
    };
    (s, out)
}

#[test]
fn checks_and_narrows_the_rights_of_handles() {
    let _s = serial();
    let before = mock::open_handles();
    let (s, out) = c_holder(holder(mock::READ | mock::WRITE | 0x1));
    assert_eq!(s, OK);
    assert_eq!(
        mock::handle_info(out[1]),
        Some((mock::EVENT, mock::READ | mock::WRITE))
    );
    assert_eq!(out[2], 0, "the absent handle");
    assert_ne!(out[3], 0);
    assert_eq!(out[4], 0, "the vector has one handle");
    for h in [out[0], out[1], out[3]] {
        mock::handle_close(h);
    }
    assert_eq!(mock::open_handles(), before);
    // Without WRITE, the message is refused and all its handles closed.
    let (s, _) = c_holder(holder(mock::READ));
    assert_eq!(s, INVALID);
    assert_eq!(mock::open_handles(), before);
}

#[test]
fn calls_a_rust_server() {
    let _s = serial();
    let before = mock::open_handles();
    let (client, server_end) = rt::endpoints::<Calc>().unwrap();
    let server = std::thread::spawn(move || serve(CalcServer::new(server_end)));
    let channel = client.into_handle().into_raw();
    let mut sum = 0;
    // SAFETY: the out pointers are valid for each call.
    unsafe {
        assert_eq!(c_add(channel, 2, 3, &mut sum), OK);
        assert_eq!(sum, 5);
        let (mut quotient, mut error) = (0, 0);
        assert_eq!(c_divide(channel, 7, 2, &mut quotient, &mut error), OK);
        assert_eq!(quotient, 3);
        assert_eq!(c_divide(channel, 7, 0, &mut quotient, &mut error), ERROR);
        assert_eq!(error, -5, "Status.FAILED");
        let event = mock::event(mock::READ);
        let mut back = 0;
        assert_eq!(c_pass(channel, event, &mut back), OK);
        assert_eq!(mock::handle_info(back), Some((mock::EVENT, mock::READ)));
        mock::handle_close(back);
        let mut text = [0u8; 17];
        assert_eq!(
            c_note(channel, c"echo".as_ptr().cast(), text.as_mut_ptr()),
            OK
        );
        assert_eq!(&text[..5], b"echo\0");
    }
    mock::handle_close(channel);
    server.join().unwrap();
    assert_eq!(mock::open_handles(), before);
}
