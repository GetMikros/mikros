//! The user's shell of `tests/console.img`, installed there as
//! /system/bin/msh, which the session manager starts once a
//! user logged in on the text console. It checks that it runs as a user
//! other than 0 with the app ID `system.cli`, leads a login session and
//! a session and process group of its own, starts in the user's home,
//! and has exactly the environment of that user's session, with the
//! user's home and a `/libraries` in its namespace, which for bob holds the README of his
//! own Libraries folder.
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
        "PATH=/system/bin".into(),
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
    for entry in managed.namespace {
        match entry.path.as_str() {
            "/libraries" => libraries = Some(entry.directory),
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
        if name == "bob" {
            let (_readme, server) = endpoints::<Node>().map_err(|_| 13)?;
            let opened = libraries
                .open("README".into(), OpenFlags(0), 0, Rights::READ, server)
                .await;
            check(matches!(opened, Ok(Ok(_))), 13)?;
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
