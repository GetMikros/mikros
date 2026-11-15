//! What the commands of Mikros share: reaching services through the
//! process's service directory, and asking the session manager whether
//! an administrator allows an action. It runs only on Mikros, and needs
//! only `alloc`, so that the commands, which use the standard library,
//! can share it with those that do not.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, DirectoryClient};
use mikros_rt::interfaces::mikros::session::{Authorization, AuthorizationClient};
use mikros_rt::sys;

const AUTHORIZATION: &str = "mikros.session.Authorization";
/// The longest reason that `Authorize` takes, in bytes.
const REASON_MAX: usize = 256;
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
        let refused = |what: String| format!("cannot connect to {name}: {what}");
        let (client, server) = endpoints::<P>().map_err(|e| refused(format!("{e:?}")))?;
        match self
            .0
            .connect(name.into(), None, server.into_handle(), FOREVER)
        {
            Ok(Ok(_)) => Ok(client),
            Ok(Err(status)) => Err(refused(format!("{status:?}"))),
            Err(error) => Err(refused(format!("{error:?}"))),
        }
    }

    /// Whether an administrator allows `action` for the process of
    /// identity `me`, asked with `reason`, cut to the length that
    /// `Authorize` takes. It is not allowed when the session manager
    /// cannot be asked.
    pub fn authorized(&self, me: ClientIdentity, action: &str, mut reason: String) -> bool {
        let Ok(client) = self.connect::<Authorization>(AUTHORIZATION) else {
            return false;
        };
        while reason.len() > REASON_MAX {
            reason.pop();
        }
        let answer = AuthorizationClient::new(client).authorize(me, action.into(), reason, FOREVER);
        matches!(answer, Ok(Ok(a)) if a.allowed)
    }
}
