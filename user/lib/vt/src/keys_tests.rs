use crate::keys::{Key, Modifiers, encode, paste};
use crate::screen::Modes;

const NONE: Modifiers = Modifiers {
    shift: false,
    alt: false,
    ctrl: false,
};
const CTRL: Modifiers = Modifiers {
    shift: false,
    alt: false,
    ctrl: true,
};
const ALT: Modifiers = Modifiers {
    shift: false,
    alt: true,
    ctrl: false,
};
const SHIFT: Modifiers = Modifiers {
    shift: true,
    alt: false,
    ctrl: false,
};

fn normal() -> Modes {
    Modes::default()
}

fn application() -> Modes {
    Modes {
        app_cursor: true,
        ..Modes::default()
    }
}

#[test]
fn characters() {
    assert_eq!(encode(Key::Char('a'), NONE, &normal()), b"a");
    assert_eq!(encode(Key::Char('é'), NONE, &normal()), "é".as_bytes());
    assert_eq!(encode(Key::Char('c'), CTRL, &normal()), b"\x03");
    assert_eq!(encode(Key::Char('['), CTRL, &normal()), b"\x1b");
    assert_eq!(encode(Key::Char(' '), CTRL, &normal()), b"\x00");
    assert_eq!(encode(Key::Char('x'), ALT, &normal()), b"\x1bx");
}

#[test]
fn editing_keys() {
    assert_eq!(encode(Key::Enter, NONE, &normal()), b"\r");
    assert_eq!(
        encode(
            Key::Enter,
            NONE,
            &Modes {
                newline: true,
                ..normal()
            }
        ),
        b"\r\n"
    );
    assert_eq!(encode(Key::Backspace, NONE, &normal()), b"\x7f");
    assert_eq!(encode(Key::Backspace, CTRL, &normal()), b"\x08");
    assert_eq!(encode(Key::Tab, SHIFT, &normal()), b"\x1b[Z");
    assert_eq!(encode(Key::Delete, NONE, &normal()), b"\x1b[3~");
    assert_eq!(encode(Key::PageUp, CTRL, &normal()), b"\x1b[5;5~");
}

#[test]
fn cursor_keys() {
    assert_eq!(encode(Key::Up, NONE, &normal()), b"\x1b[A");
    assert_eq!(encode(Key::Up, NONE, &application()), b"\x1bOA");
    assert_eq!(encode(Key::Left, CTRL, &application()), b"\x1b[1;5D");
    assert_eq!(encode(Key::Home, NONE, &application()), b"\x1bOH");
    assert_eq!(encode(Key::End, SHIFT, &normal()), b"\x1b[1;2F");
}

#[test]
fn function_keys() {
    assert_eq!(encode(Key::Function(1), NONE, &normal()), b"\x1bOP");
    assert_eq!(encode(Key::Function(4), SHIFT, &normal()), b"\x1b[1;2S");
    assert_eq!(encode(Key::Function(5), NONE, &normal()), b"\x1b[15~");
    assert_eq!(encode(Key::Function(12), NONE, &normal()), b"\x1b[24~");
    assert!(encode(Key::Function(13), NONE, &normal()).is_empty());
}

#[test]
fn pasting() {
    assert_eq!(paste(b"ls", &normal()), b"ls");
    let bracketed = Modes {
        bracketed_paste: true,
        ..normal()
    };
    assert_eq!(paste(b"ls", &bracketed), b"\x1b[200~ls\x1b[201~");
}
