use crate::midl::__midl::{self as rt, Decoder, Encoder, Error, Header, OwnedHandle};
use crate::midl::test::wire::*;
use crate::mock;
use std::sync::Mutex;

/// The mock kernel's handle count is shared, so tests run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const HEADER: Header = Header {
    txid: 0,
    ordinal: 9,
    flags: 0,
};

fn owned(raw: u32) -> OwnedHandle {
    // SAFETY: tests hand each new mock handle to exactly one owner.
    unsafe { OwnedHandle::from_raw(raw) }
}

fn everything() -> Everything {
    Everything {
        a: 7,
        b: -2,
        c: 0x1234,
        f: 1.5,
        text: "hello".into(),
        maybe: None,
        numbers: vec![1, 2, 3],
        nested: vec![Inner {
            flag: true,
            value: 9,
        }],
        words: vec!["ab".into(), "".into(), "wxyz".into()],
        fixed: [1, 2, 3],
        inner: Inner {
            flag: false,
            value: u64::MAX,
        },
        boxed: Some(Box::new(Inner {
            flag: true,
            value: 1,
        })),
        color: Color::Green,
        level: Level::from_raw(42),
        mode: Mode::READ | Mode::WRITE,
        loose: Loose(0x81),
        choice: Choice::Text("union".into()),
        absent: None,
        extra: Extra {
            first: None,
            third: Some("third".into()),
        },
    }
}

fn encode_everything(value: Everything) -> Vec<u8> {
    let mut e = Encoder::new(HEADER, Everything::__SIZE);
    value.__encode(&mut e, rt::HEADER_SIZE).unwrap();
    e.finish().0
}

fn decode_everything(bytes: &[u8]) -> Result<Everything, Error> {
    let mut d = Decoder::new(bytes, Vec::new());
    assert_eq!(d.header()?, HEADER);
    let at = d.claim(Everything::__SIZE)?;
    let value = Everything::__decode(&mut d, at)?;
    d.finish()?;
    Ok(value)
}

#[test]
fn round_trips_every_kind_of_value() {
    let _s = serial();
    let bytes = encode_everything(everything());
    assert_eq!(bytes.len() % 8, 0);
    assert_eq!(decode_everything(&bytes).unwrap(), everything());
    assert_eq!(everything().level, Level::Unknown(42));
}

#[test]
fn lays_out_structures_as_the_wire_format_says() {
    let _s = serial();
    let mut e = Encoder::new(HEADER, Inner::__SIZE);
    Inner {
        flag: true,
        value: 0x0102,
    }
    .__encode(&mut e, rt::HEADER_SIZE)
    .unwrap();
    let (bytes, _) = e.finish();
    let mut expected = vec![0, 0, 0, 0, 9, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    expected.extend([1, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(bytes, expected);
}

#[test]
fn rejects_invalid_messages() {
    let _s = serial();
    let good = encode_everything(everything());
    let mutate = |at: usize, value: u8| {
        let mut bytes = good.clone();
        bytes[at] = value;
        decode_everything(&bytes)
    };
    let body = rt::HEADER_SIZE;
    // `a` is followed by three bytes of padding before `b`.
    assert_eq!(mutate(body + 1, 1), Err(Error::Invalid), "padding");
    assert_eq!(mutate(8, 2), Err(Error::Invalid), "version");
    assert_eq!(mutate(12, 1), Err(Error::Invalid), "reserved header bytes");
    let mut longer = good.clone();
    longer.extend([0; 8]);
    assert_eq!(
        decode_everything(&longer),
        Err(Error::Invalid),
        "trailing bytes"
    );
    assert_eq!(
        decode_everything(&good[..good.len() - 8]),
        Err(Error::Invalid),
        "truncated"
    );

    let mut e = Encoder::new(HEADER, Inner::__SIZE);
    Inner {
        flag: true,
        value: 0,
    }
    .__encode(&mut e, body)
    .unwrap();
    let (mut inner, _) = e.finish();
    inner[body] = 2;
    let mut d = Decoder::new(&inner, Vec::new());
    let at = d.claim(Inner::__SIZE).unwrap();
    assert_eq!(Inner::__decode(&mut d, at), Err(Error::Invalid), "bool 2");

    assert_eq!(Color::__from_wire(3), Err(Error::Invalid));
    assert_eq!(Mode::__from_wire(0x4), Err(Error::Invalid));
    assert_eq!(Loose::__from_wire(0x4), Ok(Loose(0x4)));
}

#[test]
fn refuses_to_send_values_over_their_bounds() {
    let _s = serial();
    let mut too_long = everything();
    too_long.text = "x".repeat(17);
    let mut e = Encoder::new(HEADER, Everything::__SIZE);
    assert_eq!(
        too_long.__encode(&mut e, rt::HEADER_SIZE),
        Err(Error::TooLong)
    );
    let mut too_many = everything();
    too_many.numbers = vec![0; 5];
    let mut e = Encoder::new(HEADER, Everything::__SIZE);
    assert_eq!(
        too_many.__encode(&mut e, rt::HEADER_SIZE),
        Err(Error::TooLong)
    );
}

#[test]
fn skips_unknown_members_of_flexible_unions_and_tables() {
    let _s = serial();
    let mut value = everything();
    value.choice = Choice::Number(5);
    value.extra = Extra {
        first: Some(1),
        third: Some("t".into()),
    };
    let mut bytes = encode_everything(value);
    // `choice` is followed by `absent` and `extra`, 16 bytes each.
    // Renumber its member to an ordinal this version does not declare.
    let choice = rt::HEADER_SIZE + Everything::__SIZE - 48;
    bytes[choice] = 9;
    let decoded = decode_everything(&bytes).unwrap();
    assert_eq!(decoded.choice, Choice::Unknown(9));

    let mut e = Encoder::new(HEADER, 16);
    Strict::Number(1).__encode(&mut e, rt::HEADER_SIZE).unwrap();
    let (mut strict, _) = e.finish();
    strict[rt::HEADER_SIZE] = 2;
    let mut d = Decoder::new(&strict, Vec::new());
    let at = d.claim(16).unwrap();
    assert_eq!(Strict::__decode(&mut d, at), Err(Error::Invalid));
}

fn holder_message(event_rights: u32, event_kind_ok: bool) -> (Vec<u8>, Vec<u32>) {
    let event = if event_kind_ok {
        mock::event(event_rights)
    } else {
        mock::channel_create().unwrap().0
    };
    let holder = Holder {
        any: owned(mock::event(0x1)),
        event: owned(event),
        maybe: None,
        events: vec![owned(mock::event(0x7))],
    };
    let mut e = Encoder::new(HEADER, Holder::__SIZE);
    holder.__encode(&mut e, rt::HEADER_SIZE).unwrap();
    e.finish()
}

fn decode_holder(bytes: &[u8], handles: Vec<u32>) -> Result<Holder, Error> {
    let mut d = Decoder::new(bytes, handles);
    d.header()?;
    let at = d.claim(Holder::__SIZE)?;
    let value = Holder::__decode(&mut d, at)?;
    d.finish()?;
    Ok(value)
}

#[test]
fn checks_and_reduces_handle_rights() {
    let _s = serial();
    let before = mock::open_handles();
    let (bytes, handles) = holder_message(mock::READ | mock::WRITE | 0x80, true);
    let holder = decode_holder(&bytes, handles).unwrap();
    assert_eq!(
        mock::handle_info(holder.event.raw()),
        Some((mock::EVENT, mock::READ | mock::WRITE))
    );
    assert_eq!(
        mock::handle_info(holder.any.raw()),
        Some((mock::EVENT, 0x1))
    );
    // A type without rights checks the type and keeps the rights.
    assert_eq!(
        mock::handle_info(holder.events[0].raw()),
        Some((mock::EVENT, 0x7))
    );
    drop(holder);
    assert_eq!(mock::open_handles(), before);

    let (bytes, handles) = holder_message(mock::READ, true);
    assert_eq!(
        decode_holder(&bytes, handles).unwrap_err(),
        Error::Invalid,
        "missing a right"
    );
    let (bytes, handles) = holder_message(mock::READ | mock::WRITE, false);
    assert_eq!(
        decode_holder(&bytes, handles).unwrap_err(),
        Error::Invalid,
        "wrong object type"
    );
    let (bytes, mut handles) = holder_message(mock::READ | mock::WRITE, true);
    handles.push(mock::event(0));
    assert_eq!(
        decode_holder(&bytes, handles).unwrap_err(),
        Error::Invalid,
        "an extra handle"
    );
    // Two channel ends stay from the wrong-type case; everything else
    // that the rejected messages carried is closed.
    assert_eq!(mock::open_handles(), before + 1);
}

fn serve(server: CalcServer) {
    loop {
        match server.next(i64::MAX) {
            Ok(CalcRequest::Add(r, responder)) => {
                responder
                    .reply(&server, CalcAddResponse { sum: r.a + r.b })
                    .unwrap();
            }
            Ok(CalcRequest::Divide(r, responder)) => {
                let result = match r.b {
                    0 => Err(Status::Failed),
                    b => Ok(CalcDivideResponse { quotient: r.a / b }),
                };
                responder.reply(&server, result).unwrap();
            }
            Ok(CalcRequest::Note(r)) => server.send_noted(r.text).unwrap(),
            Ok(CalcRequest::Pass(r, responder)) => responder
                .reply(&server, CalcPassResponse { h: r.h })
                .unwrap(),
            Err(_) => return,
        }
    }
}

#[test]
fn calls_methods_and_receives_events() {
    let _s = serial();
    let (client_end, server_end) = rt::endpoints::<Calc>().unwrap();
    let server = std::thread::spawn(move || serve(CalcServer::new(server_end)));
    let client = CalcClient::new(client_end);
    assert_eq!(
        client.add(2, 3, i64::MAX).unwrap(),
        CalcAddResponse { sum: 5 }
    );
    assert_eq!(
        client.divide(9, 3, i64::MAX).unwrap(),
        Ok(CalcDivideResponse { quotient: 3 })
    );
    assert_eq!(client.divide(9, 0, i64::MAX).unwrap(), Err(Status::Failed));
    client.note("hi".into()).unwrap();
    assert_eq!(
        client.next_event(i64::MAX).unwrap(),
        CalcEvent::Noted(CalcNotedEvent { text: "hi".into() })
    );
    let event = mock::event(0x3);
    let back = client.pass(owned(event), i64::MAX).unwrap();
    assert_eq!(mock::handle_info(back.h.raw()), Some((mock::EVENT, 0x3)));
    drop(client);
    server.join().unwrap();
}

#[test]
fn answers_unknown_flexible_methods_and_closes_on_errors() {
    let _s = serial();
    let (client_end, server_end) = rt::endpoints::<Calc>().unwrap();
    let server = std::thread::spawn(move || serve(CalcServer::new(server_end)));
    let raw = client_end.into_handle();
    let unknown = Encoder::new(
        Header {
            txid: 0,
            ordinal: 77,
            flags: rt::FLAG_FLEXIBLE,
        },
        0,
    );
    let (bytes, handles) = unknown.finish();
    let (reply, _) = mock::channel_call(raw.raw(), &bytes, &handles, i64::MAX).unwrap();
    let d = Decoder::new(&reply, Vec::new());
    let header = d.header().unwrap();
    assert_eq!(
        (header.ordinal, header.flags),
        (77, rt::FLAG_UNKNOWN_METHOD)
    );
    // A strict request with an undeclared ordinal is a protocol error,
    // after which the server closes the channel.
    let strict = Encoder::new(
        Header {
            txid: 0,
            ordinal: 78,
            flags: 0,
        },
        0,
    );
    let (bytes, handles) = strict.finish();
    mock::channel_write(raw.raw(), &bytes, &handles).unwrap();
    server.join().unwrap();
    assert_eq!(
        mock::channel_read(raw.raw()).unwrap_err(),
        mock::PEER_CLOSED
    );
}
