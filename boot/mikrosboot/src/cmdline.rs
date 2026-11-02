//! Finding the kernel command line.

use alloc::string::String;
use boot_protocol::CMDLINE_MAX;
use core::fmt;

/// Why the command line could not be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `boot.conf` is not valid UTF-8.
    NotUtf8,
    /// The command line is longer than `CMDLINE_MAX` bytes.
    TooLong(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotUtf8 => write!(f, "boot.conf is not valid UTF-8"),
            Error::TooLong(len) => {
                write!(
                    f,
                    "the command line is {len} bytes, more than {CMDLINE_MAX}"
                )
            }
        }
    }
}

/// Reads the image's load options as a command line. Returns `None` unless
/// they hold a non-empty UCS-2 string followed by a null character; bytes
/// after the null character are ignored.
pub fn from_load_options(options: &[u8]) -> Option<String> {
    let units = options
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c));
    let mut text = String::new();
    for unit in units {
        if unit == 0 {
            return (!text.is_empty()).then_some(text);
        }
        // UCS-2 has no surrogates, so a surrogate means the options are not
        // a UCS-2 string.
        text.push(char::from_u32(u32::from(unit))?);
    }
    None
}

/// Reads the contents of `boot.conf` as a command line, without its trailing
/// carriage returns and line feeds.
pub fn from_boot_conf(contents: &[u8]) -> Result<String, Error> {
    let end = contents
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(0, |i| i + 1);
    let text = core::str::from_utf8(&contents[..end]).map_err(|_| Error::NotUtf8)?;
    Ok(String::from(text))
}

/// Fails when `cmdline` is too long to pass to the kernel.
pub fn check_len(cmdline: &str) -> Result<(), Error> {
    if cmdline.len() > CMDLINE_MAX {
        return Err(Error::TooLong(cmdline.len()));
    }
    Ok(())
}

/// The size asked for with the first `video=<width>x<height>` option, if it
/// is well formed.
pub fn video_mode(cmdline: &str) -> Option<(usize, usize)> {
    let value = cmdline
        .split_ascii_whitespace()
        .find_map(|option| option.strip_prefix("video="))?;
    let (width, height) = value.split_once('x')?;
    let number = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<usize>().ok())
            .flatten()
    };
    Some((number(width)?, number(height)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn ucs2(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn reads_null_terminated_load_options() {
        let mut options = ucs2("root=bootfs quiet\0");
        assert_eq!(
            from_load_options(&options).as_deref(),
            Some("root=bootfs quiet")
        );
        options.extend_from_slice(&[1, 2, 3]);
        assert_eq!(
            from_load_options(&options).as_deref(),
            Some("root=bootfs quiet")
        );
        assert_eq!(from_load_options(&ucs2("é ü\0")).as_deref(), Some("é ü"));
    }

    #[test]
    fn ignores_load_options_that_are_not_a_string() {
        assert_eq!(from_load_options(&[]), None);
        assert_eq!(from_load_options(&ucs2("\0")), None, "empty");
        assert_eq!(from_load_options(&ucs2("no null")), None);
        assert_eq!(
            from_load_options(&[0x41, 0, 0]),
            None,
            "odd length, no null"
        );
        assert_eq!(from_load_options(&ucs2("\u{1F600}\0")), None, "surrogates");
    }

    #[test]
    fn reads_boot_conf_without_trailing_line_ends() {
        assert_eq!(
            from_boot_conf(b"root=PARTUUID=x\r\n\n").unwrap(),
            "root=PARTUUID=x"
        );
        assert_eq!(from_boot_conf(b"a\nb\n").unwrap(), "a\nb");
        assert_eq!(from_boot_conf(b"\r\n").unwrap(), "");
        assert_eq!(from_boot_conf(b"bad \xff"), Err(Error::NotUtf8));
    }

    #[test]
    fn limits_the_length() {
        let long = "x".repeat(CMDLINE_MAX);
        assert_eq!(check_len(&long), Ok(()));
        assert_eq!(
            check_len(&(long + "y")),
            Err(Error::TooLong(CMDLINE_MAX + 1))
        );
    }

    #[test]
    fn finds_the_video_option() {
        assert_eq!(
            video_mode("quiet video=1920x1080 root=x"),
            Some((1920, 1080))
        );
        assert_eq!(video_mode("video=800x600 video=1024x768"), Some((800, 600)));
        assert_eq!(video_mode("video=1920x"), None);
        assert_eq!(video_mode("video=+1x2"), None);
        assert_eq!(video_mode("myvideo=1x2"), None);
        assert_eq!(video_mode(""), None);
    }
}
