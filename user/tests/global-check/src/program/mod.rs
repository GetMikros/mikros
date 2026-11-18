//! The checks, which call the install service, `mikros.packages.Installer`,
//! with packages made here in the current folder. The first install for
//! every user asks for `admin.packages.install`; the broker then holds it
//! for this process, so that the others do not ask again.

mod app;
mod grants;

use mikros_command::Directory;
use mikros_mika::format::{Builder, MANIFEST, checksum_text};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient, Rights};
use mikros_rt::interfaces::mikros::packages::{InstallerClient, PackageInfo};
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
const FORGE_PROBE: &str = "/system/bin/forge-probe";
const DAEMON: &str = "org.example.daemon";
const SERVICE: &str = "org.example.daemon.idle";
const DRIVER: &str = "org.example.thing";
const LIBRARY: &str = "org.example.greet";
const APP: &str = "org.example.libs";
const SUITE: &str = "org.example.suite";
const EXTRA: &str = "org.example.suite.extra";
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
    // As the package's service, which the service manager starts without
    // arguments, and which runs until it is stopped.
    let name = std::env::args().next().unwrap_or_default();
    if name.rsplit('/').next() == Some(IDLE) && std::env::args().len() == 1 {
        eprintln!("{}", app::STARTED);
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let checked = match std::env::args().nth(1).as_deref() {
        // As the installed app of the package with the service.
        Some("app") => app::checks(),
        Some("write") => app::write_public(),
        Some("hold") => app::hold(),
        _ => all(),
    };
    match checked {
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

/// Runs the app's checks as `path`, and fails with the number of the
/// check that failed there.
fn run_app(path: &str) -> Result<(), Failed> {
    match Command::new(path).arg("app").status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => match status.code().and_then(|c| u8::try_from(c).ok()) {
            Some(code) => failed(code, "as the app"),
            None => failed(31, format!("{path}: {status:?}")),
        },
        Err(e) => failed(31, format!("{path}: {e}")),
    }
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
    // First, so that ada's two passwords come one after the other: the
    // documents folder for the app, which an administrator allows.
    grants::allow()?;
    let installer = installer()?;
    service(&installer)?;
    driver(&installer)?;
    refused_drivers(&installer)?;
    kept(&installer)?;
    protocol_names(&installer)?;
    components(&installer)?;
    at_once()?;
    forged()?;
    run_app(&format!("/apps/{DAEMON}/bin/idle"))?;
    grants::revoke_while_open(&format!("/apps/{DAEMON}/bin/idle"))?;
    let _ = std::fs::remove_file("/users/ada/documents/global-check-note");
    remove(&installer, DAEMON, 13)?;
    wait_service(SERVICE, |s| s.is_none(), 14)
}

/// A global app with a service, this program as `bin/idle`, which the
/// service manager starts from the package's folder as soon as it is
/// installed (MIKSPEC-0101.R55), and which runs the app's checks when it
/// is given `app`. It stays until the end.
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

/// Every package that `List` gives.
fn listed(installer: &InstallerClient, code: u8) -> Result<Vec<PackageInfo>, Failed> {
    let mut all: Vec<PackageInfo> = Vec::new();
    loop {
        let after = all.last().map(|p| p.id.clone()).unwrap_or_default();
        let page = match installer.list(after, FOREVER) {
            Ok(Ok(r)) => r.packages,
            other => return failed(code, format!("List: {other:?}")),
        };
        if page.is_empty() {
            return Ok(all);
        }
        all.extend(page);
    }
}

/// Driver packages whose manifest needs the bus or matches a USB class
/// are refused, before any permission is asked for (MIKSPEC-0101.R76).
fn refused_drivers(installer: &InstallerClient) -> Result<(), Failed> {
    let manifest = format!(
        "id: {DRIVER}\nkind: driver\nversion: 1.0.0\nname: Thing\n\
         description: Drives nothing.\ndriver: driver.yaml\n"
    );
    for refused in ["needs: [bus]\n", "match: [usb:c03]\n"] {
        let driver = format!("name: {DRIVER}\nbinary: /system/drivers/{DRIVER}/thing\n{refused}");
        let files = [
            ("thing", read(ITSELF, 19)?, true),
            ("driver.yaml", driver.into_bytes(), false),
        ];
        pack("refused.mika", &manifest, &files)?;
        let package =
            connection("refused.mika").ok_or(Failed(19, "opening refused.mika".into()))?;
        let installed = installer.install(package, vec![], false, false, vec![], FOREVER);
        let _ = std::fs::remove_file("refused.mika");
        ensure(
            matches!(installed, Ok(Err(Status::InvalidArgs))),
            19,
            &format!("a driver with {} gave {installed:?}", refused.trim_end()),
        )?;
    }
    Ok(())
}

/// A package whose service provides a protocol that is not named under
/// the package's ID is refused (MIKSPEC-0101.R66).
fn protocol_names(installer: &InstallerClient) -> Result<(), Failed> {
    let manifest = "id: org.example.bad\nkind: app\nversion: 1.0.0\nname: Bad\n\
                    description: Squats.\nexecutable: bin/idle\nservices:\n  idle: idle.yaml\n";
    let service = "name: idle\nbinary: /apps/org.example.bad/bin/idle\nstart: on-demand\n\
                   provides:\n  - protocol: mikros.example.Bad\n";
    let files = [
        ("bin/idle", read(ITSELF, 18)?, true),
        ("idle.yaml", service.as_bytes().to_vec(), false),
    ];
    pack("bad.mika", manifest, &files)?;
    let package = connection("bad.mika").ok_or(Failed(18, "opening bad.mika".into()))?;
    let installed = installer.install(package, vec![], false, false, vec![], FOREVER);
    let _ = std::fs::remove_file("bad.mika");
    ensure(
        matches!(installed, Ok(Err(Status::InvalidArgs))),
        18,
        &format!("a squatted protocol gave {installed:?}"),
    )
}

/// The bytes of a package of the library `id` in `version`.
fn library_package(id: &str, version: &str) -> Vec<u8> {
    let mut builder = Builder::default();
    builder.file("words.txt", b"word".to_vec(), false);
    let checksum = checksum_text(&builder.checksum(None));
    let manifest = format!(
        "id: {id}\nkind: library\nversion: {version}\nname: Extra\ndescription: More.\n\
         checksum: {checksum}\n"
    );
    builder.file(MANIFEST, manifest.into_bytes(), false);
    builder.finish()
}

/// `InstallComponent` of the file `name` holding `bytes` for `SUITE`.
fn install_component(
    installer: &InstallerClient,
    name: &str,
    bytes: &[u8],
    code: u8,
) -> Result<Result<(), Status>, Failed> {
    std::fs::write(name, bytes).or(failed(code, format!("writing {name}")))?;
    let package = connection(name).ok_or(Failed(code, format!("opening {name}")))?;
    let installed = installer.install_component(SUITE.into(), package, FOREVER);
    let _ = std::fs::remove_file(name);
    match installed {
        Ok(r) => Ok(r.map(|_| ())),
        Err(e) => failed(code, format!("InstallComponent: {e:?}")),
    }
}

/// An optional component that an install leaves out is kept, and comes
/// with `AddComponent` by its ID; `InstallComponent` updates it from a
/// file of a newer version only; and it goes with its package
/// (MIKSPEC-0101.R71, MIKSPEC-0101.R72).
fn components(installer: &InstallerClient) -> Result<(), Failed> {
    let manifest = format!(
        "id: {SUITE}\nkind: app\nversion: 1.0.0\nname: Suite\ndescription: Parts.\n\
         executable: bin/app\ncomponents:\n  - path: parts/extra.mika\n    optional: true\n"
    );
    let files = [
        ("bin/app", b"app".to_vec(), true),
        ("parts/extra.mika", library_package(EXTRA, "1.0.0"), false),
    ];
    install(installer, "suite.mika", &manifest, &files, 26)?;
    let ids = |all: &[PackageInfo]| all.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    let all = listed(installer, 26)?;
    ensure(
        ids(&all).contains(&SUITE.into()) && !ids(&all).contains(&EXTRA.into()),
        26,
        "the suite without its optional part",
    )?;
    let added = installer.add_component(SUITE.into(), EXTRA.into(), FOREVER);
    ensure(
        matches!(added, Ok(Ok(_))),
        27,
        &format!("AddComponent gave {added:?}"),
    )?;
    let owned = listed(installer, 27)?
        .iter()
        .any(|p| p.id == EXTRA && p.owner == SUITE && p.version == "1.0.0");
    ensure(owned, 27, "the added part, owned by the suite")?;
    let again = installer.add_component(SUITE.into(), EXTRA.into(), FOREVER);
    ensure(
        matches!(again, Ok(Err(Status::AlreadyExists))),
        28,
        &format!("a second AddComponent gave {again:?}"),
    )?;
    let newer = library_package(EXTRA, "1.1.0");
    let updated = install_component(installer, "extra.mika", &newer, 29)?;
    ensure(
        updated.is_ok(),
        29,
        &format!("InstallComponent gave {updated:?}"),
    )?;
    let same = install_component(installer, "extra.mika", &newer, 29)?;
    ensure(
        same == Err(Status::BadState),
        29,
        &format!("the same version gave {same:?}"),
    )?;
    let foreign = library_package("org.example.other", "1.0.0");
    let refused = install_component(installer, "other.mika", &foreign, 29)?;
    ensure(
        refused == Err(Status::InvalidArgs),
        29,
        &format!("another package's component gave {refused:?}"),
    )?;
    remove(installer, SUITE, 30)?;
    let gone = !ids(&listed(installer, 30)?).contains(&EXTRA.into());
    ensure(gone, 30, "the part, gone with the suite")
}

/// Two local packages installed at the same time, through connections
/// of their own, each with a required component: both installs are
/// recorded, so that each component is owned by its package
/// (MIKSPEC-0101.R72). Both are removed again.
fn at_once() -> Result<(), Failed> {
    const CODE: u8 = 60;
    const IDS: [&str; 2] = ["org.example.first", "org.example.second"];
    for id in IDS {
        let manifest = format!(
            "id: {id}\nkind: app\ninstall: local\nversion: 1.0.0\nname: Part\n\
             description: Parts.\nexecutable: bin/app\ncomponents:\n  - path: part.mika\n"
        );
        let part = local_library(&format!("{id}.part"));
        let files = [
            ("bin/app", b"app".to_vec(), true),
            ("part.mika", part, false),
        ];
        pack(&format!("{id}.mika"), &manifest, &files)?;
    }
    let installs: Vec<_> = IDS
        .iter()
        .map(|id| {
            let name = format!("{id}.mika");
            std::thread::spawn(move || {
                let installer = installer()?;
                let package = connection(&name).ok_or(Failed(CODE, format!("opening {name}")))?;
                match installer.install(package, vec![], false, false, vec![], FOREVER) {
                    Ok(Ok(_)) => Ok(()),
                    other => failed(CODE, format!("installing {name}: {other:?}")),
                }
            })
        })
        .collect();
    for install in installs {
        install
            .join()
            .map_err(|_| Failed(CODE, "an install thread panicked".into()))??;
    }
    let installer = installer()?;
    let all = listed(&installer, CODE)?;
    for id in IDS {
        let _ = std::fs::remove_file(format!("{id}.mika"));
        let part = format!("{id}.part");
        ensure(
            all.iter()
                .any(|p| p.id == part && p.owner == id && !p.global),
            CODE,
            &format!("{part}, owned by {id}, after both installs"),
        )?;
    }
    for id in IDS {
        let removed = installer.remove(id.into(), false, false, FOREVER);
        ensure(
            matches!(removed, Ok(Ok(_))),
            CODE,
            &format!("removing {id} gave {removed:?}"),
        )?;
    }
    Ok(())
}

/// forge-probe, which asks the broker for a permission of its own
/// process under another app's ID, sees that refused without a question
/// (MIKSPEC-0057.R33).
fn forged() -> Result<(), Failed> {
    const CODE: u8 = 61;
    let status = Command::new(FORGE_PROBE)
        .status()
        .or(failed(CODE, "starting forge-probe"))?;
    ensure(
        status.success(),
        CODE,
        &format!("forge-probe exited with {status}"),
    )
}

/// The bytes of a package of the library `id` for its user alone.
fn local_library(id: &str) -> Vec<u8> {
    let mut builder = Builder::default();
    builder.file("words.txt", b"word".to_vec(), false);
    let checksum = checksum_text(&builder.checksum(None));
    let manifest = format!(
        "id: {id}\nkind: library\ninstall: local\nversion: 1.0.0\nname: Part\n\
         description: A part.\nchecksum: {checksum}\n"
    );
    builder.file(MANIFEST, manifest.into_bytes(), false);
    builder.finish()
}
