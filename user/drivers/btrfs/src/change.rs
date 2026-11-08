// SPDX-License-Identifier: GPL-2.0
//! The requests of a connection that change the entries of directories:
//! removing them.

use crate::fs::error;
use crate::node::{Connection, NAME_MAX};
use mikros_btrfs::node::Changes;
use mikros_rt::interfaces::mikros::fs::*;

impl Connection {
    /// Makes the connection's user the owner of a node it created.
    pub(crate) fn own(&self, node: &mikros_btrfs::node::Node<'_>) -> Result<(), Error> {
        let changes = Changes {
            owner: Some(self.user),
            ..Changes::default()
        };
        node.set_attributes(&changes).map_err(error)
    }

    pub(crate) fn unlink(&self, name: &str, directory: bool) -> Result<(), Error> {
        self.require_write()?;
        self.directory()?;
        check_component(name)?;
        self.open
            .node
            .remove(name.as_bytes(), directory)
            .map_err(error)
    }
}

/// Checks a name that `Unlink` gets.
fn check_component(name: &str) -> Result<(), Error> {
    match name {
        "" | "." | ".." => Err(Error::Invalid),
        n if n.contains('/') || n.contains('\0') => Err(Error::Invalid),
        n if n.len() > NAME_MAX => Err(Error::NameTooLong),
        _ => Ok(()),
    }
}
