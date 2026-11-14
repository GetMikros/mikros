//! Device names, `<class>/<name>`, under which the device manager
//! registers each service a driver publishes besides its service ID, and
//! `mikros.driver.Devices`, which lists the named devices to every
//! process.

use crate::devices::Device;
use crate::supervise::Manager;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::driver::{
    DeviceEntry, Devices, DevicesAsyncServer, DevicesListResponse, DevicesRequest,
};

/// The protocol that lists the named devices.
pub const PROTOCOL: &str = "mikros.driver.Devices";
/// The most entries that one `List` gives.
const PAGE: usize = 64;
/// The longest description that an entry holds, in bytes.
const DESCRIPTION_MAX: usize = 128;
/// The property that gives a device's description.
const DESCRIPTION: &str = "description";

/// The names given during this boot, and those in use.
#[derive(Default)]
pub struct Names {
    /// By device, class, and how many services of that class the
    /// device's driver published before since it was started.
    given: RefCell<BTreeMap<(u64, String, usize), String>>,
    /// The number that the next new name of each class gets.
    next: RefCell<BTreeMap<String, u32>>,
    /// The names registered now, by class and name, with their devices.
    current: RefCell<BTreeMap<(String, String), u64>>,
}

impl Names {
    /// The name of the `nth` service of `class` of device `device`: the
    /// one it was given before in this boot, or the class followed by
    /// the smallest number not given to a device of the class yet.
    pub fn get(&self, device: u64, class: &str, nth: usize) -> String {
        let mut given = self.given.borrow_mut();
        given
            .entry((device, class.into(), nth))
            .or_insert_with(|| {
                let mut next = self.next.borrow_mut();
                let number = next.entry(class.into()).or_default();
                *number += 1;
                format!("{class}{}", *number - 1)
            })
            .clone()
    }

    /// Notes that `class`/`name` of device `device` is registered.
    pub fn add(&self, class: &str, name: &str, device: u64) {
        let key = (String::from(class), String::from(name));
        self.current.borrow_mut().insert(key, device);
    }

    /// Notes that `class`/`name` is no longer registered.
    pub fn remove(&self, class: &str, name: &str) {
        let key = (String::from(class), String::from(name));
        self.current.borrow_mut().remove(&key);
    }

    /// The registered names of `class`, or of every class, after
    /// `<class>/<name>` `after`, in that order, with their devices, and
    /// whether there are more than `PAGE` of them.
    fn page(&self, class: Option<&str>, after: Option<&str>) -> (Vec<(String, String, u64)>, bool) {
        let current = self.current.borrow();
        let mut found = current
            .iter()
            .filter(|((c, _), _)| class.is_none_or(|class| c == class))
            .filter(|((c, n), _)| after.is_none_or(|after| format!("{c}/{n}").as_str() > after))
            .map(|((c, n), device)| (c.clone(), n.clone(), *device));
        let page: Vec<_> = found.by_ref().take(PAGE).collect();
        let more = found.next().is_some();
        (page, more)
    }
}

/// The description of `device`: its `description` property when its
/// driver gave one, and otherwise its device name and first hardware ID,
/// cut to what an entry holds.
fn description(device: &Device) -> String {
    let given = device.properties.iter().find(|(k, _)| k == DESCRIPTION);
    let mut text = match (given, device.hardware_ids.first()) {
        (Some((_, description)), _) => description.clone(),
        (None, Some(id)) => format!("{} {id}", device.name),
        (None, None) => device.name.clone(),
    };
    if text.len() > DESCRIPTION_MAX {
        let mut end = DESCRIPTION_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Serves `Devices` on `end` until it closes.
pub async fn serve(m: Rc<Manager>, end: ServerEnd<Devices>) {
    let server = DevicesAsyncServer::new(end, m.reactor.clone());
    while let Ok(DevicesRequest::List(request, responder)) = server.next().await {
        let (page, more) = m
            .names
            .page(request.class.as_deref(), request.after.as_deref());
        let devices = m.devices.borrow();
        let entries = page
            .into_iter()
            .map(|(class, name, device)| DeviceEntry {
                description: devices.get(device).map(description).unwrap_or_default(),
                class,
                name,
            })
            .collect();
        drop(devices);
        let _ = responder.reply(server.server(), DevicesListResponse { entries, more });
    }
}
