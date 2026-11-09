//! The `Interface` protocol that the host process serves to each
//! interface's driver on its bus channel: the interface's description,
//! and control, interrupt and bulk transfers limited to what belongs to
//! the interface.

use crate::descriptors::{self, BULK, INTERRUPT, endpoint_id};
use crate::device::{FULL, HIGH, LOW, SUPER, setup};
use crate::transfer::{Direction, Endpoint, Request};
use crate::usb::{Interface, SET_CONFIGURATION, SET_INTERFACE, deadline};
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::exec::Spawner;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::usb::{
    self, EndpointInfo, InterfaceAsyncServer, InterfaceBulkTransferResponse,
    InterfaceClearHaltResponse, InterfaceControlTransferResponse,
    InterfaceGetConfigurationDescriptorResponse, InterfaceGetInfoResponse, InterfaceInfo,
    InterfaceInterruptTransferResponse, InterfaceRequest, InterfaceSetAlternateSettingResponse,
    Setup, Speed,
};
use mikros_rt::sys;
use mikros_storage::PAGE;

const SET_ADDRESS: u8 = 5;
const CLEAR_FEATURE: u8 = 1;
const ENDPOINT_HALT: u16 = 0;
/// The largest interrupt transfer.
const INTERRUPT_MAX: u32 = 1024;
/// The options of `bti_pin`.
const DEVICE_READ: u32 = 1 << 0;
const DEVICE_WRITE: u32 = 1 << 1;
const FOUR_GIB: u64 = 1 << 32;

/// Serves `Interface` for `interface` on the bus channel `end` of its
/// driver until the channel closes. Transfers are enqueued in the order
/// their requests arrive, and each one is then awaited in a task of its
/// own, so that many may be outstanding.
pub async fn serve(interface: Rc<Interface>, end: OwnedHandle, spawner: Spawner) {
    use InterfaceRequest as R;
    let reactor = interface.device.slot.controller.reactor.clone();
    let server = Rc::new(InterfaceAsyncServer::new(
        ServerEnd::<usb::Interface>::new(end),
        reactor,
    ));
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            R::GetInfo(_, r) => r.reply(
                s,
                InterfaceGetInfoResponse {
                    info: info(&interface),
                },
            ),
            R::GetConfigurationDescriptor(_, r) => {
                let data = interface.device.configuration_bytes.clone();
                r.reply(s, InterfaceGetConfigurationDescriptorResponse { data })
            }
            R::ControlTransfer(q, r) => {
                let deadline = q.deadline;
                match control(&interface, &q.setup, q.data).await {
                    Err(status) => r.reply(s, Err(status)),
                    Ok(pending) => {
                        let server = server.clone();
                        spawner.spawn(async move {
                            let data = pending.finish(deadline).await;
                            let data = data.map(|data| InterfaceControlTransferResponse { data });
                            let _ = r.reply(server.server(), data);
                        });
                        Ok(())
                    }
                }
            }
            R::InterruptTransfer(q, r) => {
                let deadline = q.deadline;
                match interrupt(&interface, q.endpoint, q.data, q.length).await {
                    Err(status) => r.reply(s, Err(status)),
                    Ok(pending) => {
                        let server = server.clone();
                        spawner.spawn(async move {
                            let data = pending.finish(deadline).await;
                            let data = data.map(|data| InterfaceInterruptTransferResponse { data });
                            let _ = r.reply(server.server(), data);
                        });
                        Ok(())
                    }
                }
            }
            R::BulkTransfer(q, r) => {
                let deadline = q.deadline;
                match bulk(&interface, q.endpoint, q.buffer, q.offset, q.length).await {
                    Err(status) => r.reply(s, Err(status)),
                    Ok(pending) => {
                        let server = server.clone();
                        spawner.spawn(async move {
                            let actual = pending.finish(deadline).await;
                            let actual =
                                actual.map(|actual| InterfaceBulkTransferResponse { actual });
                            let _ = r.reply(server.server(), actual);
                        });
                        Ok(())
                    }
                }
            }
            R::SetAlternateSetting(q, r) => {
                let set = set_alternate(&interface, q.alternate).await;
                r.reply(s, set.map(|()| InterfaceSetAlternateSettingResponse {}))
            }
            R::ClearHalt(q, r) => {
                let cleared = clear_halt(&interface, q.endpoint).await;
                r.reply(s, cleared.map(|()| InterfaceClearHaltResponse {}))
            }
        };
    }
}

fn info(interface: &Interface) -> InterfaceInfo {
    let device = &interface.device;
    let d = &device.descriptor;
    let speed = match device.slot.speed {
        LOW => Speed::Low,
        FULL => Speed::Full,
        HIGH => Speed::High,
        SUPER => Speed::Super,
        _ => Speed::SuperPlus,
    };
    let current = device
        .configuration
        .interface(interface.number, interface.alternate.get());
    let endpoints = interface.endpoints().into_iter().map(|e| EndpointInfo {
        address: e.address,
        attributes: e.attributes,
        max_packet_size: e.max_packet,
        interval: e.interval,
    });
    InterfaceInfo {
        vendor_id: d.vendor,
        product_id: d.product,
        device_version: d.version,
        speed,
        number: interface.number,
        class: current.map_or(0, |i| i.class),
        subclass: current.map_or(0, |i| i.subclass),
        protocol: current.map_or(0, |i| i.protocol),
        endpoints: endpoints.collect(),
        manufacturer: device.manufacturer.clone(),
        product: device.product.clone(),
        serial: device.serial.clone(),
    }
}

/// What turns the bytes a transfer moved into its result.
type Finish<T> = alloc::boxed::Box<dyn FnOnce(Result<u32, Status>) -> Result<T, Status>>;

/// A transfer that was enqueued: its endpoint and TD, and what to do
/// with its data once it completes.
struct Pending<T> {
    endpoint: Rc<Endpoint>,
    id: u64,
    receiver: crate::wait::Receiver<Result<u32, Status>>,
    finish: Finish<T>,
}

impl<T> Pending<T> {
    async fn finish(self, deadline: i64) -> Result<T, Status> {
        let result = self.endpoint.wait(self.id, self.receiver, deadline).await;
        (self.finish)(result)
    }
}

/// Whether the requests of `setup` are refused to an interface's
/// driver: those that set the device's address or configuration, and
/// those for another interface or its endpoints.
fn refused(interface: &Interface, setup: &Setup) -> bool {
    let standard = setup.request_type & 0x60 == 0;
    if standard && [SET_ADDRESS, SET_CONFIGURATION].contains(&setup.request) {
        return true;
    }
    let target = setup.index as u8;
    match setup.request_type & 0x1F {
        1 => target != interface.number,
        2 => {
            let own = interface.endpoints().iter().any(|e| e.address == target);
            target & 0xF != 0 && !own
        }
        _ => false,
    }
}

/// Checks and enqueues a control transfer on the default control
/// endpoint.
async fn control(
    interface: &Interface,
    setup_packet: &Setup,
    data: Vec<u8>,
) -> Result<Pending<Vec<u8>>, Status> {
    if refused(interface, setup_packet) {
        return Err(Status::AccessDenied);
    }
    let length = setup_packet.length;
    let direction = match setup_packet.request_type & 0x80 {
        0 => Direction::Out,
        _ => Direction::In,
    };
    let bad_data = match direction {
        Direction::In => !data.is_empty(),
        Direction::Out => data.len() != usize::from(length),
    };
    if bad_data || u64::from(length) > PAGE {
        return Err(Status::InvalidArgs);
    }
    let slot = &interface.device.slot;
    let packet = setup(
        setup_packet.request_type,
        setup_packet.request,
        setup_packet.value,
        setup_packet.index,
        length,
    );
    let page = Rc::new(slot.controller.page()?);
    for (i, byte) in data.iter().enumerate() {
        page.write8(i as u64, *byte);
    }
    let request = Request::control(packet, direction, page.physical(0), u32::from(length));
    let endpoint = slot.ep0.clone();
    let controller = slot.controller.clone();
    let enqueued = endpoint.enqueue(request).await;
    let (id, receiver) = match enqueued {
        Ok(enqueued) => enqueued,
        Err(status) => {
            if let Ok(page) = Rc::try_unwrap(page) {
                controller.give_page(page);
            }
            return Err(status);
        }
    };
    let finish = move |result: Result<u32, Status>| {
        let data = result.map(|actual| {
            let mut data = vec![0; (actual as usize).min(usize::from(length))];
            if direction == Direction::In {
                page.read_bytes(0, &mut data);
            }
            data
        });
        if let Ok(page) = Rc::try_unwrap(page) {
            controller.give_page(page);
        }
        data
    };
    Ok(Pending {
        endpoint,
        id,
        receiver,
        finish: alloc::boxed::Box::new(finish),
    })
}

/// The interface's endpoint with address `address` and type `kind` in
/// its current alternate setting, and its configured endpoint.
fn endpoint(
    interface: &Interface,
    address: u8,
    kind: u8,
) -> Result<(descriptors::Endpoint, Rc<Endpoint>), Status> {
    let endpoints = interface.endpoints();
    let found = endpoints
        .iter()
        .find(|e| e.address == address && e.kind() == kind);
    let found = found.ok_or(Status::InvalidArgs)?;
    let configured = interface.device.slot.endpoint(found.id());
    Ok((*found, configured.ok_or(Status::PeerClosed)?))
}

/// Checks and enqueues an interrupt transfer.
async fn interrupt(
    interface: &Interface,
    address: u8,
    data: Vec<u8>,
    length: u32,
) -> Result<Pending<Vec<u8>>, Status> {
    let (descriptor, endpoint) = endpoint(interface, address, INTERRUPT)?;
    let input = descriptor.is_in();
    let length = match input {
        true if data.is_empty() && length <= INTERRUPT_MAX => length,
        true => return Err(Status::InvalidArgs),
        false => data.len() as u32,
    };
    let controller = interface.device.slot.controller.clone();
    let page = controller.page()?;
    for (i, byte) in data.iter().enumerate() {
        page.write8(i as u64, *byte);
    }
    let request = Request::normal(
        &[page.physical(0)],
        0,
        length,
        descriptor.max_packet & 0x7FF,
    );
    let (id, receiver) = match endpoint.enqueue(request).await {
        Ok(enqueued) => enqueued,
        Err(status) => {
            controller.give_page(page);
            return Err(status);
        }
    };
    let finish = move |result: Result<u32, Status>| {
        let data = result.map(|actual| match input {
            true => {
                let mut data = vec![0; actual.min(length) as usize];
                page.read_bytes(0, &mut data);
                data
            }
            false => Vec::new(),
        });
        controller.give_page(page);
        data
    };
    Ok(Pending {
        endpoint,
        id,
        receiver,
        finish: alloc::boxed::Box::new(finish),
    })
}

/// A pin of pages of a class driver's buffer for the controller, which
/// is unpinned when it goes.
struct Pin(sys::Handle);

impl Drop for Pin {
    fn drop(&mut self) {
        // SAFETY: the call takes no pointers; it also closes the handle,
        // which the pin owns.
        unsafe { sys::pmt_unpin(self.0) };
    }
}

fn kernel(status: i64) -> Result<(), Status> {
    match status {
        mikros_rt::OK => Ok(()),
        status => Err(Status::from_raw(status as i32)),
    }
}

/// Checks and enqueues a bulk transfer of the `length` bytes at `offset`
/// in `buffer`, which the controller reaches through a pin of their
/// pages.
async fn bulk(
    interface: &Interface,
    address: u8,
    buffer: OwnedHandle,
    offset: u64,
    length: u32,
) -> Result<Pending<u32>, Status> {
    let (descriptor, endpoint) = endpoint(interface, address, BULK)?;
    let controller = interface.device.slot.controller.clone();
    let start = offset - offset % PAGE;
    let end = offset
        .checked_add(u64::from(length))
        .and_then(|end| end.checked_next_multiple_of(PAGE))
        .ok_or(Status::OutOfRange)?
        .max(start + PAGE);
    let count = (end - start) / PAGE;
    let access = match descriptor.is_in() {
        true => DEVICE_WRITE,
        false => DEVICE_READ,
    };
    let mut pmt = 0;
    // SAFETY: the out pointer is valid for the call.
    kernel(unsafe {
        sys::bti_pin(
            controller.bti.raw(),
            buffer.raw(),
            start,
            end - start,
            access,
            &mut pmt,
        )
    })?;
    let pin = Pin(pmt);
    let mut pages = vec![0u64; count as usize];
    for (i, chunk) in pages.chunks_mut(512).enumerate() {
        // SAFETY: the pointer describes `chunk`.
        kernel(unsafe {
            sys::mem_get_physical_pages(
                buffer.raw(),
                start / PAGE + 512 * i as u64,
                chunk.len() as u64,
                chunk.as_mut_ptr(),
            )
        })?;
    }
    // A controller without 64-bit addressing cannot reach the pages
    // above 4 GiB.
    if controller.below_4g && pages.iter().any(|&page| page + PAGE > FOUR_GIB) {
        return Err(Status::NoResources);
    }
    let max_packet = descriptor.max_packet & 0x7FF;
    let request = Request::normal(&pages, offset - start, length, max_packet);
    let (id, receiver) = endpoint.enqueue(request).await?;
    let finish = move |result: Result<u32, Status>| {
        drop(pin);
        drop(buffer);
        result
    };
    Ok(Pending {
        endpoint,
        id,
        receiver,
        finish: alloc::boxed::Box::new(finish),
    })
}

/// Selects alternate setting `alternate` of the interface: configures
/// its endpoints in place of those of the current one, and sends
/// SET_INTERFACE.
async fn set_alternate(interface: &Interface, alternate: u8) -> Result<(), Status> {
    let device = &interface.device;
    let new = device
        .configuration
        .interface(interface.number, alternate)
        .ok_or(Status::InvalidArgs)?;
    let usable = |e: &&descriptors::Endpoint| e.kind() != descriptors::ISOCHRONOUS;
    let drop: Vec<u8> = interface
        .endpoints()
        .iter()
        .filter(usable)
        .map(|e| e.id())
        .collect();
    let add: Vec<descriptors::Endpoint> = new.endpoints.iter().filter(usable).copied().collect();
    device.slot.configure(&drop, &add).await?;
    interface.alternate.set(alternate);
    let number = u16::from(interface.number);
    let request = setup(0x01, SET_INTERFACE, u16::from(alternate), number, 0);
    device.slot.control(request, &[], deadline()).await?;
    Ok(())
}

/// Clears the halt of the interface's endpoint with address `address`
/// on the device with CLEAR_FEATURE, and resets the endpoint on the
/// controller, which resets its data toggle.
async fn clear_halt(interface: &Interface, address: u8) -> Result<(), Status> {
    let endpoints = interface.endpoints();
    let found = endpoints.iter().find(|e| e.address == address);
    let found = *found.ok_or(Status::InvalidArgs)?;
    let slot = &interface.device.slot;
    if let Some(endpoint) = slot.endpoint(endpoint_id(address)) {
        endpoint.idle().await;
    }
    let request = setup(0x02, CLEAR_FEATURE, ENDPOINT_HALT, u16::from(address), 0);
    slot.control(request, &[], deadline()).await?;
    if found.kind() != descriptors::ISOCHRONOUS {
        slot.configure(&[found.id()], &[found]).await?;
    }
    Ok(())
}
