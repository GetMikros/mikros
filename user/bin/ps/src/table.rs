//! What ps writes: a header line and one line per process in aligned
//! columns, or a JSON array with an object per process.

use mikros_cli::json_string;

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
pub fn text(processes: &[Process]) -> String {
    let header = ["PID", "PARENT", "USER", "APP ID", "PATH"];
    let mut rows = vec![header.map(String::from)];
    rows.extend(processes.iter().map(|p| {
        [
            p.pid.to_string(),
            p.parent.to_string(),
            p.user_name.clone(),
            p.app_id.clone(),
            p.path.clone(),
        ]
    }));
    let width = |column: usize| {
        rows.iter()
            .map(|r| r[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];
    rows.iter()
        .map(|r| {
            format!(
                "{:>w0$}  {:>w1$}  {:<w2$}  {:<w3$}  {}\n",
                r[0],
                r[1],
                r[2],
                r[3],
                r[4],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = widths[3],
            )
        })
        .collect()
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
            text(&processes()),
            "PID  PARENT  USER    APP ID          PATH\n  \
             1       0  system  system.init     init\n \
             27       3  system  system.termsvc  /usr/bin/termsvc\n\
             115      27  ada     system.cli      /usr/bin/msh\n"
        );
        assert_eq!(text(&[]), "PID  PARENT  USER  APP ID  PATH\n");
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
}
