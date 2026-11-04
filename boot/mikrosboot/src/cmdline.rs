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

/// The partition GUID of a hard drive media device path node, given by
/// type, subtype and data, when it is one with a GPT signature.
pub fn gpt_partition_guid(kind: u8, sub: u8, data: &[u8]) -> Option<[u8; 16]> {
    const MEDIA: u8 = 4;
    const HARD_DRIVE: u8 = 1;
    const GPT: u8 = 2;
    if kind != MEDIA || sub != HARD_DRIVE || *data.get(37)? != GPT {
        return None;
    }
    data.get(20..36)?.try_into().ok()
}

/// `guid`, stored as GPT stores it, in its 36-character textual form with
/// lowercase digits.
pub fn guid_text(guid: &[u8; 16]) -> String {
    let g = guid;
    alloc::format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g[3],
        g[2],
        g[1],
        g[0],
        g[5],
        g[4],
        g[7],
        g[6],
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

/// `cmdline` with `esp=PARTUUID=<guid>` appended for the partition the
/// bootloader was loaded from, unless it already has an `esp=` option or
/// no such partition is known.
pub fn with_esp(cmdline: String, partition: Option<[u8; 16]>) -> String {
    let Some(guid) = partition else {
        return cmdline;
    };
    if cmdline
        .split_ascii_whitespace()
        .any(|option| option.starts_with("esp="))
    {
        return cmdline;
    }
    let option = alloc::format!("esp=PARTUUID={}", guid_text(&guid));
    if cmdline.is_empty() {
        option
    } else {
        alloc::format!("{cmdline} {option}")
    }
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

    /// A hard drive node's data: partition number, start, size, the
    /// partition GUID, the MBR type and the signature type.
    fn hard_drive(signature_type: u8) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&2048u64.to_le_bytes());
        d.extend_from_slice(&4096u64.to_le_bytes());
        // 01234567-89ab-cdef-0123-456789abcdef as GPT stores it.
        d.extend_from_slice(&[
            0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ]);
        d.extend_from_slice(&[2, signature_type]);
        d
    }

    #[test]
    fn appends_the_boot_partition() {
        let guid = gpt_partition_guid(4, 1, &hard_drive(2));
        assert_eq!(
            guid.map(|g| guid_text(&g)).as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert_eq!(
            gpt_partition_guid(4, 1, &hard_drive(1)),
            None,
            "an MBR disk"
        );
        assert_eq!(
            gpt_partition_guid(4, 4, &hard_drive(2)),
            None,
            "a file path"
        );
        assert_eq!(gpt_partition_guid(4, 1, &hard_drive(2)[..30]), None);
        let esp = "esp=PARTUUID=01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            with_esp(String::from("root=bootfs"), guid),
            alloc::format!("root=bootfs {esp}")
        );
        assert_eq!(with_esp(String::new(), guid), esp);
        let given = String::from("quiet esp=PARTUUID=x");
        assert_eq!(
            with_esp(given.clone(), guid),
            given,
            "an esp= option is kept"
        );
        assert_eq!(with_esp(String::from("quiet"), None), "quiet");
    }
}
