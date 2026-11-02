//! `cargo xtask symbolize`: replaces the backtrace addresses in the
//! kernel panic reports of a serial log with their functions, files and
//! lines, from the debug information whose build ID each report names.

use crate::build::KERNEL_TARGET;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// The serial log whose panic reports to symbolize.
    pub log: PathBuf,
}

/// A function that the code at an address belongs to, with the file and
/// line of that code in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    function: String,
    file: Option<String>,
    line: Option<u32>,
}

impl Place {
    fn describe(&self) -> String {
        match (&self.file, self.line) {
            (Some(file), Some(line)) => format!("{} at {file}:{line}", self.function),
            (Some(file), None) => format!("{} at {file}", self.function),
            _ => self.function.clone(),
        }
    }
}

/// The frame number and return address of a backtrace line of a panic
/// report, `#N 0xADDRESS`.
fn frame_line(line: &str) -> Option<(&str, u64)> {
    let (number, address) = line.strip_prefix('#')?.split_once(" 0x")?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if address.len() != 16 {
        return None;
    }
    Some((number, u64::from_str_radix(address, 16).ok()?))
}

/// `log` with every backtrace line of its panic reports replaced by the
/// places `places` finds for the report's build ID and the frame's
/// return address, innermost inlined function first. Lines it finds no
/// place for stay as they are.
fn symbolize(log: &str, mut places: impl FnMut(&str, u64) -> Vec<Place>) -> String {
    let mut out = String::with_capacity(log.len());
    // The build ID of the report being read, empty before its build line.
    let mut report: Option<String> = None;
    for line in log.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        let ending = &line[text.len()..];
        if text.contains("KERNEL PANIC: ") {
            report = Some(String::new());
        } else if text == "END KERNEL PANIC" {
            report = None;
        } else if let Some(build) = &mut report {
            if let Some(id) = text.strip_prefix("build ") {
                *build = id.to_owned();
            } else if let Some((number, address)) = frame_line(text) {
                // A return address follows the call, which may be the
                // last instruction of an inlined function.
                let found = places(build, address.wrapping_sub(1));
                if let Some((first, outer)) = found.split_first() {
                    out.push_str(&format!("#{number} {}{ending}", first.describe()));
                    for place in outer {
                        out.push_str(&format!("    inlined into {}{ending}", place.describe()));
                    }
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

/// The debug information file of the kernel with build ID `build`,
/// which a build of any kind left in the build ID directory beside the
/// kernel.
fn debug_file(build: &str) -> Option<PathBuf> {
    if build.len() < 4 || !build.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let target = crate::workspace_root().join("target").join(KERNEL_TARGET);
    fs::read_dir(target)
        .ok()?
        .flatten()
        .map(|profile| {
            profile
                .path()
                .join(".build-id")
                .join(&build[..2])
                .join(format!("{}.debug", &build[2..]))
        })
        .find(|path| path.exists())
}

/// `path` relative to the workspace when it lies in it.
fn shorten(path: &str) -> String {
    let root = crate::workspace_root();
    Path::new(path)
        .strip_prefix(&root)
        .map_or(path.to_owned(), |p| p.display().to_string())
}

/// The places of `address` in the kernel that `loader` reads.
fn find(loader: &addr2line::Loader, address: u64) -> Vec<Place> {
    let mut places = Vec::new();
    if let Ok(mut frames) = loader.find_frames(address) {
        while let Ok(Some(frame)) = frames.next() {
            let function = frame
                .function
                .as_ref()
                .and_then(|f| f.demangle().ok())
                .map_or("??".to_owned(), |f| f.into_owned());
            let location = frame.location.as_ref();
            places.push(Place {
                function,
                file: location.and_then(|l| l.file).map(shorten),
                line: location.and_then(|l| l.line),
            });
        }
    }
    if places.is_empty()
        && let Some(symbol) = loader.find_symbol(address)
    {
        places.push(Place {
            function: addr2line::demangle_auto(symbol.into(), None).into_owned(),
            file: None,
            line: None,
        });
    }
    places
}

pub fn run(args: &Args) -> Result<()> {
    let log = fs::read(&args.log).with_context(|| format!("reading {}", args.log.display()))?;
    let log = String::from_utf8_lossy(&log);
    let mut loaders: HashMap<String, Option<addr2line::Loader>> = HashMap::new();
    let out = symbolize(&log, |build, address| {
        let loader = loaders.entry(build.to_owned()).or_insert_with(|| {
            let loader = debug_file(build).and_then(|p| addr2line::Loader::new(p).ok());
            if loader.is_none() {
                eprintln!("no debug information for build {build}");
            }
            loader
        });
        loader.as_ref().map_or(Vec::new(), |l| find(l, address))
    });
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(function: &str, line: u32) -> Place {
        Place {
            function: function.into(),
            file: Some("kernel/src/a.rs".into()),
            line: Some(line),
        }
    }

    #[test]
    fn replaces_the_frames_of_each_report() {
        let log = "boot\r\n\
                   KERNEL PANIC: oops\r\n\
                   build ab12\r\n\
                   rip=ffffffff80001234\r\n\
                   #0 0xffffffff80000043\r\n\
                   #1 0xffffffff80000100\r\n\
                   END KERNEL PANIC\r\n\
                   #2 0xffffffff80000043\r\n";
        let mut asked = Vec::new();
        let out = symbolize(log, |build, address| {
            asked.push((build.to_owned(), address));
            match address {
                0xffff_ffff_8000_0042 => vec![place("inner", 3), place("outer", 9)],
                _ => Vec::new(),
            }
        });
        assert_eq!(
            asked,
            [
                ("ab12".into(), 0xffff_ffff_8000_0042),
                ("ab12".into(), 0xffff_ffff_8000_00ff)
            ]
        );
        assert_eq!(
            out,
            "boot\r\n\
             KERNEL PANIC: oops\r\n\
             build ab12\r\n\
             rip=ffffffff80001234\r\n\
             #0 inner at kernel/src/a.rs:3\r\n    \
             inlined into outer at kernel/src/a.rs:9\r\n\
             #1 0xffffffff80000100\r\n\
             END KERNEL PANIC\r\n\
             #2 0xffffffff80000043\r\n"
        );
    }

    #[test]
    fn reads_only_frame_lines() {
        assert_eq!(frame_line("#12 0x00000000000010ff"), Some(("12", 0x10ff)));
        assert_eq!(frame_line("#x 0x00000000000010ff"), None);
        assert_eq!(frame_line("# 0x00000000000010ff"), None);
        assert_eq!(frame_line("#1 0x10ff"), None);
        assert_eq!(frame_line("rip=ffffffff80001234"), None);
    }
}
