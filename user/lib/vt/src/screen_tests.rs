use crate::cell::{BOLD, Color, REVERSE, UNDERLINE, Width};
use crate::screen::Screen;
use alloc::string::String;
use alloc::vec;

/// Row `row` of `s` as text, without trailing blanks.
fn line(s: &Screen, row: usize) -> String {
    let text: String = (0..s.cols())
        .filter(|&c| s.cell(row, c).width != Width::Continuation)
        .map(|c| s.cell(row, c).ch)
        .collect();
    text.trim_end().into()
}

fn screen(bytes: &[u8]) -> Screen {
    let mut s = Screen::new(10, 4);
    s.write(bytes);
    s
}

#[test]
fn text_wraps_at_the_margin() {
    let s = screen(b"0123456789ab");
    assert_eq!(line(&s, 0), "0123456789");
    assert_eq!(line(&s, 1), "ab");
    assert_eq!(s.cursor(), Some((1, 2)));
    // At the last column the cursor waits, and CR LF does not add a line.
    let s = screen(b"0123456789\r\nx");
    assert_eq!(line(&s, 1), "x");
}

#[test]
fn without_autowrap_the_last_column_is_overwritten() {
    let s = screen(b"\x1b[?7l0123456789abc");
    assert_eq!(line(&s, 0), "012345678c");
    assert_eq!(line(&s, 1), "");
}

#[test]
fn scrolling_at_the_bottom() {
    let s = screen(b"a\r\nb\r\nc\r\nd\r\ne");
    assert_eq!([line(&s, 0), line(&s, 3)], ["b", "e"]);
}

#[test]
fn cursor_movement() {
    let s = screen(b"\x1b[3;4Hx\x1b[Ay\x1b[2Dz\x1b[5Gw\x1b[10;10Hq");
    // CSI 5G puts w where y was.
    assert_eq!(line(&s, 1), "   zw");
    assert_eq!(line(&s, 2), "   x");
    assert_eq!(line(&s, 3), "         q");
}

#[test]
fn erasing() {
    let mut s = screen(b"abcdefghij\r\nklmnopqrst");
    s.write(b"\x1b[1;5H\x1b[K");
    assert_eq!(line(&s, 0), "abcd");
    s.write(b"\x1b[2;3H\x1b[1K");
    assert_eq!(line(&s, 1), "   nopqrst");
    s.write(b"\x1b[2;8H\x1b[2X");
    assert_eq!(line(&s, 1), "   nopq  t");
    s.write(b"\x1b[2J");
    assert_eq!(line(&s, 1), "");
}

#[test]
fn erasing_takes_the_background() {
    let s = screen(b"\x1b[44m\x1b[2J");
    assert_eq!(s.cell(2, 5).bg, Color::Indexed(4));
}

#[test]
fn inserting_and_deleting() {
    let mut s = screen(b"abcdef\x1b[1;3H\x1b[2@");
    assert_eq!(line(&s, 0), "ab  cdef");
    s.write(b"\x1b[3P");
    assert_eq!(line(&s, 0), "abdef");
    s.write(b"\x1b[4hXY\x1b[4l");
    assert_eq!(line(&s, 0), "abXYdef");
    let mut s = screen(b"1\r\n2\r\n3\r\n4\x1b[2H\x1b[L");
    assert_eq!([line(&s, 1), line(&s, 2), line(&s, 3)], ["", "2", "3"]);
    s.write(b"\x1b[2M");
    assert_eq!([line(&s, 1), line(&s, 2)], ["3", ""]);
}

#[test]
fn scroll_regions() {
    let mut s = screen(b"1\r\n2\r\n3\r\n4\x1b[2;3r");
    // The region is set and the cursor goes home.
    assert_eq!(s.cursor(), Some((0, 0)));
    s.write(b"\x1b[3H\nx");
    assert_eq!(
        [line(&s, 0), line(&s, 1), line(&s, 2), line(&s, 3)],
        ["1", "3", "x", "4"]
    );
    s.write(b"\x1b[2H\x1bMy");
    assert_eq!([line(&s, 1), line(&s, 2)], ["y", "3"]);
}

#[test]
fn tabs() {
    let s = screen(b"a\tb\x1b[1;2H\x1bH\x1b[1;1H\tc");
    assert_eq!(line(&s, 0), "ac      b");
    let s = screen(b"\x1b[3g\tx");
    assert_eq!(line(&s, 0), "         x");
}

#[test]
fn attributes_and_colours() {
    let s = screen(b"\x1b[1;4;31;42ma\x1b[22;24;7mb\x1b[0mc\x1b[38;5;196;48;2;1;2;3md\x1b[38:2::9:8:7me\x1b[95mf");
    let a = s.cell(0, 0);
    assert_eq!(
        (a.attrs, a.fg, a.bg),
        (BOLD | UNDERLINE, Color::Indexed(1), Color::Indexed(2))
    );
    assert_eq!(s.cell(0, 1).attrs, REVERSE);
    assert_eq!(
        *s.cell(0, 2),
        crate::cell::Cell {
            ch: 'c',
            ..Default::default()
        }
    );
    let d = s.cell(0, 3);
    assert_eq!((d.fg, d.bg), (Color::Indexed(196), Color::Rgb(1, 2, 3)));
    assert_eq!(s.cell(0, 4).fg, Color::Rgb(9, 8, 7));
    assert_eq!(s.cell(0, 5).fg, Color::Indexed(13));
}

#[test]
fn utf8_and_wide_characters() {
    let mut s = screen("é漢x".as_bytes());
    assert_eq!(s.cell(0, 1).width, Width::Wide);
    assert_eq!(s.cell(0, 2).width, Width::Continuation);
    assert_eq!(s.cursor(), Some((0, 4)));
    // A combining mark adds no column.
    s.write("e\u{301}".as_bytes());
    assert_eq!(s.cursor(), Some((0, 5)));
    // A wide character at the last column wraps whole.
    let s = screen("123456789漢".as_bytes());
    assert_eq!(line(&s, 1), "漢");
    // Overwriting half of one blanks the other.
    let s = screen("漢\x1b[1;2Hx".as_bytes());
    assert_eq!(line(&s, 0), " x");
}

#[test]
fn line_drawing() {
    let s = screen(b"\x1b(0lqk\x1b(Bq");
    assert_eq!(line(&s, 0), "┌─┐q");
}

#[test]
fn the_alternate_screen() {
    let mut s = screen(b"main\x1b[?1049hfull");
    assert!(s.on_alternate());
    assert_eq!(line(&s, 0), "    full");
    s.write(b"\x1b[?1049l");
    assert_eq!(line(&s, 0), "main");
    assert_eq!(s.cursor(), Some((0, 4)));
}

#[test]
fn saving_the_cursor() {
    let s = screen(b"\x1b[2;3H\x1b[1m\x1b7\x1b[H\x1b[0m\x1b8x");
    assert_eq!(s.cell(1, 2).attrs, BOLD);
    let s = screen(b"\x1b[3;3H\x1b[s\x1b[H\x1b[uy");
    assert_eq!(line(&s, 2), "  y");
}

#[test]
fn modes() {
    let s = screen(b"\x1b[?25l\x1b[?1h\x1b[?2004h\x1b=");
    assert_eq!(s.cursor(), None);
    assert!(s.modes.app_cursor && s.modes.bracketed_paste && s.modes.app_keypad);
    let mut s = screen(b"\x1b[2;3r\x1b[?6h\x1b[Hx");
    assert_eq!(line(&s, 1), "x");
    s.write(b"\x1b[6n");
    assert_eq!(s.take_responses(), b"\x1b[1;2R");
}

#[test]
fn reports() {
    let mut s = screen(b"\x1b[2;5H\x1b[6n\x1b[5n\x1b[c");
    assert_eq!(s.take_responses(), b"\x1b[2;5R\x1b[0n\x1b[?62;22c".to_vec());
    assert!(s.take_responses().is_empty());
}

#[test]
fn repeat_and_reset() {
    let mut s = screen(b"ab\x1b[3b");
    assert_eq!(line(&s, 0), "abbbb");
    s.write(b"\x1b[31m\x1bc");
    assert_eq!(line(&s, 0), "");
    assert_eq!(s.cursor(), Some((0, 0)));
}

#[test]
fn damage() {
    let mut s = Screen::new(10, 4);
    assert_eq!(s.take_dirty(), vec![0, 1, 2, 3]);
    s.write(b"\x1b[3Hx");
    assert_eq!(s.take_dirty(), vec![2]);
    assert!(s.take_dirty().is_empty());
}

#[test]
fn resizing_keeps_what_fits() {
    let mut s = screen(b"abcdefghij\r\n\r\n\r\n\x1b[4;10H");
    s.resize(5, 2);
    assert_eq!(line(&s, 0), "abcde");
    assert_eq!(s.cursor(), Some((1, 4)));
}
