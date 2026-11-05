//! A program that the service manager starts as a boot service, to
//! drive its `Directory` endpoint from a service's side. It connects
//! through its directory to the permission broker, to protocols that do
//! not exist or that it may not use, and to a protocol it provides
//! itself, and exits with 0 when every check passed or with the number
//! of the first that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, Provider, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const PROVIDER_ID: u32 = 0;
const BROKER: &str = "mikros.permissions.Broker";
const MISSING: &str = "test.directory.None";
const OPEN: &str = "test.directory.Open";
const PROTECTED: &str = "test.directory.Protected";
const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
/// The step a failure to set the checks up ends with.
const SETUP: i64 = 100;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(
        bootstrap,
        reactor.clone(),
        |_: &mut Vec<StartupHandle>| {},
    ));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let at = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider && h.id == PROVIDER_ID);
    let Some(at) = at else {
        return SETUP;
    };
    let provider = ServerEnd::<Provider>::new(managed.handles.remove(at).handle);
    let provider = ProviderAsyncServer::new(provider, reactor.clone());
    let header = managed.header;
    let own = ClientIdentity {
        pid: header.pid,
        user: header.user,
        app_id: header.app_id,
    };
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    executor.block_on(async move {
        match probe(&directory, &provider, &own, &reactor).await {
            Ok(()) => 0,
            Err(step) => step,
        }
    })
}

/// The koid of the object `handle` refers to and the koid of its peer.
fn koids(handle: &OwnedHandle) -> Option<(u64, u64)> {
    let mut record = [0u8; INFO_BASIC_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let s = unsafe {
        sys::object_get_info(
            handle.raw(),
            INFO_BASIC,
            record.as_mut_ptr(),
            INFO_BASIC_SIZE as u64,
            &mut len,
        )
    };
    if s != mikros_rt::OK || len < INFO_BASIC_SIZE as u64 {
        return None;
    }
    let at = |i: usize| u64::from_le_bytes(record[i..i + 8].try_into().expect("8 bytes"));
    Some((at(0), at(8)))
}

/// Connects to `protocol`, `instance`, through `directory`, passing the
/// server end of a new channel, and returns the client end.
async fn connect(
    directory: &DirectoryAsyncClient,
    protocol: &str,
    instance: Option<&str>,
) -> Result<ClientEnd<Broker>, Status> {
    let (client, server) = endpoints::<Broker>().map_err(|_| Status::NoMemory)?;
    let instance = instance.map(String::from);
    match directory
        .connect(protocol.into(), instance, server.into_handle())
        .await
    {
        Ok(r) => r.map(|_| client),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn list(directory: &DirectoryAsyncClient, protocol: &str) -> Result<(), Status> {
    match directory.list(protocol.into()).await {
        Ok(r) => r.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

async fn watch(directory: &DirectoryAsyncClient, protocol: &str) -> Result<(), Status> {
    match directory.watch(protocol.into()).await {
        Ok(r) => r.map(drop),
        Err(_) => Err(Status::PeerClosed),
    }
}

fn identity(user: u32, app_id: &str) -> ClientIdentity {
    ClientIdentity {
        pid: 0,
        user,
        app_id: app_id.into(),
    }
}

/// Runs every check in turn, and returns the number of the first that
/// failed. `own` is the identity the process manager recorded for this
/// program.
async fn probe(
    d: &DirectoryAsyncClient,
    provider: &ProviderAsyncServer,
    own: &ClientIdentity,
    reactor: &Reactor,
) -> Result<(), i64> {
    let mut step = 0;
    let mut check = |what: &str, passed: bool| {
        step += 1;
        if !passed {
            mikros_rt::log!(Error, "directory probe: {what} failed");
        }
        passed.then_some(()).ok_or(step)
    };

    let broker = connect(d, BROKER, None).await;
    let decision = match broker {
        Ok(client) => {
            let broker = BrokerAsyncClient::new(client, reactor.clone());
            match broker
                .check(identity(1, "test.app"), "network".into())
                .await
            {
                Ok(Ok(r)) => Some(r.decision),
                _ => None,
            }
        }
        Err(_) => None,
    };
    check(
        "checking a permission through the broker",
        decision == Some(Decision::Allowed),
    )?;
    check(
        "refusing a missing protocol",
        connect(d, MISSING, None).await.err() == Some(Status::NotFound),
    )?;
    check(
        "refusing an instance of a single-instance protocol",
        connect(d, BROKER, Some("x")).await.err() == Some(Status::InvalidArgs),
    )?;
    check(
        "refusing to list a protocol",
        list(d, BROKER).await == Err(Status::InvalidArgs)
            && list(d, MISSING).await == Err(Status::NotFound),
    )?;
    check(
        "refusing to watch a protocol",
        watch(d, BROKER).await == Err(Status::InvalidArgs),
    )?;
    check(
        "refusing a protocol without its permission",
        connect(d, PROTECTED, None).await.err() == Some(Status::AccessDenied),
    )?;

    let open = connect(d, OPEN, None).await;
    let forwarded = match &open {
        Ok(_) => provider.next().await.ok(),
        Err(_) => None,
    };
    let kept = open.ok().map(ClientEnd::into_handle);
    check(
        "forwarding a connection to its provider",
        match (forwarded, kept.as_ref().and_then(koids)) {
            (Some(ProviderRequest::Connect(r)), Some((kept, peer))) => {
                r.protocol == OPEN
                    && r.instance.is_none()
                    && r.client == *own
                    && koids(&r.server) == Some((peer, kept))
            }
            _ => false,
        },
    )
}
