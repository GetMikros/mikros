//! The USB core: the root hub's ports, enumerating the devices attached
//! to them, selecting their first configuration, adding a device for
//! each of its interfaces, and removing them when the device goes.

use crate::controller::Controller;
use crate::descriptors::{
    self, CLASS_HUB, CONFIGURATION, Configuration, DEVICE, ISOCHRONOUS, STRING,
};
use crate::device::{Device, FULL, HIGH, LOW, Place, SUPER, setup};
use crate::hub;
use crate::regs::{PORT_CCS, PORT_CHANGES, PORT_PED, PORT_PP, PORT_PR, PORT_PRC, Protocol};
use crate::wait::Waiters;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::DeviceManagerAsyncClient;
use mikros_storage::{MS, now};

/// Standard requests.
pub const GET_DESCRIPTOR: u8 = 6;
pub const SET_CONFIGURATION: u8 = 9;
pub const SET_INTERFACE: u8 = 11;
/// How long the core's own control requests may take.
const REQUEST_LIMIT: i64 = 5000 * MS;
/// US English, the language strings are read in when the device has it.
const US_ENGLISH: u16 = 0x0409;
/// How long a port reset may take.
const RESET_LIMIT: i64 = 500 * MS;

/// The deadline of a control request the core sends now.
pub fn deadline() -> i64 {
    now() + REQUEST_LIMIT
}

/// An interface of an attached device's configuration, as a device of
/// its own: its number, its current alternate setting, and the ID the
/// device manager gave it.
pub struct Interface {
    pub device: Rc<UsbDevice>,
    pub number: u8,
    pub alternate: Cell<u8>,
    pub id: Cell<u64>,
}

impl Interface {
    /// The endpoints of the current alternate setting.
    pub fn endpoints(&self) -> Vec<descriptors::Endpoint> {
        let configuration = &self.device.configuration;
        configuration
            .interface(self.number, self.alternate.get())
            .map(|i| i.endpoints.clone())
            .unwrap_or_default()
    }
}

/// An attached device: the hub it is on, if it is not on a root hub
/// port, and its port there, its slot, its descriptors and strings, its
/// interfaces once they are added, and for a hub the devices on its
/// ports.
pub struct UsbDevice {
    pub parent: Option<Weak<UsbDevice>>,
    pub port: u8,
    pub slot: Rc<Device>,
    pub descriptor: descriptors::Device,
    pub configuration: Configuration,
    pub configuration_bytes: Vec<u8>,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    pub interfaces: RefCell<Vec<Rc<Interface>>>,
    pub children: RefCell<BTreeMap<u8, Rc<UsbDevice>>>,
}

/// The USB core of one controller.
pub struct Usb {
    pub controller: Rc<Controller>,
    pub manager: DeviceManagerAsyncClient,
    protocols: Vec<Protocol>,
    /// The device on each root hub port.
    attached: RefCell<BTreeMap<u8, Rc<UsbDevice>>>,
    /// The interface devices by their device IDs.
    interfaces: RefCell<BTreeMap<u64, Rc<Interface>>>,
    /// The tasks waiting for an interface device to be added.
    added: Waiters,
}

impl Usb {
    pub fn new(controller: Rc<Controller>, manager: DeviceManagerAsyncClient) -> Rc<Usb> {
        let protocols = controller.regs.protocols();
        Rc::new(Usb {
            controller,
            manager,
            protocols,
            attached: RefCell::new(BTreeMap::new()),
            interfaces: RefCell::new(BTreeMap::new()),
            added: Waiters::default(),
        })
    }

    /// The major USB revision of root hub port `port`, from the
    /// supported protocol capabilities: 2 or 3, or 0 when none covers it.
    pub fn revision(&self, port: u8) -> u8 {
        let covers = |p: &&Protocol| (p.first..p.first.saturating_add(p.count)).contains(&port);
        self.protocols.iter().find(covers).map_or(0, |p| p.major)
    }

    /// Powers the root hub ports, and handles their connection changes:
    /// those that the controller reports, and the devices that are
    /// connected already.
    pub async fn run(self: Rc<Self>) {
        let regs = &self.controller.regs;
        for port in 1..=regs.max_ports {
            let portsc = regs.port(port);
            if portsc & PORT_PP == 0 {
                regs.write_port(port, PORT_PP);
            }
            if portsc & PORT_CCS != 0 {
                self.controller.port_changed(port);
            }
        }
        loop {
            let port = self.controller.port_change().await;
            self.port_changed(port).await;
        }
    }

    /// Handles a status change of root hub port `port`: removes the
    /// device that was on it when it was disconnected, or connected
    /// again, and enumerates the one that is connected now.
    async fn port_changed(self: &Rc<Self>, port: u8) {
        let regs = &self.controller.regs;
        let portsc = regs.port(port);
        regs.write_port(port, portsc & PORT_CHANGES);
        let connected = portsc & PORT_CCS != 0;
        let reconnected = portsc & crate::regs::PORT_CSC != 0;
        let had = self.attached.borrow().contains_key(&port);
        if had && (!connected || reconnected) {
            let device = self.attached.borrow_mut().remove(&port);
            if let Some(device) = device {
                self.detach(&device).await;
            }
        }
        if connected && !self.attached.borrow().contains_key(&port) {
            match self.attach(None, port).await {
                Ok(device) => {
                    self.attached.borrow_mut().insert(port, device);
                }
                Err(status) => mikros_rt::log!(
                    Warning,
                    "xhci: could not enumerate the device on port {}: {:?}",
                    port,
                    status
                ),
            }
        }
    }

    /// Handles a status change of port `port` of `hub`, as for a root
    /// hub port.
    pub async fn hub_port_changed(self: &Rc<Self>, hub: &Rc<UsbDevice>, port: u8) {
        let Ok((status, changes)) = hub::port_status(hub, port).await else {
            return;
        };
        let _ = hub::clear_changes(hub, port, changes).await;
        let connected = status & hub::PORT_CONNECTION != 0;
        let reconnected = changes & hub::CHANGE_CONNECTION != 0;
        let had = hub.children.borrow().contains_key(&port);
        if had && (!connected || reconnected) {
            let device = hub.children.borrow_mut().remove(&port);
            if let Some(device) = device {
                self.detach(&device).await;
            }
        }
        if connected && !hub.children.borrow().contains_key(&port) {
            match self.attach(Some(hub), port).await {
                Ok(device) => {
                    hub.children.borrow_mut().insert(port, device);
                }
                Err(status) => mikros_rt::log!(
                    Warning,
                    "xhci: could not enumerate the device on port {} of the hub in slot {}: {:?}",
                    port,
                    hub.slot.slot,
                    status
                ),
            }
        }
    }

    /// Resets root hub port `port` and waits until the reset completes
    /// and the port is enabled. Returns the speed of its device.
    async fn reset_port(&self, port: u8) -> Result<u8, Status> {
        let regs = &self.controller.regs;
        regs.write_port(port, PORT_PR);
        let limit = now() + RESET_LIMIT;
        loop {
            let portsc = regs.port(port);
            if portsc & PORT_CCS == 0 {
                return Err(Status::PeerClosed);
            }
            if portsc & PORT_PRC != 0 && portsc & PORT_PR == 0 {
                regs.write_port(port, PORT_PRC);
                if portsc & PORT_PED == 0 {
                    return Err(Status::Internal);
                }
                return Ok(((portsc >> 10) & 0xF) as u8);
            }
            if now() >= limit {
                return Err(Status::TimedOut);
            }
            let _ = self.controller.reactor.sleep_until(now() + MS).await;
        }
    }

    /// Resets port `port` of `parent`, or root hub port `port` without
    /// one, and returns the speed of the device on it.
    async fn reset(&self, parent: Option<&Rc<UsbDevice>>, port: u8) -> Result<u8, Status> {
        match parent {
            Some(hub) => hub::reset_port(hub, port).await,
            None => self.reset_port(port).await,
        }
    }

    /// Enumerates the device connected to port `port` of `parent`, or to
    /// root hub port `port` without one. Devices more tiers below the
    /// root hub than a route string covers are not enumerated.
    async fn attach(
        self: &Rc<Self>,
        parent: Option<&Rc<UsbDevice>>,
        port: u8,
    ) -> Result<Rc<UsbDevice>, Status> {
        let mut place = match parent {
            None => Place {
                root_port: port,
                route: 0,
                tt: None,
            },
            Some(hub) => {
                let above = &hub.slot.place;
                let depth = hub::depth(above.route);
                if depth >= hub::MAX_DEPTH || port > 15 {
                    return Err(Status::NotSupported);
                }
                Place {
                    root_port: above.root_port,
                    route: above.route | u32::from(port) << (4 * depth),
                    tt: above.tt,
                }
            }
        };
        let speed = self.reset(parent, port).await?;
        // A low- or full-speed device directly on a high-speed hub uses
        // that hub's transaction translator.
        if let Some(hub) = parent
            && hub.slot.speed == HIGH
            && matches!(speed, LOW | FULL)
        {
            let multi = hub.slot.hub().is_some_and(|h| h.multi_tt);
            place.tt = Some((hub.slot.slot, port, multi));
        }
        mikros_rt::log!(
            Info,
            "xhci: device on port {} (USB {}), route {:x}, speed {}",
            place.root_port,
            self.revision(place.root_port),
            place.route,
            speed
        );
        let slot = Device::new(self.controller.clone(), place, speed).await?;
        let parent = parent.map(Rc::downgrade);
        match self.enumerate(slot.clone(), parent, port).await {
            Ok(device) => Ok(device),
            Err(status) => {
                slot.disable().await;
                Err(status)
            }
        }
    }

    /// Reads the descriptors and strings of the addressed device `slot`
    /// on port `port` of `parent`, selects its first configuration and
    /// adds its interfaces, or for a hub starts handling its ports.
    async fn enumerate(
        self: &Rc<Self>,
        slot: Rc<Device>,
        parent: Option<Weak<UsbDevice>>,
        port: u8,
    ) -> Result<Rc<UsbDevice>, Status> {
        let (descriptor, configuration_bytes, configuration) = read_descriptors(&slot).await?;
        let strings = read_strings(&slot, &descriptor).await;
        configure(&slot, &configuration).await?;
        let device = Rc::new(UsbDevice {
            parent,
            port,
            slot,
            descriptor,
            configuration,
            configuration_bytes,
            manufacturer: strings[0].clone(),
            product: strings[1].clone(),
            serial: strings[2].clone(),
            interfaces: RefCell::new(Vec::new()),
            children: RefCell::new(BTreeMap::new()),
        });
        if hub::is_hub(&device) {
            let task = hub::run(self.clone(), device.clone());
            self.controller.spawner.spawn(task);
        }
        self.add_interfaces(&device).await;
        Ok(device)
    }

    /// Adds a device for each interface of `device`'s configuration
    /// other than hub interfaces.
    async fn add_interfaces(&self, device: &Rc<UsbDevice>) {
        let configuration = &device.configuration;
        for interface in configuration.interfaces.iter().filter(|i| i.alternate == 0) {
            if interface.class == CLASS_HUB {
                continue;
            }
            let d = &device.descriptor;
            let ids = vec![
                format!(
                    "usb:v{:04x}p{:04x}i{:02x}",
                    d.vendor, d.product, interface.number
                ),
                format!("usb:v{:04x}p{:04x}", d.vendor, d.product),
                format!(
                    "usb:c{:02x}s{:02x}p{:02x}",
                    interface.class, interface.subclass, interface.protocol
                ),
                format!("usb:c{:02x}s{:02x}", interface.class, interface.subclass),
                format!("usb:c{:02x}", interface.class),
            ];
            let name = format!("{}-{}", name(&device.slot.place), interface.number);
            let added = Rc::new(Interface {
                device: device.clone(),
                number: interface.number,
                alternate: Cell::new(0),
                id: Cell::new(0),
            });
            match self.manager.add_device(name, ids, vec![], vec![]).await {
                Ok(Ok(response)) => {
                    added.id.set(response.id);
                    self.interfaces
                        .borrow_mut()
                        .insert(response.id, added.clone());
                    device.interfaces.borrow_mut().push(added);
                    self.added.wake();
                }
                _ => mikros_rt::log!(
                    Warning,
                    "xhci: could not add interface {} of the device in slot {}",
                    interface.number,
                    device.slot.slot
                ),
            }
        }
    }

    /// Removes `device`, which was detached, and for a hub the devices
    /// behind it first: fails their transfers, removes their interface
    /// devices, and gives their slots back.
    pub async fn detach(&self, device: &Rc<UsbDevice>) {
        let mut order = Vec::new();
        let mut stack = vec![device.clone()];
        while let Some(next) = stack.pop() {
            let children = core::mem::take(&mut *next.children.borrow_mut());
            stack.extend(children.into_values());
            order.push(next);
        }
        for device in order.iter().rev() {
            device.slot.fail_all();
            self.remove_interfaces(device).await;
            device.slot.disable().await;
        }
    }

    /// Removes the interface devices of `device`.
    async fn remove_interfaces(&self, device: &UsbDevice) {
        let interfaces = core::mem::take(&mut *device.interfaces.borrow_mut());
        for interface in interfaces {
            let id = interface.id.get();
            self.interfaces.borrow_mut().remove(&id);
            let _ = self.manager.remove_device(id).await;
        }
    }

    /// Resets the USB device of the interface device `id`: fails the
    /// transfers of all its interfaces, resets its port, and gives it its
    /// address and configuration again. When its device and
    /// configuration descriptors are the same as before, its interface
    /// devices are kept; otherwise they are removed, the device is
    /// enumerated as a new one, and the reset fails with `NOT_FOUND`.
    pub async fn reset_device(self: &Rc<Self>, id: u64) -> Result<(), Status> {
        let interface = self.interfaces.borrow().get(&id).cloned();
        let device = interface.ok_or(Status::NotFound)?.device.clone();
        let parent = device.parent.as_ref().and_then(Weak::upgrade);
        let port = match &parent {
            Some(_) => device.port,
            None => device.slot.place.root_port,
        };
        device.slot.fail_all();
        if self.reconfigure(&device, parent.as_ref(), port).await == Ok(true) {
            for interface in device.interfaces.borrow().iter() {
                interface.alternate.set(0);
            }
            return Ok(());
        }
        let removed = match &parent {
            Some(hub) => hub.children.borrow_mut().remove(&port),
            None => self.attached.borrow_mut().remove(&port),
        };
        if let Some(removed) = removed {
            self.detach(&removed).await;
        }
        match &parent {
            Some(hub) => self.hub_port_changed(hub, port).await,
            None => self.controller.port_changed(port),
        }
        Err(Status::NotFound)
    }

    /// Resets the port of `device`, port `port` of `parent` or root hub
    /// port `port`, gives the device its address again, and when it
    /// reports the same device and configuration descriptors, its
    /// configuration. Returns whether they were the same.
    async fn reconfigure(
        &self,
        device: &UsbDevice,
        parent: Option<&Rc<UsbDevice>>,
        port: u8,
    ) -> Result<bool, Status> {
        let speed = self.reset(parent, port).await?;
        if speed != device.slot.speed {
            return Ok(false);
        }
        device.slot.reset().await?;
        let (descriptor, bytes, configuration) = read_descriptors(&device.slot).await?;
        let same = descriptor == device.descriptor && bytes == device.configuration_bytes;
        if same {
            configure(&device.slot, &configuration).await?;
        }
        Ok(same)
    }

    /// The interface device with ID `id`. The device manager replies to
    /// `AddDevice` before it asks for the device, but this driver may
    /// take the request before the reply, so the device is waited for
    /// until it is added, at most `REQUEST_LIMIT`.
    pub async fn interface(&self, id: u64) -> Option<Rc<Interface>> {
        let added = self
            .added
            .until(|| self.interfaces.borrow().get(&id).cloned());
        let limit = self.controller.reactor.sleep_until(now() + REQUEST_LIMIT);
        let mut added = core::pin::pin!(added);
        let mut limit = core::pin::pin!(limit);
        core::future::poll_fn(|cx| {
            if let core::task::Poll::Ready(found) = added.as_mut().poll(cx) {
                return core::task::Poll::Ready(Some(found));
            }
            limit.as_mut().poll(cx).map(|_| None)
        })
        .await
    }
}

/// The name of an interface device's USB device at `place`: `usb-`, the
/// root hub port, and the hub port at each tier below it.
fn name(place: &Place) -> String {
    let mut name = format!("usb-{}", place.root_port);
    let mut route = place.route;
    while route & 0xF != 0 {
        name.push_str(&format!(".{}", route & 0xF));
        route >>= 4;
    }
    name
}

/// A standard GET_DESCRIPTOR request for `length` bytes of descriptor
/// `kind` with index `index` and language `language`.
async fn get_descriptor(
    slot: &Device,
    kind: u8,
    index: u8,
    language: u16,
    length: u16,
) -> Result<Vec<u8>, Status> {
    let value = u16::from(kind) << 8 | u16::from(index);
    let request = setup(0x80, GET_DESCRIPTOR, value, language, length);
    slot.control(request, &[], deadline()).await
}

/// Reads the device descriptor, setting the default control endpoint's
/// maximum packet size from its first 8 bytes, and the first
/// configuration descriptor.
async fn read_descriptors(
    slot: &Device,
) -> Result<(descriptors::Device, Vec<u8>, Configuration), Status> {
    let first = get_descriptor(slot, DEVICE, 0, 0, 8).await?;
    let size = *first.get(7).ok_or(Status::Internal)?;
    let max_packet = match slot.speed {
        SUPER.. => 1u16.checked_shl(u32::from(size)).unwrap_or(512),
        _ => u16::from(size),
    };
    slot.set_max_packet0(max_packet).await?;
    let bytes = get_descriptor(slot, DEVICE, 0, 0, 18).await?;
    let descriptor = descriptors::Device::parse(&bytes).ok_or(Status::Internal)?;
    let header = get_descriptor(slot, CONFIGURATION, 0, 0, 9).await?;
    if header.len() < 4 {
        return Err(Status::Internal);
    }
    let total = u16::from_le_bytes([header[2], header[3]]).min(4096);
    let bytes = get_descriptor(slot, CONFIGURATION, 0, 0, total).await?;
    let configuration = Configuration::parse(&bytes).ok_or(Status::Internal)?;
    Ok((descriptor, bytes, configuration))
}

/// The manufacturer, product and serial number strings of the device of
/// `descriptor`, in US English when the device has it and otherwise in
/// the first language it lists; empty for those it does not have.
async fn read_strings(slot: &Device, descriptor: &descriptors::Device) -> [String; 3] {
    let indexes = [
        descriptor.manufacturer,
        descriptor.product_string,
        descriptor.serial,
    ];
    let mut strings = [String::new(), String::new(), String::new()];
    if indexes.iter().all(|&i| i == 0) {
        return strings;
    }
    let Ok(bytes) = get_descriptor(slot, STRING, 0, 0, 255).await else {
        return strings;
    };
    let languages = descriptors::languages(&bytes);
    let language = match languages.contains(&US_ENGLISH) {
        true => US_ENGLISH,
        false => match languages.first() {
            Some(&first) => first,
            None => return strings,
        },
    };
    for (string, index) in strings.iter_mut().zip(indexes) {
        if index != 0
            && let Ok(bytes) = get_descriptor(slot, STRING, index, language, 255).await
        {
            *string = descriptors::string(&bytes);
        }
    }
    strings
}

/// Selects `configuration`: configures the endpoints of alternate
/// setting 0 of each interface, other than isochronous ones, and sends
/// SET_CONFIGURATION.
pub async fn configure(slot: &Device, configuration: &Configuration) -> Result<(), Status> {
    let endpoints: Vec<descriptors::Endpoint> = configuration
        .interfaces
        .iter()
        .filter(|i| i.alternate == 0)
        .flat_map(|i| i.endpoints.iter().copied())
        .filter(|e| e.kind() != ISOCHRONOUS && e.address & 0xF != 0)
        .collect();
    if !endpoints.is_empty() {
        slot.configure(&[], &endpoints).await?;
    }
    let value = u16::from(configuration.value);
    let request = setup(0x00, SET_CONFIGURATION, value, 0, 0);
    slot.control(request, &[], deadline()).await?;
    Ok(())
}
