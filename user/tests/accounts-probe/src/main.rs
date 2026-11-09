//! A program that the accounts test image runs as three services, which
//! it tells apart by their app IDs:
//!
//! - `accounts-probe` checks what the accounts service lists,
//!   authenticates and changes, as a client that holds both of its
//!   permissions and that the stub authorizes.
//! - `accounts-guest` checks that a client without `accounts.authenticate`,
//!   and that the stub turns down, can list the users and nothing more.
//! - `authorization-stub` serves `mikros.session.Authorization`, allowing
//!   `accounts.manage` for the probe only, with user 1000 as the
//!   administrator.
//! - `accounts-thief` and `accounts-sneak` ask for the accounts service's
//!   data in their manifests, so they never start. The probe connects to
//!   them once its checks passed, for the service manager to try.
//!
//! The image's users file holds `ann`, an administrator, and `bob` and
//! `cy`, standard users, each with the password `<name>-password`. The
//! probe and the guest exit with 0 when every check passed, or with the
//! number of the first that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{self, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{
    DirectoryClient, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::interfaces::mikros::session::{
    Accounts, AccountsAsyncClient, AccountsClient, Authorization, AuthorizationAsyncServer,
    AuthorizationAuthorizeResponse, AuthorizationRequest, UserKind,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const ACCOUNTS: &str = "mikros.session.Accounts";
const AUTHORIZATION: &str = "mikros.session.Authorization";
const PROBE: &str = "system.accounts-probe";
const GUEST: &str = "system.accounts-guest";
const STUB: &str = "system.authorization-stub";
/// The protocols of the services that never start.
const THIEVES: [&str; 2] = ["test.accounts.Thief", "test.accounts.Sneak"];
const SECOND: i64 = 1_000_000_000;

type Reply<T> = Result<Result<T, Status>, __midl::Error>;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let header = managed.header;
    if header.app_id == STUB {
        let Some(at) = managed
            .handles
            .iter()
            .position(|h| h.role == HandleRole::Provider)
        else {
            return PANIC_EXIT_CODE;
        };
        let provider = managed.handles.remove(at).handle;
        let spawner = executor.spawner().clone();
        let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
        executor.block_on(async {
            while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
                if c.protocol == AUTHORIZATION && c.instance.is_none() {
                    spawner.spawn(authorize(ServerEnd::new(c.server), reactor.clone()));
                }
            }
        });
        return PANIC_EXIT_CODE;
    }
    let directory = DirectoryClient::new(header.directory);
    let Some(accounts) = connect(&directory) else {
        return 1;
    };
    let checked = match header.app_id.as_str() {
        PROBE => probe(&accounts).and_then(|()| race(&mut executor, &directory)),
        GUEST => guest(&accounts),
        _ => Err(2),
    };
    match checked {
        Ok(()) if header.app_id == PROBE => {
            for protocol in THIEVES {
                if let Ok((_, server)) = endpoints::<Accounts>() {
                    let _ = directory.connect(protocol.into(), None, server.into_handle(), FOREVER);
                }
            }
            0
        }
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

/// Serves `Authorize` on `end`: `accounts.manage` for the probe, by user
/// 1000, and nothing else.
async fn authorize(end: ServerEnd<Authorization>, reactor: Reactor) {
    let server = AuthorizationAsyncServer::new(end, reactor);
    while let Ok(AuthorizationRequest::Authorize(q, r)) = server.next().await {
        let allowed = q.client.app_id == PROBE && q.action == "accounts.manage";
        let admin = if allowed { 1000 } else { 0 };
        let _ = r.reply(
            server.server(),
            Ok(AuthorizationAuthorizeResponse { allowed, admin }),
        );
    }
}

/// Connects to the accounts service.
fn connect(directory: &DirectoryClient) -> Option<AccountsClient> {
    let (client, server) = endpoints::<Accounts>().ok()?;
    let connected = directory.connect(ACCOUNTS.into(), None, server.into_handle(), FOREVER);
    matches!(connected, Ok(Ok(_))).then(|| AccountsClient::new(client))
}

/// The users as `ListUsers` pages them from `after` on: ID, name, full
/// name and kind.
fn list(accounts: &AccountsClient, after: u32) -> Option<Vec<(u32, String, String, UserKind)>> {
    let listed = accounts.list_users(after, FOREVER).ok()?;
    Some(
        listed
            .users
            .into_iter()
            .map(|u| (u.id, u.name, u.full_name, u.kind))
            .collect(),
    )
}

/// The IDs and kinds of the users.
fn kinds(accounts: &AccountsClient) -> Option<Vec<(u32, UserKind)>> {
    let users = list(accounts, 0)?;
    Some(
        users
            .into_iter()
            .map(|(id, _, _, kind)| (id, kind))
            .collect(),
    )
}

fn ok<T>(reply: Reply<T>) -> bool {
    matches!(reply, Ok(Ok(_)))
}

fn fails<T>(reply: Reply<T>, status: Status) -> bool {
    matches!(reply, Ok(Err(s)) if s == status)
}

fn expect(condition: bool, code: i64) -> Result<(), i64> {
    if condition { Ok(()) } else { Err(code) }
}

fn authenticate(accounts: &AccountsClient, name: &str, password: &str) -> Reply<u32> {
    accounts
        .authenticate(name.into(), password.into(), FOREVER)
        .map(|r| r.map(|r| r.user))
}

fn probe(a: &AccountsClient) -> Result<(), i64> {
    use UserKind::{Admin, Standard};
    let s = String::from;
    // Listing, in pages from an ID on.
    let all = alloc::vec![
        (1000, s("ann"), s("Ann"), Admin),
        (1001, s("bob"), s("Bob"), Standard),
        (1002, s("cy"), s("Cy"), Standard),
    ];
    expect(list(a, 0) == Some(all.clone()), 10)?;
    expect(list(a, 1000) == Some(all[1..].to_vec()), 11)?;
    expect(list(a, 1002) == Some(Vec::new()), 12)?;
    // Authenticating.
    expect(
        matches!(authenticate(a, "ann", "ann-password"), Ok(Ok(1000))),
        13,
    )?;
    expect(
        fails(authenticate(a, "ann", "bob-password"), Status::AccessDenied),
        14,
    )?;
    expect(
        fails(authenticate(a, "nobody", "x"), Status::AccessDenied),
        15,
    )?;
    // Creating: checked, but no home folder can be made on tmpfs.
    let create = |name: &str, full: &str, password: &str| {
        a.create_user(name.into(), full.into(), Standard, password.into(), FOREVER)
    };
    expect(fails(create("Dan", "Dan", "pw"), Status::InvalidArgs), 16)?;
    expect(fails(create("dan", "", "pw"), Status::InvalidArgs), 17)?;
    expect(fails(create("dan", "Dan", ""), Status::InvalidArgs), 18)?;
    expect(fails(create("ann", "Ann", "pw"), Status::AlreadyExists), 19)?;
    expect(fails(create("dan", "Dan", "pw"), Status::NotSupported), 20)?;
    // Full names.
    expect(ok(a.set_full_name(1001, "Bob Example".into(), FOREVER)), 21)?;
    expect(
        list(a, 1000).and_then(|l| l.first().cloned()).map(|u| u.2) == Some(s("Bob Example")),
        22,
    )?;
    expect(
        fails(
            a.set_full_name(1001, "".into(), FOREVER),
            Status::InvalidArgs,
        ),
        23,
    )?;
    expect(
        fails(a.set_full_name(5000, "X".into(), FOREVER), Status::NotFound),
        24,
    )?;
    // Kinds, which always leave an administrator.
    expect(
        fails(a.set_kind(1000, Standard, FOREVER), Status::BadState),
        25,
    )?;
    expect(ok(a.set_kind(1001, Admin, FOREVER)), 26)?;
    expect(ok(a.set_kind(1000, Standard, FOREVER)), 27)?;
    let changed = alloc::vec![(1000, Standard), (1001, Admin), (1002, Standard)];
    expect(kinds(a) == Some(changed), 28)?;
    expect(
        fails(a.set_kind(5000, Admin, FOREVER), Status::NotFound),
        29,
    )?;
    // Another user's password, without the old one.
    expect(
        ok(a.set_password(1002, "".into(), "cy-new".into(), FOREVER)),
        30,
    )?;
    expect(matches!(authenticate(a, "cy", "cy-new"), Ok(Ok(1002))), 31)?;
    expect(
        fails(authenticate(a, "cy", "cy-password"), Status::AccessDenied),
        32,
    )?;
    expect(
        fails(
            a.set_password(1002, "".into(), "".into(), FOREVER),
            Status::InvalidArgs,
        ),
        33,
    )?;
    // Deleting, which keeps the last administrator.
    expect(
        fails(a.delete_user(1002, true, FOREVER), Status::NotSupported),
        34,
    )?;
    expect(ok(a.delete_user(1002, false, FOREVER)), 35)?;
    expect(
        kinds(a) == Some(alloc::vec![(1000, Standard), (1001, Admin)]),
        36,
    )?;
    expect(
        fails(a.delete_user(1002, false, FOREVER), Status::NotFound),
        37,
    )?;
    expect(
        fails(a.delete_user(1001, false, FOREVER), Status::BadState),
        38,
    )?;
    // After five failures in a row for a name, the next attempt waits a
    // second. One for "nobody" failed above.
    for _ in 0..4 {
        expect(
            fails(authenticate(a, "nobody", "x"), Status::AccessDenied),
            39,
        )?;
    }
    let start = now();
    expect(
        fails(authenticate(a, "nobody", "x"), Status::AccessDenied),
        40,
    )?;
    expect(now() - start >= SECOND, 41)?;
    Ok(())
}

/// An attempt for `ann`, made while one for `nobody` waits for the delay
/// of its failures in a row, gets its answer first: a delay holds up
/// only its own name.
fn race(executor: &mut Executor, directory: &DirectoryClient) -> Result<(), i64> {
    let reactor = executor.reactor().clone();
    let open = || {
        let (client, server) = endpoints::<Accounts>().ok()?;
        let connected = directory.connect(ACCOUNTS.into(), None, server.into_handle(), FOREVER);
        matches!(connected, Ok(Ok(_))).then(|| AccountsAsyncClient::new(client, reactor.clone()))
    };
    let (delayed, other) = (open().ok_or(42)?, open().ok_or(42)?);
    let (nobody, ann, order) = executor.block_on(async {
        let mut nobody_call = pin!(delayed.authenticate("nobody".into(), "x".into()));
        let mut ann_call = pin!(other.authenticate("ann".into(), "ann-password".into()));
        let (mut nobody, mut ann, mut order) = (None, None, Vec::new());
        poll_fn(|cx| {
            if nobody.is_none()
                && let Poll::Ready(r) = nobody_call.as_mut().poll(cx)
            {
                nobody = Some(r.map(|r| r.map(|r| r.user)));
                order.push("nobody");
            }
            if ann.is_none()
                && let Poll::Ready(r) = ann_call.as_mut().poll(cx)
            {
                ann = Some(r.map(|r| r.map(|r| r.user)));
                order.push("ann");
            }
            match nobody.is_some() && ann.is_some() {
                true => Poll::Ready(()),
                false => Poll::Pending,
            }
        })
        .await;
        (nobody, ann, order)
    });
    expect(matches!(ann, Some(Ok(Ok(1000)))), 43)?;
    expect(matches!(nobody, Some(Ok(Err(Status::AccessDenied)))), 44)?;
    expect(order == ["ann", "nobody"], 45)
}

fn guest(a: &AccountsClient) -> Result<(), i64> {
    let users = list(a, 0).ok_or(10)?;
    expect(
        users.first().map(|u| (u.0, u.1.as_str())) == Some((1000, "ann")),
        11,
    )?;
    // Without accounts.authenticate.
    expect(
        fails(authenticate(a, "ann", "ann-password"), Status::AccessDenied),
        12,
    )?;
    // The stub refuses every change, before any user is looked up.
    let denied = Status::AccessDenied;
    expect(
        fails(a.set_full_name(1000, "X".into(), FOREVER), denied),
        13,
    )?;
    expect(
        fails(a.set_kind(1001, UserKind::Admin, FOREVER), denied),
        14,
    )?;
    expect(
        fails(a.set_password(1001, "".into(), "x".into(), FOREVER), denied),
        15,
    )?;
    expect(fails(a.delete_user(1000, false, FOREVER), denied), 16)?;
    expect(fails(a.delete_user(5000, false, FOREVER), denied), 17)?;
    let create = a.create_user(
        "dan".into(),
        "Dan".into(),
        UserKind::Standard,
        "pw".into(),
        FOREVER,
    );
    expect(fails(create, denied), 18)
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
