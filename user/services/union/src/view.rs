//! Views: their layers, and directories as the layers hold them, with
//! lookup and listing from the first layer to the last.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{
    DirEntry, Error, Node, NodeAsyncClient, NodeType, OpenFlags, Rights,
};

/// The prefix of a whiteout's name, and the marker of a directory that
/// hides the directories of its name in the layers after it.
pub const WHITEOUT: &str = ".wh.";
pub const OPAQUE: &str = ".wh..wh..opq";

/// A view: its layers, the first of which may be writable, and the most
/// rights its connections may have.
pub struct View {
    pub writable: bool,
    pub rights: Rights,
    /// The root directory of each layer.
    pub roots: Vec<Rc<NodeAsyncClient>>,
    /// The directories that the view's tokens name, by the token's
    /// object ID.
    pub tokens: RefCell<BTreeMap<u64, Dir>>,
    pub reactor: Reactor,
    pub spawner: Spawner,
}

/// A directory of a view: its path from the view's root, and the
/// directory at that path in each layer that has one there and does not
/// hide it, by layer.
#[derive(Clone)]
pub struct Dir {
    pub view: Rc<View>,
    pub path: Vec<String>,
    pub layers: Vec<Option<Rc<NodeAsyncClient>>>,
}

/// What a name of a directory of a view is.
pub enum Found {
    Dir(Dir),
    /// Anything else, from the layer that has it: a file, a symbolic link
    /// or a socket.
    Other {
        layer: usize,
        kind: NodeType,
    },
    Missing,
}

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

/// The result of a call to a layer, with `IO` when its channel failed.
pub fn flat<T>(reply: Reply<T>) -> Result<T, Error> {
    reply.unwrap_or(Err(Error::Io))
}

/// Opens `name` in the directory `dir` of a layer without following a
/// link, for reading: the connection and what it is, or `NotFound`.
pub async fn open_in(
    dir: &NodeAsyncClient,
    name: &str,
    reactor: &Reactor,
) -> Result<(NodeAsyncClient, NodeType), Error> {
    let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
    let opened = flat(
        dir.open(name.into(), OpenFlags::NO_FOLLOW, 0, Rights::READ, server)
            .await,
    )?;
    Ok((NodeAsyncClient::new(client, reactor.clone()), opened.r#type))
}

/// Whether the directory `dir` of a layer holds `name`.
pub async fn has(dir: &NodeAsyncClient, name: &str, reactor: &Reactor) -> Result<bool, Error> {
    match open_in(dir, name, reactor).await {
        Ok(_) => Ok(true),
        Err(Error::NotFound) => Ok(false),
        Err(e) => Err(e),
    }
}

impl Dir {
    /// The root directory of `view`.
    pub fn root(view: Rc<View>) -> Dir {
        let layers = view.roots.iter().cloned().map(Some).collect();
        Dir {
            view,
            path: Vec::new(),
            layers,
        }
    }

    /// The first layer's directory of this one, whose attributes it has.
    pub fn first(&self) -> Option<(usize, &Rc<NodeAsyncClient>)> {
        self.layers
            .iter()
            .enumerate()
            .find_map(|(i, d)| d.as_ref().map(|d| (i, d)))
    }

    /// What `name` is in this directory: the first layer that has it
    /// decides, unless a layer before it holds a whiteout of it. A
    /// directory merges with the directories of that name in the layers
    /// after it, up to a layer that holds something else of that name or
    /// a whiteout of it. Whiteouts themselves are never found.
    pub async fn lookup(&self, name: &str) -> Result<Found, Error> {
        if name.starts_with(WHITEOUT) {
            return Ok(Found::Missing);
        }
        let reactor = &self.view.reactor;
        let whiteout = format!("{WHITEOUT}{name}");
        let mut merged: Vec<Option<Rc<NodeAsyncClient>>> = alloc::vec![None; self.layers.len()];
        let mut found_dir = false;
        for (i, layer) in self.layers.iter().enumerate() {
            let Some(dir) = layer else {
                continue;
            };
            match open_in(dir, name, reactor).await {
                Ok((node, NodeType::Directory)) => {
                    let opaque = has(&node, OPAQUE, reactor).await?;
                    merged[i] = Some(Rc::new(node));
                    found_dir = true;
                    if opaque {
                        break;
                    }
                }
                Ok(_) if found_dir => break,
                Ok((_, kind)) => return Ok(Found::Other { layer: i, kind }),
                Err(Error::NotFound) => {}
                Err(e) => return Err(e),
            }
            if has(dir, &whiteout, reactor).await? {
                break;
            }
        }
        match found_dir {
            true => {
                let mut path = self.path.clone();
                path.push(String::from(name));
                Ok(Found::Dir(Dir {
                    view: self.view.clone(),
                    path,
                    layers: merged,
                }))
            }
            false => Ok(Found::Missing),
        }
    }

    /// Every entry of this directory as a view shows it: the first
    /// layer's in its order, then each later layer's names that no layer
    /// before it holds or whites out, and no whiteouts.
    pub async fn entries(&self, attributes: bool) -> Result<Vec<DirEntry>, Error> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut hidden: BTreeSet<String> = BTreeSet::new();
        let mut out = Vec::new();
        for dir in self.layers.iter().flatten() {
            let mut cookie = 0;
            let mut whiteouts = Vec::new();
            loop {
                let listed = flat(dir.read_directory(cookie, attributes).await)?;
                if listed.entries.is_empty() {
                    break;
                }
                cookie = listed.next;
                for entry in listed.entries {
                    if let Some(name) = entry.name.strip_prefix(WHITEOUT) {
                        whiteouts.push(String::from(name));
                    } else if !seen.contains(&entry.name) && !hidden.contains(&entry.name) {
                        seen.insert(entry.name.clone());
                        out.push(entry);
                    }
                }
            }
            hidden.extend(whiteouts);
        }
        Ok(out)
    }
}
