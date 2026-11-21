//! The compositor on Mikros. It serves the `Seat` channel, through which
//! the session manager gives it the display and the input devices, shows
//! the toplevels of its clients on the display, and serves each client's
//! Wayland channel: a channel message's requests go to the server, and the
//! events they cause go back once they are handled. A frame is drawn
//! whenever a surface commits: once each shown buffer's acquire point is
//! reached, the wallpaper and the buffers are drawn into an output image,
//! the release points of buffers that no surface shows any more are
//! reached, and the image is shown.

mod host;
mod imports;
mod keys;
mod output;
mod parts;
mod pipeline;
mod render;
mod session;
mod themes;

use crate::haze;
use crate::server::{ClientId, Server};
use crate::wayland::wire::{CHANNEL_BYTES, CHANNEL_HANDLES, Fd};
use imports::{Importer, Store};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, Device as Gpu, DeviceAsyncClient as GpuClient, DisplayAsyncClient,
};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{SeatAsyncServer, SeatRequest};
use output::Output;
use render::{Haze, Layer, Look, Renderer, Under};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::poll_fn;
use std::os::mikros::startup;
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::Duration;

/// The `USER` startup handles: the `Login` channel of greeter mode, and
/// the `Seat` channel.
const USER: u32 = 16;
const SESSION_ID: u32 = 2;
const LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
const SAME_RIGHTS: u32 = 1 << 31;
const GPU: &str = "device.gpu";
/// The colour behind every surface, until the theme gives one.
const BACKGROUND: [f32; 4] = [0.11, 0.12, 0.14, 1.0];

/// The time between the frames of a haze that fades in, in nanoseconds.
const FADE_FRAME: i64 = 16_000_000;

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
    /// The compositor's `Launcher` client, which starts the shell's parts
    /// and waits for them.
    launcher: Rc<LauncherAsyncClient>,
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
                            self.server
                                .borrow_mut()
                                .set_screen((width as i32, height as i32));
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
        self.serve(client, channel).await;
    }

    /// Serves the Wayland channel of a shell component of `roles`, which
    /// the compositor created.
    async fn serve_component(self: Rc<Self>, channel: OwnedHandle, roles: Vec<String>) {
        let client = self.server.borrow_mut().connect_component(roles);
        self.serve(client, channel).await;
    }

    /// Serves the Wayland channel of `client` until it closes or breaks
    /// the protocol.
    async fn serve(self: Rc<Self>, client: ClientId, channel: OwnedHandle) {
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
            let error = match self.server.borrow_mut().client(client) {
                None => Some(String::from("the client is gone")),
                Some(c) => c.error.as_ref().map(|e| format!("{e:?}")),
            };
            if let Some(error) = error {
                mikros_rt::log!(
                    Error,
                    "compositor: client {client} broke the protocol: {error}"
                );
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
            // What the frame shows, each buffer once its acquire point is
            // reached.
            let shown = self.server.borrow().shown();
            for s in &shown {
                let Some(point) = s.acquire else {
                    continue;
                };
                let timeline = self.store.borrow().timeline(point.timeline);
                if let Some(timeline) = timeline {
                    let _ = self.reactor.timeline(timeline, point.value).await;
                }
            }
            let (drawn, fading) = {
                let store = self.store.borrow();
                let target = output.back();
                // The haze of the output's size, fading in for 250 ms
                // after it was made (MIKSPEC-0066.R18).
                let elapsed =
                    (mikros_rt::objects::now() - store.haze.as_ref().map_or(0, |h| h.made)).max(0);
                let haze = store
                    .haze
                    .as_ref()
                    .filter(|h| (h.new.width, h.new.height) == (target.width, target.height))
                    .map(|h| Haze {
                        new: &h.new,
                        old: h.old.as_ref().unwrap_or(&h.new),
                        fade: match h.old {
                            Some(_) => haze::faded_in(Duration::from_nanos(elapsed as u64)),
                            None => 1.0,
                        },
                    });
                // The wallpaper below every surface (MIKSPEC-0075.R22).
                let wallpaper = store.wallpaper.as_ref().map(|w| Layer {
                    pixels: w.buffer.pixels(),
                    x: 0,
                    y: 0,
                    under: Under::Flat,
                    haze: &[],
                    blur: &[],
                });
                // Every toplevel is told that it is activated, so each
                // shows the haze, as panels do.
                let surfaces = shown.iter().filter_map(|s| {
                    Some(Layer {
                        pixels: store.buffers.get(&s.buffer)?.pixels(),
                        x: s.x,
                        y: s.y,
                        under: Under::Haze,
                        haze: &s.haze,
                        blur: &s.blur,
                    })
                });
                let layers: Vec<Layer> = wallpaper.into_iter().chain(surfaces).collect();
                let look = Look {
                    flat: haze::flat(store.scheme),
                    translucent: haze::translucent(store.scheme),
                };
                let fading = haze.as_ref().is_some_and(|h| h.fade < 1.0);
                let drawn = self
                    .renderer
                    .draw(target, BACKGROUND, &layers, haze.as_ref(), &look);
                (drawn, fading)
            };
            if let Err(e) = drawn {
                mikros_rt::log!(Error, "compositor: drawing a frame: {e:?}");
            }
            if !fading {
                self.store.borrow_mut().faded_in(&self.renderer);
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
            // While the haze fades in, frames follow each other.
            if fading {
                let next = mikros_rt::objects::now() + FADE_FRAME;
                let _ = self.reactor.sleep_until(next).await;
                self.changed.notify();
            }
        }
    }
}

impl Compositor {
    /// Starts the greeter, the one part of greeter mode, with the `Login`
    /// channel, and serves its Wayland connection.
    async fn greet(self: Rc<Self>, login: OwnedHandle) {
        match parts::find("greeter") {
            Ok(part) => self.run_part(part, Some(login), "the greeter".into()).await,
            Err(e) => mikros_rt::log!(Error, "compositor: no greeter: {e:?}"),
        }
    }

    /// Starts every part of the packages of a user session, one after
    /// another, and serves their Wayland connections (MIKSPEC-0075.R20). A
    /// package that cannot be found leaves its roles empty.
    async fn start_shell(self: Rc<Self>) {
        for (key, package) in parts::session_packages() {
            let package = match package {
                Ok(package) => package,
                Err(e) => {
                    mikros_rt::log!(Error, "compositor: no {key}: {e:?}");
                    continue;
                }
            };
            let found = match parts::parts_of(&package) {
                Ok(found) => found,
                Err(e) => {
                    mikros_rt::log!(Error, "compositor: the {key} {package}: {e:?}");
                    continue;
                }
            };
            for part in found {
                let name = format!("the part {} of {package}", part.roles.join(" "));
                if let Some(serving) = self.clone().start_part(part, None, name).await {
                    self.spawner.spawn(serving);
                }
            }
        }
    }

    /// Starts `part`, called `name` in the log, with `login` in greeter
    /// mode, logs how it exits, and serves its Wayland connection.
    async fn run_part(self: Rc<Self>, part: parts::Part, login: Option<OwnedHandle>, name: String) {
        if let Some(serving) = self.start_part(part, login, name).await {
            serving.await;
        }
    }

    /// Starts `part` as `run_part` does, and returns the serving of its
    /// Wayland connection once it has started.
    async fn start_part(
        self: Rc<Self>,
        part: parts::Part,
        login: Option<OwnedHandle>,
        name: String,
    ) -> Option<impl std::future::Future<Output = ()> + 'static> {
        let Ok((ours, theirs)) = mikros_rt::midl::channel_create() else {
            return None;
        };
        // SAFETY: the kernel just made both ends for us.
        let (ours, theirs) =
            unsafe { (OwnedHandle::from_raw(ours), OwnedHandle::from_raw(theirs)) };
        let pid = match parts::start(&self.launcher, &part, theirs, login).await {
            Ok(pid) => pid,
            Err(e) => {
                mikros_rt::log!(Error, "compositor: starting {name}: {e}");
                return None;
            }
        };
        mikros_rt::log!(Info, "compositor: started {name} as pid {pid}");
        let launcher = self.launcher.clone();
        self.spawner.spawn(async move {
            match parts::exited(&launcher, pid).await {
                Ok(0) => mikros_rt::log!(Info, "compositor: {name} exited"),
                Ok(code) => mikros_rt::log!(Error, "compositor: {name} exited with {code}"),
                Err(e) => mikros_rt::log!(Error, "compositor: waiting for {name}: {e}"),
            }
        });
        Some(self.serve_component(ours, part.roles))
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
    // A panic's message goes to the log too.
    std::panic::set_hook(Box::new(|info| {
        mikros_rt::log!(Error, "compositor: {info}");
    }));
    let Some(seat) = take(SEAT_ID) else {
        mikros_rt::log!(Error, "compositor: the Seat channel is missing");
        std::process::exit(1);
    };
    // In greeter mode, the Login channel goes to the greeter; in a user
    // session, the session manager asks its questions on Session.
    let login = take(LOGIN_ID);
    let session = take(SESSION_ID);
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
    let launcher = match parts::launcher(&reactor) {
        Ok(launcher) => launcher,
        Err(e) => {
            mikros_rt::log!(Error, "compositor: {e}");
            std::process::exit(1);
        }
    };
    let directory = DirectoryAsyncClient::new(directory, reactor.clone());
    let mut server = Server::new();
    server.buffer_formats = imports::FORMATS.to_vec();
    let compositor = Rc::new(Compositor {
        reactor: reactor.clone(),
        spawner: spawner.clone(),
        launcher: Rc::new(launcher),
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
        c.spawner.spawn(c.clone().themes());
        c.spawner.spawn(c.clone().frames());
        match login {
            Some(login) => c.spawner.spawn(c.clone().greet(login)),
            None => c.spawner.spawn(c.clone().start_shell()),
        }
        if let Some(session) = session {
            c.spawner.spawn(session::serve(c.reactor.clone(), session));
        }
        c.clone().serve_seat(seat).await;
    });
}
