//! The display: the basic display driver's device and its one owner,
//! a commit that refuses another mode, and one that fills the screen.

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
const COMPLETE_LIMIT: i64 = 5_000_000_000;

fn commit(flags: CommitFlags, mode: Mode, buffer: u32) -> Commit {
    let (width, height) = (mode.width, mode.height);
    let whole = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
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
            src: Rect { ..whole },
            dst: whole,
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

/// Owns the display, fills it with `PAINT`, and returns once the commit
/// that shows it completed: None when any step fails.
pub async fn paint(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<()> {
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
    let mut address = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            crate::process(),
            PERM_READ | PERM_WRITE,
            0,
            created.memory.raw(),
            0,
            created.size,
            &mut address,
        )
    };
    if status != mikros_rt::OK {
        return failed("mapping the dumb buffer");
    }
    for y in 0..u64::from(mode.height) {
        for x in 0..u64::from(mode.width) {
            let at = (address + y * u64::from(created.stride) + x * 4) as *mut u32;
            // SAFETY: the pixel lies within the mapped buffer.
            unsafe { at.write_volatile(PAINT) };
        }
    }
    let mut wrong = mode_copy(&mode);
    wrong.width += 1;
    let refused = display
        .commit(commit(CommitFlags::TEST_ONLY, wrong, created.buffer))
        .await;
    if !matches!(refused, Ok(Err(Status::InvalidArgs))) {
        return failed("refusing another mode");
    }
    let shown = display
        .commit(commit(CommitFlags(0), mode, created.buffer))
        .await;
    let Ok(Ok(shown)) = shown else {
        return failed("Commit");
    };
    let deadline = now() + COMPLETE_LIMIT;
    loop {
        let event = display.next_event();
        let timeout = reactor.sleep_until(deadline);
        match crate::first(event, timeout).await {
            Some(Ok(DisplayEvent::OnCommitComplete(c))) if c.commit_id == shown.commit_id => {
                return Some(());
            }
            Some(Ok(_)) => {}
            _ => return failed("OnCommitComplete"),
        }
    }
}

fn mode_copy(m: &Mode) -> Mode {
    Mode {
        width: m.width,
        height: m.height,
        refresh_millihertz: m.refresh_millihertz,
        flags: m.flags,
    }
}
