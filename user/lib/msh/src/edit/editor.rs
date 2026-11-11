//! What each key does to the line, with zsh's Emacs key map: moving,
//! editing, killing and yanking, browsing the history among the entries
//! that start as the typed line does, and searching it with Ctrl-R.

use super::keys::Key;
use super::line::Line;

/// What the editor did with a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The line changed or the cursor moved; draw it again.
    Continue,
    /// Enter: the line is done.
    Accept(String),
    /// Ctrl-C: the line is dropped.
    Cancel,
    /// Ctrl-D on an empty line: the input ends.
    Eof,
    /// Ctrl-L: clear the screen and draw the line again.
    Clear,
}

/// An incremental search of the history, backwards from the newest
/// entry, for the entries that contain `query`.
#[derive(Clone, Debug)]
pub struct Search {
    pub query: String,
    /// The entry found, by its index in the history.
    pub found: Option<usize>,
    /// The line before the search, which Escape gives back.
    before: Line,
}

/// The line editor's state for one line.
pub struct Editor {
    line: Line,
    history: Vec<String>,
    /// While browsing the history: the entry shown, and the line typed
    /// before, whose start the entries shown begin with.
    browse: Option<(usize, String)>,
    kill: String,
    /// Whether the last key killed text, which the next kill joins.
    killed: bool,
    search: Option<Search>,
}

impl Editor {
    /// An empty line, with `history`, oldest first.
    pub fn new(history: Vec<String>) -> Editor {
        Editor {
            line: Line::default(),
            history,
            browse: None,
            kill: String::new(),
            killed: false,
            search: None,
        }
    }

    pub fn line(&self) -> &Line {
        &self.line
    }

    pub fn line_mut(&mut self) -> &mut Line {
        &mut self.line
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
    }

    /// Does what `key` does.
    pub fn key(&mut self, key: Key) -> Outcome {
        if self.search.is_some()
            && let Some(outcome) = self.search_key(key)
        {
            return outcome;
        }
        let killing = matches!(
            key,
            Key::Ctrl('k' | 'u' | 'w') | Key::Alt('d') | Key::AltBackspace
        );
        if !killing {
            self.killed = false;
        }
        if !matches!(key, Key::Up | Key::Down | Key::Ctrl('p' | 'n')) {
            self.browse = None;
        }
        let l = &mut self.line;
        match key {
            Key::Char(c) => l.insert(c.encode_utf8(&mut [0; 4])),
            Key::Enter => return Outcome::Accept(l.text().to_owned()),
            Key::Ctrl('c') => return Outcome::Cancel,
            Key::Ctrl('d') if l.text().is_empty() => return Outcome::Eof,
            Key::Ctrl('d') | Key::Delete => l.delete(),
            Key::Ctrl('l') => return Outcome::Clear,
            Key::Backspace => l.backspace(),
            Key::Left | Key::Ctrl('b') => l.left(),
            Key::Right | Key::Ctrl('f') => l.right(),
            Key::Home | Key::Ctrl('a') => l.home(),
            Key::End | Key::Ctrl('e') => l.end(),
            Key::WordLeft | Key::Alt('b') => l.word_left(),
            Key::WordRight | Key::Alt('f') => l.word_right(),
            Key::Ctrl('t') => l.transpose(),
            Key::Ctrl('k') => {
                let cut = l.kill_to_end();
                self.keep(cut, false);
            }
            Key::Ctrl('u') => {
                let cut = l.kill_to_start();
                self.keep(cut, true);
            }
            Key::Ctrl('w') => {
                let cut = l.kill_blank_word();
                self.keep(cut, true);
            }
            Key::AltBackspace => {
                let cut = l.kill_word_back();
                self.keep(cut, true);
            }
            Key::Alt('d') => {
                let cut = l.kill_word();
                self.keep(cut, false);
            }
            Key::Ctrl('y') => {
                let kill = self.kill.clone();
                self.line.insert(&kill);
            }
            Key::Up | Key::Ctrl('p') => self.older(),
            Key::Down | Key::Ctrl('n') => self.newer(),
            Key::Ctrl('r') => {
                self.search = Some(Search {
                    query: String::new(),
                    found: None,
                    before: self.line.clone(),
                })
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// Keeps `cut` for Ctrl-Y, after or, when it came from before the
    /// cursor, before what the kills just before it cut.
    fn keep(&mut self, cut: String, backwards: bool) {
        match (self.killed, backwards) {
            (false, _) => self.kill = cut,
            (true, false) => self.kill.push_str(&cut),
            (true, true) => self.kill.insert_str(0, &cut),
        }
        self.killed = true;
    }

    /// The entry from `from` on, in the direction `step`, that starts as
    /// `prefix` does and differs from the line shown.
    fn browse_from(&self, from: isize, step: isize, prefix: &str) -> Option<usize> {
        let mut at = from;
        while at >= 0 && (at as usize) < self.history.len() {
            let entry = &self.history[at as usize];
            if entry.starts_with(prefix) && entry != self.line.text() {
                return Some(at as usize);
            }
            at += step;
        }
        None
    }

    fn older(&mut self) {
        let (start, typed) = match &self.browse {
            Some((at, typed)) => (*at as isize - 1, typed.clone()),
            None => (self.history.len() as isize - 1, self.line.text().to_owned()),
        };
        if let Some(at) = self.browse_from(start, -1, &typed) {
            self.line.set(&self.history[at].clone());
            self.browse = Some((at, typed));
        }
    }

    fn newer(&mut self) {
        let Some((at, typed)) = self.browse.clone() else {
            return;
        };
        match self.browse_from(at as isize + 1, 1, &typed) {
            Some(next) => {
                self.line.set(&self.history[next].clone());
                self.browse = Some((next, typed));
            }
            None => {
                self.line.set(&typed);
                self.browse = None;
            }
        }
    }

    /// The newest entry at or before `from` that contains `query`.
    fn find(&self, query: &str, from: Option<usize>) -> Option<usize> {
        let end = from.map_or(self.history.len(), |f| f + 1);
        self.history[..end.min(self.history.len())]
            .iter()
            .rposition(|e| e.contains(query))
    }

    /// Does what `key` does while searching, or `None` when it ends the
    /// search and then does what it does on the line.
    fn search_key(&mut self, key: Key) -> Option<Outcome> {
        let search = self.search.as_mut()?;
        match key {
            Key::Char(c) => {
                search.query.push(c);
                let (query, from) = (search.query.clone(), search.found);
                let found = self.find(&query, from);
                self.show(found);
            }
            Key::Backspace => {
                search.query.pop();
                let query = search.query.clone();
                let found = self.find(&query, None);
                self.show(found);
            }
            Key::Ctrl('r') => {
                let (query, from) = (search.query.clone(), search.found);
                if let Some(from) = from.and_then(|f| f.checked_sub(1)) {
                    let found = self.find(&query, Some(from)).or(Some(from + 1));
                    self.show(found);
                }
            }
            Key::Escape | Key::Ctrl('g') => {
                self.line = search.before.clone();
                self.search = None;
            }
            Key::Ctrl('c') => {
                self.search = None;
                return Some(Outcome::Cancel);
            }
            _ => {
                self.search = None;
                return None;
            }
        }
        Some(Outcome::Continue)
    }

    /// Shows entry `found`, or the line from before the search.
    fn show(&mut self, found: Option<usize>) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.found = found;
        let text = match found {
            Some(at) => self.history[at].clone(),
            None => search.before.text().to_owned(),
        };
        self.line.set(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(history: &[&str]) -> Editor {
        Editor::new(history.iter().map(|s| s.to_string()).collect())
    }

    fn typed(e: &mut Editor, text: &str) {
        for c in text.chars() {
            e.key(Key::Char(c));
        }
    }

    #[test]
    fn edits_and_accepts_the_line() {
        let mut e = editor(&[]);
        typed(&mut e, "ls -la");
        e.key(Key::Ctrl('a'));
        e.key(Key::Ctrl('d'));
        e.key(Key::Ctrl('d'));
        typed(&mut e, "pwd");
        e.key(Key::Ctrl('k'));
        assert_eq!(e.key(Key::Enter), Outcome::Accept("pwd".into()));
        let mut e = editor(&[]);
        assert_eq!(e.key(Key::Ctrl('d')), Outcome::Eof);
        assert_eq!(e.key(Key::Ctrl('c')), Outcome::Cancel);
        assert_eq!(e.key(Key::Ctrl('l')), Outcome::Clear);
    }

    #[test]
    fn joins_kills_and_yanks_them() {
        let mut e = editor(&[]);
        typed(&mut e, "echo one two");
        e.key(Key::Ctrl('w'));
        e.key(Key::Ctrl('w'));
        assert_eq!(e.line().text(), "echo ");
        e.key(Key::Ctrl('a'));
        e.key(Key::Ctrl('y'));
        assert_eq!(e.line().text(), "one twoecho ");
        // Another key in between starts a new kill.
        e.key(Key::Ctrl('e'));
        e.key(Key::Ctrl('w'));
        e.key(Key::Ctrl('y'));
        e.key(Key::Ctrl('y'));
        assert_eq!(e.line().text(), "one twoecho twoecho ");
    }

    #[test]
    fn browses_the_entries_that_start_like_the_line() {
        let mut e = editor(&["ls /", "cd /tmp", "ls /usr", "ls /usr"]);
        e.key(Key::Up);
        assert_eq!(e.line().text(), "ls /usr");
        e.key(Key::Up);
        assert_eq!(e.line().text(), "cd /tmp");
        e.key(Key::Down);
        e.key(Key::Down);
        assert_eq!(e.line().text(), "");
        typed(&mut e, "ls");
        e.key(Key::Up);
        e.key(Key::Up);
        assert_eq!(e.line().text(), "ls /");
        e.key(Key::Up);
        assert_eq!(e.line().text(), "ls /");
        e.key(Key::Down);
        e.key(Key::Down);
        assert_eq!(e.line().text(), "ls");
    }

    #[test]
    fn searches_the_history_backwards() {
        let mut e = editor(&["make test", "ls", "make run", "cat x"]);
        typed(&mut e, "pw");
        e.key(Key::Ctrl('r'));
        typed(&mut e, "make");
        assert_eq!(e.line().text(), "make run");
        e.key(Key::Ctrl('r'));
        assert_eq!(e.line().text(), "make test");
        e.key(Key::Ctrl('r'));
        assert_eq!(e.line().text(), "make test");
        typed(&mut e, "z");
        assert_eq!(e.line().text(), "pw");
        e.key(Key::Backspace);
        assert_eq!(e.search().map(|s| s.found), Some(Some(2)));
        e.key(Key::Escape);
        assert_eq!(e.line().text(), "pw");
        e.key(Key::Ctrl('r'));
        typed(&mut e, "ls");
        e.key(Key::Ctrl('e'));
        typed(&mut e, " -l");
        assert!(e.search().is_none());
        assert_eq!(e.key(Key::Enter), Outcome::Accept("ls -l".into()));
    }
}
