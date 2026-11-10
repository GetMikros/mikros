use crate::termios::*;

#[test]
fn defaults_match_the_c_library() {
    let a = Attributes::default();
    assert_eq!(a.input_flags, 0o2400);
    assert_eq!(a.output_flags, 0o5);
    assert_eq!(a.control_flags, 0o2277);
    assert_eq!(a.local_flags, 0o105073);
    assert_eq!(&a.control_chars[..7], &[3, 0x1c, 0x7f, 0x15, 4, 0, 1]);
}
