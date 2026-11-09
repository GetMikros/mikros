//! Hubs: their descriptor, powering their ports, watching their status
//! change endpoint, and resetting their ports for the devices behind
//! them.

use crate::descriptors::{CLASS_HUB, INTERRUPT};
use crate::device::{FULL, HIGH, HubInfo, LOW, SUPER, setup};
use crate::transfer::Request;
use crate::usb::{GET_DESCRIPTOR, Usb, UsbDevice, deadline};
use alloc::rc::Rc;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_storage::{MS, now};

/// Hub class requests and descriptors.
const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const SET_FEATURE: u8 = 3;
const SET_HUB_DEPTH: u8 = 12;
const HUB_DESCRIPTOR: u16 = 0x29;
const SS_HUB_DESCRIPTOR: u16 = 0x2A;
/// Port features.
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_ENABLE: u16 = 17;
const C_PORT_SUSPEND: u16 = 18;
const C_PORT_OVER_CURRENT: u16 = 19;
const C_PORT_RESET: u16 = 20;
const C_PORT_LINK_STATE: u16 = 25;
const C_PORT_CONFIG_ERROR: u16 = 26;
const C_BH_PORT_RESET: u16 = 29;
/// Port status bits.
pub const PORT_CONNECTION: u16 = 1 << 0;
const PORT_ENABLE: u16 = 1 << 1;
const PORT_LOW_SPEED: u16 = 1 << 9;
const PORT_HIGH_SPEED: u16 = 1 << 10;
/// Port change bits.
pub const CHANGE_CONNECTION: u16 = 1 << 0;
const CHANGE_RESET: u16 = 1 << 4;
/// The tiers a route string covers below the root hub.
pub const MAX_DEPTH: u32 = 5;
/// How long a port reset may take.
const RESET_LIMIT: i64 = 500 * MS;
/// The deadline of transfers on a status change endpoint: none.
const NEVER: i64 = i64::MAX;

/// The number of hubs between `route`'s device and the root hub.
pub fn depth(route: u32) -> u32 {
    (32 - route.leading_zeros()).div_ceil(4)
}

fn is_super(hub: &UsbDevice) -> bool {
    hub.slot.speed >= SUPER
}

/// The status and change bits of port `port` of `hub`.
pub async fn port_status(hub: &UsbDevice, port: u8) -> Result<(u16, u16), Status> {
    let request = setup(0xA3, GET_STATUS, 0, u16::from(port), 4);
    let data = hub.slot.control(request, &[], deadline()).await?;
    if data.len() < 4 {
        return Err(Status::Internal);
    }
    Ok((
        u16::from_le_bytes([data[0], data[1]]),
        u16::from_le_bytes([data[2], data[3]]),
    ))
}

async fn port_feature(hub: &UsbDevice, request: u8, feature: u16, port: u8) -> Result<(), Status> {
    let request = setup(0x23, request, feature, u16::from(port), 0);
    hub.slot.control(request, &[], deadline()).await.map(|_| ())
}

/// Clears every change bit of port `port` of `hub`.
pub async fn clear_changes(hub: &UsbDevice, port: u8, changes: u16) -> Result<(), Status> {
    let features: &[(u16, u16)] = match is_super(hub) {
        true => &[
            (0, C_PORT_CONNECTION),
            (3, C_PORT_OVER_CURRENT),
            (4, C_PORT_RESET),
            (5, C_BH_PORT_RESET),
            (6, C_PORT_LINK_STATE),
            (7, C_PORT_CONFIG_ERROR),
        ],
        false => &[
            (0, C_PORT_CONNECTION),
            (1, C_PORT_ENABLE),
            (2, C_PORT_SUSPEND),
            (3, C_PORT_OVER_CURRENT),
            (4, C_PORT_RESET),
        ],
    };
    for &(bit, feature) in features {
        if changes & 1 << bit != 0 {
            port_feature(hub, CLEAR_FEATURE, feature, port).await?;
        }
    }
    Ok(())
}

/// Resets port `port` of `hub` and waits until the reset completes and
/// the port is enabled. Returns the speed of its device.
pub async fn reset_port(hub: &UsbDevice, port: u8) -> Result<u8, Status> {
    port_feature(hub, SET_FEATURE, PORT_RESET, port).await?;
    let limit = now() + RESET_LIMIT;
    loop {
        let (status, changes) = port_status(hub, port).await?;
        if status & PORT_CONNECTION == 0 {
            return Err(Status::PeerClosed);
        }
        if changes & CHANGE_RESET != 0 {
            clear_changes(hub, port, changes).await?;
            if status & PORT_ENABLE == 0 {
                return Err(Status::Internal);
            }
            return Ok(match is_super(hub) {
                true => SUPER,
                false if status & PORT_LOW_SPEED != 0 => LOW,
                false if status & PORT_HIGH_SPEED != 0 => HIGH,
                false => FULL,
            });
        }
        if now() >= limit {
            return Err(Status::TimedOut);
        }
        let _ = hub
            .slot
            .controller
            .reactor
            .sleep_until(now() + 10 * MS)
            .await;
    }
}

/// Whether `device` is a hub.
pub fn is_hub(device: &UsbDevice) -> bool {
    let interfaces = &device.configuration.interfaces;
    interfaces
        .iter()
        .any(|i| i.alternate == 0 && i.class == CLASS_HUB)
}

/// Sets up `hub`: reads its hub descriptor, tells the controller that it
/// is a hub, gives a USB 3 hub its depth, and powers its ports. Then
/// handles the changes of its ports that its status change endpoint
/// reports, starting with every port, until the hub goes.
pub async fn run(usb: Rc<Usb>, hub: Rc<UsbDevice>) {
    let ports = match set_up(&hub).await {
        Ok(ports) => ports,
        Err(status) => {
            mikros_rt::log!(
                Warning,
                "xhci: could not set up the hub in slot {}: {:?}",
                hub.slot.slot,
                status
            );
            return;
        }
    };
    for port in 1..=ports {
        usb.hub_port_changed(&hub, port).await;
    }
    let Some(endpoint) = status_endpoint(&hub) else {
        return;
    };
    let (descriptor, endpoint) = endpoint;
    let length = (u32::from(ports) + 1).div_ceil(8);
    let controller = hub.slot.controller.clone();
    loop {
        let Ok(page) = controller.page() else {
            return;
        };
        let request = Request::normal(
            &[page.physical(0)],
            0,
            length,
            descriptor.max_packet & 0x7FF,
        );
        let result = endpoint.transfer(request, NEVER).await;
        let mut bitmap = [0u8; 32];
        page.read_bytes(0, &mut bitmap[..length as usize]);
        controller.give_page(page);
        match result {
            Ok(_) => {}
            Err(Status::PeerClosed) => return,
            // A stall is recovered from before the next transfer.
            Err(_) => {
                let _ = controller.reactor.sleep_until(now() + 100 * MS).await;
                continue;
            }
        }
        for port in 1..=ports {
            if bitmap[usize::from(port / 8)] & 1 << (port % 8) != 0 {
                usb.hub_port_changed(&hub, port).await;
            }
        }
    }
}

/// The hub's status change endpoint, with its descriptor.
fn status_endpoint(
    hub: &UsbDevice,
) -> Option<(crate::descriptors::Endpoint, Rc<crate::transfer::Endpoint>)> {
    let interfaces = &hub.configuration.interfaces;
    let interface = interfaces
        .iter()
        .find(|i| i.alternate == 0 && i.class == CLASS_HUB)?;
    let descriptor = interface
        .endpoints
        .iter()
        .find(|e| e.kind() == INTERRUPT && e.is_in())?;
    Some((*descriptor, hub.slot.endpoint(descriptor.id())?))
}

/// Reads the hub descriptor, updates the hub's slot context, sets a
/// USB 3 hub's depth and powers the ports. Returns the port count.
async fn set_up(hub: &UsbDevice) -> Result<u8, Status> {
    let super_speed = is_super(hub);
    let kind = if super_speed {
        SS_HUB_DESCRIPTOR
    } else {
        HUB_DESCRIPTOR
    };
    let request = setup(0xA0, GET_DESCRIPTOR, kind << 8, 0, 12);
    let descriptor = hub.slot.control(request, &[], deadline()).await?;
    if descriptor.len() < 7 {
        return Err(Status::Internal);
    }
    let ports = descriptor[2];
    let characteristics = u16::from_le_bytes([descriptor[3], descriptor[4]]);
    let power_good = i64::from(descriptor[5]) * 2 * MS;
    let interfaces = &hub.configuration.interfaces;
    let multi_tt = interfaces
        .iter()
        .any(|i| i.alternate == 0 && i.class == CLASS_HUB && i.protocol == 2);
    let info = HubInfo {
        ports,
        multi_tt: hub.slot.speed == HIGH && multi_tt,
        think_time: match hub.slot.speed {
            HIGH => ((characteristics >> 5) & 3) as u8,
            _ => 0,
        },
    };
    hub.slot.set_hub(info).await?;
    if super_speed {
        let depth = depth(hub.slot.place.route) as u16;
        let request = setup(0x20, SET_HUB_DEPTH, depth, 0, 0);
        hub.slot.control(request, &[], deadline()).await?;
    }
    for port in 1..=ports {
        port_feature(hub, SET_FEATURE, PORT_POWER, port).await?;
    }
    let reactor = &hub.slot.controller.reactor;
    let _ = reactor.sleep_until(now() + power_good).await;
    Ok(ports)
}
