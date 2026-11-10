use crate::discipline::*;
use crate::termios::*;
use alloc::vec;
use alloc::vec::Vec;

fn typed(d: &mut Discipline, bytes: &[u8]) -> Vec<Effects> {
    bytes.iter().map(|&b| d.input(b)).collect()
}

fn echo(d: &mut Discipline, bytes: &[u8]) -> Vec<u8> {
    typed(d, bytes).into_iter().flat_map(|e| e.echo).collect()
}

fn raw() -> Attributes {
    let mut a = Attributes::default();
    a.local_flags &= !(ICANON | ECHO | ISIG);
    a
}

#[test]
fn canonical_lines_wait_for_the_delimiter() {
    let mut d = Discipline::new(Attributes::default());
    assert_eq!(echo(&mut d, b"ls"), b"ls");
    assert!(!d.readable());
    assert_eq!(d.read(10), Read::Empty);
    // CR is read as NL.
    assert_eq!(echo(&mut d, b"\r"), b"\n");
    assert!(d.readable());
    assert_eq!(d.read(2), Read::Data(b"ls".to_vec()));
    assert_eq!(d.read(10), Read::Data(b"\n".to_vec()));
    assert_eq!(d.read(10), Read::Empty);
}

#[test]
fn a_read_returns_at_most_one_line() {
    let mut d = Discipline::new(Attributes::default());
    typed(&mut d, b"a\nb\n");
    assert_eq!(d.read(100), Read::Data(b"a\n".to_vec()));
    assert_eq!(d.read(100), Read::Data(b"b\n".to_vec()));
}

#[test]
fn end_of_file() {
    let mut d = Discipline::new(Attributes::default());
    // ^D ends a line without being part of it, and alone ends input.
    typed(&mut d, b"ab\x04\x04");
    assert_eq!(d.read(100), Read::Data(b"ab".to_vec()));
    assert_eq!(d.read(100), Read::End);
}

#[test]
fn erase_kill_and_word_erase() {
    let mut d = Discipline::new(Attributes::default());
    assert_eq!(echo(&mut d, b"ab\x7f"), b"ab\x08 \x08");
    // A control character echoed as ^X takes two columns to erase.
    assert_eq!(echo(&mut d, b"\x01\x7f"), b"^A\x08 \x08\x08 \x08");
    typed(&mut d, b"cd ef\x17\n");
    assert_eq!(d.read(100), Read::Data(b"acd \n".to_vec()));
    // Kill erases the line on the screen with ECHOKE.
    assert_eq!(echo(&mut d, b"xy\x15"), b"xy\x08 \x08\x08 \x08");
    typed(&mut d, b"z\n");
    assert_eq!(d.read(100), Read::Data(b"z\n".to_vec()));
}

#[test]
fn erasing_a_tab_and_utf8() {
    let mut d = Discipline::new(Attributes::default());
    let e = echo(&mut d, b"abc\t\x7f");
    // The tab took columns 3 to 7: five backspaces.
    assert_eq!(e.iter().filter(|&&b| b == 0x08).count(), 10);
    let mut a = Attributes::default();
    a.input_flags |= IUTF8;
    let mut d = Discipline::new(a);
    typed(&mut d, "xé\x7f\n".as_bytes());
    assert_eq!(d.read(100), Read::Data(b"x\n".to_vec()));
}

#[test]
fn literal_next() {
    let mut d = Discipline::new(Attributes::default());
    typed(&mut d, b"\x16\x03\x16\x7f\n");
    assert_eq!(d.read(100), Read::Data(b"\x03\x7f\n".to_vec()));
}

#[test]
fn signals_flush_and_echo() {
    let mut d = Discipline::new(Attributes::default());
    typed(&mut d, b"abc");
    let e = d.input(0x03);
    assert_eq!(e.signal, Some(Signal::Interrupt));
    assert!(e.flushed);
    assert_eq!(e.echo, b"^C");
    typed(&mut d, b"\n");
    assert_eq!(d.read(100), Read::Data(b"\n".to_vec()));
    assert_eq!(d.input(0x1c).signal, Some(Signal::Quit));
    assert_eq!(d.input(0x1a).signal, Some(Signal::Suspend));
    let mut a = Attributes::default();
    a.local_flags |= NOFLSH;
    let mut d = Discipline::new(a);
    typed(&mut d, b"ab");
    assert!(!d.input(0x03).flushed);
    typed(&mut d, b"\n");
    assert_eq!(d.read(100), Read::Data(b"ab\n".to_vec()));
}

#[test]
fn stopping_and_starting_output() {
    let mut d = Discipline::new(Attributes::default());
    assert_eq!(d.input(0x13).output_stopped, Some(true));
    assert!(d.output_stopped());
    assert_eq!(d.input(b'a').output_stopped, None);
    assert_eq!(d.input(0x11).output_stopped, Some(false));
    assert!(!d.output_stopped());
    // Neither character is input.
    typed(&mut d, b"\n");
    assert_eq!(d.read(100), Read::Data(b"a\n".to_vec()));
}

#[test]
fn non_canonical_input() {
    let mut a = raw();
    a.control_chars[VMIN] = 3;
    let mut d = Discipline::new(a);
    assert_eq!(echo(&mut d, b"\x7f\r"), b"");
    assert!(!d.readable());
    typed(&mut d, b"x");
    assert!(d.readable());
    // CR still becomes NL, and erase is a byte like any other.
    assert_eq!(d.read(2), Read::Data(vec![0x7f, b'\n']));
    assert_eq!(d.available(), 1);
    assert_eq!(d.read(10), Read::Data(b"x".to_vec()));
}

#[test]
fn switching_modes_keeps_input() {
    let mut d = Discipline::new(Attributes::default());
    typed(&mut d, b"one\ntw");
    d.set_attributes(raw());
    assert_eq!(d.read(100), Read::Data(b"one\ntw".to_vec()));
    typed(&mut d, b"xy");
    d.set_attributes(Attributes::default());
    assert!(d.readable());
    assert_eq!(d.read(100), Read::Data(b"xy".to_vec()));
}

#[test]
fn echo_off_and_echonl() {
    let mut a = Attributes::default();
    a.local_flags &= !ECHO;
    let mut d = Discipline::new(a);
    assert_eq!(echo(&mut d, b"secret\n"), b"");
    a.local_flags |= ECHONL;
    let mut d = Discipline::new(a);
    assert_eq!(echo(&mut d, b"pw\n"), b"\n");
}

#[test]
fn input_translation() {
    let mut a = raw();
    a.input_flags = IGNCR;
    let mut d = Discipline::new(a);
    typed(&mut d, b"a\rb");
    assert_eq!(d.read(10), Read::Data(b"ab".to_vec()));
    a.input_flags = INLCR | ISTRIP;
    let mut d = Discipline::new(a);
    typed(&mut d, &[b'\n', 0xC1]);
    assert_eq!(d.read(10), Read::Data(vec![b'\r', 0x41]));
}

#[test]
fn output_processing() {
    let mut d = Discipline::new(Attributes::default());
    assert_eq!(d.output(b"a\nb"), b"a\r\nb");
    let mut a = Attributes {
        output_flags: OPOST | OCRNL,
        ..Attributes::default()
    };
    let mut d = Discipline::new(a);
    assert_eq!(d.output(b"a\r"), b"a\n");
    a.output_flags = OPOST | ONOCR;
    let mut d = Discipline::new(a);
    // No CR at column 0.
    assert_eq!(d.output(b"\rab\r"), b"ab\r");
    a.output_flags = 0;
    let mut d = Discipline::new(a);
    assert_eq!(d.output(b"a\nb"), b"a\nb");
}

#[test]
fn input_is_bounded() {
    let mut d = Discipline::new(raw());
    for _ in 0..INPUT_MAX + 10 {
        d.input(b'x');
    }
    assert_eq!(d.available(), INPUT_MAX);
    let mut d = Discipline::new(Attributes::default());
    for _ in 0..INPUT_MAX + 10 {
        d.input(b'x');
    }
    typed(&mut d, b"\n");
    assert_eq!(d.available(), INPUT_MAX);
}
