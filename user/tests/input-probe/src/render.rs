//! The render instance of the basic graphics adapter: it owns no display,
//! makes buffers of memory objects that it hands out, shares them
//! through tokens that only it knows, refuses driver commands and, for a
//! client without the permission, high priority, and tells the memory
//! that buffers take. Its tokens are not the display instance's.

use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    BufferFlags, Capabilities, Display, DisplayAsyncClient, MemoryInfo, Render, RenderAsyncClient,
    RenderPriority,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::objects;

use crate::display::open;
use crate::failed;

const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const PAGE_SIZE: u64 = 4096;
const SAME_RIGHTS: u32 = 1 << 31;
/// The bytes that the probe writes through one connection's memory
/// object and reads through the other's.
const PATTERN: [u8; 4] = *b"MKRS";

async fn connection(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Option<RenderAsyncClient> {
    let device = open(directory, reactor, Capabilities::RENDER).await?;
    let (client, server) = endpoints::<Render>().ok()?;
    if !matches!(device.open_render(server).await, Ok(Ok(_))) {
        return failed("OpenRender");
    }
    Some(RenderAsyncClient::new(client, reactor.clone()))
}

async fn memory_info(render: &RenderAsyncClient) -> Option<MemoryInfo> {
    match render.get_memory_info().await {
        Ok(r) => Some(r.info),
        Err(_) => failed("GetMemoryInfo"),
    }
}

/// Checks the render instance, and that `display`, the display
/// instance's connection, refuses its tokens.
pub async fn works(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    display: &DisplayAsyncClient,
) -> Option<()> {
    let device = open(directory, reactor, Capabilities::RENDER).await?;
    let (_, server) = endpoints::<Display>().ok()?;
    if !matches!(
        device.open_display(server, None).await,
        Ok(Err(Status::NotSupported))
    ) {
        return failed("refusing OpenDisplay on the render instance");
    }
    let render = connection(directory, reactor).await?;
    let info = memory_info(&render).await?;
    let empty = info.device_local_total == 0
        && info.device_local_used == 0
        && info.connection_used == 0
        && info.system_total > 0
        && info.connection_limit == info.system_total / 2;
    if !empty {
        return failed("the memory information of a new connection");
    }
    let created = render.create_buffer(PAGE_SIZE + 1, BufferFlags(0)).await;
    let Ok(Ok(created)) = created else {
        return failed("CreateBuffer");
    };
    let Some(memory) = created.memory else {
        return failed("a memory object for a buffer without CPU_VISIBLE");
    };
    let info = memory_info(&render).await?;
    if created.buffer == 0
        || info.connection_used != 2 * PAGE_SIZE
        || info.system_used < 2 * PAGE_SIZE
    {
        return failed("the buffer's ID and memory");
    }
    let too_large = render
        .create_buffer(info.connection_limit, BufferFlags::CPU_VISIBLE)
        .await;
    if !matches!(too_large, Ok(Err(Status::NoResources))) {
        return failed("refusing a buffer past the connection's limit");
    }
    let Ok(Ok(exported)) = render.export_buffer(created.buffer).await else {
        return failed("ExportBuffer");
    };
    let token = exported.token;
    let other = connection(directory, reactor).await?;
    let duplicate = objects::duplicate(token.raw(), SAME_RIGHTS).ok()?;
    let imported = other.import_buffer(duplicate).await;
    let Ok(Ok(imported)) = imported else {
        return failed("ImportBuffer of another connection's token");
    };
    let Some(shared) = imported.memory else {
        return failed("the imported buffer's memory object");
    };
    objects::write_memory(memory.raw(), 0, &PATTERN).ok()?;
    let mut read = [0u8; 4];
    objects::read_memory(shared.raw(), 0, &mut read).ok()?;
    if imported.size != 2 * PAGE_SIZE || read != PATTERN {
        return failed("sharing the buffer's memory through its token");
    }
    let stranger = objects::event(0).ok()?;
    if !matches!(
        other.import_buffer(stranger).await,
        Ok(Err(Status::NotFound))
    ) {
        return failed("refusing an event that is no token");
    }
    let shown = display.import_buffer(token, 1, 1, XRGB8888, 0, 4, 0).await;
    if !matches!(shown, Ok(Err(Status::NotFound))) {
        return failed("refusing the render instance's token on the display");
    }
    let command = render.driver_command(1, Vec::new(), Vec::new()).await;
    if !matches!(command, Ok(Err(Status::NotSupported))) {
        return failed("refusing driver commands");
    }
    if !matches!(render.set_priority(RenderPriority::Normal).await, Ok(Ok(_))) {
        return failed("SetPriority NORMAL");
    }
    if !matches!(
        render.set_priority(RenderPriority::High).await,
        Ok(Err(Status::AccessDenied))
    ) {
        return failed("refusing HIGH without its permission");
    }
    // Once both connections destroyed the buffer, it no longer counts.
    let before = memory_info(&render).await?.system_used;
    other.destroy_buffer(imported.buffer).ok()?;
    // A reply on the same connection comes after the destruction.
    memory_info(&other).await?;
    render.destroy_buffer(created.buffer).ok()?;
    let after = memory_info(&render).await?;
    if after.connection_used != 0 || after.system_used + 2 * PAGE_SIZE != before {
        return failed("freeing a buffer that no connection has");
    }
    Some(())
}
