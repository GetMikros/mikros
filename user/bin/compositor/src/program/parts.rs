//! Starting the parts of the shell. `shell.yaml` names the system app of
//! the shell, under `shell`, and, for each system role, the system app
//! whose part provides it: the compositor reads the app's manifest for its
//! `shell_parts`, and starts the part's executable as a child, through the
//! process manager's `Spawn`, with a channel for its Wayland connection
//! and its roles in `MIKROS_SHELL_ROLE`. In greeter mode only the
//! `greeter` part runs, which also gets the `Login` channel; in a user
//! session every part of the shell and of the `file-picker` and
//! `capture-picker` apps does.

use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, Launcher, LauncherAsyncClient, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use std::path::PathBuf;

/// The shell settings, which the second, when it exists, overrides as a
/// whole.
const SHELL: &str = "/system/share/desktop/shell.yaml";
const SHELL_OVERRIDE: &str = "/system/config/desktop/shell.yaml";
/// Where the system apps are, by ID.
const SYSTEM_APPS: &str = "/system/apps";
/// The `USER` startup handle IDs of a part's channels.
const WAYLAND_ID: u32 = 1;
const LOGIN_ID: u32 = 3;
/// The user ID that makes a child run as the caller's user.
const CALLER_USER: u32 = u32::MAX;
/// The rights that a duplicate keeps: all of the original's.
const SAME_RIGHTS: u32 = 1 << 31;
/// The deadline of a wait without one.
const FOREVER: i64 = i64::MAX;

/// A part of the shell: what to start, for which roles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The app ID of its package, which is its process's.
    pub app_id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub roles: Vec<String>,
}

/// Why a part cannot be found.
#[derive(Debug)]
pub enum Missing {
    /// No shell settings, or settings that name no package for the role.
    NoPackage,
    /// The package's manifest cannot be read or is invalid.
    Manifest(String),
    /// No part of the package provides the role, or more than one does.
    NoPart,
}

fn yaml_error(path: &str, e: mikros_yaml::Error) -> Missing {
    Missing::Manifest(format!("{path}: line {}, column {}", e.line, e.column))
}

/// The ID of the package that `shell.yaml` names under `key`.
fn package_of(key: &str) -> Result<String, Missing> {
    let (path, text) = match std::fs::read(SHELL_OVERRIDE) {
        Ok(text) => (SHELL_OVERRIDE, text),
        Err(_) => (SHELL, std::fs::read(SHELL).map_err(|_| Missing::NoPackage)?),
    };
    let root = mikros_yaml::parse(&text).map_err(|e| yaml_error(path, e))?;
    let map = root.map().map_err(|e| yaml_error(path, e))?;
    let package = map.get(key).ok_or(Missing::NoPackage)?;
    package
        .string()
        .map(str::to_owned)
        .map_err(|e| yaml_error(path, e))
}

/// The packages whose parts run in a user session, each with why it
/// cannot be found when it cannot: the shell, and the `file-picker` and
/// `capture-picker` packages that `shell.yaml` names.
pub fn session_packages() -> Vec<(&'static str, Result<String, Missing>)> {
    ["shell", "file-picker", "capture-picker"]
        .into_iter()
        .filter_map(|key| match package_of(key) {
            // A picker that no package provides leaves its role empty.
            Err(Missing::NoPackage) if key != "shell" => None,
            found => Some((key, found)),
        })
        .collect()
}

/// The part of the system app `package` that provides `role`.
fn part_of(package: &str, role: &str) -> Result<Part, Missing> {
    let mut found: Vec<Part> = parts_of(package)?
        .into_iter()
        .filter(|part| part.roles.iter().any(|r| r == role))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        _ => Err(Missing::NoPart),
    }
}

/// Every part of the system app `package`.
pub fn parts_of(package: &str) -> Result<Vec<Part>, Missing> {
    let root = PathBuf::from(SYSTEM_APPS).join(package);
    let path = root.join("manifest.yaml");
    let shown = path.display().to_string();
    let text = std::fs::read(&path).map_err(|e| Missing::Manifest(format!("{shown}: {e}")))?;
    let manifest = mikros_yaml::parse(&text).map_err(|e| yaml_error(&shown, e))?;
    let map = manifest.map().map_err(|e| yaml_error(&shown, e))?;
    let parts = match map.get("shell_parts") {
        Some(parts) => parts.list().map_err(|e| yaml_error(&shown, e))?,
        None => &[],
    };
    let mut found = Vec::new();
    for part in parts {
        let part = part.map().map_err(|e| yaml_error(&shown, e))?;
        let roles: Vec<String> = match part.get("roles") {
            Some(roles) => roles
                .list()
                .map_err(|e| yaml_error(&shown, e))?
                .iter()
                .map(|r| r.string().map(str::to_owned))
                .collect::<Result<_, _>>()
                .map_err(|e| yaml_error(&shown, e))?,
            None => Vec::new(),
        };
        let executable = part
            .get("executable")
            .ok_or_else(|| Missing::Manifest(format!("{shown}: a part without an executable")))?
            .string()
            .map_err(|e| yaml_error(&shown, e))?;
        let arguments = match part.get("arguments") {
            Some(arguments) => arguments
                .list()
                .map_err(|e| yaml_error(&shown, e))?
                .iter()
                .map(|a| a.string().map(str::to_owned))
                .collect::<Result<_, _>>()
                .map_err(|e| yaml_error(&shown, e))?,
            None => Vec::new(),
        };
        found.push(Part {
            app_id: package.into(),
            executable: root.join(executable),
            arguments,
            roles,
        });
    }
    Ok(found)
}

/// The part that provides the system role `role`.
pub fn find(role: &str) -> Result<Part, Missing> {
    part_of(&package_of(role)?, role)
}

/// The block of arguments and environment of a spawn, in a memory object.
fn block(arguments: &[String], environment: &[String]) -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(environment.len() as u32).to_le_bytes());
    for a in arguments.iter().chain(environment) {
        bytes.extend_from_slice(a.as_bytes());
        bytes.push(0);
    }
    let size = (bytes.len() as u64).next_multiple_of(4096);
    let memory = mikros_rt::objects::memory(size, 0).ok()?;
    mikros_rt::objects::write_memory(memory.raw(), 0, &bytes).ok()?;
    Some(memory)
}

/// A `Launcher` client of the compositor's own, on a duplicate of the C
/// library's connection, which all starts and waits share: replies on a
/// channel reach the one client that reads it.
pub fn launcher(reactor: &Reactor) -> Result<LauncherAsyncClient, String> {
    let raw = std::os::mikros::process::launcher().as_raw_handle();
    let mut copy = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { mikros_rt::sys::handle_duplicate(raw, SAME_RIGHTS, &mut copy) };
    if status != mikros_rt::OK {
        return Err(format!("no Launcher connection: {status}"));
    }
    // SAFETY: the kernel just made the duplicate for us.
    let launcher: ClientEnd<Launcher> = ClientEnd::new(unsafe { OwnedHandle::from_raw(copy) });
    Ok(LauncherAsyncClient::new(launcher, reactor.clone()))
}

/// Starts `part` with `wayland`, the client end of its Wayland channel, and
/// in greeter mode `login`, and returns its process ID.
pub async fn start(
    launcher: &LauncherAsyncClient,
    part: &Part,
    wayland: OwnedHandle,
    login: Option<OwnedHandle>,
) -> Result<u32, String> {
    let path = part.executable.display().to_string();
    let mut arguments = vec![path.clone()];
    arguments.extend(part.arguments.iter().cloned());
    let environment = [format!("MIKROS_SHELL_ROLE={}", part.roles.join(" "))];
    let arguments = block(&arguments, &environment).ok_or("no memory for the arguments")?;
    let mut handles = vec![StartupHandle {
        role: HandleRole::User,
        id: WAYLAND_ID,
        handle: wayland,
    }];
    if let Some(login) = login {
        handles.push(StartupHandle {
            role: HandleRole::User,
            id: LOGIN_ID,
            handle: login,
        });
    }
    let request = SpawnRequest {
        path,
        arguments,
        handles,
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    match launcher.spawn(request).await {
        Ok(Ok(spawned)) => Ok(spawned.pid),
        other => Err(format!("Spawn: {other:?}")),
    }
}

/// Waits until the part `pid` has exited, and returns its exit code, or
/// why it cannot be waited for.
pub async fn exited(launcher: &LauncherAsyncClient, pid: u32) -> Result<i64, String> {
    loop {
        match launcher.wait(pid as i32, WaitOptions(0), FOREVER).await {
            Ok(Ok(w)) if w.status.kind == StatusKind::Exited => return Ok(w.status.exit_code),
            Ok(Ok(_)) => continue,
            other => return Err(format!("Wait: {other:?}")),
        }
    }
}
