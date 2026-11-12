//! Serving GPU devices for drivers: publishing one service of class
//! `gpu`, which serves `mikros.gpu.Device`, letting one client at a time
//! that holds `gpu.display` own the display through a `Display`
//! connection, closing it when its revocation event is signaled, and
//! showing the system log on the driver's screen while no client owns
//! the display.

#![no_std]

extern crate alloc;

mod log;

pub use log::Screen;

use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::{Poll, Waker};
use log::Log;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::DeviceManagerAsyncClient;
use mikros_rt::interfaces::mikros::gpu::{
    Device, DeviceAsyncServer, DeviceGetInfoResponse, DeviceInfo, DeviceOpenDisplayResponse,
    DeviceOpenRenderResponse, DeviceRequest, DisplayAsyncServer, Render,
};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, Provider, ProviderAsyncServer, ProviderRequest,
};

/// The service class of GPU devices, and its protocol in the service
/// directory.
const CLASS: &str = "gpu";
const PROTOCOL: &str = "device.gpu";
const BROKER: &str = "mikros.permissions.Broker";
/// The permission that owning the display takes.
const GPU_DISPLAY: &str = "gpu.display";
/// The signal of the revocation event.
const SIGNALED: u32 = 1 << 3;

/// What a driver does for its device beyond what the library does.
pub trait Backend {
    /// What `GetInfo` reports.
    fn info(&self) -> DeviceInfo;

    /// Serves the display owner's `Display` connection until it closes.
    fn serve(self: Rc<Self>, display: Rc<DisplayAsyncServer>)
    -> impl Future<Output = ()> + 'static;

    /// Frees what the closed `Display` connection had.
    fn closed(&self);

    /// Answers `OpenRender` with `render`.
    fn open_render(&self, render: ServerEnd<Render>) -> Result<(), Status>;

    /// Calls `draw` with the screen that shows the log while no client
    /// owns the display, when the driver has one.
    fn with_screen(&self, draw: &mut dyn FnMut(&dyn Screen));
}

/// A published GPU device.
pub struct GpuDevice<B> {
    backend: Rc<B>,
    broker: Option<BrokerAsyncClient>,
    /// The open `Display` connection.
    owner: RefCell<Option<Rc<DisplayAsyncServer>>>,
    log: Log,
    /// Set when the log's drawing is out of date, with the drawing task
    /// to wake.
    stale: Cell<bool>,
    drawer: RefCell<Option<Waker>>,
    reactor: Reactor,
    spawner: Spawner,
}

impl<B: Backend + 'static> GpuDevice<B> {
    /// Publishes the device of `backend` with `manager`, serves the
    /// connections made to it with `spawner`, and reads the log through
    /// `directory`, which also leads to the permission broker.
    pub async fn publish(
        backend: Rc<B>,
        manager: &DeviceManagerAsyncClient,
        directory: Rc<DirectoryAsyncClient>,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Result<Rc<GpuDevice<B>>, Status> {
        let device = Rc::new(GpuDevice {
            backend,
            broker: broker(&directory, &reactor).await,
            owner: RefCell::new(None),
            log: Log::new(),
            stale: Cell::new(false),
            drawer: RefCell::new(None),
            reactor,
            spawner,
        });
        let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
        match manager.publish_service(CLASS.into(), client).await {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        }
        device.spawner.spawn(device.clone().serve_provider(server));
        device.spawner.spawn(log::follow(device.clone(), directory));
        device.spawner.spawn(device.clone().draw_logs());
        Ok(device)
    }

    pub fn backend(&self) -> &Rc<B> {
        &self.backend
    }

    /// Marks the log's drawing out of date, for `draw_logs` to bring up to
    /// date while no client owns the display.
    fn draw_log(&self) {
        self.stale.set(true);
        if let Some(drawer) = self.drawer.borrow_mut().take() {
            drawer.wake();
        }
    }

    /// Draws what changed of the log whenever it is out of date, while no
    /// client owns the display, `ROWS_PER_TURN` rows at a time with other
    /// tasks running between: drawing the whole screen can take seconds,
    /// and the display's owner must not wait for it.
    async fn draw_logs(self: Rc<Self>) {
        loop {
            core::future::poll_fn(|cx| {
                if self.stale.replace(false) {
                    return Poll::Ready(());
                }
                *self.drawer.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            })
            .await;
            loop {
                if self.owner.borrow().is_some() {
                    break;
                }
                let mut done = true;
                self.backend
                    .with_screen(&mut |screen| done = self.log.draw(screen, ROWS_PER_TURN));
                if done {
                    break;
                }
                yield_now().await;
            }
        }
    }

    async fn serve_provider(self: Rc<Self>, end: ServerEnd<Provider>) {
        let provider = ProviderAsyncServer::new(end, self.reactor.clone());
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Dropping the server end of another protocol closes it.
            if c.protocol == PROTOCOL {
                let end = ServerEnd::<Device>::new(c.server);
                self.spawner.spawn(self.clone().serve_device(end, c.client));
            }
        }
    }

    /// Whether `client` holds `gpu.display`, as the broker says.
    async fn may_own(&self, client: &ClientIdentity) -> bool {
        let Some(broker) = &self.broker else {
            return false;
        };
        let client = ClientIdentity {
            pid: client.pid,
            user: client.user,
            app_id: client.app_id.clone(),
        };
        let checked = broker.check(client, GPU_DISPLAY.into()).await;
        matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }

    async fn serve_device(self: Rc<Self>, end: ServerEnd<Device>, client: ClientIdentity) {
        let server = DeviceAsyncServer::new(end, self.reactor.clone());
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DeviceRequest::GetInfo(_, r) => r.reply(
                    s,
                    DeviceGetInfoResponse {
                        info: self.backend.info(),
                    },
                ),
                DeviceRequest::OpenDisplay(q, r) => {
                    let result = if !self.may_own(&client).await {
                        Err(Status::AccessDenied)
                    } else if self.owner.borrow().is_some() {
                        Err(Status::AlreadyExists)
                    } else {
                        // The owner's place is taken before the connection
                        // is first served, so that the next `OpenDisplay`
                        // fails, and before the reply, so that the log is
                        // no longer drawn.
                        let owner =
                            Rc::new(DisplayAsyncServer::new(q.display, self.reactor.clone()));
                        *self.owner.borrow_mut() = Some(owner.clone());
                        self.spawner.spawn(self.clone().own(owner, q.revoke));
                        Ok(DeviceOpenDisplayResponse {})
                    };
                    r.reply(s, result)
                }
                DeviceRequest::OpenRender(q, r) => {
                    let result = self.backend.open_render(q.render);
                    r.reply(s, result.map(|()| DeviceOpenRenderResponse {}))
                }
            };
        }
    }

    /// Serves the display owner until it closes its connection or
    /// `revoke` is signaled, then frees what the connection had and
    /// draws the whole log.
    async fn own(self: Rc<Self>, owner: Rc<DisplayAsyncServer>, revoke: Option<OwnedHandle>) {
        let serving = self.backend.clone().serve(owner);
        match &revoke {
            Some(event) => first(serving, self.reactor.signals(event.raw(), SIGNALED)).await,
            None => serving.await,
        }
        *self.owner.borrow_mut() = None;
        self.backend.closed();
        self.log.forget();
        self.draw_log();
    }
}

/// The permission broker, through `directory`: None when it cannot be
/// reached, so that no client gets the display.
async fn broker(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<BrokerAsyncClient> {
    let (client, server) = endpoints::<Broker>().ok()?;
    match directory
        .connect(BROKER.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => Some(BrokerAsyncClient::new(client, reactor.clone())),
        _ => None,
    }
}

/// Completes when the first of `a` and `b` completes.
async fn first<A: Future, B: Future>(a: A, b: B) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if a.as_mut().poll(cx).is_ready() || b.as_mut().poll(cx).is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The most log rows drawn before other tasks get to run.
const ROWS_PER_TURN: usize = 4;

/// Lets the executor run the other ready tasks once.
async fn yield_now() {
    let mut yielded = false;
    core::future::poll_fn(|cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}
