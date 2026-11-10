//! Starting the process manager from the boot image, registering with
//! it as process ID 1, and asking it to start the service manager.

use alloc::vec::Vec;
use core::fmt;
use mikros_rt::Startup;
use mikros_rt::interfaces::__midl::{self, ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, InitBootstrap, InitBootstrapClient, ManagerBootstrap, ManagerBootstrapClient,
    ManagerStartup, StartupHandle,
};
use mikros_rt::sys::{self, Handle};

const PAGE_SIZE: u64 = 4096;
const PERM_READ: u32 = 1 << 0;
/// Duplicates keep the rights of the original, including `TRANSFER`.
/// The receivers' bindings reduce them to the rights they declare.
const SAME_RIGHTS: u32 = 1 << 31;
/// The boot image member of the process manager.
const PROCESS_MANAGER: &[u8] = b"usr/bin/process-manager";

/// A step that failed, and why.
pub struct Failure {
    what: &'static str,
    why: Why,
}

enum Why {
    Status(i64),
    Load(mikros_loader::Error),
    Channel(__midl::Error),
    Refused(Status),
    Missing,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: ", self.what)?;
        match &self.why {
            Why::Status(s) => write!(f, "status {s}"),
            Why::Load(e) => write!(f, "{e:?}"),
            Why::Channel(e) => write!(f, "{e:?}"),
            Why::Refused(s) => write!(f, "{s:?}"),
            Why::Missing => write!(f, "not found"),
        }
    }
}

type Result<T> = core::result::Result<T, Failure>;

fn status(what: &'static str, s: i64) -> Result<()> {
    if s == mikros_rt::OK {
        Ok(())
    } else {
        Err(Failure {
            what,
            why: Why::Status(s),
        })
    }
}

/// The kernel's command line, at most 4096 bytes of it, with what is not
/// UTF-8 replaced.
fn command_line(bytes: &[u8]) -> alloc::string::String {
    let text = alloc::string::String::from_utf8_lossy(bytes);
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    alloc::string::String::from(&text[..end])
}

/// Starts the process manager, registers with it and asks it to start
/// the service manager, giving the service manager the boot handles.
pub fn start(s: &Startup) -> Result<()> {
    let file = member(s, PROCESS_MANAGER)?;
    let (process, thread, bootstrap) = load(s, &file)?;
    let channel = |e| Failure {
        what: "creating a channel",
        why: Why::Channel(e),
    };
    let (init, init_bootstrap) = endpoints::<InitBootstrap>().map_err(channel)?;
    let startup = ManagerStartup {
        version: 2,
        process,
        thread,
        init_process: duplicate(s.process)?,
        budget: duplicate(s.budget)?,
        boot_image: duplicate(s.boot_image)?,
        clock_page: duplicate(s.clock_page)?,
        log: duplicate(s.log)?,
        init_bootstrap,
        command_line: command_line(s.cmdline),
    };
    // The channel closes when the client is dropped after `Start`.
    ManagerBootstrapClient::new(bootstrap)
        .start(startup)
        .map_err(|e| Failure {
            what: "sending Start",
            why: Why::Channel(e),
        })?;
    let init = InitBootstrapClient::new(init);
    reply("registering as process ID 1", init.register(i64::MAX))?;
    let handles = boot_handles(s)?;
    let started = reply(
        "starting the service manager",
        init.start_service_manager(handles, i64::MAX),
    )?;
    mikros_rt::log!(Info, "service manager started as process {}", started.pid);
    Ok(())
}

fn reply<T>(
    what: &'static str,
    r: core::result::Result<core::result::Result<T, Status>, __midl::Error>,
) -> Result<T> {
    match r {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(s)) => Err(Failure {
            what,
            why: Why::Refused(s),
        }),
        Err(e) => Err(Failure {
            what,
            why: Why::Channel(e),
        }),
    }
}

fn duplicate(handle: Handle) -> Result<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    status("duplicating a handle", unsafe {
        sys::handle_duplicate(handle, SAME_RIGHTS, &mut out)
    })?;
    // SAFETY: the handle was just created for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// The boot handles, as `USER` startup handles with their IDs: the
/// `ROOT` resource, the log, the root budget, the framebuffer when there
/// is one, and a memory object with the startup message.
fn boot_handles(s: &Startup) -> Result<Vec<StartupHandle>> {
    let message = s.message();
    let mut platform_info = 0;
    // SAFETY: the out pointer is valid for the call.
    status("creating the platform information", unsafe {
        sys::mem_create(
            (message.len() as u64).next_multiple_of(PAGE_SIZE),
            0,
            &mut platform_info,
        )
    })?;
    // SAFETY: the handle was just created for us.
    let platform_info = unsafe { OwnedHandle::from_raw(platform_info) };
    // SAFETY: the pointer and length describe `message`.
    status("writing the platform information", unsafe {
        sys::mem_write(
            platform_info.raw(),
            0,
            message.as_ptr(),
            message.len() as u64,
        )
    })?;
    let mut handles = alloc::vec![
        (1, duplicate(s.root_resource)?),
        (2, duplicate(s.log)?),
        (3, duplicate(s.budget)?),
    ];
    if let Some(fb) = &s.framebuffer {
        handles.push((4, duplicate(fb.memory)?));
    }
    handles.push((5, platform_info));
    Ok(handles
        .into_iter()
        .map(|(id, handle)| StartupHandle {
            role: HandleRole::User,
            id,
            handle,
        })
        .collect())
}

/// A copy of the boot image member `name`.
fn member(s: &Startup, name: &[u8]) -> Result<Vec<u8>> {
    let size = s.boot_image_size.next_multiple_of(PAGE_SIZE);
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    status("mapping the boot image", unsafe {
        sys::vm_map(s.process, PERM_READ, 0, s.boot_image, 0, size, &mut at)
    })?;
    // SAFETY: the boot image is mapped readable at `at`.
    let image = unsafe { core::slice::from_raw_parts(at as *const u8, s.boot_image_size as usize) };
    let file = mikros_ustar::find(image, name).map(<[u8]>::to_vec);
    // SAFETY: nothing refers to the mapping any more.
    unsafe { sys::vm_unmap(s.process, at, size) };
    file.ok_or(Failure {
        what: "finding usr/bin/process-manager",
        why: Why::Missing,
    })
}

/// Loads `file` into a new process and starts it with a new bootstrap
/// channel. Returns the process, its thread and this end of the channel.
fn load(
    s: &Startup,
    file: &[u8],
) -> Result<(OwnedHandle, OwnedHandle, ClientEnd<ManagerBootstrap>)> {
    let loading = |e| Failure {
        what: "loading the process manager",
        why: Why::Load(e),
    };
    let mut shared = 0;
    // SAFETY: the out pointer is valid for the call.
    status("creating a memory object", unsafe {
        sys::mem_create(
            (file.len() as u64).next_multiple_of(PAGE_SIZE),
            0,
            &mut shared,
        )
    })?;
    // SAFETY: the handle was just created for us.
    let shared = unsafe { OwnedHandle::from_raw(shared) };
    // SAFETY: the pointer and length describe `file`.
    status("copying the process manager", unsafe {
        sys::mem_write(shared.raw(), 0, file.as_ptr(), file.len() as u64)
    })?;
    let private = |offset: u64, len: u64| {
        let mut copy = 0;
        // SAFETY: the out pointer is valid for the call.
        match unsafe { sys::mem_clone(shared.raw(), 0, offset, len, &mut copy) } {
            mikros_rt::OK => Ok(copy),
            st => Err(mikros_loader::Error::Status(st)),
        }
    };
    let image = mikros_loader::Image {
        file,
        shared: shared.raw(),
        private: &private,
    };
    let name = b"process-manager";
    let (mut process, mut thread) = (0, 0);
    // SAFETY: the pointers describe `name` and `process`.
    status("creating the process manager", unsafe {
        sys::process_create(s.budget, name.as_ptr(), name.len() as u64, &mut process)
    })?;
    // SAFETY: the handle was just created for us.
    let process = unsafe { OwnedHandle::from_raw(process) };
    let loaded = mikros_loader::load(process.raw(), &image).map_err(loading)?;
    let top = mikros_loader::stack(process.raw()).map_err(loading)?;
    // SAFETY: the pointers describe `name` and `thread`.
    status("creating its thread", unsafe {
        sys::thread_create(process.raw(), name.as_ptr(), name.len() as u64, &mut thread)
    })?;
    // SAFETY: the handle was just created for us.
    let thread = unsafe { OwnedHandle::from_raw(thread) };
    let channel = |e| Failure {
        what: "creating a channel",
        why: Why::Channel(e),
    };
    let (ours, theirs) = endpoints::<ManagerBootstrap>().map_err(channel)?;
    let theirs = theirs.into_handle();
    // SAFETY: the call takes no pointers.
    status("starting the process manager", unsafe {
        sys::process_start(
            process.raw(),
            thread.raw(),
            loaded.entry,
            top,
            theirs.raw(),
            0,
        )
    })?;
    // The handle moved into the process manager's table.
    let _ = theirs.into_raw();
    Ok((process, thread, ours))
}
