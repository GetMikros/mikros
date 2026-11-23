//! Builds the stand-ins that `standin` read (MIKSPEC-0023.R59): compiles their
//! programs, writes the manifests of their system apps, and makes the account
//! and the password of an image. Nothing here knows a particular stand-in.

// The build of the images uses this from the next commit.
#![allow(dead_code)]

use crate::build::{Kind, Linking, c_build_with, c_dir};
use crate::standin::{self, App, Origin, Part, Protocol, StandIn};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The outputs of the ports that the stand-ins' descriptions name.
pub struct Ports<'a> {
    pub wayland: &'a crate::wayland::Built,
    /// The install of the Vulkan loader.
    pub vulkan_loader: &'a Path,
    /// The Vulkan headers' include folder.
    pub vulkan_headers: &'a Path,
    /// The install of SDL3.
    pub sdl: &'a Path,
    pub xkbcommon: &'a crate::ports::Built,
}

impl Ports<'_> {
    /// The file `file` of the libraries of the port `port`.
    fn library(&self, port: &str, file: &str) -> Result<PathBuf> {
        let path = match port {
            "wayland" => self
                .wayland
                .library
                .parent()
                .context("the Wayland library has no folder")?
                .join(file),
            "vulkan-loader" => self.vulkan_loader.join("libraries").join(file),
            "sdl3" => self.sdl.join("libraries").join(file),
            "xkbcommon" => self.xkbcommon.install.join("libraries").join(file),
            other => bail!("{other} is not a port whose libraries are linked"),
        };
        if !path.is_file() {
            bail!(
                "the port {port} has no {file}: {} is missing",
                path.display()
            );
        }
        Ok(path)
    }

    /// The header folder that the name `name` stands for; `generated` is
    /// the folder of the generated protocol code.
    fn include(&self, name: &str, generated: &Path) -> Result<PathBuf> {
        Ok(match name {
            "vulkan-headers" => self.vulkan_headers.to_path_buf(),
            "sdl3" => self.sdl.join("include"),
            "generated-protocols" => generated.to_path_buf(),
            other => bail!("{other} is not a header folder that is known"),
        })
    }
}

/// The files and the account that the stand-ins held by an image add to it.
#[derive(Default)]
pub struct Held {
    /// The members of the system apps: their files and manifests.
    pub apps: Vec<(String, PathBuf)>,
    /// The members of the files to install as they are, and of the account
    /// store when a stand-in has an account.
    pub installs: Vec<(String, PathBuf)>,
    /// Whether one of the stand-ins is needed for the desktop to count as
    /// installed.
    pub enables_desktop: bool,
    /// The system apps with the files that they were made of, which a
    /// variant of an image can change.
    pub app_files: Vec<BuiltApp>,
    pub account: Option<Account>,
}

/// A system app and its files, by their names in the package.
pub struct BuiltApp {
    pub app: App,
    pub files: Vec<(String, PathBuf, bool)>,
    /// Where the manifest is written.
    pub dir: PathBuf,
}

/// The accounts that an image holds and their password.
pub struct Account {
    pub names: Vec<String>,
    pub password: String,
    /// The file that holds the password, a secret of development.
    pub file: PathBuf,
}

/// The characters of a password.
const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// A password of 24 random characters.
fn generate_password() -> Result<String> {
    let mut password = String::new();
    while password.len() < 24 {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| anyhow!("no random numbers: {e}"))?;
        // Bytes at or above a multiple of the alphabet's size are skipped,
        // so that every character is as likely as another.
        let limit = 256 - 256 % ALPHABET.len();
        for b in bytes {
            if (b as usize) < limit && password.len() < 24 {
                password.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    Ok(password)
}

/// `text` as a YAML scalar of a flow list: bare when it needs no quotes.
fn scalar(text: &str) -> String {
    let bare = !text.is_empty()
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b));
    if bare {
        text.to_owned()
    } else {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// The lines of the `shell_parts` of a manifest, with `{password}` in the
/// arguments replaced by `password`.
pub fn shell_parts(parts: &[Part], password: Option<&str>) -> Result<String> {
    let mut text = String::new();
    for part in parts {
        let mut arguments = Vec::new();
        for argument in &part.arguments {
            let argument = if argument.contains("{password}") {
                let password =
                    password.context("a shell part needs a password that is not made")?;
                argument.replace("{password}", password)
            } else {
                argument.clone()
            };
            arguments.push(scalar(&argument));
        }
        text.push_str(&format!(
            "  - roles: [{}]\n    executable: {}\n    arguments: [{}]\n",
            part.roles
                .iter()
                .map(|r| scalar(r))
                .collect::<Vec<_>>()
                .join(", "),
            scalar(&part.executable),
            arguments.join(", ")
        ));
    }
    Ok(text)
}

/// The members of the system app `app`, whose files are `files`, with the
/// manifest written below `dir`.
pub fn app_members(
    app: &App,
    files: &[(String, PathBuf, bool)],
    password: Option<&str>,
    dir: &Path,
) -> Result<Vec<(String, PathBuf)>> {
    let parts = shell_parts(&app.parts, password)?;
    crate::system_apps::members(
        &crate::system_apps::SystemApp {
            id: &app.id,
            version: &app.version,
            name: &app.name,
            description: &app.description,
            executable: &app.executable,
            portable: app.portable,
            installable: app.installable,
            shell_parts: &parts,
            files: files.to_vec(),
        },
        dir,
    )
}

/// The XML file of the protocol `protocol`.
fn protocol_xml(protocol: &Protocol) -> Result<PathBuf> {
    let name = &protocol.name;
    Ok(match &protocol.origin {
        Origin::Staged => crate::wayland::protocols()?.join(format!("{name}.xml")),
        Origin::Component(component) => crate::components::all()?
            .into_iter()
            .find(|c| &c.name == component)
            .with_context(|| format!("the protocol {name} comes from {component}, which is none"))?
            .dir
            .join("protocols")
            .join(format!("{name}.xml")),
        Origin::Sysroot(path) => crate::sysroot::dir().join(path),
    })
}

/// Generates the client header and the private code of each protocol of the
/// build, in `dir`, and returns the files of the code.
fn generate_protocols(
    stand_in: &StandIn,
    protocols: &[Protocol],
    scanner: &Path,
    dir: &Path,
) -> Result<Vec<String>> {
    if protocols.is_empty() {
        return Ok(Vec::new());
    }
    crate::wayland::install_protocols()?;
    let mut code = Vec::new();
    for protocol in protocols {
        let xml = protocol_xml(protocol)?;
        let stem = &protocol.name;
        let source = dir.join(format!("{stem}-protocol.c"));
        for (mode, out) in [
            (
                "client-header",
                dir.join(format!("{stem}-client-protocol.h")),
            ),
            ("private-code", source.clone()),
        ] {
            crate::port::run(Command::new(scanner).arg(mode).arg(&xml).arg(out))
                .with_context(|| format!("generating the {stem} code of {}", stand_in.name))?;
        }
        code.push(source.display().to_string());
    }
    Ok(code)
}

/// Compiles and links the program of `stand_in`, and returns its path.
fn compile(stand_in: &StandIn, ports: &Ports) -> Result<PathBuf> {
    let build = stand_in
        .build
        .as_ref()
        .with_context(|| format!("{} builds no program", stand_in.name))?;
    let dir = c_dir(&build.output);
    fs::create_dir_all(&dir)?;
    let code = generate_protocols(
        stand_in,
        &build.protocols,
        &ports.wayland.scanner_program(),
        &dir,
    )?;
    let mut args: Vec<String> = build.warnings.clone();
    for include in &build.includes {
        args.push(format!("-I{}", ports.include(include, &dir)?.display()));
    }
    for link in &build.link {
        args.push(ports.library(&link.port, &link.file)?.display().to_string());
    }
    let folder = stand_in
        .dir
        .strip_prefix(crate::workspace_root())
        .with_context(|| format!("{} is not in the repository", stand_in.dir.display()))?;
    let mut sources: Vec<String> = build
        .sources
        .iter()
        .map(|s| folder.join(s).display().to_string())
        .collect();
    sources.extend(code);
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let program = c_build_with(
        &build.output,
        &build.optimization,
        &sources,
        Linking::Dynamic,
        &args,
    )?;
    println!("built    {}", program.display());
    Ok(program)
}

/// The names of the stand-ins of `held` and of those that they use, in the
/// order that builds each after what it uses.
fn needed(all: &[StandIn], held: &[&StandIn]) -> Result<Vec<String>> {
    let mut wanted: Vec<String> = held.iter().map(|s| s.name.clone()).collect();
    let mut i = 0;
    while i < wanted.len() {
        let uses = all
            .iter()
            .find(|s| s.name == wanted[i])
            .map(|s| s.uses.clone())
            .unwrap_or_default();
        for used in uses {
            if !wanted.contains(&used) {
                wanted.push(used);
            }
        }
        i += 1;
    }
    Ok(standin::order(all)?
        .into_iter()
        .filter(|n| wanted.contains(n))
        .collect())
}

/// The account store with the accounts of `held`, whose password is
/// `password`, and the file that holds the password, in `target/boot/<kind>`.
fn write_accounts(held: &[&StandIn], password: &str, kind: Kind) -> Result<(PathBuf, Account)> {
    use mikros_userdb::{Kind as UserKind, LoginMode, Users, encode, hash};
    let mut users = Users::new();
    let mut names = Vec::new();
    let mut salt = [0u8; hash::SALT_LEN];
    for account in held.iter().filter_map(|s| s.account.as_ref()) {
        getrandom::fill(&mut salt).map_err(|e| anyhow!("no random numbers: {e}"))?;
        let phc = hash::hash(password.as_bytes(), &salt).context("hashing a password")?;
        let user_kind = if account.kind == "admin" {
            UserKind::Admin
        } else {
            UserKind::Standard
        };
        let id = users
            .create(&account.name, &account.full_name, user_kind, phc)
            .map_err(|r| anyhow!("adding {}: {r:?}", account.name))?;
        let login = if account.login == "console" {
            LoginMode::Console
        } else {
            LoginMode::Desktop
        };
        users
            .set_login_mode(id, login)
            .map_err(|r| anyhow!("setting the login mode of {}: {r:?}", account.name))?;
        names.push(account.name.clone());
    }
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let store = dir.join("stand-in-users");
    fs::write(&store, encode(&users)).with_context(|| format!("writing {}", store.display()))?;
    let file = dir.join("stand-in-password");
    let lines: String = names.iter().map(|n| format!("{n} {password}\n")).collect();
    fs::write(&file, lines).with_context(|| format!("writing {}", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    }
    Ok((
        store,
        Account {
            names,
            password: password.to_owned(),
            file,
        },
    ))
}

/// Builds the stand-ins `held` of an image of the build kind `kind`, and the
/// stand-ins that they use, of the stand-ins `all`, and returns what they add
/// to the image.
pub fn build(all: &[StandIn], held: &[&StandIn], ports: &Ports, kind: Kind) -> Result<Held> {
    let mut programs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for name in needed(all, held)? {
        let stand_in = all
            .iter()
            .find(|s| s.name == name)
            .with_context(|| format!("no stand-in is named {name}"))?;
        if stand_in.build.is_some() {
            programs.insert(name, compile(stand_in, ports)?);
        }
    }
    let mut made = Held::default();
    let password = if held.iter().any(|s| s.account.is_some()) {
        Some(generate_password()?)
    } else {
        None
    };
    for stand_in in held {
        made.enables_desktop |= stand_in.enables_desktop;
        for app in &stand_in.apps {
            let mut files = Vec::new();
            for file in &app.files {
                let from = if file.from == "self" {
                    &stand_in.name
                } else {
                    &file.from
                };
                let program = programs.get(from).with_context(|| {
                    format!(
                        "{} takes {} from {from}, which builds no program",
                        app.id, file.name
                    )
                })?;
                files.push((file.name.clone(), program.clone(), file.executable));
            }
            let dir = c_dir(&stand_in.name).join("app");
            made.apps
                .extend(app_members(app, &files, password.as_deref(), &dir)?);
            made.app_files.push(BuiltApp {
                app: app.clone(),
                files,
                dir,
            });
        }
        for install in &stand_in.installs {
            made.installs
                .push((install.path.clone(), stand_in.dir.join(&install.file)));
        }
    }
    if let Some(password) = &password {
        let (store, account) = write_accounts(held, password, kind)?;
        made.installs.push(("state/accounts/users".into(), store));
        made.account = Some(account);
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(roles: &[&str], executable: &str, arguments: &[&str]) -> Part {
        Part {
            roles: roles.iter().map(|s| s.to_string()).collect(),
            executable: executable.into(),
            arguments: arguments.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn writes_the_parts_as_the_manifests_have_them() {
        let parts = [
            part(
                &["greeter"],
                "greeter-probe",
                &["--login", "dee", "{password}"],
            ),
            part(&["start-menu", "x"], "sdl-probe", &[]),
        ];
        assert_eq!(
            shell_parts(&parts, Some("Secret42")).unwrap(),
            "  - roles: [greeter]\n    executable: greeter-probe\n    \
             arguments: [\"--login\", dee, Secret42]\n  \
             - roles: [start-menu, x]\n    executable: sdl-probe\n    arguments: []\n"
        );
        assert!(shell_parts(&parts, None).is_err());
    }

    #[test]
    fn quotes_what_a_bare_scalar_cannot_be() {
        assert_eq!(scalar("dee"), "dee");
        assert_eq!(scalar("--wallpaper"), "\"--wallpaper\"");
        assert_eq!(scalar(""), "\"\"");
        assert_eq!(scalar("a b"), "\"a b\"");
        assert_eq!(scalar("a\"b\\"), "\"a\\\"b\\\\\"");
    }

    #[test]
    fn a_password_has_24_characters_of_the_alphabet_and_differs() {
        let a = generate_password().unwrap();
        let b = generate_password().unwrap();
        assert_eq!(a.len(), 24);
        assert!(a.bytes().all(|c| ALPHABET.contains(&c)));
        assert_ne!(a, b);
    }

    #[test]
    fn builds_the_stand_ins_that_one_uses_first() {
        let simple = |name: &str, uses: &[&str]| StandIn {
            dir: PathBuf::new(),
            name: name.into(),
            reason: "MIKSPEC-0075.R14".into(),
            editions: vec![crate::build::Edition::Desktop],
            uses: uses.iter().map(|s| s.to_string()).collect(),
            enables_desktop: false,
            build: None,
            apps: Vec::new(),
            installs: Vec::new(),
            account: None,
        };
        let all = [simple("a", &["b"]), simple("b", &[]), simple("c", &[])];
        let held = [&all[0]];
        assert_eq!(needed(&all, &held).unwrap(), ["b", "a"]);
    }
}
