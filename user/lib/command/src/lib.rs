//! What the commands of Mikros share: reaching services through the
//! process's service directory, telling a connection that the directory
//! refused, which an administrator may not have allowed, apart from
//! other failures, and the names of system things that completion
//! offers. It runs only on Mikros, in programs with the standard
//! library.

#![feature(mikros_handles)]

extern crate alloc;

mod things;

pub use things::Thing;

use alloc::format;
use alloc::string::String;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::sys;

const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;

/// A client of a service directory.
pub struct Directory(DirectoryClient);

impl Directory {
    /// A client of the service directory `handle`, through a duplicate
    /// of it, so that the handle stays its owner's.
    pub fn new(handle: sys::Handle) -> Option<Directory> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::handle_duplicate(handle, SAME_RIGHTS, &mut raw) } != mikros_rt::OK {
            return None;
        }
        // SAFETY: the kernel just made the handle for us.
        let handle = unsafe { OwnedHandle::from_raw(raw) };
        Some(Directory(DirectoryClient::new(ClientEnd::new(handle))))
    }

    /// A channel to the service that provides `name`, or why there is
    /// none.
    pub fn connect<P>(&self, name: &str) -> Result<ClientEnd<P>, String> {
        match self.connect_unless_denied(name) {
            Ok(Some(client)) => Ok(client),
            Ok(None) => Err(format!(
                "cannot connect to {name}: {:?}",
                Status::AccessDenied
            )),
            Err(why) => Err(why),
        }
    }

    /// A channel to the service that provides `name`, `None` when the
    /// directory refused it with `ACCESS_DENIED`, as it does when the
    /// connection needs an `admin` permission that no administrator
    /// allowed, or why there is none.
    pub fn connect_unless_denied<P>(&self, name: &str) -> Result<Option<ClientEnd<P>>, String> {
        let refused = |what: String| format!("cannot connect to {name}: {what}");
        let (client, server) = endpoints::<P>().map_err(|e| refused(format!("{e:?}")))?;
        match self
            .0
            .connect(name.into(), None, server.into_handle(), FOREVER)
        {
            Ok(Ok(_)) => Ok(Some(client)),
            Ok(Err(Status::AccessDenied)) => Ok(None),
            Ok(Err(status)) => Err(refused(format!("{status:?}"))),
            Err(error) => Err(refused(format!("{error:?}"))),
        }
    }
}

/// The names that start with `prefix` of the system things of `kind`,
/// for `mikros_cli::start`; none for the kinds that are not system
/// things.
pub fn list(kind: mikros_cli::Kind, prefix: &str) -> alloc::vec::Vec<String> {
    use mikros_cli::Kind;
    let thing = match kind {
        Kind::Service => Thing::Service,
        Kind::User => Thing::User,
        Kind::App => Thing::App,
        Kind::Permission => Thing::Permission,
        Kind::Volume => Thing::Volume,
        Kind::Device => Thing::Device,
        _ => return alloc::vec::Vec::new(),
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return alloc::vec::Vec::new();
    };
    let mut names = directory.names(thing);
    names.retain(|n| n.starts_with(prefix));
    names
}
