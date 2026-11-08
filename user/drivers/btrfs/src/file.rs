// SPDX-License-Identifier: GPL-2.0
//! The requests of a connection about its node itself: cloning the
//! connection, and reading symbolic links.

use crate::dir::link_target;
use crate::node::Connection;
use mikros_rt::interfaces::mikros::fs::*;

impl Connection {
    pub(crate) fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        self.fs
            .serve(q.node, self.open.clone(), q.rights, self.user, self.append);
        Ok(NodeCloneResponse {})
    }

    pub(crate) fn read_link(&self) -> Result<NodeReadLinkResponse, Error> {
        self.require(Rights::READ)?;
        Ok(NodeReadLinkResponse {
            target: link_target(&self.open)?,
        })
    }
}
