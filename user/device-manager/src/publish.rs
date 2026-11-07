//! Publishing drivers' services in the service directory, as instances
//! of `device.<class>` that the device manager registers with the
//! service manager's `Registrar`.

use crate::supervise::Manager;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::__midl::ClientEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::Provider;

/// The classes of services that drivers may publish, and the permission
/// that connecting to each one's instances needs.
const CLASSES: [(&str, &str); 7] = [
    ("gpu", "gpu.render"),
    ("input", "input.seat"),
    ("acpi", "device.registry"),
    ("storage", "storage.raw"),
    ("filesystem", "filesystem.mount"),
    ("rtc", "time.set"),
    ("hid", "input.hid_raw"),
];

/// The service IDs given out during this boot, by device, class, and
/// how many services of that class the device's driver published before
/// since it was started, and the ID that the next new one gets.
pub struct ServiceIds {
    ids: RefCell<BTreeMap<(u64, String, usize), u64>>,
    next: Cell<u64>,
}

impl ServiceIds {
    pub fn new() -> ServiceIds {
        ServiceIds {
            ids: RefCell::new(BTreeMap::new()),
            next: Cell::new(1),
        }
    }

    /// The ID of the `nth` service of `class` of device `device`.
    fn get(&self, device: u64, class: &str, nth: usize) -> u64 {
        let mut ids = self.ids.borrow_mut();
        *ids.entry((device, class.into(), nth)).or_insert_with(|| {
            let id = self.next.get();
            self.next.set(id + 1);
            id
        })
    }
}

/// The services that one start of a driver published: how many of each
/// class, the registered instances by protocol and name, and whether
/// the driver has ended, after which they are unregistered.
#[derive(Default)]
pub struct Published {
    counts: RefCell<BTreeMap<String, usize>>,
    instances: RefCell<Vec<(String, String)>>,
    ended: Cell<bool>,
}

/// Registers `provider` as the next service of `class` of the driver of
/// device `caller`, which `published` keeps, and returns its ID. Fails
/// with `INVALID_ARGS` for a class that drivers may not publish.
pub async fn publish(
    m: &Manager,
    published: &Published,
    caller: u64,
    class: String,
    provider: ClientEnd<Provider>,
) -> Result<u64, Status> {
    let permission = CLASSES.iter().find(|(c, _)| *c == class);
    let (_, permission) = permission.ok_or(Status::InvalidArgs)?;
    let registrar = m.registrar.as_ref().ok_or(Status::BadState)?;
    let nth = {
        let mut counts = published.counts.borrow_mut();
        let count = counts.entry(class.clone()).or_default();
        *count += 1;
        *count - 1
    };
    let id = m.service_ids.get(caller, &class, nth);
    let (protocol, instance) = (format!("device.{class}"), format!("{id}"));
    let permission = Some(String::from(*permission));
    let registered = registrar
        .register(protocol.clone(), instance.clone(), permission, provider)
        .await;
    match registered {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    }
    mikros_rt::log!(
        Info,
        "device-manager: published {protocol} {instance} for device {caller}"
    );
    if published.ended.get() {
        unregister(m, &protocol, &instance).await;
    } else {
        published.instances.borrow_mut().push((protocol, instance));
    }
    Ok(id)
}

/// Unregisters the services in `published`, once their driver ended,
/// and those that are registered later.
pub async fn unregister_all(m: &Manager, published: &Published) {
    published.ended.set(true);
    let instances = core::mem::take(&mut *published.instances.borrow_mut());
    for (protocol, instance) in instances {
        unregister(m, &protocol, &instance).await;
    }
}

/// Unregisters the instance `instance` of `protocol`. One that the
/// service manager already removed, as its provider closed, counts as
/// unregistered.
async fn unregister(m: &Manager, protocol: &str, instance: &str) {
    let Some(registrar) = &m.registrar else {
        return;
    };
    match registrar.unregister(protocol.into(), instance.into()).await {
        Ok(Ok(_)) | Ok(Err(Status::NotFound)) => {
            mikros_rt::log!(Info, "device-manager: unregistered {protocol} {instance}")
        }
        failed => mikros_rt::log!(
            Error,
            "device-manager: unregistering {protocol} {instance} failed: {:?}",
            failed.map(|r| r.err())
        ),
    }
}
