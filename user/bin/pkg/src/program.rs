//! pkg on Mikros: it calls the install service, `mikros.packages.Installer`,
//! with connections to the package files it is given, and asks on the
//! terminal before it installs.

use crate::order::order;
use crate::request::{self, COMMAND, Request};
use crate::show::{self, Row};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
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
/// The commands of installed packages, and the library packages.
const COMMANDS: &str = "/system/commands";
const LIBRARIES: &str = "/libraries";

fn fail(run: &Run, code: u8, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
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
/// The commands of the installed package `id`, for every user or for
/// this one, as its manifest names them.
fn installed_commands(id: &str) -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("/apps/{id}/manifest.yaml"),
        format!("{home}/apps/{id}/package/manifest.yaml"),
    ]
    .iter()
    .filter_map(|path| std::fs::read(path).ok())
    .filter_map(|bytes| Manifest::parse(&bytes).ok())
    .flat_map(|m| m.bins.into_iter().map(|b| b.name))
    .collect()
}

/// The driver of the driver package in the file `file`, which `manifest`
/// describes, read from the package.
fn driver_of(file: &str, manifest: &Manifest) -> Option<mikros_manifest::Driver> {
    let path = manifest.driver.as_deref()?;
    let bytes = std::fs::read(file).ok()?;
    let package = mikros_mika::format::Package::open(bytes.as_slice()).ok()?;
    let entry = package.find(path)?;
    let text = package.read(bytes.as_slice(), entry).ok()?;
    mikros_manifest::parse_driver(&manifest.id, &text)
        .ok()
        .map(|m| m.driver)
}

/// The manifest of the component at `path` of the package `file`.
fn component_of(file: &str, path: &str) -> Option<Manifest> {
    let bytes = std::fs::read(file).ok()?;
    let package = mikros_mika::format::Package::open(bytes.as_slice()).ok()?;
    let inner = package.read(bytes.as_slice(), package.find(path)?).ok()?;
    let component = mikros_mika::format::Package::open(inner.as_slice()).ok()?;
    let entry = component.find(mikros_mika::format::MANIFEST)?;
    let text = component.read(inner.as_slice(), entry).ok()?;
    Manifest::parse(&text).ok()
}

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
    dependency: bool,
    dev: bool,
    paint: Paint,
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
                show::update_warnings(&manifest.id, &old.version, &version, paint)
            );
        }
        print!("{}", show::describe(manifest, paint));
        if let Some(driver) = driver_of(file, manifest) {
            print!("{}", show::describe_driver(&driver));
        }
        if !agrees("install? [y/N] ") {
            println!("{} is not installed", manifest.id);
            continue;
        }
        let mut components = Vec::new();
        // Only optional components are asked for (MIKSPEC-0101.R63).
        for component in &manifest.components {
            let path = &component.path;
            println!("component {path} of {}", manifest.id);
            if let Some(m) = component_of(file, path) {
                print!("{}", show::describe(&m, paint));
            }
            if !component.optional {
                println!("installed with it");
            } else if agrees(&format!("install {path}? [y/N] ")) {
                components.push(path.clone());
            }
        }
        // A command that another installed package has stays with it
        // unless it goes to this one (MIKSPEC-0101.R44).
        let own = installed_commands(&manifest.id);
        let mut commands = Vec::new();
        for bin in &manifest.bins {
            let taken = Path::new(COMMANDS).join(&bin.name).exists();
            if !taken || own.contains(&bin.name) {
                continue;
            }
            println!("another installed package has the command {}", bin.name);
            if agrees(&format!("give {} to {}? [y/N] ", bin.name, manifest.id)) {
                commands.push(bin.name.clone());
            }
        }
        status(
            &manifest.id,
            installer
                .install(
                    connection(file)?,
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
    let run = start(&COMMAND, &mikros_command::list);
    let fail = |code: u8, message: &str| fail(&run, code, message);
    let paint = Paint::stdout(&run);
    let request = match request::read(&run) {
        Ok(request) => request,
        Err(reason) => return fail(INVALID, &reason),
    };
    // On Mikros, the needed libraries' versions are those installed for
    // every user or for this one (MIKSPEC-0101.R29).
    if let Request::Pack {
        folder,
        out,
        sysroot,
    } = &request
    {
        let libraries = match sysroot {
            Some(sysroot) => Path::new(sysroot).join("libraries"),
            None => Path::new(LIBRARIES).to_path_buf(),
        };
        let code = crate::pack::run(folder, out.as_deref(), &libraries, Paint::stderr(&run));
        return ExitCode::from(code);
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
            dependency,
            dev,
        } => install(&installer, &files, dependency, dev, paint),
        Request::Add { id, component } => status(
            &component,
            installer
                .add_component(id, component.clone(), FOREVER)
                .map(|r| r.map(|_| ())),
        )
        .map(|()| println!("installed {component}")),
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
                false => print!("{}", show::table(&rows, paint)),
            }
        }),
        Request::Info(target) => info(&installer, &target, paint),
        Request::Pack { .. } => Ok(()),
    };
    match done {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(why) => fail(FAILURE, &why),
    }
}

/// Shows the package in the file `target`, or the installed package of
/// that ID.
fn info(installer: &InstallerClient, target: &str, paint: Paint) -> Result<(), String> {
    if Path::new(target).is_file() {
        let manifest = match installer.inspect(connection(target)?, FOREVER) {
            Ok(Ok(r)) => r.manifest,
            Ok(Err(status)) => return Err(format!("{target}: {status:?}")),
            Err(e) => return Err(format!("{target}: {e:?}")),
        };
        let manifest =
            Manifest::parse(&manifest).map_err(|e| format!("{target}: {} {}", e.key, e.why))?;
        print!("{}", show::describe(&manifest, paint));
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
