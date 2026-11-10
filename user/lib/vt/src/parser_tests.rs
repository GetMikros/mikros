use crate::parser::{Action, Parser};
use alloc::vec;
use alloc::vec::Vec;

fn parse(bytes: &[u8]) -> Vec<Action> {
    let mut p = Parser::new();
    let mut out = Vec::new();
    p.feed(bytes, |a| out.push(a));
    out
}

fn csi(intermediates: &[u8], params: &[Option<u32>], last: u8) -> Action {
    Action::Csi {
        intermediates: intermediates.to_vec(),
        params: params.to_vec(),
        sub: params.iter().map(|_| Vec::new()).collect(),
        last,
    }
}

#[test]
fn text_and_controls() {
    assert_eq!(
        parse(b"a\r\n\x7f"),
        vec![
            Action::Print('a'),
            Action::Execute(b'\r'),
            Action::Execute(b'\n')
        ]
    );
}

#[test]
fn utf8() {
    assert_eq!(
        parse("é€😀".as_bytes()),
        vec![Action::Print('é'), Action::Print('€'), Action::Print('😀')]
    );
    // A cut sequence, an overlong form and a stray continuation byte.
    assert_eq!(
        parse(&[0xC3, b'a', 0xE0, 0x80, 0x80, 0x80]),
        vec![
            Action::Print('\u{FFFD}'),
            Action::Print('a'),
            Action::Print('\u{FFFD}'),
            Action::Print('\u{FFFD}'),
        ]
    );
}

#[test]
fn control_sequences() {
    assert_eq!(parse(b"\x1b[H"), vec![csi(b"", &[], b'H')]);
    assert_eq!(
        parse(b"\x1b[12;40H"),
        vec![csi(b"", &[Some(12), Some(40)], b'H')]
    );
    assert_eq!(parse(b"\x1b[;5H"), vec![csi(b"", &[None, Some(5)], b'H')]);
    assert_eq!(parse(b"\x1b[?1049h"), vec![csi(b"?", &[Some(1049)], b'h')]);
    assert_eq!(parse(b"\x1b[ q"), vec![csi(b" ", &[], b'q')]);
    // Controls inside a sequence act at once.
    assert_eq!(
        parse(b"\x1b[1\n2A"),
        vec![Action::Execute(b'\n'), csi(b"", &[Some(12)], b'A')]
    );
}

#[test]
fn subparameters() {
    assert_eq!(
        parse(b"\x1b[38:5:196m"),
        vec![Action::Csi {
            intermediates: vec![],
            params: vec![Some(38)],
            sub: vec![vec![Some(5), Some(196)]],
            last: b'm',
        }]
    );
}

#[test]
fn escape_sequences() {
    assert_eq!(
        parse(b"\x1b7"),
        vec![Action::Esc {
            intermediates: vec![],
            last: b'7'
        }]
    );
    assert_eq!(
        parse(b"\x1b(0"),
        vec![Action::Esc {
            intermediates: b"(".to_vec(),
            last: b'0'
        }]
    );
}

#[test]
fn operating_system_commands() {
    assert_eq!(
        parse(b"\x1b]0;title\x07x"),
        vec![Action::Osc(b"0;title".to_vec()), Action::Print('x')]
    );
    assert_eq!(parse(b"\x1b]2;t\x1b\\"), vec![Action::Osc(b"2;t".to_vec())]);
    // A device control string is ignored up to its terminator.
    assert_eq!(parse(b"\x1bPq#0;1\x1b\\z"), vec![Action::Print('z')]);
}

#[test]
fn cancel_and_restart() {
    assert_eq!(parse(b"\x1b[12\x18A"), vec![Action::Print('A')]);
    assert_eq!(parse(b"\x1b[1\x1b[2J"), vec![csi(b"", &[Some(2)], b'J')]);
    // A private marker after parameters makes the sequence ignored.
    assert_eq!(parse(b"\x1b[1?hx"), vec![Action::Print('x')]);
}

#[test]
fn bounded_parameters() {
    let mut long = b"\x1b[".to_vec();
    for _ in 0..100 {
        long.extend_from_slice(b"1;");
    }
    long.extend_from_slice(b"99999999m");
    let out = parse(&long);
    let Action::Csi { params, .. } = &out[0] else {
        panic!("{out:?}")
    };
    assert_eq!(params.len(), crate::parser::PARAMS_MAX);
}
