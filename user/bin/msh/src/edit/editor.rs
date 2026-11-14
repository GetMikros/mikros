//! What each key does to the line, with zsh's Emacs key map: moving,
//! editing, killing and yanking, browsing the history among the entries
//! that start as the typed line does, and searching it with Ctrl-R.
//! As fish does, Tab completes the word, or shows the candidates in a
//! menu that Tab, Shift-Tab and the arrow keys move through, and the
//! line shows the most likely rest of it, from the history or else the
//! one candidate, which Right or End takes.

use super::complete::{Sources, complete};
use super::history::Entry;
use super::keys::Key;
use super::line::Line;
use super::menu::{Menu, common_prefix};

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
    /// The history's lines, oldest first, and its entries.
    history: Vec<String>,
    entries: Vec<Entry>,
    /// While browsing the history: the entry shown, and the line typed
    /// before, whose start the entries shown begin with.
    browse: Option<(usize, String)>,
    kill: String,
    /// Whether the last key killed text, which the next kill joins.
    killed: bool,
    search: Option<Search>,
    menu: Option<Menu>,
    /// The line the last suggestion was found for, and that suggestion,
    /// which goes on as long as what is typed after the line follows it.
    suggested: Option<(String, String)>,
}

const NORMAL: &str = "\x1b[0m";

impl Editor {
    /// An empty line, with `history`, oldest first.
    pub fn new(history: Vec<Entry>) -> Editor {
        Editor {
            line: Line::default(),
            history: history.iter().map(|e| e.line.clone()).collect(),
            entries: history,
            browse: None,
            kill: String::new(),
            killed: false,
            search: None,
            menu: None,
            suggested: None,
        }
    }

    pub fn line(&self) -> &Line {
        &self.line
    }

    /// The search in progress, which the tests check.
    #[cfg(test)]
    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
    }

    /// The menu shown, which the tests check.
    #[cfg(test)]
    pub fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }

    /// Does what `key` does, completing from `sources`.
    pub fn key(&mut self, key: Key, sources: &dyn Sources) -> Outcome {
        if self.search.is_some()
            && let Some(outcome) = self.search_key(key)
        {
            return outcome;
        }
        if self.menu.is_some()
            && let Some(outcome) = self.menu_key(key)
        {
            return outcome;
        }
        let rest = match key {
            Key::Right | Key::End | Key::Ctrl('f' | 'e') => self.suggestion(sources),
            _ => String::new(),
        };
        if !rest.is_empty() {
            self.line.insert(&rest);
            return Outcome::Continue;
        }
        if matches!(key, Key::Tab | Key::BackTab) {
            self.tab(sources);
            return Outcome::Continue;
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
            // In an entry of several lines, they move between its lines,
            // and from its first or last on through the history.
            Key::Up | Key::Ctrl('p') => {
                if !l.up() {
                    self.older();
                }
            }
            Key::Down | Key::Ctrl('n') => {
                if !l.down() {
                    self.newer();
                }
            }
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

    /// Tab: the one candidate, with a space when the word is done, or
    /// what all candidates start with and the menu of them.
    fn tab(&mut self, sources: &dyn Sources) {
        let found = complete(self.line.text(), self.line.cursor(), sources);
        let typed = self.line.cursor() - found.start;
        match found.candidates.as_slice() {
            [] => {}
            [one] => {
                self.line.replace_before(found.start, &one.text);
                if one.done && !self.line.text()[self.line.cursor()..].starts_with(' ') {
                    self.line.insert(" ");
                }
            }
            many => {
                let prefix = common_prefix(many.iter().map(|c| c.text.as_str()));
                if prefix.len() > typed {
                    self.line.replace_before(found.start, &prefix);
                }
                self.menu = Some(Menu::new(found.start, found.candidates));
            }
        }
    }

    /// Does what `key` does while the menu shows, or `None` when it
    /// closes the menu and then does what it does on the line.
    fn menu_key(&mut self, key: Key) -> Option<Outcome> {
        let menu = self.menu.as_mut()?;
        let columns = menu.columns as isize;
        match key {
            Key::Tab | Key::Right => menu.select(1),
            Key::BackTab | Key::Left => menu.select(-1),
            Key::Down if menu.selected.is_none() => menu.select(1),
            Key::Down => menu.select(columns),
            Key::Up => menu.select(-columns),
            Key::Enter if menu.selected.is_some() => {
                let done = menu.chosen().is_some_and(|c| c.done);
                self.menu = None;
                if done {
                    self.line.insert(" ");
                }
                return Some(Outcome::Continue);
            }
            Key::Escape | Key::Ctrl('g') => {
                self.menu = None;
                return Some(Outcome::Continue);
            }
            _ => {
                self.menu = None;
                return None;
            }
        }
        if let Some(chosen) = menu.chosen() {
            let (start, text) = (menu.start, chosen.text.clone());
            self.line.replace_before(start, &text);
        }
        Some(Outcome::Continue)
    }

    /// The most likely rest of the line, with the cursor at its end:
    /// from the newest history entry that starts with it and was entered
    /// in the current folder, or else from the newest such entry whose
    /// paths are valid in the current folder, or else the rest of the
    /// word's one candidate. While what is typed after the
    /// line of the last suggestion follows that suggestion, the rest of
    /// it goes on without being looked for again.
    pub fn suggestion(&mut self, sources: &dyn Sources) -> String {
        let text = self.line.text();
        if self.menu.is_some() || self.search.is_some() || !self.line.at_end() || text.is_empty() {
            self.suggested = None;
            return String::new();
        }
        if let Some((line, suggestion)) = &self.suggested
            && let Some(typed) = text.strip_prefix(line.as_str())
            && let Some(rest) = suggestion.strip_prefix(typed)
            && !rest.is_empty()
        {
            return rest.to_owned();
        }
        let found = self.find_suggestion(sources);
        self.suggested = (!found.is_empty()).then(|| (text.to_owned(), found.clone()));
        found
    }

    /// The suggestion for the line, found anew.
    fn find_suggestion(&self, sources: &dyn Sources) -> String {
        let text = self.line.text();
        let here = sources.current_folder();
        let here = here.as_deref();
        let starting = || {
            self.entries
                .iter()
                .rev()
                .filter(|e| e.line.len() > text.len() && e.line.starts_with(text))
        };
        let entered_here = |e: &Entry| here.is_some() && e.folder.as_deref() == here;
        let found = starting()
            .find(|e| entered_here(e))
            .or_else(|| starting().find(|e| !entered_here(e) && valid_here(e, sources)));
        if let Some(entry) = found {
            return entry.line[text.len()..].to_owned();
        }
        let found = complete(text, text.len(), sources);
        let word = &text[found.start..];
        match found.candidates.as_slice() {
            [one] if !word.is_empty() && one.text.starts_with(word) && one.text != word => {
                one.text[word.len()..].to_owned()
            }
            _ => String::new(),
        }
    }

    /// The rows to show below the line, at most `height` of them on a
    /// terminal `width` columns wide: the search, or the menu, with
    /// `dim`, the escape sequence of dimmed text.
    pub fn below(&mut self, width: usize, height: usize, dim: &str) -> Vec<String> {
        if let Some(search) = &self.search {
            let failing = if search.found.is_none() && !search.query.is_empty() {
                "failing "
            } else {
                ""
            };
            return vec![format!(
                "{dim}{failing}bck-i-search:{NORMAL} {}_",
                search.query
            )];
        }
        match &mut self.menu {
            Some(menu) => menu.rows(width, height, dim),
            None => Vec::new(),
        }
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
                // Further presses go down through its lines.
                self.line.set(&self.history[next].clone());
                self.line.first_line_end();
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

/// Whether the paths of `entry` are valid in the current folder: the
/// target of a `cd`, unless it is absolute, names a folder there, and the
/// words recorded as naming files or folders name ones there.
fn valid_here(entry: &Entry, sources: &dyn Sources) -> bool {
    let mut words = entry.line.split_whitespace();
    if words.next() == Some("cd")
        && let Some(target) = words.next().map(|w| w.trim_matches(['\'', '"']))
        && !target.starts_with(['/', '~', '-', '$'])
        && kind(target, sources) != Some(true)
    {
        return false;
    }
    entry.paths.iter().all(|p| kind(p, sources).is_some())
}

/// Whether `path`, relative to the current folder, is a folder; `None`
/// when it is not there.
fn kind(path: &str, sources: &dyn Sources) -> Option<bool> {
    let path = path.trim_end_matches('/');
    let last = path.rsplit('/').next().unwrap_or(path);
    if path.is_empty() || last == "." || last == ".." {
        return Some(true);
    }
    let (folder, name) = path.rsplit_once('/').unwrap_or((".", path));
    let entries = sources.entries(if folder.is_empty() { "/" } else { folder })?;
    entries.into_iter().find(|(n, _)| n == name).map(|(_, f)| f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Param;

    /// Sources with nothing in them.
    struct Nothing;

    impl Sources for Nothing {
        fn entries(&self, _: &str) -> Option<Vec<(String, bool)>> {
            None
        }
        fn programs(&self) -> Vec<String> {
            Vec::new()
        }
        fn functions(&self) -> Vec<(String, Vec<String>)> {
            Vec::new()
        }
        fn variables(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn environment(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn params(&self, _: &str) -> Option<Vec<Param>> {
            None
        }
    }

    fn editor(history: &[&str]) -> Editor {
        Editor::new(history.iter().map(|&s| Entry::from(s)).collect())
    }

    fn typed(e: &mut Editor, text: &str) {
        for c in text.chars() {
            e.key(Key::Char(c), &Nothing);
        }
    }

    #[test]
    fn edits_and_accepts_the_line() {
        let mut e = editor(&[]);
        typed(&mut e, "ls -la");
        e.key(Key::Ctrl('a'), &Nothing);
        e.key(Key::Ctrl('d'), &Nothing);
        e.key(Key::Ctrl('d'), &Nothing);
        typed(&mut e, "pwd");
        e.key(Key::Ctrl('k'), &Nothing);
        assert_eq!(e.key(Key::Enter, &Nothing), Outcome::Accept("pwd".into()));
        let mut e = editor(&[]);
        assert_eq!(e.key(Key::Ctrl('d'), &Nothing), Outcome::Eof);
        assert_eq!(e.key(Key::Ctrl('c'), &Nothing), Outcome::Cancel);
        assert_eq!(e.key(Key::Ctrl('l'), &Nothing), Outcome::Clear);
    }

    #[test]
    fn joins_kills_and_yanks_them() {
        let mut e = editor(&[]);
        typed(&mut e, "echo one two");
        e.key(Key::Ctrl('w'), &Nothing);
        e.key(Key::Ctrl('w'), &Nothing);
        assert_eq!(e.line().text(), "echo ");
        e.key(Key::Ctrl('a'), &Nothing);
        e.key(Key::Ctrl('y'), &Nothing);
        assert_eq!(e.line().text(), "one twoecho ");
        // Another key in between starts a new kill.
        e.key(Key::Ctrl('e'), &Nothing);
        e.key(Key::Ctrl('w'), &Nothing);
        e.key(Key::Ctrl('y'), &Nothing);
        e.key(Key::Ctrl('y'), &Nothing);
        assert_eq!(e.line().text(), "one twoecho twoecho ");
    }

    #[test]
    fn moves_through_the_lines_of_an_entry_and_on_through_the_history() {
        let entry = "if x {\n  long line\n}";
        let mut e = editor(&["pwd", entry, "ls"]);
        e.key(Key::Up, &Nothing);
        e.key(Key::Up, &Nothing);
        assert_eq!((e.line().text(), e.line().cursor()), (entry, entry.len()));
        // Up goes to the same column of the line above, or its end.
        e.key(Key::Up, &Nothing);
        assert_eq!(e.line().cursor(), "if x {\n ".len());
        e.key(Key::Up, &Nothing);
        assert_eq!(e.line().cursor(), 1);
        e.key(Key::Ctrl('p'), &Nothing);
        assert_eq!(e.line().text(), "pwd");
        // Down enters the entry on its first line and goes through it.
        e.key(Key::Down, &Nothing);
        assert_eq!(
            (e.line().text(), e.line().cursor()),
            (entry, "if x {".len())
        );
        e.key(Key::Down, &Nothing);
        e.key(Key::Ctrl('n'), &Nothing);
        assert_eq!(e.line().cursor(), entry.len());
        e.key(Key::Down, &Nothing);
        assert_eq!(e.line().text(), "ls");
        // Enter takes the whole entry.
        e.key(Key::Up, &Nothing);
        assert_eq!(e.key(Key::Enter, &Nothing), Outcome::Accept(entry.into()));
    }

    #[test]
    fn browses_the_entries_that_start_like_the_line() {
        let mut e = editor(&["ls /", "cd /tmp", "ls /usr", "ls /usr"]);
        e.key(Key::Up, &Nothing);
        assert_eq!(e.line().text(), "ls /usr");
        e.key(Key::Up, &Nothing);
        assert_eq!(e.line().text(), "cd /tmp");
        e.key(Key::Down, &Nothing);
        e.key(Key::Down, &Nothing);
        assert_eq!(e.line().text(), "");
        typed(&mut e, "ls");
        e.key(Key::Up, &Nothing);
        e.key(Key::Up, &Nothing);
        assert_eq!(e.line().text(), "ls /");
        e.key(Key::Up, &Nothing);
        assert_eq!(e.line().text(), "ls /");
        e.key(Key::Down, &Nothing);
        e.key(Key::Down, &Nothing);
        assert_eq!(e.line().text(), "ls");
    }

    #[test]
    fn searches_the_history_backwards() {
        let mut e = editor(&["make test", "ls", "make run", "cat x"]);
        typed(&mut e, "pw");
        e.key(Key::Ctrl('r'), &Nothing);
        typed(&mut e, "make");
        assert_eq!(e.line().text(), "make run");
        e.key(Key::Ctrl('r'), &Nothing);
        assert_eq!(e.line().text(), "make test");
        e.key(Key::Ctrl('r'), &Nothing);
        assert_eq!(e.line().text(), "make test");
        typed(&mut e, "z");
        assert_eq!(e.line().text(), "pw");
        e.key(Key::Backspace, &Nothing);
        assert_eq!(e.search().map(|s| s.found), Some(Some(2)));
        e.key(Key::Escape, &Nothing);
        assert_eq!(e.line().text(), "pw");
        e.key(Key::Ctrl('r'), &Nothing);
        typed(&mut e, "ls");
        e.key(Key::Ctrl('e'), &Nothing);
        typed(&mut e, " -l");
        assert!(e.search().is_none());
        assert_eq!(e.key(Key::Enter, &Nothing), Outcome::Accept("ls -l".into()));
    }

    /// A folder with two notes and a program.
    struct Files;

    impl Sources for Files {
        fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
            (folder == ".")
                .then(|| vec![("notes.txt".to_string(), false), ("news".to_string(), true)])
        }
        fn programs(&self) -> Vec<String> {
            vec!["printf".into()]
        }
        fn functions(&self) -> Vec<(String, Vec<String>)> {
            Vec::new()
        }
        fn variables(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn environment(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn params(&self, _: &str) -> Option<Vec<Param>> {
            None
        }
    }

    fn press(e: &mut Editor, keys: &[Key]) {
        for &key in keys {
            e.key(key, &Files);
        }
    }

    #[test]
    fn completes_one_candidate_or_shows_the_menu() {
        let mut e = editor(&[]);
        typed(&mut e, "cat not");
        press(&mut e, &[Key::Tab]);
        assert_eq!(e.line().text(), "cat notes.txt ");
        typed(&mut e, "ne");
        press(&mut e, &[Key::Tab]);
        assert_eq!(e.line().text(), "cat notes.txt news/");
        let mut e = editor(&[]);
        typed(&mut e, "pr");
        press(&mut e, &[Key::Tab]);
        assert_eq!(e.line().text(), "print");
        assert_eq!(e.menu().map(|m| m.candidates.len()), Some(2));
        press(&mut e, &[Key::Tab, Key::Tab]);
        assert_eq!(e.line().text(), "printf");
        press(&mut e, &[Key::BackTab]);
        assert_eq!(e.line().text(), "print");
        press(&mut e, &[Key::Enter]);
        assert!(e.menu().is_none());
        assert_eq!(e.line().text(), "print ");
        // A key that the menu does not use closes it and edits.
        let mut e = editor(&[]);
        typed(&mut e, "cat n");
        press(&mut e, &[Key::Tab, Key::Down, Key::Char('x')]);
        assert!(e.menu().is_none());
        assert_eq!(e.line().text(), "cat news/x");
    }

    #[test]
    fn suggests_the_rest_from_history_or_a_candidate() {
        let mut e = editor(&["ls /usr/bin", "cat notes.txt"]);
        typed(&mut e, "ls /u");
        assert_eq!(e.suggestion(&Files), "sr/bin");
        press(&mut e, &[Key::Right]);
        assert_eq!(e.line().text(), "ls /usr/bin");
        assert_eq!(e.suggestion(&Files), "");
        let mut e = editor(&[]);
        typed(&mut e, "less not");
        assert_eq!(e.suggestion(&Files), "es.txt");
        press(&mut e, &[Key::Left]);
        assert_eq!(e.suggestion(&Files), "");
        press(&mut e, &[Key::End]);
        assert_eq!(e.line().text(), "less not");
        press(&mut e, &[Key::End]);
        assert_eq!(e.line().text(), "less notes.txt");
    }

    /// Sources that count how often they are asked for programs.
    struct Counting(std::cell::Cell<usize>);

    impl Sources for Counting {
        fn entries(&self, _folder: &str) -> Option<Vec<(String, bool)>> {
            None
        }
        fn programs(&self) -> Vec<String> {
            self.0.set(self.0.get() + 1);
            vec!["zz-tool".to_string()]
        }
        fn functions(&self) -> Vec<(String, Vec<String>)> {
            Vec::new()
        }
        fn variables(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn environment(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn params(&self, _path: &str) -> Option<Vec<Param>> {
            None
        }
    }

    /// Sources whose current folder is ada's `Developer`, holding `src`
    /// and `notes.txt`.
    struct InDeveloper;

    impl Sources for InDeveloper {
        fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
            match folder {
                "." => Some(vec![("src".into(), true), ("notes.txt".into(), false)]),
                _ => None,
            }
        }
        fn programs(&self) -> Vec<String> {
            Vec::new()
        }
        fn functions(&self) -> Vec<(String, Vec<String>)> {
            Vec::new()
        }
        fn variables(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn environment(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn params(&self, _path: &str) -> Option<Vec<Param>> {
            None
        }
        fn current_folder(&self) -> Option<String> {
            Some("/Users/ada/Developer".into())
        }
    }

    fn entry(line: &str, folder: Option<&str>, paths: &[&str]) -> Entry {
        Entry {
            line: line.into(),
            folder: folder.map(str::to_owned),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn suggests_from_the_history_by_folder() {
        let entries = vec![
            entry(
                "cat notes.txt",
                Some("/Users/ada/Developer"),
                &["notes.txt"],
            ),
            entry("cd Developer", Some("/Users/ada"), &["Developer"]),
            entry("cat notes.old", Some("/elsewhere"), &["notes.old"]),
            entry("cd src", Some("/elsewhere"), &["src"]),
            entry("ls", None, &[]),
        ];
        let mut e = Editor::new(entries);
        // The entry of another folder, whose cd target is not here, is
        // not suggested.
        typed(&mut e, "cd D");
        assert_eq!(e.suggestion(&InDeveloper), "");
        // One of another folder whose paths are here is.
        let mut e = Editor::new(e.entries.clone());
        typed(&mut e, "cd ");
        assert_eq!(e.suggestion(&InDeveloper), "src");
        // An entry made here comes before a newer one from elsewhere
        // whose paths are not here.
        let mut e = Editor::new(e.entries.clone());
        typed(&mut e, "cat n");
        assert_eq!(e.suggestion(&InDeveloper), "otes.txt");
        // An entry without a folder counts as from elsewhere.
        let mut e = Editor::new(e.entries.clone());
        typed(&mut e, "l");
        assert_eq!(e.suggestion(&InDeveloper), "s");
    }

    #[test]
    fn keeps_a_suggestion_while_the_typing_follows_it() {
        let sources = Counting(std::cell::Cell::new(0));
        let mut e = editor(&[]);
        typed(&mut e, "zz");
        assert_eq!(e.suggestion(&sources), "-tool");
        assert_eq!(e.suggestion(&sources), "-tool");
        typed(&mut e, "-t");
        assert_eq!(e.suggestion(&sources), "ool");
        assert_eq!(sources.0.get(), 1);
        // Another character looks for a suggestion again.
        typed(&mut e, "x");
        assert_eq!(e.suggestion(&sources), "");
        assert_eq!(sources.0.get(), 2);
        press(&mut e, &[Key::Backspace]);
        assert_eq!(e.suggestion(&sources), "ool");
        assert_eq!(sources.0.get(), 3);
        // Typed to its end, the suggestion is looked for again.
        typed(&mut e, "ool");
        assert_eq!(e.suggestion(&sources), "");
        assert_eq!(sources.0.get(), 4);
    }

    #[test]
    fn shows_the_search_or_the_menu_below() {
        let mut e = editor(&["make"]);
        press(&mut e, &[Key::Ctrl('r'), Key::Char('q')]);
        let below = e.below(80, 10, "\x1b[90m");
        assert_eq!(
            super::super::render::visible(&below[0]),
            "failing bck-i-search: q_"
        );
        press(&mut e, &[Key::Escape]);
        typed(&mut e, "pr");
        press(&mut e, &[Key::Tab]);
        let below = e.below(80, 10, "\x1b[90m");
        assert_eq!(
            super::super::render::visible(&below[0]),
            "print  (built-in)  printf  (program)"
        );
    }
}
