//! Host tools that the development environment needs, with their minimum
//! versions and the commands that install them.

use std::fmt;
use std::process::Command;

/// A version number of the form `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Finds the first `major.minor[.patch]` number in `text`, optionally
    /// preceded by a `v`, as printed by `--version` options.
    pub fn find_in(text: &str) -> Option<Version> {
        text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .filter_map(Version::parse)
            .next()
    }

    fn parse(word: &str) -> Option<Version> {
        let mut parts = word.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        Some(Version(major, minor, patch))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A Linux distribution family that `cargo xtask setup` has install commands
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distro {
    Arch,
    Debian,
    Ubuntu,
    Fedora,
}

impl Distro {
    pub const ALL: [Distro; 4] = [Distro::Arch, Distro::Debian, Distro::Ubuntu, Distro::Fedora];

    pub fn name(self) -> &'static str {
        match self {
            Distro::Arch => "Arch Linux",
            Distro::Debian => "Debian",
            Distro::Ubuntu => "Ubuntu",
            Distro::Fedora => "Fedora",
        }
    }

    /// Detects the distribution from the contents of `/etc/os-release`. The
    /// `ID` field is preferred over `ID_LIKE`, so that Ubuntu, which declares
    /// itself like Debian, is recognized as Ubuntu.
    pub fn from_os_release(text: &str) -> Option<Distro> {
        let field = |key: &str| {
            text.lines()
                .filter_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(|value| value.trim_matches('"').to_owned())
                .next()
                .unwrap_or_default()
        };
        let id = field("ID");
        let id_like = field("ID_LIKE");
        let words = id.split_whitespace().chain(id_like.split_whitespace());
        words
            .filter_map(|word| match word {
                "arch" => Some(Distro::Arch),
                "debian" => Some(Distro::Debian),
                "ubuntu" => Some(Distro::Ubuntu),
                "fedora" => Some(Distro::Fedora),
                _ => None,
            })
            .next()
    }

    /// Detects the distribution of the running host.
    pub fn detect() -> Option<Distro> {
        let text = std::fs::read_to_string("/etc/os-release").ok()?;
        Distro::from_os_release(&text)
    }

    /// The command that installs `package` on this distribution.
    fn install_command(self, package: &str) -> Vec<String> {
        let prefix: &[&str] = match self {
            Distro::Arch => &["sudo", "pacman", "-S", "--needed"],
            Distro::Debian | Distro::Ubuntu => &["sudo", "apt-get", "install"],
            Distro::Fedora => &["sudo", "dnf", "install"],
        };
        prefix
            .iter()
            .copied()
            .chain([package])
            .map(str::to_owned)
            .collect()
    }
}

/// How a tool is installed.
pub enum Source {
    /// From the distribution package of this name.
    Package(&'static str),
    /// With `cargo install` of this crate, which needs no privileges.
    Crate(&'static str),
    /// GDB with support for x86_64: `gdb-multiarch` on Debian and Ubuntu
    /// hosts that are not x86_64, whose `gdb` debugs only their own
    /// architecture, and `gdb` otherwise.
    Gdb,
}

/// The package of GDB with support for x86_64 on `distro`, on a host of
/// architecture `arch`.
fn gdb_package(distro: Distro, arch: &str) -> &'static str {
    match distro {
        Distro::Debian | Distro::Ubuntu if arch != "x86_64" => "gdb-multiarch",
        _ => "gdb",
    }
}

/// A host tool, the version of it the repository needs, and where it
/// comes from.
pub struct Tool {
    pub name: &'static str,
    pub program: &'static str,
    pub min_version: Version,
    source: Source,
}

/// The host tools the development environment needs, with the minimum version
/// of each.
pub const TOOLS: [Tool; 5] = [
    Tool {
        name: "QEMU",
        program: "qemu-system-x86_64",
        min_version: Version(8, 2, 0),
        source: Source::Package("qemu-system-x86"),
    },
    Tool {
        name: "GDB",
        program: "gdb",
        min_version: Version(12, 1, 0),
        source: Source::Gdb,
    },
    Tool {
        name: "mkfs.btrfs",
        program: "mkfs.btrfs",
        min_version: Version(6, 12, 0),
        source: Source::Package("btrfs-progs"),
    },
    Tool {
        name: "mkfs.exfat",
        program: "mkfs.exfat",
        min_version: Version(1, 2, 0),
        source: Source::Package("exfatprogs"),
    },
    Tool {
        name: "cargo-fuzz",
        program: "cargo-fuzz",
        min_version: Version(0, 13, 0),
        source: Source::Crate("cargo-fuzz"),
    },
];

/// The result of looking for a tool on the host.
pub enum Status {
    Found(Version),
    TooOld(Version),
    Missing,
}

impl Tool {
    /// Runs the tool with `--version` and compares the reported version with
    /// the minimum.
    pub fn check(&self) -> Status {
        let program = match self.source {
            Source::Gdb => crate::debug::gdb(),
            _ => self.program,
        };
        let Ok(output) = Command::new(program).arg("--version").output() else {
            return Status::Missing;
        };
        if matches!(self.source, Source::Gdb) && !debugs_x86_64(program) {
            return Status::Missing;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let first_line = text.lines().next().unwrap_or_default();
        match Version::find_in(first_line) {
            Some(v) if v >= self.min_version => Status::Found(v),
            Some(v) => Status::TooOld(v),
            None => Status::Missing,
        }
    }

    /// The command that installs or upgrades the tool on `distro`.
    pub fn install_command(&self, distro: Distro) -> Vec<String> {
        match self.source {
            Source::Package(package) => distro.install_command(package),
            Source::Crate(name) => ["cargo", "install", name, "--locked"]
                .map(String::from)
                .to_vec(),
            Source::Gdb => distro.install_command(gdb_package(distro, std::env::consts::ARCH)),
        }
    }
}

/// Whether `gdb` can debug x86_64 code.
fn debugs_x86_64(gdb: &str) -> bool {
    let output = Command::new(gdb)
        .args(["-batch", "-nx", "-ex", "set architecture i386:x86-64"])
        .output();
    output.is_ok_and(|o| {
        o.status.success() && !String::from_utf8_lossy(&o.stderr).contains("Undefined")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_gdb_multiarch_where_gdb_has_one_architecture() {
        assert_eq!(gdb_package(Distro::Debian, "aarch64"), "gdb-multiarch");
        assert_eq!(gdb_package(Distro::Ubuntu, "aarch64"), "gdb-multiarch");
        assert_eq!(gdb_package(Distro::Debian, "x86_64"), "gdb");
        assert_eq!(gdb_package(Distro::Fedora, "aarch64"), "gdb");
        assert_eq!(gdb_package(Distro::Arch, "aarch64"), "gdb");
    }

    #[test]
    fn finds_versions_in_tool_output() {
        let cases = [
            (
                "QEMU emulator version 9.2.0 (Debian 1:9.2+ds-1)",
                Version(9, 2, 0),
            ),
            ("GNU gdb (GDB) 15.1", Version(15, 1, 0)),
            (
                "GNU gdb (Ubuntu 12.1-0ubuntu1~22.04) 12.1",
                Version(12, 1, 0),
            ),
            ("mkfs.btrfs, part of btrfs-progs v6.10.1", Version(6, 10, 1)),
        ];
        for (text, expected) in cases {
            assert_eq!(Version::find_in(text), Some(expected), "{text}");
        }
        assert_eq!(Version::find_in("no version here"), None);
    }

    #[test]
    fn orders_versions() {
        assert!(Version(8, 2, 0) < Version(8, 10, 0));
        assert!(Version(9, 0, 0) > Version(8, 99, 99));
    }

    #[test]
    fn detects_distributions() {
        let ubuntu = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        let endeavour = "ID=endeavouros\nID_LIKE=arch\n";
        let fedora = "ID=fedora\n";
        let alpine = "ID=alpine\n";
        assert_eq!(Distro::from_os_release(ubuntu), Some(Distro::Ubuntu));
        assert_eq!(Distro::from_os_release(mint), Some(Distro::Ubuntu));
        assert_eq!(Distro::from_os_release(endeavour), Some(Distro::Arch));
        assert_eq!(Distro::from_os_release(fedora), Some(Distro::Fedora));
        assert_eq!(Distro::from_os_release(alpine), None);
    }
}
