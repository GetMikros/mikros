//! The session manager. It starts the text console with a `Seat`
//! channel, through which it gives the console the display and the input
//! devices, and a `TextLogin` channel, through which the console runs
//! first-boot setup and logs users in, checked with the accounts
//! service. A login starts the user's shell in a new login session on the
//! console's terminal; once the shell exits, the session's processes are
//! ended and the console shows its login prompt again.

#![no_std]
#![no_main]

extern crate alloc;

mod login;
mod seat;
mod spawn;

use alloc::rc::Rc;
use alloc::vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{
    Accounts, AccountsAsyncClient, Seat, SeatAsyncClient, TextLogin,
};
use mikros_rt::{managed, sys};

mikros_rt::raw_main!(main);

const CONSOLE: &str = "/usr/bin/console";
const CONSOLE_APP: &str = "system.console";
const ACCOUNTS: &str = "mikros.session.Accounts";
/// The `USER` startup handle IDs of the console's channels.
const TEXT_LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
/// The `USER` startup handle ID of a kernel log handle.
const LOG_ID: u32 = 2;
const CALLER_USER: u32 = u32::MAX;
pub const SECOND: i64 = 1_000_000_000;

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return 1;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(managed) = received else {
        return 1;
    };
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
        let (Ok((seat, seat_server)), Ok((login_client, login_server))) =
            (endpoints::<Seat>(), endpoints::<TextLogin>())
        else {
            return;
        };
        let Some(arguments) = spawn::block(&[CONSOLE], &[]) else {
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
        };
        match launcher.spawn(request).await {
            Ok(Ok(spawned)) => {
                mikros_rt::log!(
                    Info,
                    "sessionmgr: started the console as pid {}",
                    spawned.pid
                )
            }
            other => {
                mikros_rt::log!(Error, "sessionmgr: starting the console failed: {other:?}");
                return;
            }
        }
        let seat = SeatAsyncClient::new(seat, reactor.clone());
        let (d, r) = (directory.clone(), reactor.clone());
        spawner.spawn(async move { seat::serve(&d, &r, seat).await });
        let login = ServerEnd::new(login_server.into_handle());
        login::serve(login, accounts, launcher, reactor, spawner).await;
        mikros_rt::log!(Error, "sessionmgr: the console closed TextLogin");
    });
    0
}
