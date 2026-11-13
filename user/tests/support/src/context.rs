//! What a test knows of its own process: from init's startup message in
//! the kernel's tests, or from the process manager's startup transfer in
//! the system tests.

use core::mem::ManuallyDrop;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
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
