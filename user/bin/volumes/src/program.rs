//! volumes on Mikros: it pages through the device manager's
//! `device.volumes`, names each mounted volume by the entry of `/volumes`
//! whose file system gives its ID, and calls `Eject` or `Mount`, for which
//! the device manager asks for the permissions.

use crate::listing::{self, COMMAND, Call, Request, State, Volume};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::driver::VolumesClient;
use mikros_rt::interfaces::mikros::fs::{FilesystemInfo, NodeClient};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::mikros::io::AsHandleExt;
use std::process::ExitCode;

const VOLUMES: &str = "device.volumes";
const FOLDER: &str = "/volumes";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let fail = |status: u8, message: &str| fail(&run, status, message);
    let request = match listing::read(&run) {
        Ok(request) => request,
        Err(reason) => return fail(INVALID, &reason),
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    let volumes = match directory.connect(VOLUMES) {
        Ok(client) => VolumesClient::new(client),
        Err(why) => return fail(FAILURE, &why),
    };
    match request {
        Request::List { json } => {
            let listed = match list(&volumes) {
                Ok(listed) => listed,
                Err(why) => return fail(FAILURE, &why),
            };
            match json {
                true => write(&listing::json(&listed)),
                false => write(&listing::text(&listed, Paint::stdout(&run))),
            }
        }
        Request::Call(call, volume) => match call_on(&volumes, call, &volume) {
            Ok(()) => ExitCode::from(SUCCESS),
            Err(why) => fail(FAILURE, &why),
        },
    }
}

/// What `QueryFilesystem` gives for the path `path`.
fn query(path: &str) -> Option<FilesystemInfo> {
    let file = std::fs::File::open(path).ok()?;
    let handle = file.handle().ok()?;
    // SAFETY: the handle is ours, and is handed over once.
    let handle = unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) };
    let node = NodeClient::new(ClientEnd::new(handle));
    node.query_filesystem(FOREVER).ok()?.ok().map(|r| r.info)
}

/// The entries of `/volumes` by the IDs of their file systems, with their
/// names and their file systems' types.
fn mounted() -> BTreeMap<String, (String, String)> {
    let mut by_id = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(FOLDER) else {
        return by_id;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if let Some(info) = query(&format!("{FOLDER}/{name}")) {
            by_id.insert(info.id, (name, info.r#type));
        }
    }
    by_id
}

/// Every volume of `List`, page by page, with the name and type of those
/// in `/volumes`.
fn list(volumes: &VolumesClient) -> Result<Vec<Volume>, String> {
    let names = mounted();
    let mut all = Vec::new();
    loop {
        let after = all.last().map_or(String::new(), |v: &Volume| v.id.clone());
        let page = match volumes.list(after, FOREVER) {
            Ok(Ok(page)) => page.volumes,
            Ok(Err(status)) => return Err(listing::lowercase(&format!("{status:?}"))),
            Err(e) => return Err(format!("List failed: {e:?}")),
        };
        if page.is_empty() {
            return Ok(all);
        }
        all.extend(page.into_iter().map(|v| {
            let named = names.get(&v.id).filter(|_| v.mounted);
            Volume {
                name: named.map(|(name, _)| name.clone()),
                kind: named.map(|(_, kind)| kind.clone()),
                id: v.id,
                state: State::of(v.mounted, v.hidden),
                removable: v.removable,
            }
        }));
    }
}

/// Calls `call` on `volume`, a name in `/volumes` or an ID, and says why
/// it failed. `Mount` takes the ID as it is.
fn call_on(volumes: &VolumesClient, call: Call, volume: &str) -> Result<(), String> {
    let id = match call {
        Call::Mount => volume.to_string(),
        Call::Eject => match is_entry(volume).then(|| query(&format!("{FOLDER}/{volume}"))) {
            Some(Some(info)) => info.id,
            Some(None) => return Err(listing::failure(call, volume, "not_found")),
            None => volume.to_string(),
        },
    };
    let done = match call {
        Call::Eject => volumes.eject(id, FOREVER).map(|r| r.map(drop)),
        Call::Mount => volumes.mount(id, FOREVER).map(|r| r.map(drop)),
    };
    match done {
        Ok(Ok(())) => Ok(()),
        Ok(Err(status)) => Err(listing::failure(
            call,
            volume,
            &listing::lowercase(&format!("{status:?}")),
        )),
        Err(error) => Err(format!("{volume}: {error:?}")),
    }
}

/// Whether `/volumes` lists `name`.
fn is_entry(name: &str) -> bool {
    std::fs::read_dir(FOLDER).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_str() == Some(name))
    })
}

/// Writes `text` to standard output.
fn write(text: &str) -> ExitCode {
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(run: &Run, status: u8, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(status)
}
