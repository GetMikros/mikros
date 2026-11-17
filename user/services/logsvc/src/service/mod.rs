//! The log service: a system service that collects the records of the
//! kernel log and those that clients write through `mikros.log.Writer`
//! into one log, kept in memory, and lets clients that hold `admin.logs.read`
//! read it through `mikros.log.Reader`, and every client its own app's
//! records and its components' through `mikros.log.OwnReader`.
//!
//! It reads the kernel log with a `LogReader` on the `log_manage` boot
//! handle, from the oldest record the kernel still holds. A record that
//! a client writes takes its pid and app ID from the client's identity
//! as the directory gave it, and its message is also copied into the
//! kernel log, whose records from this process are not taken again.
//! The service manager adds the standard output and error of services
//! through `mikros.log.Sources`, which `system.log_sources.create` lets it use.
//!
//! Once the root file system and the EFI system partition are
//! published, the records also go to files on them (see `files`), or on
//! a live system, which the command line of the `platform_info` boot
//! handle says, to the temporary file system. On
//! `TERM`, which the service manager sends to stop it, the service writes
//! the last records to them and exits.

mod files;
mod kernel;
mod log;
mod own;
mod reader;
mod sources;
mod writer;
use crate::service::log::Log;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    DirectoryAsyncClient, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The protocols the service provides.
const WRITER: &str = "mikros.log.Writer";
const READER: &str = "mikros.log.Reader";
const OWN_READER: &str = "mikros.log.OwnReader";
const SAME_RIGHTS: u32 = 1 << 31;
const SOURCES: &str = "mikros.log.Sources";
/// The `id` of the `log_manage` boot handle among the `USER` startup
/// handles.
const LOG_MANAGE_ID: u32 = 2;
/// The `id` of the `platform_info` boot handle, and where its header
/// gives the command line.
const PLATFORM_INFO_ID: u32 = 5;
const CMDLINE_OFFSET: u64 = 0x48;
const CMDLINE_LEN: u64 = 0x4C;
const CMDLINE_MAX: usize = 4096;
/// The command-line word of live systems.
const LIVE: &str = "live";
/// The mask bit of `TERM`, signal 15.
const TERM: u64 = 1 << (15 - 1);
/// How often the service looks for a pending `TERM`, and for its files
/// to be written out once it came.
const POLL: i64 = 100_000_000;
/// The longest the service waits for its files to be written out.
const FINISH_LIMIT: i64 = 5_000_000_000;

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
    let Some(provider) = take(&mut managed.handles, |h| h.role == HandleRole::Provider) else {
        return PANIC_EXIT_CODE;
    };
    let kernel_log = take(&mut managed.handles, |h| {
        h.role == HandleRole::User && h.id == LOG_MANAGE_ID
    });
    let platform_info = take(&mut managed.handles, |h| {
        h.role == HandleRole::User && h.id == PLATFORM_INFO_ID
    });
    let live = platform_info
        .as_ref()
        .is_some_and(|info| command_line(info).split_whitespace().any(|w| w == LIVE));
    let log = Rc::new(Log::new(kernel_log.as_ref().map(|h| h.raw())));
    match &kernel_log {
        Some(handle) => {
            mikros_rt::log::set_handle(handle.raw());
            let own = koid(managed.header.process.raw());
            spawner.spawn(crate::service::kernel::read(
                log.clone(),
                reactor.clone(),
                handle.raw(),
                own,
            ));
        }
        None => mikros_rt::log!(Error, "log: there is no log_manage handle"),
    }
    // `TERM` stays pending for `stop` instead of ending the process.
    // SAFETY: the call takes no pointers but `old`, which is null.
    unsafe { sys::signal_set_mask(0, TERM, core::ptr::null_mut()) };
    spawner.spawn(stop(log.clone(), reactor.clone()));
    // A second connection to the directory, for the readers of their own
    // records, which find the packages that their apps own.
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    let duplicated = unsafe {
        sys::handle_duplicate(
            managed.header.directory.handle().raw(),
            SAME_RIGHTS,
            &mut raw,
        )
    };
    let own_directory = (duplicated == mikros_rt::OK).then(|| {
        // SAFETY: the kernel just made the handle for us.
        let handle = unsafe { OwnedHandle::from_raw(raw) };
        Rc::new(DirectoryAsyncClient::new(
            ClientEnd::new(handle),
            reactor.clone(),
        ))
    });
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    spawner.spawn(crate::service::files::watch(
        log.clone(),
        directory,
        reactor.clone(),
        spawner.clone(),
        live,
    ));
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.instance.is_some() {
                continue;
            }
            if c.protocol == WRITER {
                let end = ServerEnd::new(c.server);
                spawner.spawn(crate::service::writer::serve(
                    log.clone(),
                    end,
                    reactor.clone(),
                    c.client,
                ));
            } else if c.protocol == READER {
                let end = ServerEnd::new(c.server);
                let task = crate::service::reader::serve(
                    log.clone(),
                    end,
                    reactor.clone(),
                    spawner.clone(),
                );
                spawner.spawn(task);
            } else if c.protocol == OWN_READER
                && let Some(directory) = own_directory.clone()
            {
                let end = ServerEnd::new(c.server);
                let task = crate::service::own::serve(
                    log.clone(),
                    end,
                    reactor.clone(),
                    c.client,
                    directory,
                );
                spawner.spawn(task);
            } else if c.protocol == SOURCES {
                let end = ServerEnd::new(c.server);
                let task = crate::service::sources::serve(
                    log.clone(),
                    end,
                    reactor.clone(),
                    spawner.clone(),
                );
                spawner.spawn(task);
            }
        }
    });
    // The kernel log's handle stays open while the program runs.
    drop(kernel_log);
    PANIC_EXIT_CODE
}

/// Takes the first startup handle that `wanted` picks out of `handles`.
fn take(
    handles: &mut Vec<StartupHandle>,
    wanted: impl Fn(&StartupHandle) -> bool,
) -> Option<OwnedHandle> {
    let at = handles.iter().position(wanted)?;
    Some(handles.remove(at).handle)
}

/// The koid of the object behind `handle`, or 0 when it cannot be read.
fn koid(handle: sys::Handle) -> u64 {
    const INFO_BASIC: u32 = 1;
    let mut record = [0u8; 24];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status =
        unsafe { sys::object_get_info(handle, INFO_BASIC, record.as_mut_ptr(), 24, &mut len) };
    if status != mikros_rt::OK {
        return 0;
    }
    u64::from_le_bytes(record[..8].try_into().expect("8 bytes"))
}

/// Waits for `TERM`, then has the files written out, waiting at most
/// `FINISH_LIMIT` for them, and exits.
async fn stop(log: Rc<Log>, reactor: mikros_rt::exec::Reactor) {
    let mut info = [0u8; 64];
    // SAFETY: the pointer refers to room for the signal's information.
    while unsafe { sys::signal_wait(TERM, 0, info.as_mut_ptr()) } != mikros_rt::OK {
        let _ = reactor
            .sleep_until(crate::service::log::now().saturating_add(POLL))
            .await;
    }
    log.stop();
    let deadline = crate::service::log::now().saturating_add(FINISH_LIMIT);
    while log.open_files.get() > 0 && crate::service::log::now() < deadline {
        let _ = reactor
            .sleep_until(crate::service::log::now().saturating_add(POLL))
            .await;
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::process_exit(0) };
}

/// The command line that the platform information `mem` holds, or an
/// empty one when it cannot be read or is not UTF-8.
fn command_line(mem: &OwnedHandle) -> alloc::string::String {
    let word = |at: u64| -> Option<u32> {
        let mut bytes = [0u8; 4];
        // SAFETY: the pointer describes `bytes`.
        let status = unsafe { sys::mem_read(mem.raw(), at, bytes.as_mut_ptr(), 4) };
        (status == mikros_rt::OK).then(|| u32::from_le_bytes(bytes))
    };
    let (Some(offset), Some(len)) = (word(CMDLINE_OFFSET), word(CMDLINE_LEN)) else {
        return alloc::string::String::new();
    };
    let len = (len as usize).min(CMDLINE_MAX);
    let mut bytes = alloc::vec![0u8; len];
    // SAFETY: the pointer describes `bytes`.
    let status = unsafe { sys::mem_read(mem.raw(), offset.into(), bytes.as_mut_ptr(), len as u64) };
    match status == mikros_rt::OK {
        true => alloc::string::String::from_utf8(bytes).unwrap_or_default(),
        false => alloc::string::String::new(),
    }
}
