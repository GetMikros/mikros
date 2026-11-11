use crate::*;
use alloc::vec::Vec;

fn one(_: char) -> usize {
    1
}

#[test]
fn a_line_has_the_time_app_id_and_message() {
    assert_eq!(
        line(12_345_678_901, "system.devicemgr", "started"),
        "[12.345678] system.devicemgr: started"
    );
    assert_eq!(line(5_000, "a", "b"), "[0.000005] a: b");
}

#[test]
fn a_time_before_boot_is_zero() {
    assert_eq!(line(-7, "a", "b"), "[0.000000] a: b");
}

#[test]
fn control_characters_are_question_marks() {
    assert_eq!(
        line(0, "a\tb", "x\ny\x1b[2J\u{9b}z"),
        "[0.000000] a?b: x?y?[2J?z"
    );
    assert_eq!(line(0, "a", "é ✓"), "[0.000000] a: é ✓");
}

#[test]
fn fit_cuts_at_the_width() {
    assert_eq!(fit("abcdef", 4, one), "abcd");
    assert_eq!(fit("abc", 4, one), "abc");
    assert_eq!(fit("abc", 0, one), "");
    // A character two columns wide that does not fit is left out whole.
    let wide = |c: char| if c == '漢' { 2 } else { 1 };
    assert_eq!(fit("ab漢c", 3, wide), "ab");
    assert_eq!(fit("ab漢c", 4, wide), "ab漢");
    // Characters of no columns stay with the one before them.
    let combining = |c: char| if c == '\u{301}' { 0 } else { 1 };
    assert_eq!(fit("ae\u{301}b", 2, combining), "ae\u{301}");
}

#[test]
fn the_tail_keeps_the_most_recent_lines() {
    let mut tail = Tail::new(2);
    assert_eq!(tail.latest(), 0);
    for (s, l) in [(1, "one"), (2, "two"), (3, "three")] {
        assert!(tail.push(s, l.into()));
    }
    let lines: Vec<_> = tail.last(5).collect();
    assert_eq!(lines, [(2, "two"), (3, "three")]);
    assert_eq!(tail.last(1).collect::<Vec<_>>(), [(3, "three")]);
    assert_eq!(tail.latest(), 3);
}

#[test]
fn the_tail_takes_each_record_once() {
    let mut tail = Tail::new(4);
    assert!(tail.push(5, "five".into()));
    assert!(!tail.push(5, "again".into()));
    assert!(!tail.push(4, "older".into()));
    assert!(tail.push(7, "seven".into()));
    let lines: Vec<_> = tail.last(4).collect();
    assert_eq!(lines, [(5, "five"), (7, "seven")]);
}
