//! A USB device's slot: its input and device contexts, addressing it,
//! its default control endpoint, and configuring its other endpoints.

use crate::controller::Controller;
use crate::descriptors::{self, BULK, INTERRUPT};
use crate::ring::{
    TRB_ADDRESS_DEVICE, TRB_CONFIGURE_ENDPOINT, TRB_DISABLE_SLOT, TRB_ENABLE_SLOT,
    TRB_EVALUATE_CONTEXT, TRB_RESET_DEVICE, trb_type,
};
use crate::transfer::{Direction, Endpoint, Request};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_storage::Contiguous;

/// Speed IDs, as `PORTSC` and the slot context give them.
pub const FULL: u8 = 1;
pub const LOW: u8 = 2;
pub const HIGH: u8 = 3;
pub const SUPER: u8 = 4;
/// Endpoint types in endpoint contexts.
const TYPE_CONTROL: u32 = 4;
/// The segments of the transfer rings of bulk endpoints, and of others.
const BULK_SEGMENTS: usize = 16;
const OTHER_SEGMENTS: usize = 1;
/// The error count endpoints start with.
const ERROR_COUNT: u32 = 3;

/// The setup packet of a control request.
pub fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    let [v0, v1] = value.to_le_bytes();
    let [i0, i1] = index.to_le_bytes();
    let [l0, l1] = length.to_le_bytes();
    [request_type, request, v0, v1, i0, i1, l0, l1]
}

/// Where a device is on the bus: its root hub port, its route string
/// below it, and for a low- or full-speed device behind a high-speed
/// hub, that hub's slot ID and port and whether it has a TT per port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub root_port: u8,
    pub route: u32,
    pub tt: Option<(u8, u8, bool)>,
}

/// What the slot context says about a hub: its port count, whether it
/// has a TT per port, and its TT think time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubInfo {
    pub ports: u8,
    pub multi_tt: bool,
    pub think_time: u8,
}

pub struct Device {
    pub controller: Rc<Controller>,
    pub slot: u8,
    pub speed: u8,
    pub place: Place,
    input: Contiguous,
    /// The device context, which the controller writes.
    _output: Contiguous,
    pub ep0: Rc<Endpoint>,
    max_packet0: Cell<u16>,
    hub: Cell<Option<HubInfo>>,
    /// The configured endpoints other than the default control
    /// endpoint, by endpoint ID.
    endpoints: RefCell<BTreeMap<u8, Rc<Endpoint>>>,
}

/// The maximum packet size of the default control endpoint before the
/// device descriptor gives it.
fn default_max_packet0(speed: u8) -> u16 {
    match speed {
        LOW | FULL => 8,
        HIGH => 64,
        _ => 512,
    }
}

impl Device {
    /// Obtains a slot for a new device at `place` with speed `speed`,
    /// and addresses it.
    pub async fn new(
        controller: Rc<Controller>,
        place: Place,
        speed: u8,
    ) -> Result<Rc<Device>, Status> {
        let enable = [0, 0, 0, trb_type(TRB_ENABLE_SLOT)];
        let slot = controller.command_ok(enable).await?.slot;
        let made = (|| {
            let size = controller.regs.context_size;
            let input = controller.memory(33 * size)?;
            let output = controller.memory(32 * size)?;
            let ring = controller.ring(OTHER_SEGMENTS)?;
            Ok::<_, Status>((input, output, ring))
        })();
        let (input, output, ring) = match made {
            Ok(made) => made,
            Err(status) => {
                let disable = [0, 0, 0, trb_type(TRB_DISABLE_SLOT) | u32::from(slot) << 24];
                let _ = controller.command(disable).await;
                return Err(status);
            }
        };
        controller.set_device_context(slot, output.physical(0));
        let device = Rc::new(Device {
            ep0: Endpoint::new(controller.clone(), slot, 1, ring),
            controller,
            slot,
            speed,
            place,
            input,
            _output: output,
            max_packet0: Cell::new(default_max_packet0(speed)),
            hub: Cell::new(None),
            endpoints: RefCell::new(BTreeMap::new()),
        });
        if let Err(status) = device.address().await {
            device.disable().await;
            return Err(status);
        }
        Ok(device)
    }

    fn input_context(&self, index: u64) -> u64 {
        index * self.controller.regs.context_size
    }

    /// Writes the input control context's drop and add flags, and the
    /// slot context, with the last endpoint ID `entries`.
    fn write_slot(&self, drop: u32, add: u32, entries: u8) {
        let input = &self.input;
        input.zero();
        input.write32(0, drop);
        input.write32(4, add | 1);
        let at = self.input_context(1);
        let place = &self.place;
        let mut dword0 = place.route | u32::from(self.speed) << 20 | u32::from(entries) << 27;
        let mut dword1 = u32::from(place.root_port) << 16;
        let mut dword2 = 0;
        if let Some(hub) = self.hub.get() {
            dword0 |= 1 << 26 | u32::from(hub.multi_tt) << 25;
            dword1 |= u32::from(hub.ports) << 24;
            dword2 |= u32::from(hub.think_time) << 16;
        }
        if let Some((hub, port, multi)) = place.tt {
            dword0 |= u32::from(multi) << 25;
            dword2 |= u32::from(hub) | u32::from(port) << 8;
        }
        input.write32(at, dword0);
        input.write32(at + 4, dword1);
        input.write32(at + 8, dword2);
    }

    /// Writes the default control endpoint's context.
    fn write_ep0(&self) {
        let at = self.input_context(2);
        let position = self.ep0.ring.position();
        let max_packet = u32::from(self.max_packet0.get());
        self.input.write32(
            at + 4,
            ERROR_COUNT << 1 | TYPE_CONTROL << 3 | max_packet << 16,
        );
        self.input
            .write64(at + 8, position.at | u64::from(position.cycle));
        self.input.write32(at + 16, 8);
    }

    /// The last endpoint ID in use.
    fn entries(&self) -> u8 {
        let endpoints = self.endpoints.borrow();
        endpoints.keys().next_back().copied().unwrap_or(1)
    }

    /// Gives the device its address with an Address Device command, with
    /// its default control endpoint's ring started from its first TRB.
    pub async fn address(&self) -> Result<(), Status> {
        self.ep0.restart(Status::PeerClosed);
        self.write_slot(0, 1 << 1, 1);
        self.write_ep0();
        let input = self.input.physical(0);
        let trb = [
            input as u32,
            (input >> 32) as u32,
            0,
            trb_type(TRB_ADDRESS_DEVICE) | u32::from(self.slot) << 24,
        ];
        self.controller.command_ok(trb).await.map(|_| ())
    }

    /// Sets the default control endpoint's maximum packet size with an
    /// Evaluate Context command, when it differs.
    pub async fn set_max_packet0(&self, max_packet: u16) -> Result<(), Status> {
        if max_packet == self.max_packet0.get() || max_packet == 0 {
            return Ok(());
        }
        self.max_packet0.set(max_packet);
        self.write_slot(0, 1 << 1, self.entries());
        self.write_ep0();
        self.evaluate().await
    }

    /// Makes the device a hub with `hub` in its slot context.
    pub async fn set_hub(&self, hub: HubInfo) -> Result<(), Status> {
        self.hub.set(Some(hub));
        self.write_slot(0, 0, self.entries());
        let input = self.input.physical(0);
        let trb = [
            input as u32,
            (input >> 32) as u32,
            0,
            trb_type(TRB_CONFIGURE_ENDPOINT) | u32::from(self.slot) << 24,
        ];
        self.controller.command_ok(trb).await.map(|_| ())
    }

    /// What the slot context says about the device as a hub.
    pub fn hub(&self) -> Option<HubInfo> {
        self.hub.get()
    }

    /// Puts the slot back in the default state with a Reset Device
    /// command, after the device's port was reset, and gives the device
    /// its address again. Its endpoints other than the default control
    /// endpoint are gone.
    pub async fn reset(&self) -> Result<(), Status> {
        self.fail_all();
        let trb = [
            0,
            0,
            0,
            trb_type(TRB_RESET_DEVICE) | u32::from(self.slot) << 24,
        ];
        self.controller.command_ok(trb).await?;
        self.address().await
    }

    async fn evaluate(&self) -> Result<(), Status> {
        let input = self.input.physical(0);
        let trb = [
            input as u32,
            (input >> 32) as u32,
            0,
            trb_type(TRB_EVALUATE_CONTEXT) | u32::from(self.slot) << 24,
        ];
        self.controller.command_ok(trb).await.map(|_| ())
    }

    /// Sends a control request on the default control endpoint: `setup`,
    /// with `out` as its data, or for IN requests (`setup[0]` bit 7)
    /// receiving up to the length in `setup`. Returns the data received.
    pub async fn control(
        &self,
        setup: [u8; 8],
        out: &[u8],
        deadline: i64,
    ) -> Result<Vec<u8>, Status> {
        let length = u16::from_le_bytes([setup[6], setup[7]]);
        let direction = match setup[0] & 0x80 {
            0 => Direction::Out,
            _ => Direction::In,
        };
        if usize::from(length) > mikros_storage::PAGE as usize
            || (direction == Direction::Out && out.len() != usize::from(length))
        {
            return Err(Status::InvalidArgs);
        }
        let page = self.controller.page()?;
        for (i, byte) in out.iter().enumerate() {
            page.write8(i as u64, *byte);
        }
        let request = Request::control(setup, direction, page.physical(0), u32::from(length));
        let result = self.ep0.transfer(request, deadline).await;
        let data = result.map(|actual| {
            let mut data = alloc::vec![0; (actual as usize).min(usize::from(length))];
            if direction == Direction::In {
                page.read_bytes(0, &mut data);
            }
            data
        });
        self.controller.give_page(page);
        data
    }

    /// Drops the endpoints with IDs in `drop` and adds `add` with a
    /// Configure Endpoint command. Returns the added endpoints.
    pub async fn configure(
        &self,
        drop: &[u8],
        add: &[descriptors::Endpoint],
    ) -> Result<Vec<Rc<Endpoint>>, Status> {
        let mut rings = Vec::new();
        for endpoint in add {
            let segments = match endpoint.kind() {
                BULK => BULK_SEGMENTS,
                _ => OTHER_SEGMENTS,
            };
            rings.push(self.controller.ring(segments)?);
        }
        let drop_flags = drop.iter().fold(0, |f, &id| f | 1u32 << id);
        let add_flags = add.iter().fold(0, |f, e| f | 1u32 << e.id());
        let mut ids: Vec<u8> = self.endpoints.borrow().keys().copied().collect();
        ids.retain(|id| !drop.contains(id));
        ids.extend(add.iter().map(|e| e.id()));
        let entries = ids.iter().copied().max().unwrap_or(1);
        self.write_slot(drop_flags, add_flags, entries);
        for (endpoint, ring) in add.iter().zip(&rings) {
            let at = self.input_context(u64::from(endpoint.id()) + 1);
            let context = endpoint_context(endpoint, self.speed);
            let position = ring.position();
            self.input.write32(at, context[0]);
            self.input.write32(at + 4, context[1]);
            self.input
                .write64(at + 8, position.at | u64::from(position.cycle));
            self.input.write32(at + 16, context[2]);
        }
        let input = self.input.physical(0);
        let trb = [
            input as u32,
            (input >> 32) as u32,
            0,
            trb_type(TRB_CONFIGURE_ENDPOINT) | u32::from(self.slot) << 24,
        ];
        self.controller.command_ok(trb).await?;
        for id in drop {
            if let Some(endpoint) = self.endpoints.borrow_mut().remove(id) {
                endpoint.remove();
            }
        }
        let mut added = Vec::new();
        for (endpoint, ring) in add.iter().zip(rings) {
            let new = Endpoint::new(self.controller.clone(), self.slot, endpoint.id(), ring);
            self.endpoints
                .borrow_mut()
                .insert(endpoint.id(), new.clone());
            added.push(new);
        }
        Ok(added)
    }

    /// The configured endpoint with ID `id`.
    pub fn endpoint(&self, id: u8) -> Option<Rc<Endpoint>> {
        self.endpoints.borrow().get(&id).cloned()
    }

    /// Fails every transfer of the device with `PEER_CLOSED`, and forgets
    /// its endpoints other than the default control endpoint.
    pub fn fail_all(&self) {
        self.ep0.restart(Status::PeerClosed);
        let endpoints = core::mem::take(&mut *self.endpoints.borrow_mut());
        for endpoint in endpoints.values() {
            endpoint.remove();
        }
    }

    /// Fails every transfer with `PEER_CLOSED`, and gives the slot back
    /// with a Disable Slot command.
    pub async fn disable(&self) {
        self.fail_all();
        self.ep0.remove();
        let trb = [
            0,
            0,
            0,
            trb_type(TRB_DISABLE_SLOT) | u32::from(self.slot) << 24,
        ];
        let _ = self.controller.command(trb).await;
        self.controller.set_device_context(self.slot, 0);
    }
}

/// The first, second and fifth dwords of the context of `endpoint` on a
/// device of `speed`: its interval, type, error count, burst, maximum
/// packet size, average TRB length and largest payload per service
/// interval. Isochronous endpoints are not configured.
fn endpoint_context(endpoint: &descriptors::Endpoint, speed: u8) -> [u32; 3] {
    let kind = endpoint.kind();
    let max_packet = u32::from(endpoint.max_packet & 0x7FF);
    let burst = match speed {
        SUPER.. => u32::from(endpoint.burst),
        HIGH if kind == INTERRUPT => u32::from(endpoint.max_packet >> 11) & 3,
        _ => 0,
    };
    let type_code = match (kind, endpoint.is_in()) {
        (BULK, false) => 2,
        (INTERRUPT, false) => 3,
        (BULK, true) => 6,
        _ => 7,
    };
    let interval = match kind {
        INTERRUPT => interval(endpoint.interval, speed),
        _ => 0,
    };
    let payload = match (kind, speed) {
        (INTERRUPT, SUPER..) => u32::from(endpoint.bytes_per_interval),
        (INTERRUPT, _) => max_packet * (burst + 1),
        _ => 0,
    };
    let average = match kind {
        INTERRUPT => payload.max(1),
        _ => 3072,
    };
    [
        interval << 16 | (payload >> 16) << 24,
        ERROR_COUNT << 1 | type_code << 3 | burst << 8 | max_packet << 16,
        average | (payload & 0xFFFF) << 16,
    ]
}

/// The interval of an interrupt endpoint context, as a power of two of
/// 125 µs, from the descriptor's `bInterval` on a device of `speed`: an
/// exponent from 1 at high speed and above, and a number of frames at
/// low and full speed.
pub fn interval(b_interval: u8, speed: u8) -> u32 {
    match speed {
        LOW | FULL => {
            let microframes = u32::from(b_interval.max(1)) * 8;
            (31 - microframes.leading_zeros()).clamp(3, 10)
        }
        _ => u32::from(b_interval.clamp(1, 16)) - 1,
    }
}
