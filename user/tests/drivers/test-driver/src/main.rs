//! The test drivers of the device manager tests, which do as their app
//! ID says.
//!
//! Each serves `Driver` on its `PROVIDER` startup handle, and `Stop`
//! ends the driver. `Start` depends on the app ID:
//! - As `system.driver.test-host`, a bus driver, it succeeds when the
//!   driver is started for the q35 host bridge, `pci-host-0`, as
//!   `platform:pci-host-ecam`, with its properties, no bus channel, and
//!   an `MMIO` and a `DMA` resource in that order, and fails with
//!   `INVALID_ARGS` otherwise. Once it succeeded, the driver checks that
//!   the device manager refuses to add a device with an invalid hardware
//!   ID, with `INVALID_ARGS`, and to remove its own device, with
//!   `ACCESS_DENIED`, or one that does not exist, with `NOT_FOUND`. It
//!   checks that `RequestResource` gives it a shared `MMIO` resource in
//!   its ECAM range and a `BTI` resource for requester IDs of bus 0, and
//!   refuses a `DMA` resource with `INVALID_ARGS` and a `BTI` resource
//!   for segment 1 with `ACCESS_DENIED`. It checks that it can publish
//!   two `storage` services, which get consecutive IDs, but not one of
//!   the reserved class `audio`, with `INVALID_ARGS`. It then adds a
//!   device `test-child` as `test:child` with a property `slot` of `0`,
//!   or `checks-failed` when a check failed. It accepts
//!   `ConnectChild`, and once the child's driver has written on the bus
//!   channel, it removes the child, when `ConnectChild` said that the
//!   child's driver needs `dma`. `ResetChild` succeeds.
//! - As `system.driver.test-child`, it succeeds when it has a bus
//!   channel and `RequestResource` refuses it, which does not need
//!   `bus`, an `MMIO` resource with `ACCESS_DENIED`, after writing a
//!   message on the bus channel, and fails with `INVALID_ARGS`
//!   otherwise.
//! - As `system.driver.test-pci`, the driver of the q35 host bridge's
//!   function 00:00.0, it succeeds when its bus channel serves
//!   `mikros.pci.Device` for that function: `GetInfo` reports it, a
//!   `ReadConfig` of the vendor ID reads 0x8086 and a misaligned one
//!   fails with `INVALID_ARGS`, a `WriteConfig` of the command register
//!   fails with `ACCESS_DENIED`, and, as its manifest does not need
//!   `dma`, `GetBti` and `SetBusMaster(true)` fail with `ACCESS_DENIED`
//!   while `SetBusMaster(false)` succeeds, and `GetInterrupt` fails with
//!   `NOT_SUPPORTED`, as the function has no INTx pin. It fails with
//!   `INVALID_ARGS` otherwise.
//! - As `system.driver.test-edu`, the driver of QEMU's `edu` device,
//!   whose manifest needs `dma`, it succeeds when its bus channel gives
//!   it the device's interrupts: `EnableMsi` gives one interrupt with
//!   `WAIT`, `WRITE` and `INSPECT`, which the device triggers when told
//!   to raise one through BAR 0, while `GetInterrupt` fails with
//!   `BAD_STATE`; after `DisableMsi`, `GetInterrupt` gives an event with
//!   `WAIT` and without `SIGNAL`, and `AckInterrupt` succeeds. It fails
//!   with `INVALID_ARGS` otherwise.
//! - As `system.driver.test-disk`, the driver of each of the test
//!   machine's disks, it succeeds when its bus channel serves `Block` for
//!   it: `GetInfo` reports 512-byte blocks, the model `virtio-blk`,
//!   `QEMU NVMe Ctrl` or `QEMU HARDDISK`, the serial `mikros` and a
//!   writable disk; reads find the protective MBR's
//!   signature in block 0 and the GPT header in block 1; requests of no
//!   blocks or beyond the disk fail with `INVALID_ARGS` and
//!   `OUT_OF_RANGE`, and a buffer above the connection's limit with
//!   `NO_RESOURCES`; and the last block, written with `fua`, reads back
//!   as written before it is written back as it was and flushed. It fails
//!   with `INVALID_ARGS` otherwise.
//! - As `system.driver.test-decline`, it fails with `NOT_SUPPORTED`.
//! - As `system.driver.test-close`, the driver publishes a `storage`
//!   service and ends without replying.
//! - As `system.driver.test-reset`, the driver ends without replying.
//! - As any other, it fails with `INVALID_ARGS`.
//!
//! Drivers other than `test-host` fail `ResetChild` and `ConnectChild`
//! with `NOT_SUPPORTED`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use core::cell::RefCell;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DeviceManagerRequestResourceResponse, DriverAsyncServer,
    DriverConnectChildResponse, DriverRequest, DriverResetChildResponse, DriverStartRequest,
    DriverStartResponse, DriverStopResponse, Property,
};
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::Provider;
use mikros_rt::interfaces::mikros::storage::{BlockAsyncClient, BlockFlags};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const HOST: &str = "system.driver.test-host";
const CHILD: &str = "system.driver.test-child";
const PCI: &str = "system.driver.test-pci";
const EDU: &str = "system.driver.test-edu";
const DISK: &str = "system.driver.test-disk";
/// The models of the test machine's disks: the virtio disk, and the
/// scratch disks behind QEMU's NVMe and AHCI controllers.
const MODELS: [&str; 3] = ["virtio-blk", "QEMU NVMe Ctrl", "QEMU HARDDISK"];
const DECLINE: &str = "system.driver.test-decline";
const CLOSE: &str = "system.driver.test-close";
const RESET: &str = "system.driver.test-reset";
const KIND_MMIO: u32 = 1;
const KIND_DMA: u32 = 4;
const KIND_BTI: u32 = 8;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
const READABLE: u32 = 1 << 0;
const SIGNALED: u32 = 1 << 3;
/// Handle rights, and where the basic information record holds them.
const WRITE: u32 = 1 << 3;
const WAIT: u32 = 1 << 6;
const SIGNAL: u32 = 1 << 7;
const INSPECT: u32 = 1 << 9;
const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
/// The registers of `edu` that raise and acknowledge its interrupt.
const EDU_RAISE: u64 = 0x60;
const EDU_ACK: u64 = 0x64;
const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE_SIZE: u64 = 4096;
/// How long the device has to raise its interrupt.
const RAISE_LIMIT: i64 = 5_000_000_000;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let app_id = managed.header.app_id.clone();
    let process = managed.header.process.raw();
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = DriverAsyncServer::new(end, reactor.clone());
    let manager: Rc<RefCell<Option<Rc<DeviceManagerAsyncClient>>>> = Rc::default();
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let host = app_id == HOST;
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    let ecam = request
                        .device
                        .properties
                        .iter()
                        .find(|p| p.key == "ecam_base");
                    let ecam = ecam.and_then(|p| p.value.strip_prefix("0x"));
                    let ecam = ecam.and_then(|b| u64::from_str_radix(b, 16).ok());
                    let as_expected = host && expected(&request);
                    let bus = request.bus;
                    let client = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
                    let client = Rc::new(client);
                    let result = match app_id.as_str() {
                        HOST if as_expected => Ok(DriverStartResponse {}),
                        CHILD => {
                            let refused =
                                client.request_resource(KIND_MMIO, 0xFE00_0000, 0x1000, false);
                            match matches!(refused.await, Ok(Err(Status::AccessDenied))) {
                                true => signal(bus.as_ref()).map(|()| DriverStartResponse {}),
                                false => Err(Status::InvalidArgs),
                            }
                        }
                        PCI => {
                            let works = match bus {
                                Some(bus) => pci_device_works(bus, &reactor).await,
                                None => false,
                            };
                            match works {
                                true => Ok(DriverStartResponse {}),
                                false => Err(Status::InvalidArgs),
                            }
                        }
                        EDU => {
                            let works = match bus {
                                Some(bus) => edu_interrupts_work(bus, &reactor, process).await,
                                None => false,
                            };
                            match works {
                                true => Ok(DriverStartResponse {}),
                                false => Err(Status::InvalidArgs),
                            }
                        }
                        DISK => {
                            let works = match bus {
                                Some(bus) => disk_works(bus, &reactor, process).await,
                                None => false,
                            };
                            match works {
                                true => Ok(DriverStartResponse {}),
                                false => Err(Status::InvalidArgs),
                            }
                        }
                        DECLINE => Err(Status::NotSupported),
                        RESET => return 0,
                        CLOSE => {
                            let _ = publish(&client, "storage").await;
                            return 0;
                        }
                        _ => Err(Status::InvalidArgs),
                    };
                    let started = result.is_ok();
                    let replied = responder.reply(s, result);
                    if host && started {
                        *manager.borrow_mut() = Some(client.clone());
                        spawner.spawn(add_child(client, ecam.unwrap_or(0)));
                    }
                    replied
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => match host {
                    true => responder.reply(s, Ok(DriverResetChildResponse {})),
                    false => responder.reply(s, Err(Status::NotSupported)),
                },
                DriverRequest::ConnectChild(request, responder) => {
                    let client = manager.borrow().clone();
                    match (host, client) {
                        (true, Some(client)) => {
                            let replied = responder.reply(s, Ok(DriverConnectChildResponse {}));
                            let (device, bus, dma) = (request.device, request.bus, request.dma);
                            spawner.spawn(remove_child(client, reactor.clone(), device, bus, dma));
                            replied
                        }
                        _ => responder.reply(s, Err(Status::NotSupported)),
                    }
                }
            };
        }
        0
    })
}

/// As the host bridge's driver, with its ECAM range at `ecam`: checks
/// that the device manager refuses the requests it should and grants
/// the others, and adds the child device, named after the outcome.
async fn add_child(manager: Rc<DeviceManagerAsyncClient>, ecam: u64) {
    let bad = manager
        .add_device("bad".into(), vec!["Bad:1".into()], vec![], vec![])
        .await;
    let own = manager.remove_device(1).await;
    let unknown = manager.remove_device(999).await;
    let request = |kind, base, size, shared| manager.request_resource(kind, base, size, shared);
    let granted = |reply: Result<Result<DeviceManagerRequestResourceResponse, Status>, _>,
                   wanted| {
        matches!(reply, Ok(Ok(r)) if kind(&r.resource) == Some(wanted))
    };
    let mmio = granted(request(KIND_MMIO, ecam, 0x1000, true).await, KIND_MMIO);
    let bti = granted(request(KIND_BTI, 0, 0x100, false).await, KIND_BTI);
    let dma = request(KIND_DMA, 0, 0, false).await;
    let other_segment = request(KIND_BTI, 0x1_0000, 1, false).await;
    let first = publish(&manager, "storage").await;
    let second = publish(&manager, "storage").await;
    let reserved = publish(&manager, "audio").await;
    let published = matches!((first, second), (Ok(a), Ok(b)) if b == a + 1);
    let checked = published
        && reserved == Err(Status::InvalidArgs)
        && matches!(bad, Ok(Err(Status::InvalidArgs)))
        && matches!(own, Ok(Err(Status::AccessDenied)))
        && matches!(unknown, Ok(Err(Status::NotFound)))
        && mmio
        && bti
        && matches!(dma, Ok(Err(Status::InvalidArgs)))
        && matches!(other_segment, Ok(Err(Status::AccessDenied)));
    let name = if checked {
        "test-child"
    } else {
        "checks-failed"
    };
    let slot = Property {
        key: "slot".into(),
        value: "0".into(),
    };
    let _ = manager
        .add_device(name.into(), vec!["test:child".into()], vec![slot], vec![])
        .await;
}

/// As the host bridge's driver: once the driver of the child `device`
/// wrote on its bus channel `bus`, removes the child, when `dma` says
/// that its driver needs `dma`.
async fn remove_child(
    manager: Rc<DeviceManagerAsyncClient>,
    reactor: Reactor,
    device: u64,
    bus: OwnedHandle,
    dma: bool,
) {
    if reactor.signals(bus.raw(), READABLE).await.is_ok() && dma {
        let _ = manager.remove_device(device).await;
    }
    drop(bus);
}

/// Publishes a service of `class`, whose provider channel stays open
/// for as long as the driver runs, and returns its ID.
async fn publish(manager: &DeviceManagerAsyncClient, class: &str) -> Result<u64, Status> {
    let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
    core::mem::forget(server);
    match manager.publish_service(class.into(), client).await {
        Ok(Ok(published)) => Ok(published.id),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// As the driver of PCI function 00:00.0 of q35, without `dma`: whether
/// `bus` serves `mikros.pci.Device` for that function as it should.
async fn pci_device_works(bus: OwnedHandle, reactor: &Reactor) -> bool {
    let device = DeviceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let Ok(info) = device.get_info().await.map(|r| r.info) else {
        return false;
    };
    let at_host_bridge = (info.bus, info.device, info.function) == (0, 0, 0)
        && (info.vendor_id, info.device_id) == (0x8086, 0x29c0);
    let vendor = device.read_config(0, 2).await;
    let misaligned = device.read_config(1, 2).await;
    let command = device.write_config(0x04, 2, 0).await;
    let bti = device.get_bti().await;
    let master = device.set_bus_master(true).await;
    let no_master = device.set_bus_master(false).await;
    let no_intx = device.get_interrupt().await;
    at_host_bridge
        && matches!(vendor, Ok(Ok(r)) if r.value == 0x8086)
        && matches!(misaligned, Ok(Err(Status::InvalidArgs)))
        && matches!(command, Ok(Err(Status::AccessDenied)))
        && matches!(bti, Ok(Err(Status::AccessDenied)))
        && matches!(master, Ok(Err(Status::AccessDenied)))
        && matches!(no_master, Ok(Ok(_)))
        && matches!(no_intx, Ok(Err(Status::NotSupported)))
}

/// As the driver of `edu`, with `dma`: whether `bus` gives it the
/// device's interrupts as it should, and its message-signaled interrupt
/// arrives when the device raises it.
async fn edu_interrupts_work(bus: OwnedHandle, reactor: &Reactor, process: sys::Handle) -> bool {
    let device = DeviceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let Ok(Ok(bar)) = device.get_bar(0).await.map(|r| r.map(|r| r.bar)) else {
        return false;
    };
    let Some(registers) = map(&bar.resource, bar.base, process) else {
        return false;
    };
    let master = device.set_bus_master(true).await;
    let Ok(Ok(enabled)) = device.enable_msi(4).await else {
        return false;
    };
    let busy = device.get_interrupt().await;
    let interrupt = &enabled.interrupts[0];
    let msi_rights = rights(interrupt);
    let write = |offset: u64, value: u32| {
        // SAFETY: the register lies in the mapped page of BAR 0.
        unsafe { core::ptr::write_volatile((registers + offset) as *mut u32, value) };
    };
    write(EDU_RAISE, 1);
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    let raised = unsafe {
        sys::object_wait_one(
            interrupt.raw(),
            SIGNALED,
            now() + RAISE_LIMIT,
            &mut observed,
        )
    };
    write(EDU_ACK, 1);
    // SAFETY: the call takes no pointers.
    let acked = unsafe { sys::interrupt_ack(interrupt.raw()) };
    let disabled = device.disable_msi().await;
    let intx = device.get_interrupt().await;
    let event_rights = match &intx {
        Ok(Ok(r)) => rights(&r.event),
        _ => None,
    };
    let intx_acked = device.ack_interrupt().await;
    matches!(master, Ok(Ok(_)))
        && enabled.interrupts.len() == 1
        && matches!(busy, Ok(Err(Status::BadState)))
        && msi_rights.is_some_and(|r| r & (WAIT | WRITE | INSPECT) == WAIT | WRITE | INSPECT)
        && raised == mikros_rt::OK
        && acked == mikros_rt::OK
        && matches!(disabled, Ok(Ok(_)))
        && event_rights.is_some_and(|r| r & WAIT != 0 && r & SIGNAL == 0)
        && matches!(intx_acked, Ok(Ok(_)))
}

/// As the driver of the test machine's disk: whether `bus` serves
/// `Block` for it as it should.
async fn disk_works(bus: OwnedHandle, reactor: &Reactor, process: sys::Handle) -> bool {
    let disk = BlockAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let Ok(info) = disk.get_info().await.map(|r| r.info) else {
        return false;
    };
    let described = info.block_size == 512
        && info.block_count > 34
        && info.max_transfer_blocks > 0
        && MODELS.contains(&info.model.as_str())
        && info.serial == "mikros"
        && info.flags.0 & BlockFlags::READ_ONLY.0 == 0;
    let Ok(Ok(created)) = disk.create_buffer(8192).await else {
        return false;
    };
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let mapped = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            created.memory.raw(),
            0,
            8192,
            &mut at,
        )
    };
    if mapped != mikros_rt::OK {
        return false;
    }
    // SAFETY: the buffer's 8192 bytes are mapped at `at`.
    let bytes = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, 8192) };
    let id = created.buffer;
    fn ok<T, E>(reply: Result<Result<T, Status>, E>) -> bool {
        matches!(reply, Ok(Ok(_)))
    }
    let mbr = ok(disk.read(id, 0, 0, 1).await) && bytes[510..512] == [0x55, 0xAA];
    let gpt = ok(disk.read(id, 512, 1, 1).await) && &bytes[512..520] == b"EFI PART";
    let none = disk.read(id, 0, 0, 0).await;
    let beyond = disk.read(id, 0, info.block_count, 1).await;
    let too_big = disk.create_buffer(65 << 20).await;
    let last = info.block_count - 1;
    let saved = ok(disk.read(id, 1024, last, 1).await);
    for (i, byte) in bytes[2048..2560].iter_mut().enumerate() {
        *byte = i as u8 ^ 0x5A;
    }
    let written = ok(disk.write(id, 2048, last, 1, true).await);
    let read_back =
        ok(disk.read(id, 4096, last, 1).await) && bytes[4096..4608] == bytes[2048..2560];
    let restored = ok(disk.write(id, 1024, last, 1, false).await) && ok(disk.flush().await);
    described
        && mbr
        && gpt
        && matches!(none, Ok(Err(Status::InvalidArgs)))
        && matches!(beyond, Ok(Err(Status::OutOfRange)))
        && matches!(too_big, Ok(Err(Status::NoResources)))
        && saved
        && written
        && read_back
        && restored
}

/// Maps the first page of the BAR at `base` from its `resource`.
fn map(resource: &OwnedHandle, base: u64, process: sys::Handle) -> Option<u64> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::mem_create_physical(resource.raw(), base, PAGE_SIZE, UNCACHED_DEVICE, &mut mem)
    };
    if status != mikros_rt::OK {
        return None;
    }
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            mem,
            0,
            PAGE_SIZE,
            &mut at,
        )
    };
    (status == mikros_rt::OK).then_some(at)
}

/// The rights of `handle`.
fn rights(handle: &OwnedHandle) -> Option<u32> {
    let mut info = [0u8; INFO_BASIC_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `info` and `len`.
    let status = unsafe {
        sys::object_get_info(
            handle.raw(),
            INFO_BASIC,
            info.as_mut_ptr(),
            info.len() as u64,
            &mut len,
        )
    };
    (status == mikros_rt::OK).then(|| u32::from_le_bytes([info[20], info[21], info[22], info[23]]))
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// As the child's driver: writes a message on the bus channel `bus`.
fn signal(bus: Option<&OwnedHandle>) -> Result<(), Status> {
    let bus = bus.ok_or(Status::InvalidArgs)?;
    mikros_rt::midl::channel_write(bus.raw(), b"started", &[]).map_err(|_| Status::Internal)
}

/// Whether `Start` describes the q35 host bridge with the resources
/// the driver's manifest asks for.
fn expected(request: &DriverStartRequest) -> bool {
    let device = &request.device;
    let property = |key: &str| {
        device
            .properties
            .iter()
            .find(|p| p.key == key)
            .map(|p| p.value.as_str())
    };
    let kinds: alloc::vec::Vec<Option<u32>> = request.resources.iter().map(kind).collect();
    device.name == "pci-host-0"
        && device.hardware_ids == ["platform:pci-host-ecam"]
        && property("segment") == Some("0")
        && property("bus_start") == Some("0")
        && property("bus_end") == Some("255")
        && property("ecam_base").is_some_and(|b| b.starts_with("0x"))
        && request.bus.is_none()
        && kinds == [Some(KIND_MMIO), Some(KIND_DMA)]
}

/// The kind of `resource`, if it is one.
fn kind(resource: &OwnedHandle) -> Option<u32> {
    let mut info = [0u8; INFO_RESOURCE_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `info` and `len`.
    let status = unsafe {
        sys::object_get_info(
            resource.raw(),
            INFO_RESOURCE,
            info.as_mut_ptr(),
            info.len() as u64,
            &mut len,
        )
    };
    (status == mikros_rt::OK).then(|| u32::from_le_bytes([info[0], info[1], info[2], info[3]]))
}
