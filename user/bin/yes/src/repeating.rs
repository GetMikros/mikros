//! What yes takes, and the lines it writes (MIKSPEC-0098.R50).

use mikros_cli::{Kind, Operand, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "text",
        Kind::Text,
        "the text of each line, y when there is none",
    )],
    ..Spec::new(
        "yes",
        "Writes the text given, separated by spaces, or y, on a line again\n\
         and again, until writing fails.",
    )
};

/// The line for `texts`, with its line feed.
pub fn line(texts: &[String]) -> Vec<u8> {
    let mut line = match texts.is_empty() {
        true => b"y".to_vec(),
        false => texts.join(" ").into_bytes(),
    };
    line.push(b'\n');
    line
}

/// Copies of `line`, whole, filling at least `size` bytes, so that each
/// write carries many lines.
pub fn block(line: &[u8], size: usize) -> Vec<u8> {
    let copies = size.div_ceil(line.len()).max(1);
    line.repeat(copies)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_the_line_of_its_operands_or_y() {
        assert_eq!(line(&[]), b"y\n");
        assert_eq!(line(&["a b".into(), "c".into()]), b"a b c\n");
        assert_eq!(line(&["".into()]), b"\n");
    }

    #[test]
    fn fills_blocks_with_whole_lines() {
        assert_eq!(block(b"y\n", 5), b"y\ny\ny\n");
        assert_eq!(block(b"abc\n", 4), b"abc\n");
        assert_eq!(block(b"\n", 0), b"\n");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/yes.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
