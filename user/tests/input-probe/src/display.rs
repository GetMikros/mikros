//! The display: the basic display driver's device and its one owner,
//! a commit that refuses another mode, one that fills the top half of
//! the screen from a dumb buffer, imports of memory objects that do not
//! hold their image refused, and a commit that fills the bottom half
//! from an imported memory object.

use alloc::vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, Colorspace, Commit, CommitFlags, ConnectorType, CrtcState, Device,
    DeviceAsyncClient, Display, DisplayAsyncClient, DisplayEvent, Mode, PlaneState, Rect, Render,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::sys;

use crate::{connect, failed, instances, now};

const GPU: &str = "device.gpu";
const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
/// The colour the screen is filled with, which the runner looks for.
pub const PAINT: u32 = 0x0012_3456;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const SAME_RIGHTS: u32 = 1 << 31;
const COMPLETE_LIMIT: i64 = 5_000_000_000;
const PAGE_SIZE: u64 = 4096;

fn commit(flags: CommitFlags, mode: Mode, buffer: u32) -> Commit {
    let (width, height) = (mode.width, mode.height);
    let whole = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    placed(flags, mode, buffer, Rect { ..whole }, whole)
}

/// A commit that shows the part `src` of `buffer` at `dst`.
fn placed(flags: CommitFlags, mode: Mode, buffer: u32, src: Rect, dst: Rect) -> Commit {
    Commit {
        flags,
        crtcs: vec![CrtcState {
            crtc: 1,
            active: true,
            connector: 1,
            mode,
            vrr: false,
            gamma: None,
            colorspace: Colorspace::Default,
            hdr: None,
        }],
        planes: vec![PlaneState {
            plane: 1,
            crtc: 1,
            buffer,
            src,
            dst,
            damage: vec![],
            acquire_sync: 0,
            acquire_value: 0,
        }],
    }
}

async fn open(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<DeviceAsyncClient> {
    let instance = instances(directory, reactor, GPU, 1)
        .await?
        .into_iter()
        .next()?;
    let channel = connect::<Device>(directory, GPU, instance).await?;
    Some(DeviceAsyncClient::new(channel, reactor.clone()))
}

/// Maps the `size` bytes of `memory` for reading and writing, and returns
/// their address.
fn map(memory: sys::Handle, size: u64) -> Option<u64> {
    let mut address = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            crate::process(),
            PERM_READ | PERM_WRITE,
            0,
            memory,
            0,
            size,
            &mut address,
        )
    };
    (status == mikros_rt::OK).then_some(address)
}

/// Fills the `rows` lines of `width` pixels at `address`, `stride` bytes
/// apart, with `colour`.
fn fill(address: u64, stride: u32, width: u32, rows: u32, colour: u32) {
    for y in 0..u64::from(rows) {
        for x in 0..u64::from(width) {
            let at = (address + y * u64::from(stride) + x * 4) as *mut u32;
            // SAFETY: the caller mapped the lines.
            unsafe { at.write_volatile(colour) };
        }
    }
}

/// Waits for `OnCommitComplete` of `commit_id`.
async fn completed(display: &DisplayAsyncClient, reactor: &Reactor, commit_id: u64) -> Option<()> {
    let deadline = now() + COMPLETE_LIMIT;
    loop {
        let event = display.next_event();
        let timeout = reactor.sleep_until(deadline);
        match crate::first(event, timeout).await {
            Some(Ok(DisplayEvent::OnCommitComplete(c))) if c.commit_id == commit_id => {
                return Some(());
            }
            Some(Ok(_)) => {}
            _ => return failed("OnCommitComplete"),
        }
    }
}

/// Owns the display, fills it with `PAINT`, its top half from a dumb
/// buffer and then its bottom half from an imported memory object, and
/// returns the display's connection once the commit that shows the
/// bottom half completed: None when any step fails. Closing the
/// connection gives the display up.
pub async fn paint(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Option<DisplayAsyncClient> {
    let Some(device) = open(directory, reactor).await else {
        return failed("connecting to the gpu service");
    };
    match device.get_info().await {
        Ok(r) if r.info.name == "Basic Display" && r.info.capabilities == Capabilities::DISPLAY => {
        }
        _ => return failed("GetInfo"),
    }
    let (_, render) = endpoints::<Render>().ok()?;
    if !matches!(
        device.open_render(render).await,
        Ok(Err(Status::NotSupported))
    ) {
        return failed("refusing OpenRender");
    }
    let (client, server) = endpoints::<Display>().ok()?;
    if !matches!(device.open_display(server, None).await, Ok(Ok(_))) {
        return failed("OpenDisplay");
    }
    let display = DisplayAsyncClient::new(client, reactor.clone());
    // A second owner is refused while the first is open.
    let other = open(directory, reactor).await?;
    let (_, server) = endpoints::<Display>().ok()?;
    if !matches!(
        other.open_display(server, None).await,
        Ok(Err(Status::AlreadyExists))
    ) {
        return failed("refusing a second OpenDisplay");
    }
    let Ok(resources) = display.get_resources().await else {
        return failed("GetResources");
    };
    let resources = resources.resources;
    let [connector] = &resources.connectors[..] else {
        return failed("finding one connector");
    };
    let [mode] = &connector.modes[..] else {
        return failed("finding one mode");
    };
    if connector.r#type != ConnectorType::BootFramebuffer || !connector.connected {
        return failed("the connector's type");
    }
    let mode = Mode {
        width: mode.width,
        height: mode.height,
        refresh_millihertz: mode.refresh_millihertz,
        flags: mode.flags,
    };
    let created = match display
        .create_dumb_buffer(mode.width, mode.height, XRGB8888)
        .await
    {
        Ok(Ok(r)) => r.buffer,
        _ => return failed("CreateDumbBuffer"),
    };
    let Some(address) = map(created.memory.raw(), created.size) else {
        return failed("mapping the dumb buffer");
    };
    // The bottom half stays black until the imported buffer covers it.
    let top = mode.height / 2;
    fill(address, created.stride, mode.width, top, PAINT);
    let mut wrong = mode_copy(&mode);
    wrong.width += 1;
    let refused = display
        .commit(commit(CommitFlags::TEST_ONLY, wrong, created.buffer))
        .await;
    if !matches!(refused, Ok(Err(Status::InvalidArgs))) {
        return failed("refusing another mode");
    }
    let shown = display
        .commit(commit(CommitFlags(0), mode_copy(&mode), created.buffer))
        .await;
    let Ok(Ok(shown)) = shown else {
        return failed("Commit");
    };
    completed(&display, reactor, shown.commit_id).await?;
    let bottom = mode.height - top;
    let stride = mode.width * 4;
    // The image starts one page in, so that its offset is used.
    let size = (PAGE_SIZE + u64::from(stride) * u64::from(bottom)).next_multiple_of(PAGE_SIZE);
    let Ok(memory) = mikros_rt::objects::memory(size, 0) else {
        return failed("creating a memory object");
    };
    let Some(address) = map(memory.raw(), size) else {
        return failed("mapping the memory object");
    };
    fill(address + PAGE_SIZE, stride, mode.width, bottom, PAINT);
    let duplicate = || mikros_rt::objects::duplicate(memory.raw(), SAME_RIGHTS).ok();
    // An image that runs past the object's end, and lines shorter than
    // the image's width, are refused.
    let past_end = display
        .import_memory(
            duplicate()?,
            mode.width,
            bottom + 1,
            XRGB8888,
            stride,
            size - u64::from(stride) * u64::from(bottom),
        )
        .await;
    if !matches!(past_end, Ok(Err(Status::InvalidArgs))) {
        return failed("refusing an image past the memory object's end");
    }
    let short = display
        .import_memory(
            duplicate()?,
            mode.width,
            bottom,
            XRGB8888,
            stride - 4,
            PAGE_SIZE,
        )
        .await;
    if !matches!(short, Ok(Err(Status::InvalidArgs))) {
        return failed("refusing a stride shorter than a line");
    }
    let imported = display
        .import_memory(memory, mode.width, bottom, XRGB8888, stride, PAGE_SIZE)
        .await;
    let Ok(Ok(imported)) = imported else {
        return failed("ImportMemory");
    };
    let src = Rect {
        x: 0,
        y: 0,
        width: mode.width,
        height: bottom,
    };
    let dst = Rect {
        y: top as i32,
        ..src
    };
    let shown = display
        .commit(placed(CommitFlags(0), mode, imported.buffer, src, dst))
        .await;
    let Ok(Ok(shown)) = shown else {
        return failed("committing the imported buffer");
    };
    completed(&display, reactor, shown.commit_id).await?;
    Some(display)
}

fn mode_copy(m: &Mode) -> Mode {
    Mode {
        width: m.width,
        height: m.height,
        refresh_millihertz: m.refresh_millihertz,
        flags: m.flags,
    }
}
