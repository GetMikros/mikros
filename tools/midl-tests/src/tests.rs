use crate::midl::__midl::{self as rt, Decoder, Encoder, Error, Header};
use crate::midl::test::wire::*;
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
