//! What a test knows of its own process: from init's startup message in
//! the kernel's tests, or from the process manager's startup transfer in
//! the system tests.

use core::mem::ManuallyDrop;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::fs::NodeClient;
use mikros_rt::interfaces::mikros::process::LauncherClient;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::sys::Handle;

/// The handles of a test's own process.
pub trait Context {
    /// The process, with every right.
    fn process(&self) -> Handle;
    /// The thread that runs the test.
    fn thread(&self) -> Handle;
    /// The clock page.
    fn clock_page(&self) -> Handle;
}

impl Context for mikros_rt::Startup {
    fn process(&self) -> Handle {
        self.process
    }

    fn thread(&self) -> Handle {
        self.thread
    }

    fn clock_page(&self) -> Handle {
        self.clock_page
    }
}

/// The handles of a system test's own process, which its startup
/// transfer gave.
#[derive(Clone, Copy, Debug)]
pub struct Own {
    pub process: Handle,
    pub thread: Handle,
    pub clock_page: Handle,
    /// The service directory, through which it connects to services.
    pub directory: Handle,
    /// The directory of its namespace's `/tmp`, or 0 when it has none.
    pub tmp: Handle,
    /// The process manager's `Launcher`.
    pub launcher: Handle,
}

impl Own {
    /// The handles of the process whose startup transfer is `managed`.
    pub fn of(managed: &mikros_rt::Managed) -> Own {
        let header = &managed.header;
        Own {
            process: header.process.raw(),
            thread: header.thread.raw(),
            clock_page: header.clock_page.raw(),
            directory: header.directory.handle().raw(),
            tmp: managed
                .namespace
                .iter()
                .find(|entry| entry.path == "/tmp")
                .map_or(0, |entry| entry.directory.handle().raw()),
            launcher: header.launcher.handle().raw(),
        }
    }

    /// A client of the service directory that borrows its handle, which
    /// stays open for as long as the test runs.
    pub fn services(&self) -> ManuallyDrop<DirectoryClient> {
        // SAFETY: the client is never dropped, so it never closes the
        // handle, which the startup transfer keeps open.
        let handle = unsafe { OwnedHandle::from_raw(self.directory) };
        ManuallyDrop::new(DirectoryClient::new(ClientEnd::new(handle)))
    }

    /// A client of the process manager's `Launcher` that borrows its
    /// handle as `services` does.
    pub fn launcher(&self) -> ManuallyDrop<LauncherClient> {
        // SAFETY: the client is never dropped, so it never closes the
        // handle, which the startup transfer keeps open.
        let handle = unsafe { OwnedHandle::from_raw(self.launcher) };
        ManuallyDrop::new(LauncherClient::new(ClientEnd::new(handle)))
    }

    /// A client of the namespace's `/tmp`, borrowing its handle as
    /// `services` does, or none when the namespace has no `/tmp`.
    pub fn tmp(&self) -> Option<ManuallyDrop<NodeClient>> {
        if self.tmp == 0 {
            return None;
        }
        // SAFETY: the client is never dropped, so it never closes the
        // handle, which the startup transfer keeps open.
        let handle = unsafe { OwnedHandle::from_raw(self.tmp) };
        Some(ManuallyDrop::new(NodeClient::new(ClientEnd::new(handle))))
    }
}

impl Context for Own {
    fn process(&self) -> Handle {
        self.process
    }

    fn thread(&self) -> Handle {
        self.thread
    }

    fn clock_page(&self) -> Handle {
        self.clock_page
    }
}
