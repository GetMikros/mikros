//! The compositor on Mikros. It serves the `Seat` channel, through which
//! the session manager gives it the display and the input devices, shows
//! the toplevels of its clients on the display, and serves each client's
//! Wayland channel: a channel message's requests go to the server, and the
//! events they cause go back once they are handled. A frame is drawn
//! whenever something changed, and whenever an acquire point that a
//! buffer waits for is reached: each surface shows the newest buffer
//! whose acquire point is reached when the frame starts, which no frame
//! waits for (MIKSPEC-0074.R14). The wallpaper and the buffers are drawn
//! into an output image, the release points of buffers that no surface
//! shows any more are reached, and the image is shown.

mod host;
mod imports;
mod keys;
mod mouse;
mod output;
mod parts;
mod pipeline;
mod pointer;
mod render;
mod session;
mod themes;

use crate::haze;
use crate::server::{ClientId, Cursor, Point, Rect, Server, shape_name};
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
use std::future::{Future, poll_fn};
use std::os::mikros::startup;
use std::pin::pin;
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
    /// The pointer and its cursor (MIKSPEC-0066.R28).
    pointer: RefCell<pointer::Pointer>,
    /// Whether the next frame changes all of the output, and the cursor
    /// that the last frame drew, with where (MIKSPEC-0066.R17).
    damaged_all: Cell<bool>,
    drawn_cursor: Cell<Option<(Cursor, Rect)>>,
    /// The compositor itself, for the tasks it starts.
    me: std::rc::Weak<Compositor>,
    /// The clients whose events wait until their channels take more.
    flushing: RefCell<std::collections::BTreeSet<ClientId>>,
    /// The acquire points that a task watches, with what ends the watch
    /// (`watch_acquires`).
    acquiring: RefCell<BTreeMap<(crate::server::TimelineId, u64), Rc<Notify>>>,
    /// The keymap that the keyboards share, once one came, and the
    /// layout it was compiled from (`keys`).
    keymap: RefCell<Option<mikros_xkb::Keymap>>,
    layout: RefCell<Option<crate::keyboard::Layout>>,
}

/// The startup handle `id` of the `USER` role, as mikros-rt's handle.
fn take(id: u32) -> Option<OwnedHandle> {
    let handle = startup::take_handle(USER, id)?;
    // SAFETY: the handle is ours, and is handed over once.
    Some(unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) })
}

/// The most bytes and handles of events that a client may leave unread
/// (MIKSPEC-0074.R26).
const UNREAD_BYTES_MAX: usize = 4 << 20;
const UNREAD_FDS_MAX: usize = 1024;
/// The signals of a channel that it takes more, and that its peer closed.
const WRITABLE: u32 = 1 << 1;
const PEER_CLOSED: u32 = 1 << 2;

/// The value that the kernel timeline `timeline` has reached.
fn timeline_value(timeline: u32) -> Option<u64> {
    let mut value = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { mikros_rt::sys::timeline_get(timeline, &mut value) };
    (status == mikros_rt::OK).then_some(value)
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
                    let (instance, device) = (added.instance, added.device);
                    let reading =
                        self.clone()
                            .read_input(server.clone(), instance, device, added.accepted);
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

    /// Serves the Wayland channel of a shell component of `roles` and
    /// `app_id`, which the compositor created.
    async fn serve_component(
        self: Rc<Self>,
        channel: OwnedHandle,
        roles: Vec<String>,
        app_id: String,
    ) {
        let client = self.server.borrow_mut().connect_component(roles, app_id);
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
    /// message. Those that its full channel does not take wait, and are
    /// sent once it can take more; a client that leaves more of them than
    /// it may is disconnected, by closing its channel, which ends its
    /// serve loop (MIKSPEC-0074.R26).
    fn flush(&self, client: ClientId) {
        let Some(channel) = self.channels.borrow().get(&client).map(|c| c.raw()) else {
            return;
        };
        let mut server = self.server.borrow_mut();
        let Some(c) = server.client(client) else {
            return;
        };
        while let Some((bytes, fds)) = c.out.front() {
            debug_assert!(bytes.len() <= CHANNEL_BYTES && fds.len() <= CHANNEL_HANDLES);
            let handles: Vec<u32> = fds.iter().map(|fd| fd.as_raw_handle()).collect();
            match mikros_rt::midl::channel_write(channel, bytes, &handles) {
                // The handles went with the message.
                Ok(()) => {
                    for fd in c.out.sent() {
                        let _ = fd.into_raw_handle();
                    }
                }
                Err(mikros_rt::midl::SHOULD_WAIT) => break,
                // The serve loop ends a channel that broke.
                Err(_) => return,
            }
        }
        let (bytes, fds) = c.out.queued();
        let app_id = c.app_id.clone().unwrap_or_default();
        drop(server);
        if bytes == 0 {
            return;
        }
        let exceeded = match (bytes > UNREAD_BYTES_MAX, fds > UNREAD_FDS_MAX) {
            (true, _) => Some("4 MiB of unread events"),
            (_, true) => Some("1024 handles of unread events"),
            _ => None,
        };
        if let Some(limit) = exceeded {
            mikros_rt::log!(
                Error,
                "compositor: closed the connection of {app_id}, past {limit}"
            );
            self.channels.borrow_mut().remove(&client);
            return;
        }
        if !self.flushing.borrow_mut().insert(client) {
            return;
        }
        let (Some(me), Some(watched)) = (self.me.upgrade(), duplicate(channel)) else {
            self.flushing.borrow_mut().remove(&client);
            return;
        };
        self.spawner.spawn(async move {
            let writable = me.reactor.signals(watched.raw(), WRITABLE | PEER_CLOSED);
            let _ = writable.await;
            me.flushing.borrow_mut().remove(&client);
            me.flush(client);
        });
    }

    fn flush_all(&self) {
        let clients: Vec<ClientId> = self.channels.borrow().keys().copied().collect();
        for client in clients {
            self.flush(client);
        }
    }

    /// Watches each acquire point that a buffer waits for, once, on a
    /// duplicate of its timeline's handle, and wakes the frame loop once
    /// it is reached. The watch of a point that no buffer waits for any
    /// more ends.
    fn watch_acquires(self: &Rc<Self>) {
        let awaited: std::collections::BTreeSet<_> = self
            .server
            .borrow()
            .awaited()
            .into_iter()
            .map(|p| (p.timeline, p.value))
            .collect();
        self.acquiring.borrow_mut().retain(|key, ended| {
            let keep = awaited.contains(key);
            if !keep {
                ended.notify();
            }
            keep
        });
        for key @ (timeline, value) in awaited {
            if self.acquiring.borrow().contains_key(&key) {
                continue;
            }
            let handle = self.store.borrow().timeline(timeline);
            let Some(handle) = handle.and_then(duplicate) else {
                continue;
            };
            let ended = Rc::new(Notify::default());
            self.acquiring.borrow_mut().insert(key, ended.clone());
            let me = self.clone();
            self.spawner.spawn(async move {
                let mut reached = pin!(me.reactor.timeline(handle.raw(), value));
                let mut ended = pin!(ended.changed());
                let woken = poll_fn(|cx| {
                    if reached.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(true);
                    }
                    match ended.as_mut().poll(cx) {
                        Poll::Ready(()) => Poll::Ready(false),
                        Poll::Pending => Poll::Pending,
                    }
                })
                .await;
                if woken {
                    me.acquiring.borrow_mut().remove(&key);
                    me.changed.notify();
                }
            });
        }
    }

    /// Draws and shows a frame whenever something changed.
    async fn frames(self: Rc<Self>) {
        loop {
            self.changed.changed().await;
            let Some(mut output) = self.output.borrow_mut().take() else {
                continue;
            };
            // The pointer goes to what now lies under it.
            {
                let mut server = self.server.borrow_mut();
                server.refocus();
                server.pointer_frame();
            }
            // Each surface shows the newest of its buffers whose acquire
            // point is reached now, and the frame loop wakes when one of
            // those that buffers still wait for is reached.
            {
                let store = self.store.borrow();
                let reached = |point: Point| match store.timeline(point.timeline) {
                    Some(timeline) => timeline_value(timeline).is_none_or(|v| v >= point.value),
                    None => true,
                };
                self.server.borrow_mut().settle(reached);
            }
            self.watch_acquires();
            // What the frame shows, and the cursor of the client under the
            // pointer.
            let shown = self.server.borrow().shown();
            let (cursor, at) = {
                let server = self.server.borrow();
                (
                    server.cursor(),
                    (server.pointer.x as i32, server.pointer.y as i32),
                )
            };
            let cursor_surface = match cursor {
                Cursor::Surface { surface, hotspot } => self
                    .server
                    .borrow()
                    .surfaces()
                    .find(|(id, _)| **id == surface)
                    .and_then(|(_, s)| Some((s.current.buffer?, hotspot))),
                _ => None,
            };
            // What changed: the server's damage, the cursor's old and new
            // area when it moved, changed or is animated, or everything
            // (MIKSPEC-0066.R17).
            let mut damage = self.server.borrow_mut().take_damage();
            let cursor_committed = match cursor {
                Cursor::Surface { surface, .. } => {
                    self.server.borrow_mut().take_cursor_damage(surface)
                }
                _ => false,
            };
            let (drawn, fading, next_cursor, changed) = {
                let mut store = self.store.borrow_mut();
                let whole = std::mem::take(&mut store.damaged_all);
                drop(store);
                let store = self.store.borrow();
                let mut pointer = self.pointer.borrow_mut();
                // The pointer above everything (MIKSPEC-0066.R17): a shape
                // of the cursor theme, or the client's own cursor surface
                // (MIKSPEC-0066.R28).
                let now_ms = milliseconds().into();
                let mut next_cursor = None;
                let cursor_part = match (cursor, cursor_surface) {
                    (Cursor::Shape(shape), _) => pointer
                        .image(&self.renderer, now_ms, shape_name(shape), at)
                        .map(|(image, corner, next)| {
                            next_cursor = next;
                            (image.pixels(), corner, (image.width, image.height))
                        }),
                    (Cursor::Surface { .. }, Some((buffer, (hx, hy)))) => store
                        .buffers
                        .get(&buffer)
                        .map(|b| (b.pixels(), (at.0 - hx, at.1 - hy), (b.width, b.height))),
                    _ => None,
                };
                let cursor_rect = cursor_part.map(|(_, (x, y), (width, height))| Rect {
                    x,
                    y,
                    width: width as i32,
                    height: height as i32,
                });
                let drawn_now = cursor_rect.map(|r| (cursor, r));
                let last = self.drawn_cursor.replace(drawn_now);
                if last != drawn_now || next_cursor.is_some() || cursor_committed {
                    damage.extend(last.map(|l| l.1));
                    damage.extend(cursor_rect);
                }
                let fading_now = store.haze.as_ref().is_some_and(|h| h.old.is_some());
                let whole = whole || fading_now || self.damaged_all.take();
                output.add_damage(&damage, whole);
                let target_damage = output.back_damage();
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
                let mut layers: Vec<Layer> = wallpaper.into_iter().chain(surfaces).collect();
                if let Some((pixels, (x, y), _)) = cursor_part {
                    layers.push(Layer {
                        pixels,
                        x,
                        y,
                        under: Under::Flat,
                        haze: &[],
                        blur: &[],
                    });
                }
                let look = Look {
                    flat: haze::flat(store.scheme),
                    translucent: haze::translucent(store.scheme),
                };
                let fading = haze.as_ref().is_some_and(|h| h.fade < 1.0);
                // The image is drawn and shown where it lacks something,
                // which a new output lacks all of; an image that lacks
                // nothing is neither.
                let changed = match target_damage {
                    None => Some(None),
                    Some(rects) if rects.is_empty() => None,
                    Some(rects) => Some(Some(rects)),
                };
                let drawn = match &changed {
                    None => Ok(()),
                    Some(rects) => self.renderer.draw(
                        target,
                        BACKGROUND,
                        &layers,
                        haze.as_ref(),
                        &look,
                        rects.as_deref(),
                    ),
                };
                (drawn, fading, next_cursor, changed)
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
            if let Some(changed) = &changed
                && let Err(e) = output.show(changed.as_deref()).await
            {
                mikros_rt::log!(Error, "compositor: showing a frame: {e:?}");
            }
            // Unless the display was given again meanwhile.
            if self.output.borrow().is_none() {
                *self.output.borrow_mut() = Some(output);
            }
            self.server.borrow_mut().frames_done(milliseconds());
            self.flush_all();
            // While the haze fades in, frames follow each other, and an
            // animated cursor asks for a frame when its next one is due.
            let wait = match (fading, next_cursor) {
                (true, _) => Some(FADE_FRAME),
                (false, Some(ms)) => Some(ms as i64 * 1_000_000),
                (false, None) => None,
            };
            if let Some(wait) = wait {
                let next = mikros_rt::objects::now() + wait;
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
        Some(self.serve_component(ours, part.roles, part.app_id))
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
    let compositor = Rc::new_cyclic(|me| Compositor {
        me: me.clone(),
        flushing: RefCell::default(),
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
        pointer: RefCell::new(pointer::Pointer::new()),
        damaged_all: Cell::new(false),
        drawn_cursor: Cell::new(None),
        acquiring: RefCell::default(),
        keymap: RefCell::new(None),
        layout: RefCell::new(None),
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
