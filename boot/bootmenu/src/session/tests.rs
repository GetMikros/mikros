use super::*;
use alloc::collections::VecDeque;

/// A console with pending key strokes.
struct FakeKeyboard {
    strokes: VecDeque<KeyStroke>,
    exposed: bool,
    /// Whether a stroke was read before the key state was exposed.
    read_unexposed: bool,
}

impl Keyboard for FakeKeyboard {
    fn expose_key_state(&mut self) {
        self.exposed = true;
    }

    fn read_key_stroke(&mut self) -> Option<KeyStroke> {
        self.read_unexposed |= !self.exposed;
        self.strokes.pop_front()
    }
}

fn keyboard(strokes: &[KeyStroke]) -> FakeKeyboard {
    FakeKeyboard {
        strokes: strokes.iter().copied().collect(),
        exposed: false,
        read_unexposed: false,
    }
}

#[test]
fn treats_any_pending_key_stroke_as_held_and_discards_them() {
    let enter = KeyStroke {
        scan_code: 0,
        unicode_char: u16::from(b'\r'),
        shift_state: 0,
    };
    // Left Shift on its own, with the shift state valid.
    let shift = KeyStroke {
        scan_code: 0,
        unicode_char: 0,
        shift_state: 0x8000_0000 | 0x2,
    };
    for strokes in [&[shift][..], &[enter, shift, enter]] {
        let mut keyboard = keyboard(strokes);
        assert!(key_held(Some(&mut keyboard)));
        assert!(keyboard.strokes.is_empty(), "all strokes read");
        assert!(keyboard.exposed);
        assert!(!keyboard.read_unexposed);
    }

    let mut keyboard = keyboard(&[]);
    assert!(!key_held(Some(&mut keyboard)));
    assert!(keyboard.exposed);
    assert!(!key_held(None::<&mut FakeKeyboard>), "no console input");
}
