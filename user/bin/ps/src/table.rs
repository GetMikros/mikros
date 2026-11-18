//! What ps takes, and what it writes: a header line and one line per
//! process in aligned columns, or a JSON array with an object per process
//! (MIKSPEC-0098.R9).

use mikros_cli::{Opt, Paint, Role, Spec, json_string};

pub static COMMAND: Spec = Spec {
    options: &[Opt::long(
        "json",
        "write the processes as a JSON array of objects",
    )],
    ..Spec::new(
        "ps",
        "Lists the processes, sorted by process ID, with their parent, user,\n\
         app ID and executable.",
    )
};

/// A process, as ps shows it.
#[derive(Debug, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub process_group: u32,
    pub session: u32,
    pub login_session: u32,
    pub user: u32,
    pub user_name: String,
    pub app_id: String,
    pub path: String,
}

/// The header and lines of `processes`: the process and parent IDs
/// aligned to the right, the user and app ID to the left, and the path.
pub fn text(processes: &[Process], paint: Paint) -> String {
    let header = ["PID", "PARENT", "USER", "APP ID", "PATH"];
    let rows: Vec<Vec<Cell>> = processes
        .iter()
        .map(|p| {
            vec![
                (p.pid.to_string(), Some(Role::Number)),
                (p.parent.to_string(), Some(Role::Number)),
                (p.user_name.clone(), None),
                (p.app_id.clone(), None),
                (p.path.clone(), Some(Role::Path)),
            ]
        })
        .collect();
    columns(&header, &rows, &[true, true, false, false, false], paint)
}

/// A cell of a table: its text, and the role it is coloured in.
type Cell = (String, Option<Role>);

/// The header and `rows` in columns two spaces apart, the header in the
/// secondary colour, each column as wide as its widest text without
/// colours, aligned to the right where `right` says, the last one not
/// padded.
fn columns(header: &[&str], rows: &[Vec<Cell>], right: &[bool], paint: Paint) -> String {
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

/// `processes` as a JSON array of objects, on one line.
pub fn json(processes: &[Process]) -> String {
    let objects: Vec<String> = processes
        .iter()
        .map(|p| {
            format!(
                "{{\"pid\":{},\"parent\":{},\"process_group\":{},\"session\":{},\
                 \"login_session\":{},\"user\":{},\"user_name\":{},\"app_id\":{},\"path\":{}}}",
                p.pid,
                p.parent,
                p.process_group,
                p.session,
                p.login_session,
                p.user,
                json_string(&p.user_name),
                json_string(&p.app_id),
                json_string(&p.path),
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processes() -> Vec<Process> {
        let process = |pid, parent, user_name: &str, app_id: &str, path: &str| Process {
            pid,
            parent,
            process_group: pid,
            session: 1,
            login_session: 0,
            user: if user_name == "system" { 0 } else { 1000 },
            user_name: user_name.into(),
            app_id: app_id.into(),
            path: path.into(),
        };
        vec![
            process(1, 0, "system", "system.init", "init"),
            process(27, 3, "system", "system.termsvc", "/usr/bin/termsvc"),
            process(115, 27, "ada", "system.cli", "/usr/bin/msh"),
        ]
    }

    #[test]
    fn aligns_the_columns_under_a_header() {
        assert_eq!(
            text(&processes(), Paint::never()),
            "PID  PARENT  USER    APP ID          PATH\n  \
             1       0  system  system.init     init\n \
             27       3  system  system.termsvc  /usr/bin/termsvc\n\
             115      27  ada     system.cli      /usr/bin/msh\n"
        );
        assert_eq!(
            text(&[], Paint::never()),
            "PID  PARENT  USER  APP ID  PATH\n"
        );
    }

    #[test]
    fn colours_by_role_and_keeps_the_columns() {
        let on = Paint::new(Some("always"), false, None);
        let paint = |sgr: &str, t: &str| format!("\x1b[{sgr}m{t}\x1b[0m");
        let number = |t| paint("38;2;21;193;160", t);
        let line = text(&processes()[..2], on);
        let second = line.lines().nth(2).unwrap();
        assert_eq!(
            second,
            format!(
                " {}       {}  system  system.termsvc  {}",
                number("27"),
                number("3"),
                paint("38;2;27;213;177", "/usr/bin/termsvc")
            )
        );
        assert!(line.starts_with("\x1b[90mPID\x1b[0m  \x1b[90mPARENT\x1b[0m  "));
    }

    #[test]
    fn writes_an_array_of_objects() {
        let processes = processes();
        assert_eq!(
            json(&processes[2..]),
            "[{\"pid\":115,\"parent\":27,\"process_group\":115,\"session\":1,\
             \"login_session\":0,\"user\":1000,\"user_name\":\"ada\",\
             \"app_id\":\"system.cli\",\"path\":\"/usr/bin/msh\"}]\n"
        );
        assert_eq!(json(&[]), "[]\n");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/ps.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
