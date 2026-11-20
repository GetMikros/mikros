//! The compositor on Mikros. It serves the `Seat` channel, through which
//! the session manager gives it the display and the input devices, shows
//! the toplevels of its clients on the display, and serves each client's
//! Wayland channel: a channel message's requests go to the server, and the
//! events they cause go back once they are handled. A frame is drawn
//! whenever a surface commits: once each shown buffer's acquire point is
//! reached, the buffers are copied into an output image, the release
//! points of buffers that no surface shows any more are reached, and the
//! image is shown.

mod imports;
mod keys;
mod output;
mod parts;
mod render;

use crate::server::{ClientId, Role, Server};
use crate::wayland::wire::{CHANNEL_BYTES, CHANNEL_HANDLES, Fd};
use imports::{Importer, Store};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, Device as Gpu, DeviceAsyncClient as GpuClient, DisplayAsyncClient,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{SeatAsyncServer, SeatRequest};
use output::Output;
use render::{Layer, Renderer};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::poll_fn;
use std::os::mikros::startup;
use std::rc::Rc;
use std::task::{Poll, Waker};

/// The `USER` startup handles: the `Login` channel of greeter mode, and
/// the `Seat` channel.
const USER: u32 = 16;
const LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
const SAME_RIGHTS: u32 = 1 << 31;
const GPU: &str = "device.gpu";
/// The colour behind every surface, until the theme gives one.
const BACKGROUND: [f32; 4] = [0.11, 0.12, 0.14, 1.0];

/// Completes the wait of `changed` after the next `notify`, or at once
/// when one came since the last wait.
#[derive(Default)]
struct Notify {
    waiters: RefCell<Vec<Waker>>,
    pending: Cell<bool>,
}

impl Notify {
    async fn changed(&self) {
        poll_fn(|cx| {
            if self.pending.replace(false) {
                return Poll::Ready(());
            }
            self.waiters.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn notify(&self) {
        self.pending.set(true);
        for waker in self.waiters.take() {
            waker.wake();
        }
    }
}

pub struct Compositor {
    reactor: Reactor,
    spawner: Spawner,
    directory: DirectoryAsyncClient,
    server: RefCell<Server>,
    renderer: Renderer,
    store: RefCell<Store>,
    /// The display, once the session manager gave one.
    output: RefCell<Option<Output>>,
    /// The instance of the display shown on, which is shown on again when
    /// the session manager gives it again.
    display_instance: RefCell<Option<String>>,
    /// The channels of the clients.
    channels: RefCell<BTreeMap<ClientId, OwnedHandle>>,
    /// Set when a surface committed, or the display came.
    changed: Notify,
}

/// The startup handle `id` of the `USER` role, as mikros-rt's handle.
fn take(id: u32) -> Option<OwnedHandle> {
    let handle = startup::take_handle(USER, id)?;
    // SAFETY: the handle is ours, and is handed over once.
    Some(unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) })
}

fn duplicate(raw: u32) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { mikros_rt::sys::handle_duplicate(raw, SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// Milliseconds of the monotonic clock, for frame callbacks.
fn milliseconds() -> u32 {
    (mikros_rt::objects::now() / 1_000_000) as u32
}

impl Compositor {
    /// Serves the `Seat` channel: the first display becomes the output,
    /// also when it is given again after it was taken away, and the
    /// keyboards are read for the keys that switch screens.
    async fn serve_seat(self: Rc<Self>, seat: OwnedHandle) {
        let server = Rc::new(SeatAsyncServer::new(
            ServerEnd::new(seat),
            self.reactor.clone(),
        ));
        while let Ok(request) = server.next().await {
            match request {
                SeatRequest::AddDisplay(added) => {
                    let shown_on = self.display_instance.borrow().clone();
                    if shown_on.is_some_and(|shown_on| shown_on != added.instance) {
                        continue;
                    }
                    *self.display_instance.borrow_mut() = Some(added.instance.clone());
                    *self.output.borrow_mut() = None;
                    let display = DisplayAsyncClient::new(added.display, self.reactor.clone());
                    match Output::new(display, &self.renderer).await {
                        Ok(output) => {
                            let (width, height) = output.size();
                            mikros_rt::log!(
                                Info,
                                "compositor: showing {width}x{height} on the display {}",
                                added.instance
                            );
                            self.server.borrow_mut().screen = Some((width as i32, height as i32));
                            *self.output.borrow_mut() = Some(output);
                            self.changed.notify();
                        }
                        Err(e) => mikros_rt::log!(
                            Error,
                            "compositor: the display {}: {e}",
                            added.instance
                        ),
                    }
                }
                SeatRequest::AddInput(added) => {
                    let (reactor, server) = (self.reactor.clone(), server.clone());
                    let (instance, device) = (added.instance, added.device);
                    let reading = keys::read(reactor, server, instance, device, added.accepted);
                    self.spawner.spawn(reading);
                }
                // The compositor shows only the desktop's screen.
                SeatRequest::SetScreen(_) => {}
            }
        }
    }

    /// Serves the Wayland channel of a client until it closes or breaks
    /// the protocol.
    pub async fn serve_client(self: Rc<Self>, channel: OwnedHandle) {
        let client = self.server.borrow_mut().connect();
        let raw = channel.raw();
        self.channels.borrow_mut().insert(client, channel);
        loop {
            let (bytes, handles) = match mikros_rt::midl::channel_read(raw) {
                Ok(message) => message,
                Err(mikros_rt::midl::SHOULD_WAIT) => {
                    if self.reactor.readable(raw).await.is_err() {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            };
            // SAFETY: the kernel gave us these handles.
            let fds = handles
                .into_iter()
                .map(|h| unsafe { Fd::from_raw_handle(h) })
                .collect();
            {
                let mut store = self.store.borrow_mut();
                let mut importer = Importer {
                    renderer: &self.renderer,
                    store: &mut store,
                };
                self.server
                    .borrow_mut()
                    .dispatch(&mut importer, client, &bytes, fds);
            }
            self.flush(client);
            self.changed.notify();
            let failed = self
                .server
                .borrow_mut()
                .client(client)
                .is_none_or(|c| c.error.is_some());
            if failed {
                break;
            }
        }
        self.channels.borrow_mut().remove(&client);
        let mut store = self.store.borrow_mut();
        let mut importer = Importer {
            renderer: &self.renderer,
            store: &mut store,
        };
        self.server.borrow_mut().disconnect(&mut importer, client);
        self.changed.notify();
    }

    /// Sends a client the events it has, whole messages in each channel
    /// message.
    fn flush(&self, client: ClientId) {
        let channels = self.channels.borrow();
        let Some(channel) = channels.get(&client) else {
            return;
        };
        let mut server = self.server.borrow_mut();
        let Some(c) = server.client(client) else {
            return;
        };
        while let Some((bytes, fds)) = c.out.take() {
            debug_assert!(bytes.len() <= CHANNEL_BYTES && fds.len() <= CHANNEL_HANDLES);
            let handles: Vec<u32> = fds.into_iter().map(Fd::into_raw_handle).collect();
            if mikros_rt::midl::channel_write(channel.raw(), &bytes, &handles).is_err() {
                return;
            }
        }
    }

    fn flush_all(&self) {
        let clients: Vec<ClientId> = self.channels.borrow().keys().copied().collect();
        for client in clients {
            self.flush(client);
        }
    }

    /// Draws and shows a frame whenever something changed.
    async fn frames(self: Rc<Self>) {
        loop {
            self.changed.changed().await;
            let Some(mut output) = self.output.borrow_mut().take() else {
                continue;
            };
            // The buffers of the toplevels, oldest surface first, each
            // once its acquire point is reached.
            let shown: Vec<_> = self
                .server
                .borrow()
                .surfaces()
                .filter(|(_, s)| s.role == Role::Toplevel)
                .filter_map(|(_, s)| Some((s.current.buffer?, s.current.acquire)))
                .collect();
            for &(_, acquire) in &shown {
                let Some(point) = acquire else {
                    continue;
                };
                let timeline = self.store.borrow().timeline(point.timeline);
                if let Some(timeline) = timeline {
                    let _ = self.reactor.timeline(timeline, point.value).await;
                }
            }
            let drawn = {
                let store = self.store.borrow();
                let layers: Vec<Layer> = shown
                    .iter()
                    .filter_map(|(buffer, _)| store.buffers.get(buffer))
                    .map(|buffer| Layer { buffer, x: 0, y: 0 })
                    .collect();
                self.renderer.draw(output.back(), BACKGROUND, &layers)
            };
            if let Err(e) = drawn {
                mikros_rt::log!(Error, "compositor: drawing a frame: {e:?}");
            }
            // No frame reads the buffers that surfaces no longer show.
            let releases = self.server.borrow_mut().take_releases();
            for point in releases {
                self.store.borrow().reach(point);
            }
            if let Err(e) = output.show().await {
                mikros_rt::log!(Error, "compositor: showing a frame: {e:?}");
            }
            // Unless the display was given again meanwhile.
            if self.output.borrow().is_none() {
                *self.output.borrow_mut() = Some(output);
            }
            self.server.borrow_mut().frames_done(milliseconds());
            self.flush_all();
        }
    }
}

impl Compositor {
    /// Starts the greeter, the one part of greeter mode, with the `Login`
    /// channel, and serves its Wayland connection.
    async fn greet(self: Rc<Self>, login: OwnedHandle) {
        let part = match parts::find("greeter") {
            Ok(part) => part,
            Err(e) => {
                mikros_rt::log!(Error, "compositor: no greeter: {e:?}");
                return;
            }
        };
        let Ok((ours, theirs)) = mikros_rt::midl::channel_create() else {
            return;
        };
        // SAFETY: the kernel just made both ends for us.
        let (ours, theirs) =
            unsafe { (OwnedHandle::from_raw(ours), OwnedHandle::from_raw(theirs)) };
        let pid = match parts::start(&self.reactor, &part, theirs, Some(login)).await {
            Ok(pid) => pid,
            Err(e) => {
                mikros_rt::log!(Error, "compositor: starting the greeter: {e}");
                return;
            }
        };
        mikros_rt::log!(Info, "compositor: started the greeter as pid {pid}");
        let reactor = self.reactor.clone();
        self.spawner.spawn(async move {
            match parts::exited(&reactor, pid).await {
                Ok(0) => mikros_rt::log!(Info, "compositor: the greeter exited"),
                Ok(code) => mikros_rt::log!(Error, "compositor: the greeter exited with {code}"),
                Err(e) => mikros_rt::log!(Error, "compositor: waiting for the greeter: {e}"),
            }
        });
        self.serve_client(ours).await;
    }
}

/// The device name of the render device that clients should allocate
/// buffers on: the first GPU device with `RENDER`.
async fn render_device(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<String> {
    let listed = directory.list(GPU.into()).await.ok()?.ok()?;
    for instance in listed.instances {
        if !instance.starts_with(|c: char| c.is_ascii_lowercase()) {
            continue;
        }
        let Ok((client, server)) = endpoints::<Gpu>() else {
            continue;
        };
        let connected = directory
            .connect(GPU.into(), Some(instance.clone()), server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            continue;
        }
        let device = GpuClient::new(client, reactor.clone());
        if let Ok(r) = device.get_info().await
            && r.info.capabilities.contains(Capabilities::RENDER)
        {
            return Some(format!("gpu/{instance}"));
        }
    }
    None
}

pub fn main() {
    let Some(directory) = duplicate(std::os::mikros::directory().as_raw_handle()) else {
        std::process::exit(1);
    };
    // The messages go to the log service: the session manager gives no
    // standard error.
    let directory = ClientEnd::new(directory);
    mikros_rt::log::connect(&directory);
    let Some(seat) = take(SEAT_ID) else {
        mikros_rt::log!(Error, "compositor: the Seat channel is missing");
        std::process::exit(1);
    };
    // In greeter mode, the Login channel goes to the greeter.
    let login = take(LOGIN_ID);
    let renderer = match Renderer::new() {
        Ok(renderer) => renderer,
        Err(e) => {
            mikros_rt::log!(Error, "compositor: no Vulkan device: {e:?}");
            std::process::exit(1);
        }
    };
    let Ok(mut executor) = Executor::new() else {
        std::process::exit(1);
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let directory = DirectoryAsyncClient::new(directory, reactor.clone());
    let mut server = Server::new();
    server.buffer_formats = imports::FORMATS.to_vec();
    let compositor = Rc::new(Compositor {
        reactor: reactor.clone(),
        spawner: spawner.clone(),
        directory,
        server: RefCell::new(server),
        renderer,
        store: RefCell::default(),
        output: RefCell::new(None),
        display_instance: RefCell::new(None),
        channels: RefCell::default(),
        changed: Notify::default(),
    });
    let c = compositor.clone();
    executor.block_on(async move {
        if let Some(name) = render_device(&c.directory, &c.reactor).await {
            c.server.borrow_mut().buffer_device = name;
        }
        c.spawner.spawn(c.clone().frames());
        if let Some(login) = login {
            c.spawner.spawn(c.clone().greet(login));
        }
        c.clone().serve_seat(seat).await;
    });
}
