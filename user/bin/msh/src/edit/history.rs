//! The history file: one entry a line, with a backslash ending each
//! line of an entry of several lines but its last, as zsh writes them.
//! Before an entry, a line `#folder <folder>` gives the folder it was
//! entered in, and lines `#path <path>` the words of it that named files
//! or folders in that folder then. An entry whose text starts with `#` or
//! a backslash has a backslash put before it. Entries written before
//! folders were recorded have none.

/// An entry of the history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub line: String,
    /// The folder the entry was entered in, when that was recorded.
    pub folder: Option<String>,
    /// The words of the entry that named files or folders relative to
    /// its folder when it was entered.
    pub paths: Vec<String>,
}

impl From<&str> for Entry {
    fn from(line: &str) -> Entry {
        Entry {
            line: line.to_owned(),
            ..Entry::default()
        }
    }
}

/// The lines that hold `entry` in the file, with their line feeds.
pub fn encode(entry: &Entry) -> String {
    let mut out = String::new();
    // A folder or path with a line feed in it cannot be written.
    let one_line = |s: &String| !s.contains('\n');
    if let Some(folder) = entry.folder.as_ref().filter(|f| one_line(f)) {
        out.push_str(&format!("#folder {folder}\n"));
        for path in entry.paths.iter().filter(|p| one_line(p)) {
            out.push_str(&format!("#path {path}\n"));
        }
    }
    if entry.line.starts_with(['#', '\\']) {
        out.push('\\');
    }
    out.push_str(&entry.line.replace('\n', "\\\n"));
    out.push('\n');
    out
}

/// The entries of the file's `text`, oldest first.
pub fn decode(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut next = Entry::default();
    let mut open: Option<String> = None;
    for line in text.lines() {
        let mut part = line;
        if open.is_none() {
            if let Some(folder) = line.strip_prefix("#folder ") {
                next.folder = Some(folder.to_owned());
                next.paths.clear();
                continue;
            }
            if let Some(path) = line.strip_prefix("#path ") {
                next.paths.push(path.to_owned());
                continue;
            }
            part = line.strip_prefix('\\').unwrap_or(line);
        }
        let mut text = match open.take() {
            Some(mut text) => {
                text.push('\n');
                text
            }
            None => String::new(),
        };
        match part.strip_suffix('\\') {
            Some(start) => {
                text.push_str(start);
                open = Some(text);
            }
            None => {
                text.push_str(part);
                let place = std::mem::take(&mut next);
                entries.push(Entry {
                    line: text,
                    ..place
                });
            }
        }
    }
    if let Some(text) = open {
        entries.push(Entry { line: text, ..next });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.line.as_str()).collect()
    }

    #[test]
    fn keeps_entries_of_several_lines_together() {
        let text = [
            encode(&"ls".into()),
            encode(&"if x {\n  y\n}".into()),
            encode(&"pwd".into()),
        ]
        .concat();
        assert_eq!(text, "ls\nif x {\\\n  y\\\n}\npwd\n");
        assert_eq!(lines(&decode(&text)), ["ls", "if x {\n  y\n}", "pwd"]);
        assert_eq!(lines(&decode("a\\")), ["a"]);
        assert!(decode("").is_empty());
    }

    #[test]
    fn keeps_the_folder_and_paths_of_each_entry() {
        let entry = Entry {
            line: "cd Developer".into(),
            folder: Some("/Users/ada".into()),
            paths: vec!["Developer".into()],
        };
        let text = [
            encode(&entry),
            encode(&"# a comment".into()),
            encode(&"ls".into()),
        ]
        .concat();
        assert_eq!(
            text,
            "#folder /Users/ada\n#path Developer\ncd Developer\n\\# a comment\nls\n"
        );
        assert_eq!(decode(&text), [entry, "# a comment".into(), "ls".into()]);
        // An old file has no folders.
        assert_eq!(decode("pwd\n"), [Entry::from("pwd")]);
    }
}
