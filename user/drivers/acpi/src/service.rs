//! The `acpi` service: the routes of PCI interrupt pins, powering the
//! machine off and restarting it for clients that hold `power.control`,
//! and `OnPowerButton` to every client each time the power button is
//! pressed.

use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_acpica::Status as AcpiStatus;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::acpi::{
    AcpiAsyncServer, AcpiGetPciRouteResponse, AcpiRequest, PciRoute,
};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::sys;

/// The protocol of the `acpi` service class in the service directory.
const PROTOCOL: &str = "device.acpi";
const POWER_CONTROL: &str = "power.control";
const SIGNALED: u32 = 1 << 3;

/// What every connection shares.
pub struct Acpi {
    broker: Option<BrokerAsyncClient>,
    connections: RefCell<Vec<Weak<AcpiAsyncServer>>>,
}

impl Acpi {
    pub fn new(broker: Option<BrokerAsyncClient>) -> Acpi {
        Acpi {
            broker,
            connections: RefCell::new(Vec::new()),
        }
    }

    /// Whether `client` holds `power.control`, as the broker says.
    async fn may_control(&self, client: &ClientIdentity) -> bool {
        let Some(broker) = &self.broker else {
            return false;
        };
        let client = ClientIdentity {
            pid: client.pid,
            user: client.user,
            app_id: client.app_id.clone(),
        };
        let checked = broker.check(client, POWER_CONTROL.into()).await;
        matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }

    /// Sends `OnPowerButton` to every connection, and forgets the closed
    /// ones.
    fn pressed(&self) {
        self.connections.borrow_mut().retain(|c| {
            c.upgrade()
                .is_some_and(|server| server.server().send_on_power_button().is_ok())
        });
    }
}

/// The status of the base library for an ACPICA status.
pub fn status(status: AcpiStatus) -> Status {
    match status {
        mikros_acpica::AE_NOT_FOUND | mikros_acpica::AE_NOT_EXIST => Status::NotFound,
        mikros_acpica::AE_BAD_PARAMETER => Status::InvalidArgs,
        mikros_acpica::AE_NO_MEMORY => Status::NoMemory,
        mikros_acpica::AE_SUPPORT => Status::NotSupported,
        mikros_acpica::AE_TIME => Status::TimedOut,
        _ => Status::Io,
    }
}

/// Serves the connections that `end` passes on to the server.
pub async fn serve_provider(
    acpi: Rc<Acpi>,
    end: ServerEnd<mikros_rt::interfaces::mikros::service::Provider>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let provider = ProviderAsyncServer::new(end, reactor.clone());
    while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
        // Dropping the server end of another protocol closes it.
        if c.protocol == PROTOCOL {
            let end = ServerEnd::new(c.server);
            spawner.spawn(serve(acpi.clone(), end, reactor.clone(), c.client));
        }
    }
}

async fn serve(
    acpi: Rc<Acpi>,
    end: ServerEnd<mikros_rt::interfaces::mikros::acpi::Acpi>,
    reactor: Reactor,
    client: ClientIdentity,
) {
    let server = Rc::new(AcpiAsyncServer::new(end, reactor));
    acpi.connections.borrow_mut().push(Rc::downgrade(&server));
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            AcpiRequest::GetPciRoute(r, responder) => {
                let routed = mikros_acpica::route::pci_route(r.segment, r.bus, r.device, r.pin);
                let routed = routed.map_err(status).map(|route| AcpiGetPciRouteResponse {
                    route: PciRoute {
                        gsi: route.gsi,
                        level: route.level,
                        active_low: route.active_low,
                    },
                });
                responder.reply(s, routed)
            }
            AcpiRequest::PowerOff(_, responder) => {
                if !acpi.may_control(&client).await {
                    responder.reply(s, Err(Status::AccessDenied))
                } else {
                    let failed = mikros_acpica::power::power_off();
                    mikros_rt::log!(Error, "acpi: entering S5 failed: {failed:#x}");
                    responder.reply(s, Err(status(failed)))
                }
            }
            AcpiRequest::Restart(_, responder) => {
                if !acpi.may_control(&client).await {
                    responder.reply(s, Err(Status::AccessDenied))
                } else {
                    let failed = mikros_acpica::power::restart();
                    mikros_rt::log!(Error, "acpi: restarting failed: {failed:#x}");
                    responder.reply(s, Err(status(failed)))
                }
            }
        };
    }
}

/// The event that the power button signals, from a thread of ACPICA's
/// operating system layer.
static BUTTON: AtomicU32 = AtomicU32::new(0);

fn button_pressed() {
    // SAFETY: the call takes no pointers.
    unsafe { sys::object_signal(BUTTON.load(Ordering::Acquire), 0, SIGNALED) };
}

/// Watches the power button, and sends `OnPowerButton` each time it is
/// pressed: false when it cannot be watched.
pub fn watch_button(acpi: Rc<Acpi>, reactor: Reactor, spawner: &Spawner) -> bool {
    let mut event = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::event_create(0, &mut event) } != mikros_rt::OK {
        return false;
    }
    // SAFETY: the kernel just created the handle for us.
    let event = unsafe { OwnedHandle::from_raw(event) };
    BUTTON.store(event.raw(), Ordering::Release);
    if let Err(failed) = mikros_acpica::power::watch_power_button(button_pressed) {
        mikros_rt::log!(
            Warning,
            "acpi: watching the power button failed: {failed:#x}"
        );
        return false;
    }
    spawner.spawn(async move {
        while reactor.signals(event.raw(), SIGNALED).await.is_ok() {
            // SAFETY: the call takes no pointers.
            unsafe { sys::object_signal(event.raw(), SIGNALED, 0) };
            acpi.pressed();
        }
    });
    true
}
