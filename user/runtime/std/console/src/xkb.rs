//! xkbcommon: compiling a keymap from the keymap data and turning key
//! presses into keysyms and text.

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;

#[repr(C)]
struct RuleNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

const XKB_KEY_UP: c_int = 0;
const XKB_KEY_DOWN: c_int = 1;
const XKB_STATE_MODS_EFFECTIVE: c_int = 1 << 3;
/// evdev key codes are xkb key codes less 8.
const EVDEV_OFFSET: u32 = 8;

#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_context_new(flags: c_int) -> *mut c_void;
    fn xkb_context_unref(context: *mut c_void);
    fn xkb_keymap_new_from_names(
        context: *mut c_void,
        names: *const RuleNames,
        flags: c_int,
    ) -> *mut c_void;
    fn xkb_keymap_unref(keymap: *mut c_void);
    fn xkb_keymap_key_repeats(keymap: *mut c_void, key: u32) -> c_int;
    fn xkb_state_new(keymap: *mut c_void) -> *mut c_void;
    fn xkb_state_unref(state: *mut c_void);
    fn xkb_state_update_key(state: *mut c_void, key: u32, direction: c_int) -> c_int;
    fn xkb_state_key_get_one_sym(state: *mut c_void, key: u32) -> u32;
    fn xkb_state_key_get_utf8(
        state: *mut c_void,
        key: u32,
        buffer: *mut c_char,
        size: usize,
    ) -> c_int;
    fn xkb_state_mod_name_is_active(state: *mut c_void, name: *const c_char, kind: c_int) -> c_int;
}

/// The layout to compile: the keys of `/etc/keyboard.yaml`.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// A compiled keymap and the state of its modifiers.
pub struct Keymap {
    context: *mut c_void,
    keymap: *mut c_void,
    state: *mut c_void,
}

impl Keymap {
    /// The keymap of `names` with the evdev rules, or None when
    /// xkbcommon cannot compile it.
    pub fn new(names: &Names) -> Option<Keymap> {
        let c = |s: &str| CString::new(s).ok();
        let (rules, model, layout, variant, options) = (
            c("evdev")?,
            c(&names.model)?,
            c(&names.layout)?,
            c(&names.variant)?,
            c(&names.options)?,
        );
        let names = RuleNames {
            rules: rules.as_ptr(),
            model: model.as_ptr(),
            layout: layout.as_ptr(),
            variant: variant.as_ptr(),
            options: options.as_ptr(),
        };
        // SAFETY: the names stay alive for the call; the results are
        // checked before use.
        unsafe {
            let context = xkb_context_new(0);
            if context.is_null() {
                return None;
            }
            let keymap = xkb_keymap_new_from_names(context, &names, 0);
            if keymap.is_null() {
                xkb_context_unref(context);
                return None;
            }
            let state = xkb_state_new(keymap);
            if state.is_null() {
                xkb_keymap_unref(keymap);
                xkb_context_unref(context);
                return None;
            }
            Some(Keymap {
                context,
                keymap,
                state,
            })
        }
    }

    /// Takes in a press or release of evdev key `code`.
    pub fn update(&mut self, code: u16, pressed: bool) {
        let direction = if pressed { XKB_KEY_DOWN } else { XKB_KEY_UP };
        // SAFETY: the state is valid while `self` lives.
        unsafe { xkb_state_update_key(self.state, u32::from(code) + EVDEV_OFFSET, direction) };
    }

    /// The keysym of evdev key `code` with the current modifiers.
    pub fn keysym(&self, code: u16) -> u32 {
        // SAFETY: as for `update`.
        unsafe { xkb_state_key_get_one_sym(self.state, u32::from(code) + EVDEV_OFFSET) }
    }

    /// The text of evdev key `code` with the current modifiers.
    pub fn text(&self, code: u16) -> String {
        let mut buffer = [0 as c_char; 64];
        // SAFETY: the buffer's length is given.
        let n = unsafe {
            xkb_state_key_get_utf8(
                self.state,
                u32::from(code) + EVDEV_OFFSET,
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        let n = usize::try_from(n).unwrap_or(0).min(buffer.len() - 1);
        let bytes: Vec<u8> = buffer[..n].iter().map(|&b| b as u8).collect();
        String::from_utf8(bytes).unwrap_or_default()
    }

    /// Whether evdev key `code` repeats while held.
    pub fn repeats(&self, code: u16) -> bool {
        // SAFETY: as for `update`.
        unsafe { xkb_keymap_key_repeats(self.keymap, u32::from(code) + EVDEV_OFFSET) != 0 }
    }

    /// Whether the modifier `name`, such as "Control", is in effect.
    pub fn modifier(&self, name: &str) -> bool {
        let Ok(name) = CString::new(name) else {
            return false;
        };
        // SAFETY: the name stays alive for the call.
        unsafe {
            xkb_state_mod_name_is_active(self.state, name.as_ptr(), XKB_STATE_MODS_EFFECTIVE) > 0
        }
    }
}

impl Drop for Keymap {
    fn drop(&mut self) {
        // SAFETY: each was made by `new` and is dropped once.
        unsafe {
            xkb_state_unref(self.state);
            xkb_keymap_unref(self.keymap);
            xkb_context_unref(self.context);
        }
        self.state = ptr::null_mut();
    }
}
