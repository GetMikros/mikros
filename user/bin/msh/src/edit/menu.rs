//! The completion menu below the line: the candidates in a grid, row by
//! row, each with its description dimmed in parentheses, the selected
//! one in reverse video, and only as many rows as fit, around the
//! selection.

use super::complete::Candidate;
use super::render::visible;

/// The candidates shown for the word that starts at `start`.
#[derive(Clone, Debug)]
pub struct Menu {
    pub start: usize,
    pub candidates: Vec<Candidate>,
    pub selected: Option<usize>,
    /// The columns of the grid as last drawn, which Up and Down move
    /// by.
    pub columns: usize,
}

const REVERSE: &str = "\x1b[7m";
const NORMAL: &str = "\x1b[0m";

/// The longest start that all of `texts` share.
pub fn common_prefix<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    let mut texts = texts.into_iter();
    let Some(first) = texts.next() else {
        return String::new();
    };
    let mut len = first.len();
    for text in texts {
        len = first
            .char_indices()
            .zip(text.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8())
            .min(len);
    }
    first[..len].to_owned()
}

impl Menu {
    pub fn new(start: usize, candidates: Vec<Candidate>) -> Menu {
        Menu {
            start,
            candidates,
            selected: None,
            columns: 1,
        }
    }

    /// Moves the selection by `step` candidates, wrapping around; with
    /// none selected, forwards selects the first and backwards the last.
    pub fn select(&mut self, step: isize) {
        let n = self.candidates.len() as isize;
        if n == 0 {
            return;
        }
        let next = match self.selected {
            None if step >= 0 => 0,
            None => n - 1,
            Some(at) => (at as isize + step).rem_euclid(n),
        };
        self.selected = Some(next as usize);
    }

    /// The candidate selected.
    pub fn chosen(&self) -> Option<&Candidate> {
        self.candidates.get(self.selected?)
    }

    /// The rows of the menu for a terminal `width` columns wide, at most
    /// `height` of them, with the descriptions drawn with `dim`.
    pub fn rows(&mut self, width: usize, height: usize, dim: &str) -> Vec<String> {
        if self.candidates.is_empty() {
            return Vec::new();
        }
        let cells: Vec<(String, String)> = self
            .candidates
            .iter()
            .map(|c| {
                let description = c
                    .description
                    .as_ref()
                    .map_or(String::new(), |d| format!("  ({d})"));
                (c.display.clone(), description)
            })
            .collect();
        let widest = cells
            .iter()
            .map(|(d, e)| visible(d).chars().count() + e.chars().count())
            .max()
            .unwrap_or(0);
        let cell = (widest + 2).min(width.saturating_sub(1)).max(1);
        self.columns = (width.saturating_sub(1) / cell).max(1);
        let total = cells.len().div_ceil(self.columns);
        let height = height.max(1);
        let shown = total.min(height);
        // The rows shown start so that the selected one is among them.
        let selected_row = self.selected.map_or(0, |s| s / self.columns);
        let first = selected_row.saturating_sub(shown - 1).min(total - shown);
        let mut rows = Vec::new();
        for row in first..first + shown {
            let mut line = String::new();
            for column in 0..self.columns {
                let at = row * self.columns + column;
                let Some((display, description)) = cells.get(at) else {
                    break;
                };
                let used = display.chars().count() + description.chars().count();
                let pad = " ".repeat(cell.saturating_sub(used));
                let on = self.selected == Some(at);
                line.push_str(if on { REVERSE } else { "" });
                line.push_str(display);
                line.push_str(NORMAL);
                line.push_str(dim);
                line.push_str(description);
                line.push_str(NORMAL);
                line.push_str(&pad);
            }
            rows.push(line.trim_end().to_owned());
        }
        if shown < total {
            let more = total - shown;
            rows.push(format!("{dim}…and {more} more rows{NORMAL}"));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(names: &[&str]) -> Menu {
        let candidates = names
            .iter()
            .map(|n| Candidate {
                text: n.to_string(),
                display: n.to_string(),
                description: None,
                done: true,
            })
            .collect();
        Menu::new(0, candidates)
    }

    #[test]
    fn finds_the_common_prefix() {
        assert_eq!(common_prefix(["print", "printf", "pri"]), "pri");
        assert_eq!(common_prefix(["été", "étage"]), "ét");
        assert_eq!(common_prefix(["a", "b"]), "");
        assert_eq!(common_prefix([]), "");
    }

    #[test]
    fn moves_the_selection_around() {
        let mut m = menu(&["a", "b", "c"]);
        m.select(1);
        assert_eq!(m.selected, Some(0));
        m.select(-1);
        assert_eq!(m.selected, Some(2));
        m.select(1);
        assert_eq!(m.chosen().map(|c| c.text.as_str()), Some("a"));
        let mut m = menu(&["a"]);
        m.select(-1);
        assert_eq!(m.selected, Some(0));
    }

    #[test]
    fn lays_the_candidates_out_in_rows() {
        let mut m = menu(&["one", "two", "three", "four", "five"]);
        m.candidates[0].description = Some("1".into());
        // Cells of 10 columns, 2 of them on 25 columns.
        let rows = m.rows(25, 10, "\x1b[90m");
        assert_eq!(m.columns, 2);
        assert_eq!(rows.len(), 3);
        assert_eq!(visible(&rows[0]), "one  (1)  two");
        assert_eq!(visible(&rows[2]), "five");
        m.selected = Some(4);
        let rows = m.rows(25, 2, "\x1b[90m");
        assert_eq!(rows.len(), 3);
        assert_eq!(visible(&rows[0]), "three     four");
        assert!(rows[1].starts_with("\x1b[7mfive"));
        assert_eq!(visible(&rows[2]), "…and 1 more rows");
    }
}
