//! The time service: a system service that keeps UTC as the monotonic
//! clock plus an offset, in a time page that every process may map, and
//! provides `mikros.time.Clock`.
//!
//! At start, UTC at monotonic time 0 is the boot time of the platform
//! information that the `platform_info` boot handle holds, when it says
//! that time is valid, and 0 otherwise. The page is made before the
//! first connection is served, so that no client gets it unset. Once the
//! root file system is published, the system time zone and whether the
//! real-time clock keeps local time come from there (see `root`). Once a
//! real-time clock is published, UTC comes from it, unless `SetUtc` set
//! the time first, and setting the time writes it (see `rtc`).

#![no_std]
#![no_main]

extern crate alloc;

mod clock;
mod page;
mod root;
mod rtc;
mod watch;

use alloc::rc::Rc;
use alloc::vec::Vec;
use clock::Time;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    DirectoryAsyncClient, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use page::Page;

mikros_rt::raw_main!(main);

const CLOCK: &str = "mikros.time.Clock";
const BROKER: &str = "mikros.permissions.Broker";
/// The `id` of the `platform_info` boot handle among the `USER` startup
/// handles.
const PLATFORM_INFO_ID: u32 = 5;
/// The platform information's flags, the flag that says its boot time is
/// valid, and the boot time, in seconds since 1970-01-01T00:00:00.
const FLAGS: u64 = 0x04;
const BOOT_TIME_VALID: u32 = 1 << 1;
const BOOT_TIME: u64 = 0x60;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    // The service has no log handle: its messages go to the log service.
    mikros_rt::log::connect(&managed.header.directory);
    let Some(provider) = take(&mut managed.handles, |h| h.role == HandleRole::Provider) else {
        return PANIC_EXIT_CODE;
    };
    let info = take(&mut managed.handles, |h| {
        h.role == HandleRole::User && h.id == PLATFORM_INFO_ID
    });
    let boot = info.as_ref().and_then(|info| boot_time(info.raw()));
    let offset = match boot.and_then(|seconds| seconds.checked_mul(clock::NANOS_PER_SECOND)) {
        Some(offset) => offset,
        None => {
            mikros_rt::log!(
                Warning,
                "time: the boot time is not known, so UTC starts at 0 at boot"
            );
            0
        }
    };
    let process = managed.header.process.raw();
    let Some(page) = Page::new(process, offset) else {
        return PANIC_EXIT_CODE;
    };
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    let broker = executor.block_on(async {
        let (client, server) = endpoints::<Broker>().ok()?;
        let connected = directory
            .connect(BROKER.into(), None, server.into_handle())
            .await;
        matches!(connected, Ok(Ok(_))).then(|| BrokerAsyncClient::new(client, reactor.clone()))
    });
    let time = Rc::new(Time::new(page, broker, boot));
    let task = watch::watch(time.clone(), directory, reactor.clone(), spawner.clone());
    spawner.spawn(task);
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.instance.is_none() && c.protocol == CLOCK {
                let end = ServerEnd::new(c.server);
                spawner.spawn(clock::serve(time.clone(), end, reactor.clone(), c.client));
            }
        }
    });
    PANIC_EXIT_CODE
}

/// The boot time of the platform information `info`, in seconds since
/// 1970-01-01T00:00:00, when it says that time is valid.
fn boot_time(info: sys::Handle) -> Option<i64> {
    let read = |at: u64, buf: &mut [u8]| {
        // SAFETY: the pointer and length describe `buf`.
        let status = unsafe { sys::mem_read(info, at, buf.as_mut_ptr(), buf.len() as u64) };
        (status == mikros_rt::OK).then_some(())
    };
    let mut flags = [0u8; 4];
    read(FLAGS, &mut flags)?;
    if u32::from_le_bytes(flags) & BOOT_TIME_VALID == 0 {
        return None;
    }
    let mut seconds = [0u8; 8];
    read(BOOT_TIME, &mut seconds)?;
    Some(i64::from_le_bytes(seconds))
}

/// Takes the first startup handle that `wanted` picks out of `handles`.
fn take(
    handles: &mut Vec<StartupHandle>,
    wanted: impl Fn(&StartupHandle) -> bool,
) -> Option<OwnedHandle> {
    let at = handles.iter().position(wanted)?;
    Some(handles.remove(at).handle)
}
