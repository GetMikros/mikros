//! Lists in aligned columns under a header line, as the utilities show
//! their entries, coloured by role (MIKSPEC-0098.R31).

use crate::colour::{Paint, Role};

/// A cell of a table: its text, and the role it is coloured in.
pub type Cell = (String, Option<Role>);

/// The header line, in the secondary colour, and the lines of `rows`,
/// two spaces apart, each column as wide as its widest cell, measured
/// without colours, aligned to the right where `right` says so; the last
/// column is not padded when it is aligned to the left.
pub fn columns(header: &[&str], rows: &[Vec<Cell>], right: &[bool], paint: Paint) -> String {
    let header: Vec<Cell> = header
        .iter()
        .map(|h| (h.to_string(), Some(Role::Secondary)))
        .collect();
    let all: Vec<&Vec<Cell>> = std::iter::once(&header).chain(rows).collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|c| {
            all.iter()
                .map(|r| r[c].0.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut text = String::new();
    for row in all {
        for (c, (cell, role)) in row.iter().enumerate() {
            let pad = " ".repeat(widths[c] - cell.chars().count());
            let shown = match role {
                Some(role) => paint.paint(*role, cell),
                None => cell.clone(),
            };
            if c > 0 {
                text.push_str("  ");
            }
            match (right[c], c + 1 == row.len()) {
                (true, _) => text.push_str(&(pad + &shown)),
                (false, true) => text.push_str(&shown),
                (false, false) => text.push_str(&(shown + &pad)),
            }
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str, role: Option<Role>) -> Cell {
        (text.into(), role)
    }

    #[test]
    fn aligns_columns_measured_without_colours() {
        let rows = vec![
            vec![
                cell("logsvc", None),
                cell("7", Some(Role::Number)),
                cell("running", Some(Role::Ok)),
            ],
            vec![
                cell("termsvc", None),
                cell("12", Some(Role::Number)),
                cell("-", None),
            ],
        ];
        let plain = columns(
            &["NAME", "PID", "STATE"],
            &rows,
            &[false, true, false],
            Paint::never(),
        );
        assert_eq!(
            plain,
            "NAME     PID  STATE\nlogsvc     7  running\ntermsvc   12  -\n"
        );
        let on = Paint::new(Some("always"), false, None);
        let coloured = columns(&["NAME", "PID", "STATE"], &rows, &[false, true, false], on);
        let first = coloured.lines().nth(1).unwrap();
        assert_eq!(first, "logsvc     7  \x1b[32mrunning\x1b[0m");
        assert!(coloured.starts_with("\x1b[90mNAME\x1b[0m"));
    }
}
