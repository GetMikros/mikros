//! Whether a key is held while the boot manager starts.

/// A key stroke read from the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub scan_code: u16,
    pub unicode_char: u16,
    /// The shift state bits, or 0 when the console reports none.
    pub shift_state: u32,
}

/// The console input as `key_held` uses it.
pub trait Keyboard {
    /// Asks the console to also report keys such as Shift that are pressed
    /// on their own. Consoles that cannot do this are left as they are.
    fn expose_key_state(&mut self);
    /// Reads the next pending key stroke. Returns `None` when there is none
    /// or it cannot be read.
    fn read_key_stroke(&mut self) -> Option<KeyStroke>;
}

/// Whether a key is held while the boot manager starts, which is the case
/// when `keyboard` has any key stroke pending. Reads, and so discards, every
/// pending key stroke.
pub fn key_held(keyboard: Option<&mut impl Keyboard>) -> bool {
    let Some(keyboard) = keyboard else {
        return false;
    };
    keyboard.expose_key_state();
    let mut held = false;
    while keyboard.read_key_stroke().is_some() {
        held = true;
    }
    held
}

#[cfg(test)]
mod tests;
