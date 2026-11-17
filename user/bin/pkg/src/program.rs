//! pkg on Mikros: it calls the install service, `mikros.packages.Installer`,
//! with connections to the package files it is given, and asks on the
//! terminal before it installs.

use crate::order::order;
use crate::pack::pack;
use crate::request::{self, COMMAND, Place, Request};
use crate::show::{self, Row};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient, Rights};
use mikros_rt::interfaces::mikros::packages::{InstallerClient, PackageInfo};
use std::io::{BufRead, Write};
use std::os::mikros::io::AsHandleExt;
use std::path::Path;
use std::process::ExitCode;

const INSTALLER: &str = "mikros.packages.Installer";
const FOREVER: i64 = i64::MAX;

fn fail(code: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(code)
}

fn status(
    what: &str,
    reply: Result<Result<(), Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<(), String> {
    match reply {
        Ok(Ok(())) => Ok(()),
        Ok(Err(status)) => Err(format!("{what}: {status:?}")),
        Err(e) => Err(format!("{what}: {e:?}")),
    }
}

/// A new connection, for reading, to the file at `path`.
fn connection(path: &str) -> Result<ClientEnd<Node>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let handle = file.handle().map_err(|e| format!("{path}: {e}"))?;
    // SAFETY: the handle is ours.
    let handle = unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) };
    let node = NodeClient::new(ClientEnd::new(handle));
    let (client, server) = endpoints::<Node>().map_err(|e| format!("{path}: {e:?}"))?;
    match node.clone(server, Rights::READ, FOREVER) {
        Ok(Ok(_)) => Ok(client),
        _ => Err(format!("{path}: cannot be opened again")),
    }
}

/// Every installed package that `List` gives, by ID.
fn installed(installer: &InstallerClient) -> Result<Vec<PackageInfo>, String> {
    let mut all: Vec<PackageInfo> = Vec::new();
    loop {
        let after = all.last().map(|p| p.id.clone()).unwrap_or_default();
        let page = match installer.list(after, FOREVER) {
            Ok(Ok(r)) => r.packages,
            Ok(Err(status)) => return Err(format!("listing the packages: {status:?}")),
            Err(e) => return Err(format!("listing the packages: {e:?}")),
        };
        if page.is_empty() {
            return Ok(all);
        }
        all.extend(page);
    }
}

/// Whether the user answers `question` with `y`.
fn agrees(question: &str) -> bool {
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    answer.trim() == "y"
}

fn install(
    installer: &InstallerClient,
    files: &[String],
    place: Place,
    dependency: bool,
    dev: bool,
) -> Result<(), String> {
    let mut packages = Vec::new();
    for file in files {
        let manifest = match installer.inspect(connection(file)?, FOREVER) {
            Ok(Ok(r)) => r.manifest,
            Ok(Err(status)) => return Err(format!("{file}: {status:?}")),
            Err(e) => return Err(format!("{file}: {e:?}")),
        };
        let manifest =
            Manifest::parse(&manifest).map_err(|e| format!("{file}: {} {}", e.key, e.why))?;
        packages.push((file.clone(), manifest));
    }
    let needs: Vec<(String, Vec<String>)> = packages
        .iter()
        .map(|(_, m)| {
            (
                m.id.clone(),
                m.needs.iter().map(|(id, _)| id.clone()).collect(),
            )
        })
        .collect();
    let order = order(&needs).map_err(|id| format!("{id} needs itself through other packages"))?;
    for at in order {
        let (file, manifest) = &packages[at];
        let installed = installed(installer)?;
        if let Some(old) = installed.iter().find(|p| p.id == manifest.id) {
            let version = manifest
                .version
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default();
            print!(
                "{}",
                show::update_warnings(&manifest.id, &old.version, &version)
            );
        }
        print!("{}", show::describe(manifest));
        if !agrees("install? [y/N] ") {
            println!("{} is not installed", manifest.id);
            continue;
        }
        let mut components = Vec::new();
        for component in &manifest.components {
            println!("component {component} of {}", manifest.id);
            if agrees(&format!("install {component}? [y/N] ")) {
                components.push(component.clone());
            }
        }
        let global = match place {
            Place::Global => true,
            Place::Local => false,
            Place::Preferred => manifest.global,
        };
        let commands = Vec::new();
        status(
            &manifest.id,
            installer
                .install(
                    connection(file)?,
                    global,
                    components,
                    dependency,
                    dev,
                    commands,
                    FOREVER,
                )
                .map(|r| r.map(|_| ())),
        )?;
        println!("installed {}", manifest.id);
    }
    Ok(())
}

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let request = request::read(arguments);
    match &request {
        Request::Help => {
            print!("{}", COMMAND.help());
            return ExitCode::from(SUCCESS);
        }
        Request::Invalid(reason) => return fail(INVALID, reason),
        Request::Pack { folder, out } => {
            let (bytes, manifest) = match pack(Path::new(folder)) {
                Ok(packed) => packed,
                Err(why) => return fail(FAILURE, &why),
            };
            let version = manifest.version.map(|v| v.to_string()).unwrap_or_default();
            let out = out
                .clone()
                .unwrap_or(format!("{}-{version}.mika", manifest.id));
            if let Err(e) = std::fs::write(&out, bytes) {
                return fail(FAILURE, &format!("{out}: {e}"));
            }
            println!("{out}");
            return ExitCode::from(SUCCESS);
        }
        _ => {}
    }
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    let installer = match directory.connect(INSTALLER) {
        Ok(client) => InstallerClient::new(client),
        Err(why) => return fail(FAILURE, &why),
    };
    let done = match request {
        Request::Install {
            files,
            place,
            dependency,
            dev,
        } => install(&installer, &files, place, dependency, dev),
        Request::Remove { id, keep_data } => installed(&installer).and_then(|all| {
            let package = all
                .iter()
                .find(|p| p.id == id)
                .ok_or(format!("{id} is not installed"))?;
            status(
                &id,
                installer
                    .remove(id.clone(), package.global, keep_data, FOREVER)
                    .map(|r| r.map(|_| ())),
            )
        }),
        Request::List { json } => installed(&installer).map(|all| {
            let rows: Vec<Row> = all
                .into_iter()
                .map(|p| Row {
                    id: p.id,
                    version: p.version,
                    kind: p.kind,
                    global: p.global,
                })
                .collect();
            match json {
                true => print!("{}", show::json(&rows)),
                false => print!("{}", show::table(&rows)),
            }
        }),
        Request::Info(target) => info(&installer, &target),
        _ => Ok(()),
    };
    match done {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(why) => fail(FAILURE, &why),
    }
}

/// Shows the package in the file `target`, or the installed package of
/// that ID.
fn info(installer: &InstallerClient, target: &str) -> Result<(), String> {
    if Path::new(target).is_file() {
        let manifest = match installer.inspect(connection(target)?, FOREVER) {
            Ok(Ok(r)) => r.manifest,
            Ok(Err(status)) => return Err(format!("{target}: {status:?}")),
            Err(e) => return Err(format!("{target}: {e:?}")),
        };
        let manifest =
            Manifest::parse(&manifest).map_err(|e| format!("{target}: {} {}", e.key, e.why))?;
        print!("{}", show::describe(&manifest));
        return Ok(());
    }
    let all = installed(installer)?;
    let p = all
        .iter()
        .find(|p| p.id == target)
        .ok_or(format!("{target} is neither a package file nor installed"))?;
    let place = if p.global { "global" } else { "local" };
    println!(
        "{} ({}) {}\n  kind: {}\n  where: {place}",
        p.name, p.id, p.version, p.kind
    );
    if !p.owner.is_empty() {
        println!("  component of: {}", p.owner);
    }
    Ok(())
}
