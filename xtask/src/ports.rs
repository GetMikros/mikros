//! The ported C and C++ projects: each is pinned to a release archive by
//! its SHA-256, patched with the files in `ports/<name>/patches/`, and
//! built for Mikros with Meson through the sysroot's cross file.

use anyhow::Result;
use std::path::PathBuf;

/// A ported project.
pub struct Port {
    /// Its name, which names its directories.
    pub name: &'static str,
    url: &'static str,
    /// SHA-256 of the archive at `url`.
    sha256: &'static str,
    /// The directory the archive holds the source in.
    source: &'static str,
    /// The options it is configured with.
    options: &'static [&'static str],
    /// The directories of its source, relative to it, that its tests
    /// read.
    pub test_data: &'static [&'static str],
}

/// libxkbcommon, without the parts that need X11, Wayland or libxml2,
/// none of which Mikros has, and without its documentation and shell
/// completions. It is given the directories of keymap extensions, which
/// it derives itself only from an installed xkeyboard-config, next to
/// its default keymap directory, `/usr/share/X11/xkb`, and the directory
/// its tests read their data from on Mikros, below `test_source`.
pub const XKBCOMMON: Port = Port {
    name: "xkbcommon",
    url: "https://github.com/xkbcommon/libxkbcommon/archive/refs/tags/xkbcommon-1.13.2.tar.gz",
    sha256: "acc4d5f7c3cbba5f9f8d08d8bdbeede84ecede46792f47929aa9321873385528",
    source: "libxkbcommon-xkbcommon-1.13.2",
    options: &[
        "-Denable-x11=false",
        "-Denable-wayland=false",
        "-Denable-xkbregistry=false",
        "-Denable-docs=false",
        "-Denable-bash-completion=false",
        "-Dxkb-config-versioned-extensions-path=/usr/share/X11/xkb.d",
        "-Dxkb-config-unversioned-extensions-path=/usr/share/xkeyboard-config.d",
        "-Dtest-data-dir=/usr/share/xkbcommon-tests/test/data",
    ],
    test_data: &["test/data"],
};

/// xkeyboard-config, the keymap data that xkbcommon compiles keyboard
/// layouts from, without translations of its layout descriptions.
pub const XKEYBOARD_CONFIG: Port = Port {
    name: "xkeyboard-config",
    url: "https://gitlab.freedesktop.org/xkeyboard-config/xkeyboard-config/-/archive/xkeyboard-config-2.48/xkeyboard-config-xkeyboard-config-2.48.tar.gz",
    sha256: "1e304d3c7a74bfbad0bae16b25d4c2b2eda06714b988953607a06cc013bf3077",
    source: "xkeyboard-config-xkeyboard-config-2.48",
    options: &["-Dnls=false"],
    test_data: &[],
};

/// Where a port was built.
pub struct Built {
    /// Its patched source.
    pub source: PathBuf,
    /// Its build directory, which holds its tests.
    pub build: PathBuf,
    /// The directory its prefix, `/usr`, was installed in.
    pub install: PathBuf,
}

/// Where the tests of `port` find its source on Mikros.
pub fn test_source(port: &Port) -> String {
    format!("/usr/share/{}-tests", port.name)
}

/// Where the programs of `port`'s build directory are on Mikros.
pub fn test_programs(port: &Port) -> String {
    format!("/usr/libexec/{}-tests", port.name)
}

/// Fetches, patches and builds `port`.
pub fn build(port: &Port) -> Result<Built> {
    let root = crate::workspace_root();
    let dir = root.join("target/sources").join(port.name);
    let archive = dir.join(format!("{}.tar.gz", port.source));
    let what = format!("{} source", port.name);
    crate::fetch::cached(&what, port.url, port.sha256, &archive)?;
    let patches = root.join("ports").join(port.name).join("patches");
    let source = crate::port::patched_source(&archive, &dir, port.source, &patches, &[])?;
    let options: Vec<String> = port.options.iter().map(|o| o.to_string()).collect();
    let install = crate::build_systems::meson(port.name, &source, &options)?;
    Ok(Built {
        source,
        build: crate::build_systems::build_dir(port.name),
        install,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xkbcommon_tests_read_their_data_from_their_source() {
        let option = format!("-Dtest-data-dir={}/test/data", test_source(&XKBCOMMON));
        assert!(XKBCOMMON.options.contains(&option.as_str()));
    }
}
