//! The history file: one entry a line, with a backslash ending each
//! line of an entry of several lines but its last, as zsh writes them.

/// The lines that hold `entry` in the file, with their line feeds.
pub fn encode(entry: &str) -> String {
    format!("{}\n", entry.replace('\n', "\\\n"))
}

/// The entries of the file's `text`, oldest first.
pub fn decode(text: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut open: Option<String> = None;
    for line in text.lines() {
        let mut entry = match open.take() {
            Some(mut entry) => {
                entry.push('\n');
                entry
            }
            None => String::new(),
        };
        match line.strip_suffix('\\') {
            Some(part) => {
                entry.push_str(part);
                open = Some(entry);
            }
            None => {
                entry.push_str(line);
                entries.push(entry);
            }
        }
    }
    entries.extend(open);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_entries_of_several_lines_together() {
        let text = [encode("ls"), encode("if x {\n  y\n}"), encode("pwd")].concat();
        assert_eq!(text, "ls\nif x {\\\n  y\\\n}\npwd\n");
        assert_eq!(decode(&text), ["ls", "if x {\n  y\n}", "pwd"]);
        assert_eq!(decode("a\\"), ["a"]);
        assert!(decode("").is_empty());
    }
}
