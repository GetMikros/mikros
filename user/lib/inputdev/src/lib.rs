//! Serving input devices for drivers: publishing one service of class
//! `input` for each device, which serves `mikros.input.Input`, letting
//! one reader at a time open a `Device` connection, closing it when its
//! revocation event is signaled, and sending it what the driver reports.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_evdev::{Event, State, Stream};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::DeviceManagerAsyncClient;
use mikros_rt::interfaces::mikros::input::{
    self as proto, AbsInfo, AbsValue, DeviceAsyncServer, DeviceGetInfoResponse,
    DeviceGetStateResponse, DeviceInfo, DeviceRequest, DeviceSetLedsResponse, Input,
    InputAsyncServer, InputOpenResponse, InputRequest,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};

/// The service class of input devices, and its protocol in the service
/// directory.
const CLASS: &str = "input";
const PROTOCOL: &str = "device.input";
/// The signal of the revocation event.
const SIGNALED: u32 = 1 << 3;
/// The most events one `OnEvents` carries.
const BATCH_MAX: usize = 1024;

/// What a device does for its reader beyond reporting events.
pub trait Backend {
    /// Sets the device's LEDs to `leds`, bit `n` standing for LED code
    /// `n`.
    fn set_leds(&self, leds: u16) -> impl Future<Output = Result<(), Status>>;
}

/// A published input device.
pub struct InputDevice<B> {
    info: DeviceInfo,
    backend: B,
    inner: RefCell<Inner>,
    reactor: Reactor,
}

struct Inner {
    state: State,
    /// The open `Device` connection, and the stream of its events.
    reader: Option<(Rc<DeviceAsyncServer>, Stream)>,
}

impl<B: Backend + 'static> InputDevice<B> {
    /// A device described by `info`, whose state starts with its absolute
    /// axes at 0.
    pub fn new(info: DeviceInfo, backend: B, reactor: Reactor) -> Rc<InputDevice<B>> {
        let axes: Vec<u16> = info.absolute_axes.iter().map(|a| a.code).collect();
        Rc::new(InputDevice {
            info,
            backend,
            inner: RefCell::new(Inner {
                state: State::new(&axes),
                reader: None,
            }),
            reactor,
        })
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Publishes the device's service with `manager`, and serves the
    /// connections made to it with `spawner`.
    pub async fn publish(
        self: &Rc<Self>,
        manager: &DeviceManagerAsyncClient,
        spawner: &Spawner,
    ) -> Result<(), Status> {
        let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
        match manager.publish_service(CLASS.into(), client).await {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        }
        spawner.spawn(serve_provider(self.clone(), server, spawner.clone()));
        Ok(())
    }

    /// Takes in `group`, the events of one report ending with
    /// `SYN_REPORT`, and sends it to the reader, if there is one.
    pub fn report(&self, group: &[Event]) {
        let mut inner = self.inner.borrow_mut();
        for event in group {
            inner.state.apply(event);
        }
        if let Some((server, stream)) = &mut inner.reader {
            let group = &group[..group.len().min(BATCH_MAX)];
            stream.send(group, |g| {
                server.server().send_on_events(g.iter().map(wire).collect())
            });
        }
    }

    /// The LEDs lit, as the reader last set them.
    pub fn leds(&self) -> u16 {
        self.inner.borrow().state.leds
    }

    /// Serves a `Device` connection until it closes or `revoke` is
    /// signaled.
    /// The reader's place must already be `server`'s.
    async fn serve_device(
        self: Rc<Self>,
        server: Rc<DeviceAsyncServer>,
        revoke: Option<OwnedHandle>,
    ) {
        let requests = async {
            while let Ok(request) = server.next().await {
                self.answer(&server, request).await;
            }
        };
        match &revoke {
            Some(event) => {
                let revoked = self.reactor.signals(event.raw(), SIGNALED);
                first(requests, revoked).await;
            }
            None => requests.await,
        }
        self.inner.borrow_mut().reader = None;
        // Dropping the last reference to `server` closes the connection.
    }

    async fn answer(&self, server: &DeviceAsyncServer, request: DeviceRequest) {
        let s = server.server();
        let _ = match request {
            DeviceRequest::GetInfo(_, responder) => responder.reply(
                s,
                DeviceGetInfoResponse {
                    info: copy(&self.info),
                },
            ),
            DeviceRequest::GetState(_, responder) => {
                let state = self.inner.borrow().state.clone();
                let absolute = state
                    .absolute
                    .iter()
                    .map(|&(code, value)| AbsValue { code, value })
                    .collect();
                responder.reply(
                    s,
                    DeviceGetStateResponse {
                        keys: state.keys,
                        leds: state.leds,
                        absolute,
                    },
                )
            }
            DeviceRequest::SetLeds(request, responder) => {
                let result = if self.info.leds == 0 {
                    Err(Status::NotSupported)
                } else {
                    let leds = request.leds & self.info.leds;
                    self.backend.set_leds(leds).await.map(|()| {
                        self.inner.borrow_mut().state.leds = leds;
                        DeviceSetLedsResponse {}
                    })
                };
                responder.reply(s, result)
            }
            // No driver serving through this crate drives vibration motors.
            DeviceRequest::Rumble(_, responder) => responder.reply(s, Err(Status::NotSupported)),
        };
    }
}

/// A copy of `info`, whose generated type has no `Clone`.
fn copy(info: &DeviceInfo) -> DeviceInfo {
    DeviceInfo {
        name: info.name.clone(),
        bus: info.bus,
        vendor: info.vendor,
        product: info.product,
        version: info.version,
        event_types: info.event_types,
        keys: info.keys,
        relative_axes: info.relative_axes,
        absolute_axes: info
            .absolute_axes
            .iter()
            .map(|a| AbsInfo {
                code: a.code,
                minimum: a.minimum,
                maximum: a.maximum,
                resolution: a.resolution,
            })
            .collect(),
        leds: info.leds,
    }
}

/// `event` as `OnEvents` carries it.
fn wire(event: &Event) -> proto::Event {
    proto::Event {
        timestamp: event.timestamp,
        r#type: event.kind,
        code: event.code,
        value: event.value,
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

/// Serves the connections that `end` passes on to the device.
async fn serve_provider<B: Backend + 'static>(
    device: Rc<InputDevice<B>>,
    end: ServerEnd<Provider>,
    spawner: Spawner,
) {
    let provider = ProviderAsyncServer::new(end, device.reactor.clone());
    while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
        // Dropping the server end of another protocol closes it.
        if c.protocol == PROTOCOL {
            let end = ServerEnd::<Input>::new(c.server);
            spawner.spawn(serve_input(device.clone(), end, spawner.clone()));
        }
    }
}

async fn serve_input<B: Backend + 'static>(
    device: Rc<InputDevice<B>>,
    end: ServerEnd<Input>,
    spawner: Spawner,
) {
    let server = InputAsyncServer::new(end, device.reactor.clone());
    while let Ok(InputRequest::Open(request, responder)) = server.next().await {
        let result = if device.inner.borrow().reader.is_some() {
            Err(Status::AlreadyExists)
        } else {
            // The reader's place is taken here, before the connection
            // is first served, so that the next `Open` is refused.
            let reader = Rc::new(DeviceAsyncServer::new(
                request.device,
                device.reactor.clone(),
            ));
            device.inner.borrow_mut().reader = Some((reader.clone(), Stream::new()));
            spawner.spawn(device.clone().serve_device(reader, request.revoke));
            Ok(InputOpenResponse {})
        };
        let _ = responder.reply(server.server(), result);
    }
}
