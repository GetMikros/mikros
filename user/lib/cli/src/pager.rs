//! Help that is longer than the terminal goes through less
//! (MIKSPEC-0098.R62).

use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};

/// The pager, which takes the text on its standard input.
const LESS: &str = "/system/bin/less";

/// The rows that `text` takes on a terminal `columns` wide, each line at
/// least one, long lines as many as they wrap to.
pub fn rows(text: &str, columns: usize) -> usize {
    let columns = columns.max(1);
    text.lines()
        .map(|line| line.chars().count().div_ceil(columns).max(1))
        .sum()
}

/// Whether `text` needs the pager on a terminal of `rows` by `columns`:
/// it takes more rows than all but one.
pub fn needs_paging(text: &str, rows_cols: (usize, usize)) -> bool {
    let (terminal_rows, columns) = rows_cols;
    rows(text, columns) > terminal_rows.saturating_sub(1)
}

/// Writes `text`, a command's help, to standard output, through less when
/// standard output is a terminal that it would not fit on, and directly
/// when less does not start, for commands that write their help
/// themselves.
pub fn show(text: &str) {
    let paged = std::io::stdout().is_terminal()
        && window().is_some_and(|size| needs_paging(text, size))
        && page(text);
    if !paged {
        let _ = std::io::stdout().write_all(text.as_bytes());
    }
}

/// Runs less on `text` until it exits; whether it started.
fn page(text: &str) -> bool {
    let Ok(mut less) = Command::new(LESS).stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(mut input) = less.stdin.take() {
        // Quitting less early closes its input, which is no failure.
        let _ = input.write_all(text.as_bytes());
    }
    let _ = less.wait();
    true
}

/// The rows and columns of the terminal of standard output, from the
/// terminal service's GetWindowSize.
#[cfg(target_os = "mikros")]
fn window() -> Option<(usize, usize)> {
    use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
    use mikros_rt::interfaces::mikros::service::DirectoryClient;
    use mikros_rt::interfaces::mikros::terminal::{Terminal, TerminalClient};
    use std::os::mikros::io::AsHandleExt;
    const TERMINAL: &str = "mikros.terminal.Terminal";
    const SAME_RIGHTS: u32 = 1 << 31;
    const FOREVER: i64 = i64::MAX;
    // The token of the terminal behind standard output.
    let token = std::io::stdout().handle().ok()?.into_raw_handle();
    // SAFETY: the handle is a new one, which the call below takes.
    let token = unsafe { OwnedHandle::from_raw(token) };
    let mut raw = 0;
    let directory = std::os::mikros::directory().as_raw_handle();
    // SAFETY: the out pointer is valid for the call.
    if unsafe { mikros_rt::sys::handle_duplicate(directory, SAME_RIGHTS, &mut raw) }
        != mikros_rt::OK
    {
        return None;
    }
    // SAFETY: the kernel just made the handle for us.
    let directory = DirectoryClient::new(ClientEnd::new(unsafe { OwnedHandle::from_raw(raw) }));
    let (client, server) = endpoints::<Terminal>().ok()?;
    match directory.connect(TERMINAL.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    let size = TerminalClient::new(client)
        .get_window_size(token, FOREVER)
        .ok()?
        .ok()?
        .size;
    (size.rows > 0 && size.columns > 0).then(|| (usize::from(size.rows), usize::from(size.columns)))
}

/// Other systems have no terminal service, so help is not paged there.
#[cfg(not(target_os = "mikros"))]
fn window() -> Option<(usize, usize)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_wrapped_rows() {
        assert_eq!(rows("a\nb\n", 80), 2);
        assert_eq!(rows("\n\n", 80), 2);
        assert_eq!(rows(&"x".repeat(81), 80), 2);
        assert_eq!(rows(&"x".repeat(160), 80), 2);
        assert_eq!(rows("äöü", 2), 2);
    }

    #[test]
    fn pages_what_does_not_fit_above_the_last_row() {
        let text = "1\n2\n3\n";
        assert!(!needs_paging(text, (4, 80)));
        assert!(needs_paging(text, (3, 80)));
        assert!(needs_paging(&"x".repeat(100), (2, 40)));
    }
}
