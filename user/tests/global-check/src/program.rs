//! The checks, which call the install service, `mikros.packages.Installer`,
//! with packages made here in the current folder. The first install for
//! every user asks for `admin.packages.install`; the broker then holds it
//! for this process, so that the others do not ask again.

use mikros_command::Directory;
use mikros_mika::format::{Builder, MANIFEST, checksum_text};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient, Rights};
use mikros_rt::interfaces::mikros::packages::InstallerClient;
use std::os::mikros::io::AsHandleExt;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

const INSTALLER: &str = "mikros.packages.Installer";
const FOREVER: i64 = i64::MAX;
/// The name under which this program, as the package's service, idles.
const IDLE: &str = "idle";
/// The program and libraries of the C library test of packages.
const PACKAGE_C: &str = "/system/share/package-c";
/// This program, which the package's service and driver copy.
const ITSELF: &str = "/system/bin/global-check";
const DAEMON: &str = "org.example.daemon";
const SERVICE: &str = "org.example.daemon.idle";
const DRIVER: &str = "org.example.thing";
const LIBRARY: &str = "org.example.greet";
const APP: &str = "org.example.libs";
/// How long a service has to start or stop.
const SERVICE_WAIT: Duration = Duration::from_secs(30);

/// A failed check: its number and what failed.
struct Failed(u8, String);

fn failed<T>(code: u8, what: impl Into<String>) -> Result<T, Failed> {
    Err(Failed(code, what.into()))
}

fn ensure(ok: bool, code: u8, what: &str) -> Result<(), Failed> {
    if ok { Ok(()) } else { failed(code, what) }
}

pub fn main() -> ExitCode {
    // As the package's service, which runs until it is stopped.
    let name = std::env::args().next().unwrap_or_default();
    if name.rsplit('/').next() == Some(IDLE) {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    match all() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failed(code, what)) => {
            eprintln!("global-check: {code}: {what}");
            ExitCode::from(code)
        }
    }
}

fn installer() -> Result<InstallerClient, Failed> {
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return failed(10, "no service directory");
    };
    match directory.connect(INSTALLER) {
        Ok(client) => Ok(InstallerClient::new(client)),
        Err(why) => failed(10, why),
    }
}

/// Writes a package of `manifest`, without its checksum, and `files` to
/// the file `name` of the current folder.
fn pack(name: &str, manifest: &str, files: &[(&str, Vec<u8>, bool)]) -> Result<(), Failed> {
    let mut builder = Builder::default();
    // Each folder once, before the files in it.
    let mut folders = Vec::new();
    for (path, bytes, executable) in files {
        if let Some((folder, _)) = path.rsplit_once('/')
            && !folders.contains(&folder)
        {
            folders.push(folder);
            builder.folder(folder);
        }
        builder.file(path, bytes.clone(), *executable);
    }
    let checksum = checksum_text(&builder.checksum(None));
    let manifest = format!("{manifest}checksum: {checksum}\n");
    builder.file(MANIFEST, manifest.into_bytes(), false);
    std::fs::write(name, builder.finish()).or(failed(10, format!("writing {name}")))
}

/// A new connection, for reading, to the file `path`.
fn connection(path: &str) -> Option<ClientEnd<Node>> {
    let file = std::fs::File::open(path).ok()?;
    let handle = file.handle().ok()?;
    // SAFETY: the handle is ours.
    let handle = unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) };
    let node = NodeClient::new(ClientEnd::new(handle));
    let (client, server) = endpoints::<Node>().ok()?;
    matches!(node.clone(server, Rights::READ, FOREVER), Ok(Ok(_))).then_some(client)
}

/// Packs and installs the package `name`, removing its file again;
/// fails with `code`.
fn install(
    installer: &InstallerClient,
    name: &str,
    manifest: &str,
    files: &[(&str, Vec<u8>, bool)],
    code: u8,
) -> Result<(), Failed> {
    pack(name, manifest, files)?;
    let package = connection(name).ok_or(Failed(code, format!("opening {name}")))?;
    let installed = installer.install(package, vec![], false, false, vec![], FOREVER);
    let _ = std::fs::remove_file(name);
    match installed {
        Ok(Ok(_)) => Ok(()),
        other => failed(code, format!("installing {name}: {other:?}")),
    }
}

/// Removes the global package `id`; fails with `code`.
fn remove(installer: &InstallerClient, id: &str, code: u8) -> Result<(), Failed> {
    match installer.remove(id.into(), true, false, FOREVER) {
        Ok(Ok(_)) => Ok(()),
        other => failed(code, format!("removing {id}: {other:?}")),
    }
}

fn read(path: &str, code: u8) -> Result<Vec<u8>, Failed> {
    std::fs::read(path).or(failed(code, format!("reading {path}")))
}

/// Runs `path` with `args`, which must exit with 0; fails with `code`.
fn run(path: &str, args: &[&str], code: u8) -> Result<(), Failed> {
    match Command::new(path).args(args).status() {
        Ok(status) if status.success() => Ok(()),
        other => failed(code, format!("{path}: {other:?}")),
    }
}

/// The state that `services --json` gives the service `name`, if it
/// lists it.
fn service_state(name: &str) -> Option<String> {
    let out = Command::new("services").arg("--json").output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let at = text.find(&format!("{{\"name\":\"{name}\",\"state\":\""))?;
    let rest = &text[at..];
    let state = rest.split("\"state\":\"").nth(1)?;
    Some(state.split('"').next()?.to_string())
}

/// Waits until `ok` holds for the state of the service `name`.
fn wait_service(name: &str, ok: impl Fn(Option<String>) -> bool, code: u8) -> Result<(), Failed> {
    let start = Instant::now();
    loop {
        let state = service_state(name);
        if ok(state.clone()) {
            return Ok(());
        }
        if start.elapsed() > SERVICE_WAIT {
            return failed(code, format!("{name} is {state:?}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn all() -> Result<(), Failed> {
    let installer = installer()?;
    service(&installer)?;
    driver(&installer)?;
    kept(&installer)?;
    remove(&installer, DAEMON, 13)?;
    wait_service(SERVICE, |s| s.is_none(), 14)
}

/// A global app with a service, this program as `bin/idle`, which the
/// service manager starts from the package's folder as soon as it is
/// installed (MIKSPEC-0101.R55). It stays until the end.
fn service(installer: &InstallerClient) -> Result<(), Failed> {
    let manifest = format!(
        "id: {DAEMON}\nkind: app\nversion: 1.0.0\nname: Daemon\ndescription: Idles.\n\
         executable: bin/idle\nservices:\n  idle: services/idle.yaml\n"
    );
    let service = format!(
        "name: idle\nbinary: /apps/{DAEMON}/bin/idle\nstart: boot\nrestart: always\n\
         stop_timeout: 1\n"
    );
    let files = [
        ("bin/idle", read(ITSELF, 11)?, true),
        ("services/idle.yaml", service.into_bytes(), false),
    ];
    install(installer, "daemon.mika", &manifest, &files, 11)?;
    wait_service(SERVICE, |s| s.as_deref() == Some("running"), 12)
}

/// A driver package, which matches no device, whose manifest shows in
/// `/system/drivers` while it is installed (MIKSPEC-0100.R19).
fn driver(installer: &InstallerClient) -> Result<(), Failed> {
    let manifest = format!(
        "id: {DRIVER}\nkind: driver\nversion: 1.0.0\nname: Thing\n\
         description: Drives nothing.\ndriver: driver.yaml\n"
    );
    let driver = format!("name: {DRIVER}\nbinary: /system/drivers/{DRIVER}/thing\n");
    let files = [
        ("thing", read(ITSELF, 15)?, true),
        ("driver.yaml", driver.into_bytes(), false),
    ];
    install(installer, "thing.mika", &manifest, &files, 15)?;
    let path = format!("/system/drivers/{DRIVER}/driver.yaml");
    ensure(
        std::fs::read(&path).is_ok(),
        16,
        "the installed driver's manifest",
    )?;
    remove(installer, DRIVER, 17)?;
    ensure(
        std::fs::read(&path).is_err(),
        17,
        "the removed driver's manifest",
    )
}

/// A global library and an app whose program needs it; the library's
/// next version no longer has the library, and the program still runs
/// with the kept version (MIKSPEC-0101.R51).
fn kept(installer: &InstallerClient) -> Result<(), Failed> {
    let file = |name: &str| format!("{PACKAGE_C}/{name}");
    let library = |version: &str| {
        format!(
            "id: {LIBRARY}\nkind: library\nversion: {version}\nname: Greet\n\
             description: Greets.\n"
        )
    };
    let greet = read(&file("greet.dlib"), 20)?;
    install(
        installer,
        "greet.mika",
        &library("1.0.0"),
        &[("greet.dlib", greet, true)],
        20,
    )?;
    let app = format!(
        "id: {APP}\nkind: app\nversion: 1.0.0\nname: Libraries\n\
         description: Uses libraries.\nexecutable: bin/package-user\n"
    );
    let files = [
        ("bin/package-user", read(&file("package-user"), 21)?, true),
        (
            "lib/librunpath.dlib",
            read(&file("librunpath.dlib"), 21)?,
            true,
        ),
        (
            "libraries/libown.dlib",
            read(&file("libown.dlib"), 21)?,
            true,
        ),
    ];
    install(installer, "libs.mika", &app, &files, 21)?;
    let program = format!("/apps/{APP}/bin/package-user");
    run(&program, &[], 22)?;
    let notes = b"no library\n".to_vec();
    install(
        installer,
        "greet.mika",
        &library("2.0.0"),
        &[("notes.txt", notes, false)],
        23,
    )?;
    run(&program, &[], 24)?;
    remove(installer, APP, 25)?;
    remove(installer, LIBRARY, 25)
}
