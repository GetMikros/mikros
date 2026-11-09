//! Waiting for the services that the time service uses once they are
//! published: the `device.filesystem` instance `root`, and the first
//! instance of `device.rtc`. One task reads the directory's events for
//! both, since a second reader of the same channel would take events that
//! are not its own, and starts a task for each service when it appears.

use crate::clock::Time;
use crate::root::{self, FILESYSTEM, ROOT};
use crate::rtc::{self, RTC};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, DirectoryEvent};

/// The instances of `protocol` that `directory` lists now.
async fn listed(directory: &DirectoryAsyncClient, protocol: &str) -> Vec<String> {
    match directory.list(protocol.into()).await {
        Ok(Ok(listed)) => listed.instances,
        _ => Vec::new(),
    }
}

pub async fn watch(
    time: Rc<Time>,
    directory: DirectoryAsyncClient,
    reactor: Reactor,
    spawner: Spawner,
) {
    let directory = Rc::new(directory);
    for protocol in [FILESYSTEM, RTC] {
        if !matches!(directory.watch(protocol.into()).await, Ok(Ok(_))) {
            mikros_rt::log!(Warning, "time: watching {protocol} failed");
        }
    }
    let (mut root_seen, mut rtc_seen) = (false, false);
    let mut seen = |protocol: &str, instance: String| {
        if protocol == FILESYSTEM && instance == ROOT && !root_seen {
            root_seen = true;
            let task = root::connected(time.clone(), directory.clone(), reactor.clone());
            spawner.spawn(task);
        } else if protocol == RTC && !rtc_seen {
            rtc_seen = true;
            let task = rtc::connected(time.clone(), directory.clone(), instance, reactor.clone());
            spawner.spawn(task);
        }
    };
    // Instances published before the watches were set are only listed.
    for protocol in [FILESYSTEM, RTC] {
        for instance in listed(&directory, protocol).await {
            seen(protocol, instance);
        }
    }
    while let Ok(event) = directory.next_event().await {
        if let DirectoryEvent::OnInstanceAdded(added) = event {
            seen(&added.protocol, added.instance);
        }
    }
}
