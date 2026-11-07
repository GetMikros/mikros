//! A disk's raw access service: `BlockDevice` on the connections that
//! the service directory forwards through the published provider. Of
//! the writable connections that `Open` opens, one at a time may be
//! open.

use crate::{Disk, block};
use alloc::rc::Rc;
use core::cell::Cell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};
use mikros_rt::interfaces::mikros::storage::{
    BlockDevice, BlockDeviceAsyncServer, BlockDeviceOpenResponse, BlockDeviceRequest,
};

/// The protocol of the published service.
const PROTOCOL: &str = "device.storage";

/// Serves the provider `end` of the disk's raw access service, published
/// with the class `storage`, until it closes: each connection to
/// `device.storage` gets `BlockDevice`.
pub async fn serve_provider<D: Disk>(
    disk: Rc<D>,
    end: ServerEnd<Provider>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let server = ProviderAsyncServer::new(end, reactor.clone());
    let writer = Rc::new(Cell::new(false));
    while let Ok(ProviderRequest::Connect(request)) = server.next().await {
        if request.protocol != PROTOCOL {
            continue;
        }
        let end = ServerEnd::<BlockDevice>::new(request.server);
        let task = open(
            disk.clone(),
            end,
            writer.clone(),
            reactor.clone(),
            spawner.clone(),
        );
        spawner.spawn(task);
    }
}

/// Serves `BlockDevice` on `end`. `Open` serves `Block` on the channel it
/// gets, failing with `ALREADY_EXISTS` for a writable one while another
/// writable one is open.
async fn open<D: Disk>(
    disk: Rc<D>,
    end: ServerEnd<BlockDevice>,
    writer: Rc<Cell<bool>>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let server = BlockDeviceAsyncServer::new(end, reactor.clone());
    while let Ok(BlockDeviceRequest::Open(request, responder)) = server.next().await {
        let writable = request.writable;
        if writable && writer.replace(true) {
            let _ = responder.reply(server.server(), Err(Status::AlreadyExists));
            continue;
        }
        let _ = responder.reply(server.server(), Ok(BlockDeviceOpenResponse {}));
        let writer = writer.clone();
        let closed = move || {
            if writable {
                writer.set(false);
            }
        };
        let task = block::serve(
            disk.clone(),
            request.block.into_handle(),
            writable,
            reactor.clone(),
            spawner.clone(),
            closed,
        );
        spawner.spawn(task);
    }
}
