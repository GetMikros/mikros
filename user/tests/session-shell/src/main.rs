//! The user's shell of `tests/console.img`, installed there as
//! /system/bin/msh, which the session manager starts once a
//! user logged in on the text console. It checks that it runs as a user
//! other than 0 with the app ID `system.cli`, leads a login session and
//! a session and process group of its own, starts in the user's home,
//! and has exactly the environment of that user's session, with the
//! user's home and a `/libraries` in its namespace, which for bob holds the README of his
//! own Libraries folder. It installs a package of an app for its user,
//! updates it with one that also holds the app probe, starts the probe
//! from there, and removes the package.
//! On the terminal it was given it asks
//! for the cursor position, which the console answers, and reads the
//! line that the runner types. It exits with 0 once all of that held,
//! and otherwise with the number of the first check that failed, which
//! the session manager logs.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    AclEntry, AclTag, Error, Node, NodeAsyncClient, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::terminal::{Attributes, Terminal, TerminalAsyncClient, When};
use mikros_rt::{managed, sys};

mikros_rt::raw_main!(main);

const TERMINAL: &str = "mikros.terminal.Terminal";
const SAME_RIGHTS: u32 = 1 << 31;
const FOREVER: i64 = i64::MAX;
const SECOND: i64 = 1_000_000_000;
/// Bits of a terminal's local flags: canonical input and echo.
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
/// The environment that the session of user `name` must have, in order.
fn expected(name: &str) -> [String; 7] {
    [
        format!("HOME=/users/{name}"),
        format!("USER={name}"),
        format!("LOGNAME={name}"),
        "SHELL=/system/bin/msh".into(),
        "PATH=/system/bin:/system/commands".into(),
        "TERM=xterm-256color".into(),
        // The process manager's, for the session's `/tmp`.
        "TMPDIR=/tmp".into(),
    ]
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// The arguments and then the environment entries in `mem`.
fn strings(mem: &OwnedHandle) -> Option<(Vec<String>, Vec<String>)> {
    let mut bytes = vec![0u8; 4096];
    // SAFETY: the pointer describes `bytes`.
    if unsafe { sys::mem_read(mem.raw(), 0, bytes.as_mut_ptr(), bytes.len() as u64) }
        != mikros_rt::OK
    {
        return None;
    }
    let count = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    let (arguments, environment) = (count(0) as usize, count(4) as usize);
    let mut all = bytes[8..]
        .split(|&b| b == 0)
        .take(arguments + environment)
        .map(|s| String::from_utf8_lossy(s).into_owned());
    let arguments = all.by_ref().take(arguments).collect();
    Some((arguments, all.collect()))
}

fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

fn copy(a: &Attributes) -> Attributes {
    Attributes {
        input_flags: a.input_flags,
        output_flags: a.output_flags,
        control_flags: a.control_flags,
        local_flags: a.local_flags,
        control_chars: a.control_chars,
        input_speed: a.input_speed,
        output_speed: a.output_speed,
    }
}

/// The home folder's entry, which the process manager bound to this
/// process, takes no other subject; a file made through it is the user's;
/// and an extended access list decides what the user may do with it, and
/// the owner may set it (MIKSPEC-0050.R44, MIKSPEC-0049.R25,
/// MIKSPEC-0049.R26). Fails with the number of the step.
async fn acls(
    home: &NodeAsyncClient,
    user: u32,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    const NAME: &str = "acl-check";
    let entry = |tag, id, rights| AclEntry { tag, id, rights };
    let (_, server) = endpoints::<Node>().map_err(|_| 17)?;
    let again = home.restrict(user, 0, 1, 1, 0, server).await;
    check(matches!(again, Ok(Err(Error::BadState))), 17)?;
    let (file, server) = endpoints::<Node>().map_err(|_| 18)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let opened = home.open(NAME.into(), flags, 0o600, rw, server).await;
    check(matches!(opened, Ok(Ok(_))), 18)?;
    let file = NodeAsyncClient::new(file, reactor.clone());
    let owner = file.get_attributes().await.ok().and_then(|r| r.ok());
    check(owner.is_some_and(|a| a.attributes.owner == user), 18)?;
    check(matches!(file.write(0, b"x".to_vec()).await, Ok(Ok(_))), 18)?;
    // Only another user may read it, so its owner may not.
    let none = Rights(0);
    let denying = vec![
        entry(AclTag::Owner, 0, none),
        entry(AclTag::User, 4242, Rights::READ),
        entry(AclTag::Group, 0, none),
        entry(AclTag::Mask, 0, Rights::READ),
        entry(AclTag::Other, 0, none),
    ];
    check(matches!(file.set_acl(false, denying).await, Ok(Ok(_))), 19)?;
    let listed = file.get_acl(false).await.ok().and_then(|r| r.ok());
    check(listed.is_some_and(|l| l.entries.len() == 5), 19)?;
    check(
        matches!(file.read(0, 1).await, Ok(Err(Error::AccessDenied))),
        20,
    )?;
    // Its owner gives it back the entries of a mode.
    let owner_only = vec![
        entry(AclTag::Owner, 0, rw),
        entry(AclTag::Group, 0, none),
        entry(AclTag::Other, 0, none),
    ];
    check(
        matches!(file.set_acl(false, owner_only).await, Ok(Ok(_))),
        21,
    )?;
    check(matches!(file.read(0, 1).await, Ok(Ok(_))), 21)?;
    check(
        matches!(home.unlink(NAME.into(), false).await, Ok(Ok(_))),
        21,
    )?;
    Ok(())
}

/// A local install through the install service: a package made here,
/// written to the home folder, installs into the user's
/// `apps/<id>/package`, which `List` then shows as local, and goes again
/// with `Remove` (MIKSPEC-0101.R25, MIKSPEC-0101.R27). Fails with the
/// number of the step.
async fn packages(shell: &Shell<'_>, reactor: &mikros_rt::exec::Reactor) -> Result<(), i64> {
    use mikros_rt::interfaces::mikros::packages::{Installer, InstallerAsyncClient};
    const ID: &str = "org.example.hello";
    const FILE: &str = "hello.mika";
    let (home, directory) = (shell.home, shell.directory);
    let probe = read_all(shell.system, PROBE, reactor).await?;
    let (installer, server) = endpoints::<Installer>().map_err(|_| 23)?;
    let connected = directory
        .connect(
            "mikros.packages.Installer".into(),
            None,
            server.into_handle(),
        )
        .await;
    check(matches!(connected, Ok(Ok(_))), 23)?;
    let installer = InstallerAsyncClient::new(installer, reactor.clone());
    // Version 1.0.0, then 1.1.0 over it, which swaps the new subvolume
    // with the old one.
    for (version, step) in [("1.0.0", 24), ("1.1.0", 29)] {
        // The second version also holds the app probe.
        let program = (step == 29).then_some(probe.as_slice());
        let file = write_package(home, FILE, ID, version, program, reactor).await?;
        let (client, server) = endpoints::<Node>().map_err(|_| step)?;
        check(
            matches!(file.clone(server, Rights::READ).await, Ok(Ok(_))),
            step,
        )?;
        let inspected = installer.inspect(client).await;
        let named =
            matches!(&inspected, Ok(Ok(r)) if r.manifest.starts_with(b"id: org.example.hello"));
        check(named, step)?;
        let (client, server) = endpoints::<Node>().map_err(|_| step + 1)?;
        check(
            matches!(file.clone(server, Rights::READ).await, Ok(Ok(_))),
            step + 1,
        )?;
        let installed = installer
            .install(client, false, vec![], false, false, vec![])
            .await;
        check(matches!(installed, Ok(Ok(_))), step + 1)?;
        let listed = installer.list(String::new()).await;
        let shown = matches!(&listed, Ok(Ok(r)) if r.packages.iter().any(|p| p.id == ID && !p.global && p.version == version));
        check(shown, step + 2)?;
        let (program, server) = endpoints::<Node>().map_err(|_| step + 3)?;
        let path = format!("apps/{ID}/package/bin/hello");
        let found = home.open(path, OpenFlags(0), 0, Rights::READ, server).await;
        check(matches!(found, Ok(Ok(_))), step + 3)?;
        let program = NodeAsyncClient::new(program, reactor.clone());
        let read = program.read(0, 64).await;
        let says = format!("hello {version}");
        check(
            matches!(&read, Ok(Ok(r)) if r.data == says.as_bytes()),
            step + 3,
        )?;
    }
    run_app(shell, ID, reactor).await?;
    run_command(shell, &installer, ID, reactor).await?;
    run_portable(shell, &probe, reactor).await?;
    run_libraries(shell, &installer, reactor).await?;
    let removed = installer.remove(ID.into(), false, false).await;
    check(matches!(removed, Ok(Ok(_))), 28)?;
    let (_, server) = endpoints::<Node>().map_err(|_| 28)?;
    let path = format!("apps/{ID}/package/bin/hello");
    let gone = home.open(path, OpenFlags(0), 0, Rights::READ, server).await;
    check(matches!(gone, Ok(Err(Error::NotFound))), 28)?;
    check(
        matches!(home.unlink(FILE.into(), false).await, Ok(Ok(_))),
        28,
    )?;
    Ok(())
}

/// What the checks of installed apps use of the shell's namespace and
/// identity.
struct Shell<'a> {
    home: &'a NodeAsyncClient,
    /// The entry `/users/<name>/apps`.
    apps: &'a NodeAsyncClient,
    /// The entry `/system/commands`.
    commands: &'a NodeAsyncClient,
    system: &'a NodeAsyncClient,
    directory: &'a DirectoryAsyncClient,
    launcher: &'a LauncherAsyncClient,
    name: &'a str,
    user: u32,
}

/// The app probe, from `/system`, and the user given to `Spawn` for the
/// caller's own.
const PROBE: &str = "bin/app-probe";
const CALLER_USER: u32 = u32::MAX;

/// The file `path` of `dir`, whole.
async fn read_all(
    dir: &NodeAsyncClient,
    path: &str,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<Vec<u8>, i64> {
    let (file, server) = endpoints::<Node>().map_err(|_| 33)?;
    let opened = dir
        .open(path.into(), OpenFlags(0), 0, Rights::READ, server)
        .await;
    check(matches!(opened, Ok(Ok(_))), 33)?;
    let file = NodeAsyncClient::new(file, reactor.clone());
    let mut bytes = Vec::new();
    loop {
        let read = file.read(bytes.len() as u64, 32768).await;
        let data = read.ok().and_then(|r| r.ok()).ok_or(33)?.data;
        if data.is_empty() {
            return Ok(bytes);
        }
        bytes.extend(data);
    }
}

/// Starts the app probe from the local package of `id` without naming an
/// app ID, which makes it that app (MIKSPEC-0101.R39, MIKSPEC-0034.R45),
/// and checks that it and its child passed their checks
/// (MIKSPEC-0101.R7, MIKSPEC-0101.R41), that the note it wrote is in the
/// user's `data` folder of the app, which belongs to the user, and that
/// the shell's `/users/<name>/apps` takes no new files (MIKSPEC-0101.R42).
async fn run_app(
    shell: &Shell<'_>,
    id: &str,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    use mikros_rt::interfaces::mikros::process::{SpawnRequest, StatusKind, WaitOptions};
    let path = format!("/users/{}/apps/{id}/package/bin/probe", shell.name);
    let request = SpawnRequest {
        path: path.clone(),
        arguments: arguments(&[&path]).ok_or(34)?,
        handles: Vec::new(),
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let spawned = shell.launcher.spawn(request).await;
    let pid = spawned.ok().and_then(|r| r.ok()).ok_or(34)?.pid;
    let info = shell.launcher.get_info(pid).await;
    let info = info.ok().and_then(|r| r.ok()).ok_or(35)?.info;
    check(info.app_id == id, 35)?;
    let waited = shell
        .launcher
        .wait(pid as i32, WaitOptions(0), now() + 60 * SECOND)
        .await;
    let status = waited.ok().and_then(|r| r.ok()).ok_or(36)?.status;
    check(status.kind == StatusKind::Exited, 36)?;
    // The probe's own number for the check that failed.
    if status.exit_code != 0 {
        return Err(status.exit_code);
    }
    let note = read_all(shell.home, &format!("apps/{id}/data/note"), reactor).await;
    check(note.as_deref() == Ok(b"from the app".as_slice()), 37)?;
    let (data, server) = endpoints::<Node>().map_err(|_| 38)?;
    let opened = shell
        .home
        .open(
            format!("apps/{id}/data"),
            OpenFlags::DIRECTORY,
            0,
            Rights::READ,
            server,
        )
        .await;
    check(matches!(opened, Ok(Ok(_))), 38)?;
    let data = NodeAsyncClient::new(data, reactor.clone());
    let owner = data.get_attributes().await.ok().and_then(|r| r.ok());
    check(owner.is_some_and(|a| a.attributes.owner == shell.user), 38)?;
    let (_, server) = endpoints::<Node>().map_err(|_| 39)?;
    let flags = OpenFlags(OpenFlags::CREATE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let made = shell
        .apps
        .open("made".into(), flags, 0o644, rw, server)
        .await;
    check(matches!(made, Ok(Err(_))), 39)?;
    Ok(())
}

/// Checks that `/system/commands` lists the command `hello-probe` of the
/// installed package `id`, that run from there in the home folder it
/// starts as that app, in the home folder, which it gets in its
/// namespace with the shell's rights there, and that a package with a
/// command of the system's is refused (MIKSPEC-0101.R44).
async fn run_command(
    shell: &Shell<'_>,
    installer: &mikros_rt::interfaces::mikros::packages::InstallerAsyncClient,
    id: &str,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    use mikros_rt::interfaces::mikros::base::Status;
    use mikros_rt::interfaces::mikros::process::{SpawnRequest, StatusKind, WaitOptions};
    const COMMAND: &str = "hello-probe";
    let listed = shell.commands.read_directory(0, false).await;
    let entries = listed.ok().and_then(|r| r.ok()).ok_or(70)?.entries;
    check(entries.iter().any(|e| e.name == COMMAND), 70)?;
    let path = format!("/system/commands/{COMMAND}");
    let home = format!("/users/{}", shell.name);
    let request = SpawnRequest {
        path: path.clone(),
        arguments: arguments(&[&path, "command", &home]).ok_or(71)?,
        handles: Vec::new(),
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: Some(home.clone()),
        umask: u32::MAX,
    };
    let spawned = shell.launcher.spawn(request).await;
    let pid = spawned.ok().and_then(|r| r.ok()).ok_or(71)?.pid;
    let info = shell.launcher.get_info(pid).await;
    let info = info.ok().and_then(|r| r.ok()).ok_or(72)?.info;
    check(info.app_id == id, 72)?;
    let waited = shell
        .launcher
        .wait(pid as i32, WaitOptions(0), now() + 60 * SECOND)
        .await;
    let status = waited.ok().and_then(|r| r.ok()).ok_or(73)?.status;
    check(status.kind == StatusKind::Exited, 73)?;
    // The probe's own number for the check that failed.
    if status.exit_code != 0 {
        return Err(status.exit_code);
    }
    let note = read_all(shell.home, "from-command", reactor).await;
    check(note.as_deref() == Ok(b"from the command".as_slice()), 74)?;
    let unlinked = shell.home.unlink("from-command".into(), false).await;
    check(matches!(unlinked, Ok(Ok(_))), 74)?;
    // The system has msh.
    let manifest = "id: org.example.shell\nkind: app\nversion: 1.0.0\nname: Shell\n\
                    description: Is msh.\nexecutable: bin/msh\ninstall: local\n\
                    bins:\n  - name: msh\n    path: bin/msh\n";
    let file = pack(
        shell,
        "shell.mika",
        manifest,
        &[("bin/msh", b"#!/system/bin/msh\n".to_vec())],
        75,
        reactor,
    )
    .await?;
    let (client, server) = endpoints::<Node>().map_err(|_| 75)?;
    let cloned = file.clone(server, Rights::READ).await;
    check(matches!(cloned, Ok(Ok(_))), 75)?;
    let installed = installer
        .install(client, false, vec![], false, false, vec![])
        .await;
    check(matches!(installed, Ok(Err(Status::AlreadyExists))), 75)?;
    let unlinked = shell.home.unlink("shell.mika".into(), false).await;
    check(matches!(unlinked, Ok(Ok(_))), 75)
}

/// Starts the program of `request`, checks that it runs as `id`, and waits
/// for it: fails with `code` when it does not, and with the program's own
/// code when that is not 0.
async fn run_as(
    shell: &Shell<'_>,
    request: mikros_rt::interfaces::mikros::process::SpawnRequest,
    id: &str,
    code: i64,
) -> Result<(), i64> {
    use mikros_rt::interfaces::mikros::process::{StatusKind, WaitOptions};
    let spawned = shell.launcher.spawn(request).await;
    let pid = spawned.ok().and_then(|r| r.ok()).ok_or(code)?.pid;
    let info = shell.launcher.get_info(pid).await;
    let info = info.ok().and_then(|r| r.ok()).ok_or(code)?.info;
    check(info.app_id == id, code)?;
    let waited = shell
        .launcher
        .wait(pid as i32, WaitOptions(0), now() + 60 * SECOND)
        .await;
    let status = waited.ok().and_then(|r| r.ok()).ok_or(code)?.status;
    check(status.kind == StatusKind::Exited, code)?;
    match status.exit_code {
        0 => Ok(()),
        own => Err(own),
    }
}

/// A request to start the program at `path` with `args` and no app ID.
fn request(
    path: &str,
    args: &[&str],
) -> Option<mikros_rt::interfaces::mikros::process::SpawnRequest> {
    Some(mikros_rt::interfaces::mikros::process::SpawnRequest {
        path: path.into(),
        arguments: arguments(args)?,
        handles: Vec::new(),
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    })
}

/// Writes `contents` to the new file `path` of `home` with `mode`.
async fn write_file(
    home: &NodeAsyncClient,
    path: &str,
    contents: &[u8],
    mode: u32,
    code: i64,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    let (file, server) = endpoints::<Node>().map_err(|_| code)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let opened = home.open(path.into(), flags, mode, rw, server).await;
    check(matches!(opened, Ok(Ok(_))), code)?;
    let file = NodeAsyncClient::new(file, reactor.clone());
    for (at, chunk) in contents.chunks(32768).enumerate() {
        let written = file.write((at * 32768) as u64, chunk.to_vec()).await;
        check(matches!(written, Ok(Ok(_))), code)?;
    }
    Ok(())
}

/// Removes the files `names` of the folder `folder` of `home`, and the
/// folder itself; fails with `code`.
async fn remove(
    home: &NodeAsyncClient,
    folder: &str,
    names: &[&str],
    code: i64,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    let (dir, server) = endpoints::<Node>().map_err(|_| code)?;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let opened = home
        .open(folder.into(), OpenFlags::DIRECTORY, 0, rw, server)
        .await;
    check(matches!(opened, Ok(Ok(_))), code)?;
    let dir = NodeAsyncClient::new(dir, reactor.clone());
    for name in names {
        let unlinked = dir.unlink((*name).into(), false).await;
        check(matches!(unlinked, Ok(Ok(_))), code)?;
    }
    let unlinked = home.unlink(folder.into(), true).await;
    check(matches!(unlinked, Ok(Ok(_))), code)
}

/// Starts the app probe from the portable package `Probe.mika` in the
/// home folder, which makes it the portable app `org.example.portable`
/// with `Probe/` as its folder, and checks that the probe passed its
/// checks there and that the note it wrote is in that folder; that the
/// same package by another name is refused; and that the probe started
/// from a build folder, `build/`, runs as the build's app
/// `org.example.build` there (MIKSPEC-0101.R46, MIKSPEC-0101.R47).
async fn run_portable(
    shell: &Shell<'_>,
    probe: &[u8],
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    use mikros_rt::interfaces::mikros::base::Status;
    const PORTABLE: &str = "org.example.portable";
    const BUILD: &str = "org.example.build";
    let home = format!("/users/{}", shell.name);
    let manifest = format!(
        "id: {PORTABLE}\nkind: app\nversion: 1.0.0\nname: Probe\ndescription: Checks itself.\n\
         executable: bin/probe\nportable: true\n"
    );
    pack(
        shell,
        "Probe.mika",
        &manifest,
        &[("bin/probe", probe.to_vec())],
        76,
        reactor,
    )
    .await?;
    let package = format!("{home}/Probe.mika");
    let folder = format!("{home}/Probe");
    let args = [package.as_str(), "portable", &folder, &package, PORTABLE];
    run_as(shell, request(&package, &args).ok_or(76)?, PORTABLE, 76).await?;
    let note = read_all(shell.home, "Probe/note", reactor).await;
    check(
        note.as_deref() == Ok(b"from the portable app".as_slice()),
        77,
    )?;
    remove(shell.home, "Probe", &["note"], 77, reactor).await?;
    // A package starts only by a name that ends in `.mika`.
    let renamed = shell
        .home
        .rename(
            "Probe.mika".into(),
            token(shell.home).await.ok_or(78)?,
            "probe.pkg".into(),
        )
        .await;
    check(matches!(renamed, Ok(Ok(_))), 78)?;
    let other = format!("{home}/probe.pkg");
    let spawned = shell
        .launcher
        .spawn(request(&other, &[&other]).ok_or(78)?)
        .await;
    check(matches!(spawned, Ok(Err(Status::InvalidArgs))), 78)?;
    let unlinked = shell.home.unlink("probe.pkg".into(), false).await;
    check(matches!(unlinked, Ok(Ok(_))), 78)?;
    // A build: a folder with a manifest and the program.
    let (_, server) = endpoints::<Node>().map_err(|_| 79)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let made = shell
        .home
        .open("build".into(), flags, 0o755, rw, server)
        .await;
    check(matches!(made, Ok(Ok(_))), 79)?;
    let manifest = format!("id: {BUILD}\n");
    write_file(
        shell.home,
        "build/manifest.yaml",
        manifest.as_bytes(),
        0o644,
        79,
        reactor,
    )
    .await?;
    write_file(shell.home, "build/probe", probe, 0o755, 79, reactor).await?;
    let program = format!("{home}/build/probe");
    let folder = format!("{home}/build");
    let args = [program.as_str(), "build", &folder, &program, BUILD];
    run_as(shell, request(&program, &args).ok_or(80)?, BUILD, 80).await?;
    let files = ["note", "manifest.yaml", "probe"];
    remove(shell.home, "build", &files, 80, reactor).await
}

/// A token of `dir`, for renaming in it.
async fn token(dir: &NodeAsyncClient) -> Option<OwnedHandle> {
    dir.get_token().await.ok()?.ok().map(|r| r.token)
}

/// Where the console image keeps the program and the libraries that
/// `run_libraries` packs, below `/system`.
const PACKAGE_C: &str = "share/package-c";

/// Installs for the user the library package `org.example.greet` with
/// `greet.dlib`, named `org.example.greet:greet`, and the app
/// `org.example.libs` whose program needs it, `lib/librunpath.dlib`
/// through its `DT_RUNPATH` of `$ORIGIN/../lib`, and
/// `libraries/libown.dlib`, and checks that the program finds all three
/// (MIKSPEC-0101.R16, MIKSPEC-0035.R21, MIKSPEC-0034.R46,
/// MIKSPEC-0050.R50). Removes both again.
async fn run_libraries(
    shell: &Shell<'_>,
    installer: &mikros_rt::interfaces::mikros::packages::InstallerAsyncClient,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    use mikros_rt::interfaces::mikros::process::{SpawnRequest, StatusKind, WaitOptions};
    const LIBRARY: &str = "org.example.greet";
    const APP: &str = "org.example.libs";
    let file = |name: &str| format!("{PACKAGE_C}/{name}");
    let program = read_all(shell.system, &file("package-user"), reactor).await?;
    let runpath = read_all(shell.system, &file("librunpath.dlib"), reactor).await?;
    let own = read_all(shell.system, &file("libown.dlib"), reactor).await?;
    let greet = read_all(shell.system, &file("greet.dlib"), reactor).await?;
    let library = format!(
        "id: {LIBRARY}\nkind: library\nversion: 1.0.0\nname: Greet\n\
         description: Greets.\ninstall: local\n"
    );
    install(
        shell,
        installer,
        "greet.mika",
        &library,
        &[("greet.dlib", greet)],
        66,
        reactor,
    )
    .await?;
    let app = format!(
        "id: {APP}\nkind: app\nversion: 1.0.0\nname: Libraries\n\
         description: Uses libraries.\nexecutable: bin/package-user\ninstall: local\n"
    );
    let files = [
        ("bin/package-user", program),
        ("lib/librunpath.dlib", runpath),
        ("libraries/libown.dlib", own),
    ];
    install(shell, installer, "libs.mika", &app, &files, 67, reactor).await?;
    let path = format!("/users/{}/apps/{APP}/package/bin/package-user", shell.name);
    let request = SpawnRequest {
        path: path.clone(),
        arguments: arguments(&[&path]).ok_or(68)?,
        handles: Vec::new(),
        app_id: None,
        user: CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    let spawned = shell.launcher.spawn(request).await;
    let pid = spawned.ok().and_then(|r| r.ok()).ok_or(68)?.pid;
    let waited = shell
        .launcher
        .wait(pid as i32, WaitOptions(0), now() + 60 * SECOND)
        .await;
    let status = waited.ok().and_then(|r| r.ok()).ok_or(68)?.status;
    check(status.kind == StatusKind::Exited, 68)?;
    // The program's own number for the check that failed.
    if status.exit_code != 0 {
        return Err(status.exit_code);
    }
    for id in [APP, LIBRARY] {
        let removed = installer.remove(id.into(), false, false).await;
        check(matches!(removed, Ok(Ok(_))), 69)?;
    }
    Ok(())
}

/// Packs `files`, each executable, with the manifest `manifest` and its
/// checksum into the file `name` of the user's home, installs it, and
/// removes the file again; fails with `code`.
async fn install(
    shell: &Shell<'_>,
    installer: &mikros_rt::interfaces::mikros::packages::InstallerAsyncClient,
    name: &str,
    manifest: &str,
    files: &[(&str, Vec<u8>)],
    code: i64,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<(), i64> {
    let file = pack(shell, name, manifest, files, code, reactor).await?;
    let (client, server) = endpoints::<Node>().map_err(|_| code)?;
    let cloned = file.clone(server, Rights::READ).await;
    check(matches!(cloned, Ok(Ok(_))), code)?;
    let installed = installer
        .install(client, false, vec![], false, false, vec![])
        .await;
    check(matches!(installed, Ok(Ok(_))), code)?;
    let unlinked = shell.home.unlink(name.into(), false).await;
    check(matches!(unlinked, Ok(Ok(_))), code)
}

/// Packs `files`, each executable, with the manifest `manifest` and its
/// checksum into the file `name` of the user's home, and returns it;
/// fails with `code`.
async fn pack(
    shell: &Shell<'_>,
    name: &str,
    manifest: &str,
    files: &[(&str, Vec<u8>)],
    code: i64,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<NodeAsyncClient, i64> {
    use mikros_mika::format::{Builder, MANIFEST, checksum_text};
    let mut builder = Builder::default();
    for (path, bytes) in files {
        if let Some((folder, _)) = path.rsplit_once('/') {
            builder.folder(folder);
        }
        builder.file(path, bytes.clone(), true);
    }
    let checksum = checksum_text(&builder.checksum(None));
    let manifest = format!("{manifest}checksum: {checksum}\n");
    builder.file(MANIFEST, manifest.into_bytes(), false);
    let bytes = builder.finish();
    let (file, server) = endpoints::<Node>().map_err(|_| code)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let opened = shell.home.open(name.into(), flags, 0o644, rw, server).await;
    check(matches!(opened, Ok(Ok(_))), code)?;
    let file = NodeAsyncClient::new(file, reactor.clone());
    for (at, chunk) in bytes.chunks(32768).enumerate() {
        let written = file.write((at * 32768) as u64, chunk.to_vec()).await;
        check(matches!(written, Ok(Ok(_))), code)?;
    }
    Ok(file)
}

/// A memory object of `args` with no environment, as `Spawn` takes them.
fn arguments(args: &[&str]) -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(args.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for arg in args {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    // SAFETY: the pointers describe `mem` and `bytes`.
    let made = unsafe { sys::mem_create(bytes.len().next_multiple_of(4096) as u64, 0, &mut mem) };
    if made != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer describes `bytes`.
    let written = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    (written == mikros_rt::OK).then_some(mem)
}

/// Writes a package of the app `id` in `version`, whose program says
/// hello with its version, to the file `name` of `home`, with `probe`
/// as its `bin/probe` when given.
async fn write_package(
    home: &NodeAsyncClient,
    name: &str,
    id: &str,
    version: &str,
    probe: Option<&[u8]>,
    reactor: &mikros_rt::exec::Reactor,
) -> Result<NodeAsyncClient, i64> {
    use mikros_mika::format::{Builder, MANIFEST, checksum_text};
    let mut builder = Builder::default();
    builder.folder("bin");
    builder.file("bin/hello", format!("hello {version}").into_bytes(), true);
    if let Some(probe) = probe {
        builder.file("bin/probe", probe.to_vec(), true);
    }
    let checksum = checksum_text(&builder.checksum(None));
    // The probe is also the package's command.
    let bins = match probe {
        Some(_) => "bins:\n  - name: hello-probe\n    path: bin/probe\n",
        None => "",
    };
    let manifest = format!(
        "id: {id}\nkind: app\nversion: {version}\nname: Hello\ndescription: Says hello.\n\
         executable: bin/hello\ninstall: local\nchecksum: {checksum}\n{bins}"
    );
    builder.file(MANIFEST, manifest.into_bytes(), false);
    let bytes = builder.finish();
    let (file, server) = endpoints::<Node>().map_err(|_| 22)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let opened = home.open(name.into(), flags, 0o644, rw, server).await;
    check(matches!(opened, Ok(Ok(_))), 22)?;
    let file = NodeAsyncClient::new(file, reactor.clone());
    for (at, chunk) in bytes.chunks(32768).enumerate() {
        let written = file.write((at * 32768) as u64, chunk.to_vec()).await;
        check(matches!(written, Ok(Ok(_))), 22)?;
    }
    Ok(file)
}

/// Fails with `code` unless `ok`.
fn check(ok: bool, code: i64) -> Result<(), i64> {
    if ok { Ok(()) } else { Err(code) }
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return 1;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(managed) = received else {
        return 1;
    };
    let header = managed.header;
    let (pid, user) = (header.pid, header.user);
    let app_ok = header.app_id == "system.cli";
    let folder = header.working_directory;
    let environment = strings(&header.arguments).map(|(_, e)| e);
    let mut home = None;
    let mut home_dir = None;
    let mut libraries = None;
    let mut system = None;
    let mut apps = None;
    let mut commands = None;
    for entry in managed.namespace {
        match entry.path.as_str() {
            "/libraries" => libraries = Some(entry.directory),
            "/system" => system = Some(entry.directory),
            "/system/commands" => commands = Some(entry.directory),
            path if path.starts_with("/users/") && path.ends_with("/apps") => {
                apps = Some(entry.directory);
            }
            path if path.starts_with("/users/") => {
                home = Some(entry.path);
                home_dir = Some(entry.directory);
            }
            _ => {}
        }
    }
    let token = managed
        .handles
        .into_iter()
        .find(|h| h.role == HandleRole::Stdin)
        .map(|h| h.handle);
    let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    let result: Result<(), i64> = executor.block_on(async {
        check(user != 0, 2)?;
        check(app_ok, 3)?;
        let info = launcher
            .get_info(pid)
            .await
            .ok()
            .and_then(|r| r.ok())
            .ok_or(4)?
            .info;
        check(info.login_session == pid, 4)?;
        check(info.session == pid && info.process_group == pid, 14)?;
        let environment = environment.ok_or(5)?;
        let name = environment
            .get(1)
            .and_then(|e| e.strip_prefix("USER="))
            .map(String::from)
            .ok_or(5)?;
        check(environment == expected(&name), 5)?;
        check(home.as_deref() == Some(&*format!("/users/{name}")), 12)?;
        check(folder == format!("/users/{name}"), 15)?;
        // Only bob's home in tests/console.img holds a Libraries folder
        // with a README; an account that setup made has an empty one.
        let libraries = NodeAsyncClient::new(libraries.ok_or(13)?, reactor.clone());
        let home = NodeAsyncClient::new(home_dir.ok_or(16)?, reactor.clone());
        acls(&home, user, &reactor).await?;
        let system = NodeAsyncClient::new(system.ok_or(33)?, reactor.clone());
        let apps = NodeAsyncClient::new(apps.ok_or(33)?, reactor.clone());
        let commands = NodeAsyncClient::new(commands.ok_or(70)?, reactor.clone());
        let shell = Shell {
            home: &home,
            apps: &apps,
            commands: &commands,
            system: &system,
            directory: &directory,
            launcher: &launcher,
            name: &name,
            user,
        };
        packages(&shell, &reactor).await?;
        if name == "bob" {
            let (_readme, server) = endpoints::<Node>().map_err(|_| 13)?;
            let opened = libraries
                .open("README".into(), OpenFlags(0), 0, Rights::READ, server)
                .await;
            check(matches!(opened, Ok(Ok(_))), 13)?;
            // The image made the README user 0's; the accounts service
            // made everything in bob's home that user 0 owned bob's.
            let (readme, server) = endpoints::<Node>().map_err(|_| 16)?;
            let opened = home
                .open(
                    "libraries/README".into(),
                    OpenFlags(0),
                    0,
                    Rights::READ,
                    server,
                )
                .await;
            check(matches!(opened, Ok(Ok(_))), 16)?;
            let readme = NodeAsyncClient::new(readme, reactor.clone());
            let owner = readme.get_attributes().await.ok().and_then(|r| r.ok());
            check(owner.is_some_and(|a| a.attributes.owner == user), 16)?;
        }
        let token = token.ok_or(6)?;
        let (client, server) = endpoints::<Terminal>().map_err(|_| 6)?;
        let connected = directory
            .connect(TERMINAL.into(), None, server.into_handle())
            .await;
        check(matches!(connected, Ok(Ok(_))), 6)?;
        let terminal = TerminalAsyncClient::new(client, reactor.clone());
        let t = || duplicate(&token).ok_or(7);
        let mut attributes = terminal
            .get_attributes(t()?)
            .await
            .ok()
            .and_then(|r| r.ok())
            .ok_or(7)?
            .attributes;
        let canonical = attributes.local_flags;
        attributes.local_flags &= !(ICANON | ECHO);
        let raw = terminal
            .set_attributes(t()?, copy(&attributes), When::Now)
            .await;
        check(matches!(raw, Ok(Ok(_))), 7)?;
        let written = terminal
            .write(t()?, b"Welcome.\r\n\x1b[6n".to_vec(), FOREVER)
            .await;
        check(matches!(written, Ok(Ok(_))), 8)?;
        let mut answer = Vec::new();
        let deadline = now() + 10 * SECOND;
        while !answer.ends_with(b"R") {
            match terminal.read(t()?, 32, deadline).await {
                Ok(Ok(r)) => answer.extend(r.data),
                _ => return Err(9),
            }
        }
        check(answer == b"\x1b[2;1R", 9)?;
        attributes.local_flags = canonical;
        let back = terminal.set_attributes(t()?, attributes, When::Now).await;
        check(matches!(back, Ok(Ok(_))), 10)?;
        let prompt = terminal.write(t()?, b"$ ".to_vec(), FOREVER).await;
        check(matches!(prompt, Ok(Ok(_))), 10)?;
        let line = terminal.read(t()?, 64, now() + 120 * SECOND).await;
        check(matches!(line, Ok(Ok(r)) if r.data == b"ls\n"), 11)?;
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(code) => code,
    }
}
