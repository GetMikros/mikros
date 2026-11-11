//! Prompt expansion, as zsh's: `%n` the user's name, `%m` the host's
//! name up to its first dot, `%~` the current folder with the home
//! folder as `~`, `%d` the current folder, `%#` `#` in an elevated
//! process and `%` otherwise, `%?` the last status, `%T` the time as
//! HH:MM, `%D` the date as YY-MM-DD, `%F{color}` and `%f` to start and
//! end a colour, `%B` and `%b` bold, and `%%` a `%`.

use crate::value::civil_from_days;

/// The default prompt.
pub const DEFAULT: &str = "%n@%m %~ %# ";

/// What prompt expansion shows.
pub struct Context<'a> {
    pub user: &'a str,
    pub host: &'a str,
    pub folder: &'a str,
    pub home: Option<&'a str>,
    pub elevated: bool,
    pub status: i64,
    /// The local time, in seconds since 1970-01-01T00:00:00 local.
    pub local_time: i64,
}

/// The colours that `%F{name}` takes, with their numbers.
const COLOURS: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// `template` with its escapes replaced.
pub fn expand(template: &str, cx: &Context) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push_str(cx.user),
            Some('m') => out.push_str(cx.host.split('.').next().unwrap_or("")),
            Some('~') => out.push_str(&tilde(cx.folder, cx.home)),
            Some('d') => out.push_str(cx.folder),
            Some('#') => out.push(if cx.elevated { '#' } else { '%' }),
            Some('?') => out.push_str(&cx.status.to_string()),
            Some('T') => {
                let secs = cx.local_time.rem_euclid(86_400);
                out.push_str(&format!("{:02}:{:02}", secs / 3600, secs / 60 % 60));
            }
            Some('D') => {
                let (y, m, d) = civil_from_days(cx.local_time.div_euclid(86_400));
                out.push_str(&format!("{:02}-{m:02}-{d:02}", y.rem_euclid(100)));
            }
            Some('F') => {
                let mut name = String::new();
                if chars.peek() == Some(&'{') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        name.push(c);
                    }
                }
                if let Some(code) = colour(&name) {
                    out.push_str(&code);
                }
            }
            Some('f') => out.push_str("\x1b[39m"),
            Some('B') => out.push_str("\x1b[1m"),
            Some('b') => out.push_str("\x1b[22m"),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// `folder` with the home folder at its start written `~`.
fn tilde(folder: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if !home.is_empty() && folder == home => "~".into(),
        Some(home) if !home.is_empty() => match folder.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => folder.into(),
        },
        _ => folder.into(),
    }
}

/// The escape sequence of a colour by name or number.
fn colour(name: &str) -> Option<String> {
    if let Some(n) = COLOURS.iter().position(|c| *c == name) {
        return Some(format!("\x1b[3{n}m"));
    }
    let n: u8 = name.parse().ok()?;
    Some(format!("\x1b[38;5;{n}m"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx<'a>() -> Context<'a> {
        Context {
            user: "ada",
            host: "box.local",
            folder: "/Users/ada/src",
            home: Some("/Users/ada"),
            elevated: false,
            status: 2,
            // 2026-11-10T09:05:00
            local_time: 1_794_268_800 + 9 * 3600 + 5 * 60,
        }
    }

    #[test]
    fn expands_the_default_prompt() {
        assert_eq!(expand(DEFAULT, &cx()), "ada@box ~/src % ");
        let root = Context {
            elevated: true,
            folder: "/Users/ada",
            ..cx()
        };
        assert_eq!(expand(DEFAULT, &root), "ada@box ~ # ");
        let other = Context {
            folder: "/Users/adam",
            ..cx()
        };
        assert_eq!(expand("%~", &other), "/Users/adam");
    }

    #[test]
    fn expands_status_time_date_colours_and_percent() {
        assert_eq!(
            expand("%? %T %D %d %%", &cx()),
            "2 09:05 26-11-10 /Users/ada/src %"
        );
        assert_eq!(
            expand("%F{red}x%f%B!%b", &cx()),
            "\x1b[31mx\x1b[39m\x1b[1m!\x1b[22m"
        );
        assert_eq!(expand("%F{208}", &cx()), "\x1b[38;5;208m");
        assert_eq!(expand("%z", &cx()), "%z");
    }
}
