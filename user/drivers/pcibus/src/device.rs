//! The `Device` protocol that the driver of each function gets as its
//! bus channel.

use crate::bus::{Bus, Function};
use crate::interrupts::{self, Msi};
use crate::pci::{
    BarKind, COMMAND, COMMAND_BUS_MASTER, COMMAND_IO, COMMAND_MEMORY, Config, msi_registers,
};
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::pci::{
    self, Device, DeviceAckInterruptResponse, DeviceAsyncServer, DeviceDisableMsiResponse,
    DeviceEnableMsiResponse, DeviceGetBarResponse, DeviceGetBtiResponse, DeviceGetInfoResponse,
    DeviceGetInterruptResponse, DeviceInfo, DeviceReadConfigResponse, DeviceRequest,
    DeviceSetBusMasterResponse, DeviceWriteConfigResponse,
};
use mikros_rt::sys;

const SAME_RIGHTS: u32 = 1 << 31;
/// The size of a function's configuration space, and the start of what
/// its driver may write.
const CONFIG_SIZE: u32 = 4096;
const WRITABLE_FROM: u16 = 0x40;

/// Serves `Device` for `function` of `bus` on the bus channel `end` of
/// its driver, whose manifest needs `dma` when `dma` is true, until the
/// channel closes. The function decodes memory and I/O meanwhile; once
/// the channel closes, its message-signaled interrupts are disabled,
/// bus mastering and decoding are disabled and the quarantine of its
/// bus transaction initiator is released. `process` is the bus
/// driver's, into which MSI-X tables are mapped.
///
/// While message-signaled interrupts are enabled, `GetInterrupt` and
/// `EnableMsi` fail with `BAD_STATE`, and so does `DisableMsi` while
/// they are not.
pub async fn serve(
    bus: Rc<Bus>,
    function: Rc<Function>,
    end: OwnedHandle,
    dma: bool,
    process: sys::Handle,
) {
    use DeviceRequest as R;
    let at = function.at;
    let command = |change: &dyn Fn(u32) -> u32| {
        let mut config = &bus.ecam;
        let value = config.read(at, COMMAND, 2);
        config.write(at, COMMAND, 2, change(value));
    };
    command(&|c| c | u32::from(COMMAND_IO | COMMAND_MEMORY));
    let server = DeviceAsyncServer::new(ServerEnd::<Device>::new(end), bus.reactor.clone());
    let mut msi: Option<Msi> = None;
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            R::GetInfo(_, r) => r.reply(
                s,
                DeviceGetInfoResponse {
                    info: info(&bus, &function),
                },
            ),
            R::ReadConfig(q, r) => {
                let value =
                    checked(q.offset, q.width).map(|()| (&bus.ecam).read(at, q.offset, q.width));
                r.reply(s, value.map(|value| DeviceReadConfigResponse { value }))
            }
            R::WriteConfig(q, r) => {
                let written = write_config(&bus, at, q.offset, q.width, q.value);
                r.reply(s, written.map(|()| DeviceWriteConfigResponse {}))
            }
            R::GetBar(q, r) => r.reply(
                s,
                bar(&function, q.index).map(|bar| DeviceGetBarResponse { bar }),
            ),
            R::SetBusMaster(q, r) => {
                let set = match q.enabled && !dma {
                    true => Err(Status::AccessDenied),
                    false => {
                        let bit = u32::from(COMMAND_BUS_MASTER);
                        match q.enabled {
                            true => command(&|c| c | bit),
                            false => command(&|c| c & !bit),
                        }
                        Ok(DeviceSetBusMasterResponse {})
                    }
                };
                r.reply(s, set)
            }
            R::GetInterrupt(_, r) => {
                let event = match msi {
                    Some(_) => Err(Status::BadState),
                    None => interrupts::get_interrupt(&bus, &function).await,
                };
                r.reply(s, event.map(|event| DeviceGetInterruptResponse { event }))
            }
            R::AckInterrupt(_, r) => {
                let acked = interrupts::ack_interrupt(&bus, &function);
                r.reply(s, acked.map(|()| DeviceAckInterruptResponse {}))
            }
            R::EnableMsi(q, r) => {
                let enabled = match msi {
                    Some(_) => Err(Status::BadState),
                    None => interrupts::enable_msi(&bus, &function, process, q.count).await,
                };
                let reply = match enabled {
                    Ok((state, interrupts)) => {
                        msi = Some(state);
                        Ok(DeviceEnableMsiResponse { interrupts })
                    }
                    Err(status) => Err(status),
                };
                r.reply(s, reply)
            }
            R::DisableMsi(_, r) => {
                let disabled = match msi.take() {
                    Some(state) => {
                        interrupts::disable(&bus, process, state);
                        Ok(DeviceDisableMsiResponse {})
                    }
                    None => Err(Status::BadState),
                };
                r.reply(s, disabled)
            }
            R::GetBti(_, r) => {
                let bti = match (dma, &function.bti) {
                    (false, _) => Err(Status::AccessDenied),
                    (true, Ok(bti)) => duplicate(bti).map(|bti| DeviceGetBtiResponse { bti }),
                    (true, Err(status)) => Err(*status),
                };
                r.reply(s, bti)
            }
        };
    }
    if let Some(state) = msi.take() {
        interrupts::disable(&bus, process, state);
    }
    let off = u32::from(COMMAND_IO | COMMAND_MEMORY | COMMAND_BUS_MASTER);
    command(&|c| c & !off);
    if let Ok(bti) = &function.bti {
        // SAFETY: the call takes no pointers.
        unsafe { sys::bti_release_quarantine(bti.raw()) };
    }
}

/// What `GetInfo` says about `function`.
fn info(bus: &Bus, function: &Function) -> DeviceInfo {
    let (at, h) = (function.at, &function.header);
    DeviceInfo {
        segment: bus.segment,
        bus: at.bus,
        device: at.device,
        function: at.function,
        vendor_id: h.vendor_id,
        device_id: h.device_id,
        subsystem_vendor_id: h.subsystem_vendor_id,
        subsystem_id: h.subsystem_id,
        revision: h.revision,
        class: h.class,
        subclass: h.subclass,
        prog_if: h.prog_if,
        interrupt_pin: h.interrupt_pin,
    }
}

/// Whether an access of `width` bytes at `offset` is one of 1, 2 or 4
/// bytes, aligned to its width, within the configuration space.
fn checked(offset: u16, width: u8) -> Result<(), Status> {
    let valid = matches!(width, 1 | 2 | 4)
        && offset.is_multiple_of(u16::from(width))
        && u32::from(offset) + u32::from(width) <= CONFIG_SIZE;
    if valid {
        Ok(())
    } else {
        Err(Status::InvalidArgs)
    }
}

/// Writes `value` to the configuration space of `at`, except in the
/// header's first 64 bytes and the MSI and MSI-X capabilities, which
/// fail with `ACCESS_DENIED`.
fn write_config(
    bus: &Bus,
    at: crate::pci::Address,
    offset: u16,
    width: u8,
    value: u32,
) -> Result<(), Status> {
    checked(offset, width)?;
    let end = offset + u16::from(width);
    let mut config = &bus.ecam;
    let in_msi = msi_registers(&config, at)
        .iter()
        .any(|r| offset < r.end && r.start < end);
    if offset < WRITABLE_FROM || in_msi {
        return Err(Status::AccessDenied);
    }
    config.write(at, offset, width, value);
    Ok(())
}

/// BAR `index` of `function`, with a copy of its resource.
fn bar(function: &Function, index: u8) -> Result<pci::Bar, Status> {
    let (bar, resource) = function
        .bars
        .iter()
        .find(|(bar, _)| bar.index == index)
        .ok_or(Status::NotFound)?;
    let resource = match resource {
        Ok(resource) => duplicate(resource)?,
        Err(status) => return Err(*status),
    };
    Ok(pci::Bar {
        index,
        kind: match bar.kind {
            BarKind::Mmio => pci::BarKind::Mmio,
            BarKind::Io => pci::BarKind::Io,
        },
        prefetchable: bar.prefetchable,
        base: bar.base,
        size: bar.size,
        resource,
    })
}

fn duplicate(handle: &OwnedHandle) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}
