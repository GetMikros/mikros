//! The session manager. It starts the text console with a `Seat`
//! channel, through which it gives the console the display and the input
//! devices, and a `TextLogin` channel, through which the console runs
//! first-boot setup and logs users in, checked with the accounts
//! service. When a desktop is installed, the compositor in greeter mode
//! gets the display and the input devices instead, until it fails
//! (`greeter`). A login starts the user's shell in a new login session on the
//! console's terminal; once the shell exits, the session's processes are
//! ended and the console shows its login prompt again. Once the console
//! has started, the session manager marks the boot good through the boot
//! attempts service, without the login waiting for it.

#![no_std]
#![no_main]
#![forbid(unsafe_code)]

extern crate alloc;

mod authorize;
mod boot;
mod elevate;
mod greeter;
mod login;
mod seat;
mod spawn;
mod wait;

use alloc::rc::Rc;
use alloc::vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::process::{
    Elevations, ElevationsAsyncClient, HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::{
    DirectoryAsyncClient, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::interfaces::mikros::session::{
    Accounts, AccountsAsyncClient, Seat, SeatAsyncClient, TextLogin,
};
use mikros_rt::{managed, sys};

mikros_rt::raw_main!(main);

const CONSOLE: &str = "/system/bin/console";
const CONSOLE_APP: &str = "system.console";
const ACCOUNTS: &str = "mikros.session.Accounts";
/// The protocols that the session manager provides, and the app ID of
/// the permission broker, the one client of `AUTHORIZATION`.
const AUTHORIZATION: &str = "mikros.session.Authorization";
const BROKER_APP: &str = "system.permbroker";
const ELEVATION: &str = "mikros.session.Elevation";
/// The `USER` startup handle IDs of the console's channels.
const TEXT_LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
/// The `USER` startup handle ID of a kernel log handle.
const LOG_ID: u32 = 2;
const CALLER_USER: u32 = u32::MAX;
pub const SECOND: i64 = 1_000_000_000;

pub fn now() -> i64 {
    mikros_rt::objects::now()
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return 1;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return 1;
    };
    let provider = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
        .map(|at| managed.handles.remove(at).handle);
    // A kernel log handle, which a test image gives, is used; otherwise
    // messages go to the log service.
    match managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID)
    {
        Some(log) => mikros_rt::log::set_handle(log.handle.raw()),
        None => {
            mikros_rt::log::connect(&managed.header.directory);
        }
    }
    // The directories of `/system`, where the compositor's program
    // manifest is, and of `/system/config`, where the session settings are.
    let system = managed::take_entry(&mut managed.namespace, "/system")
        .map(|(dir, _)| NodeAsyncClient::new(dir, reactor.clone()));
    let config = managed::take_entry(&mut managed.namespace, "/system/config")
        .map(|(dir, _)| NodeAsyncClient::new(dir, reactor.clone()));
    let header = managed.header;
    let directory = Rc::new(DirectoryAsyncClient::new(header.directory, reactor.clone()));
    let launcher = Rc::new(LauncherAsyncClient::new(header.launcher, reactor.clone()));
    executor.block_on(async move {
        let Ok((accounts, server)) = endpoints::<Accounts>() else {
            return;
        };
        let connected = directory
            .connect(ACCOUNTS.into(), None, server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            mikros_rt::log!(Error, "sessionmgr: no accounts service: {connected:?}");
            return;
        }
        let accounts = AccountsAsyncClient::new(accounts, reactor.clone());
        let Ok((authorizing, server)) = endpoints::<Accounts>() else {
            return;
        };
        let connected = directory
            .connect(ACCOUNTS.into(), None, server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            return;
        }
        let requests = Rc::new(authorize::Requests::new(
            Rc::new(AccountsAsyncClient::new(authorizing, reactor.clone())),
            launcher.clone(),
            reactor.clone(),
        ));
        // The process manager's record of elevated processes.
        if let Ok((elevations, server)) = endpoints::<Elevations>() {
            match launcher.open_elevations(server).await {
                Ok(Ok(_)) => {
                    let elevations = ElevationsAsyncClient::new(elevations, reactor.clone());
                    *requests.elevations.borrow_mut() = Some(Rc::new(elevations));
                }
                other => mikros_rt::log!(Error, "sessionmgr: no Elevations: {other:?}"),
            }
        }
        // The broker asks users through us alone (MIKSPEC-0057.R30).
        {
            let (directory, requests, s) = (directory.clone(), requests.clone(), spawner.clone());
            spawner
                .spawn(async move { authorize::register_prompter(&directory, requests, s).await });
        }
        if let Some(provider) = provider {
            let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
            let (requests, spawner) = (requests.clone(), spawner.clone());
            spawner.clone().spawn(async move {
                while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
                    // Other protocols and named instances are not ours;
                    // dropping the server end closes the connection.
                    if c.instance.is_some() {
                        continue;
                    }
                    if c.protocol == AUTHORIZATION && c.client.app_id == BROKER_APP {
                        let end = ServerEnd::new(c.server);
                        let serving = authorize::serve(requests.clone(), end, spawner.clone());
                        spawner.spawn(serving);
                    } else if c.protocol == ELEVATION {
                        let end = ServerEnd::new(c.server);
                        let client = c.client;
                        let serving =
                            elevate::serve(requests.clone(), end, client, spawner.clone());
                        spawner.spawn(serving);
                    }
                }
            });
        }
        let (Ok((seat, seat_server)), Ok((login_client, login_server))) =
            (endpoints::<Seat>(), endpoints::<TextLogin>())
        else {
            return;
        };
        // With a desktop installed, the console's screens follow the
        // desktop's.
        let desktop = match (&system, &config) {
            (Some(system), Some(config)) => greeter::installed(system, config, &reactor).await,
            _ => false,
        };
        let console_arguments: &[&str] = match desktop {
            true => &[CONSOLE, "--desktop"],
            false => &[CONSOLE],
        };
        let Some(arguments) = spawn::block(console_arguments, &[]) else {
            return;
        };
        let request = SpawnRequest {
            path: CONSOLE.into(),
            arguments,
            handles: vec![
                StartupHandle {
                    role: HandleRole::User,
                    id: TEXT_LOGIN_ID,
                    handle: login_client.into_handle(),
                },
                StartupHandle {
                    role: HandleRole::User,
                    id: SEAT_ID,
                    handle: seat_server.into_handle(),
                },
            ],
            app_id: Some(CONSOLE_APP.into()),
            user: CALLER_USER,
            login_session: false,
            new_session: false,
            working_directory: None,
            umask: u32::MAX,
        };
        match launcher.spawn(request).await {
            Ok(Ok(spawned)) => {
                mikros_rt::log!(
                    Info,
                    "sessionmgr: started the console as pid {}",
                    spawned.pid
                );
                // The boot counts as good once the console can show the
                // login prompt or run setup; the login does not wait.
                let (d, r) = (directory.clone(), reactor.clone());
                spawner.spawn(async move { boot::mark_good(&d, &r).await });
            }
            other => {
                mikros_rt::log!(Error, "sessionmgr: starting the console failed: {other:?}");
                return;
            }
        }
        let seats = seat::Seats::new(
            directory.clone(),
            reactor.clone(),
            desktop,
            SeatAsyncClient::new(seat, reactor.clone()),
        );
        spawner.spawn(seats.clone().switches(seats.console()));
        spawner.spawn(seats.clone().keep_giving());
        let greeting = match desktop {
            true => connect_accounts(&directory, &reactor).await,
            false => None,
        };
        let (d, r, l, s) = (
            directory.clone(),
            reactor.clone(),
            launcher.clone(),
            spawner.clone(),
        );
        spawner.spawn(async move {
            // The compositor in greeter mode owns the seat while it runs,
            // and the console once the compositor failed.
            if let Some(accounts) = greeting {
                greeter::run(d, l, Rc::new(accounts), seats, r, s).await;
                mikros_rt::log!(Error, "sessionmgr: the text console's login takes over");
            }
        });
        let login = ServerEnd::new(login_server.into_handle());
        login::serve(login, accounts, launcher, requests, reactor, spawner).await;
        mikros_rt::log!(Error, "sessionmgr: the console closed TextLogin");
    });
    0
}

/// A new connection to the accounts service.
async fn connect_accounts(
    directory: &DirectoryAsyncClient,
    reactor: &mikros_rt::exec::Reactor,
) -> Option<AccountsAsyncClient> {
    let (accounts, server) = endpoints::<Accounts>().ok()?;
    let connected = directory
        .connect(ACCOUNTS.into(), None, server.into_handle())
        .await;
    matches!(connected, Ok(Ok(_))).then(|| AccountsAsyncClient::new(accounts, reactor.clone()))
}
