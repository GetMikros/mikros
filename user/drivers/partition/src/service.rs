//! A partition's raw access service: `BlockDevice` on the connections
//! that the service directory forwards through the published provider.
//! Of the writable connections that `Open` opens, one at a time may be
//! open across all partitions of the disk, while no partition's `bus`
//! connection holds a writable lock and the driver holds the disk's.

use crate::partition::{self, Access, Partition};
use crate::writers::Writers;
use alloc::rc::Rc;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};
use mikros_rt::interfaces::mikros::storage::{
    BlockAsyncClient, BlockDevice, BlockDeviceAsyncServer, BlockDeviceOpenResponse,
    BlockDeviceRequest,
};

/// The protocol of the published service.
const PROTOCOL: &str = "device.storage";

/// Serves the provider `end` of `partition`'s raw access service until it
/// closes: each connection to `device.storage` gets `BlockDevice`.
/// `writers` are those of the disk's partitions.
pub async fn serve_provider(
    disk: Rc<BlockAsyncClient>,
    partition: Rc<Partition>,
    end: ServerEnd<Provider>,
    writers: Rc<Writers>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let server = ProviderAsyncServer::new(end, reactor.clone());
    while let Ok(ProviderRequest::Connect(request)) = server.next().await {
        if request.protocol != PROTOCOL {
            continue;
        }
        let end = ServerEnd::<BlockDevice>::new(request.server);
        let (disk, partition, writers) = (disk.clone(), partition.clone(), writers.clone());
        let task = open(
            disk,
            partition,
            end,
            writers,
            reactor.clone(),
            spawner.clone(),
        );
        spawner.spawn(task);
    }
}

/// Serves `BlockDevice` on `end`. `Open` serves `Block` on the channel it
/// gets, failing with `ALREADY_EXISTS` for a writable one when `writers`
/// refuse it.
async fn open(
    disk: Rc<BlockAsyncClient>,
    partition: Rc<Partition>,
    end: ServerEnd<BlockDevice>,
    writers: Rc<Writers>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let server = BlockDeviceAsyncServer::new(end, reactor.clone());
    while let Ok(BlockDeviceRequest::Open(request, responder)) = server.next().await {
        let writable = request.writable;
        if writable && !writers.open().await {
            let _ = responder.reply(server.server(), Err(Status::AlreadyExists));
            continue;
        }
        let _ = responder.reply(server.server(), Ok(BlockDeviceOpenResponse {}));
        let closer = writers.clone();
        let closed = move || {
            if writable {
                closer.close();
            }
        };
        let block = request.block.into_handle();
        let task = partition::serve(
            disk.clone(),
            partition.clone(),
            block,
            Access::Open(writable),
            writers.clone(),
            reactor.clone(),
            spawner.clone(),
            closed,
        );
        spawner.spawn(task);
    }
}
