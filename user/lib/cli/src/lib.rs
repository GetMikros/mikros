//! What the command-line utilities share: reading their arguments, the
//! text of `--help`, their exit statuses and error messages, and JSON
//! strings. The tables of `spec` describe what each utility takes.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]

extern crate alloc;

pub mod colour;
pub mod complete;
mod pager;
pub mod size;
pub mod spec;
mod start;
pub mod time;
mod usage;

pub use colour::{Paint, Role};
pub use spec::{Kind, Operand, Opt, Parsed, Run, Spec};
pub use start::{complain, start};
pub use usage::declared;

use alloc::string::String;
use core::fmt::Write;

/// The exit status when the utility did what it was asked.
pub const SUCCESS: u8 = 0;
/// The exit status when it failed.
pub const FAILURE: u8 = 1;
/// The exit status when its arguments are invalid.
pub const INVALID: u8 = 2;

/// `text` as a JSON string, in quotes, with the characters that JSON
/// does not take as they are escaped.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a \"b\" \\ c"), "\"a \\\"b\\\" \\\\ c\"");
        assert_eq!(json_string("1\n2\t\u{1}"), "\"1\\n2\\t\\u0001\"");
        assert_eq!(json_string("äö"), "\"äö\"");
    }
}
