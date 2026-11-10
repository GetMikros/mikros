//! The functions behind the host bridge: finding them, sizing their
//! BARs and claiming those with `RequestResource`, memory BARs in whole
//! pages, creating their bus transaction initiators, and adding them as
//! devices.

use crate::ecam::Ecam;
use crate::interrupts::{Intx, Line};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_pci::{Address, Bar, Claim, Header, claims, scan, size_bars};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::DeviceManagerAsyncClient;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::sys;

pub const KIND_MMIO: u32 = 1;
pub const KIND_IOPORT: u32 = 2;
pub const KIND_BTI: u32 = 8;

/// A function: its address and header, its BARs, each with the
/// resource claimed for it, which memory BARs of the function that share
/// pages share, or why there is none, its bus transaction initiator or
/// why there is none, and its INTx once a driver asked for it.
pub struct Function {
    pub at: Address,
    pub header: Header,
    pub bars: Vec<(Bar, Result<Rc<OwnedHandle>, Status>)>,
    pub bti: Result<OwnedHandle, Status>,
    pub intx: RefCell<Option<Intx>>,
}

/// The host bridge's buses: their configuration space, segment, the
/// device manager, the driver's directory, the `BTI` resource of their
/// requester IDs, the `MSI` resource once one was needed, the INTx
/// lines in use by global system interrupt, the executor, and the
/// functions found, by the ID of their device, with whether they have
/// all been added.
pub struct Bus {
    pub ecam: Ecam,
    pub segment: u16,
    pub manager: DeviceManagerAsyncClient,
    pub directory: DirectoryAsyncClient,
    pub bti_resource: Result<OwnedHandle, Status>,
    pub msi_resource: RefCell<Option<OwnedHandle>>,
    pub lines: RefCell<BTreeMap<u32, Rc<Line>>>,
    pub reactor: Reactor,
    pub spawner: Spawner,
    functions: RefCell<BTreeMap<u64, Rc<Function>>>,
    complete: Cell<bool>,
    waiters: RefCell<Vec<Waker>>,
}

impl Bus {
    pub fn new(
        ecam: Ecam,
        segment: u16,
        manager: DeviceManagerAsyncClient,
        directory: DirectoryAsyncClient,
        bti_resource: Result<OwnedHandle, Status>,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Bus {
        Bus {
            ecam,
            segment,
            reactor,
            manager,
            directory,
            bti_resource,
            msi_resource: RefCell::new(None),
            lines: RefCell::new(BTreeMap::new()),
            spawner,
            functions: RefCell::new(BTreeMap::new()),
            complete: Cell::new(false),
            waiters: RefCell::new(Vec::new()),
        }
    }

    /// The function of device `id`, once it has been added, or none when
    /// no function has that device.
    pub async fn function(&self, id: u64) -> Option<Rc<Function>> {
        poll_fn(|cx| {
            if let Some(function) = self.functions.borrow().get(&id) {
                return Poll::Ready(Some(function.clone()));
            }
            if self.complete.get() {
                return Poll::Ready(None);
            }
            self.waiters.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn wake(&self) {
        for waker in self.waiters.borrow_mut().drain(..) {
            waker.wake();
        }
    }
}

/// The requester ID of the function at `at` in `segment`.
pub fn requester_id(segment: u16, at: Address) -> u64 {
    (u64::from(segment) << 16)
        | (u64::from(at.bus) << 8)
        | (u64::from(at.device) << 3)
        | u64::from(at.function)
}

/// Finds the functions of `bus`, sizes their BARs and claims them as
/// `mikros_pci::claims` plans, a resource for each group of a function's
/// memory BARs that share pages, creates their bus transaction
/// initiators, and adds each function as a device, named
/// `pci-<segment>:<bus>:<device>.<function>` in hexadecimal, with its
/// hardware IDs. A BAR without a resource has `ACCESS_DENIED`.
pub async fn enumerate(bus: Rc<Bus>) {
    let ecam = &bus.ecam;
    // Whether a memory BAR gets its pages depends on the BARs of every
    // other function, so all are sized first.
    let found: Vec<(Address, Header, Vec<Bar>)> = scan(&ecam, ecam.bus_start(), ecam.bus_end())
        .into_iter()
        .map(|at| (at, Header::read(&ecam, at), size_bars(&mut &bus.ecam, at)))
        .collect();
    let planned = {
        let all: Vec<&[Bar]> = found.iter().map(|(_, _, bars)| bars.as_slice()).collect();
        claims(&all)
    };
    for ((at, header, sized), planned) in found.into_iter().zip(planned) {
        let mut groups: BTreeMap<usize, Result<Rc<OwnedHandle>, Status>> = BTreeMap::new();
        let mut bars = Vec::new();
        for (bar, claim) in sized.into_iter().zip(planned) {
            let claimed = match claim {
                Claim::Io { base, size } => {
                    request(&bus, KIND_IOPORT, base, size).await.map(Rc::new)
                }
                Claim::Pages { group, start, end } => match groups.get(&group) {
                    Some(claimed) => claimed.clone(),
                    None => {
                        let claimed = request(&bus, KIND_MMIO, start, end - start)
                            .await
                            .map(Rc::new);
                        groups.insert(group, claimed.clone());
                        claimed
                    }
                },
                Claim::Shared => Err(Status::AccessDenied),
            };
            bars.push((bar, claimed));
        }
        let bti = match &bus.bti_resource {
            Ok(resource) => bti_create(resource, requester_id(bus.segment, at)),
            Err(status) => Err(*status),
        };
        let name = format!(
            "pci-{:04x}:{:02x}:{:02x}.{:x}",
            bus.segment, at.bus, at.device, at.function
        );
        let ids = header.hardware_ids().into_iter().collect();
        let added = bus.manager.add_device(name, ids, vec![], vec![]).await;
        if let Ok(Ok(added)) = added {
            let function = Function {
                at,
                header,
                bars,
                bti,
                intx: RefCell::new(None),
            };
            bus.functions
                .borrow_mut()
                .insert(added.id, Rc::new(function));
            bus.wake();
        }
    }
    bus.complete.set(true);
    bus.wake();
}

/// An exclusive resource of `kind` from `base` for `size` bytes, through
/// the device manager: `ACCESS_DENIED` when it is not granted.
async fn request(bus: &Bus, kind: u32, base: u64, size: u64) -> Result<OwnedHandle, Status> {
    match bus.manager.request_resource(kind, base, size, false).await {
        Ok(Ok(claimed)) => Ok(claimed.resource),
        _ => Err(Status::AccessDenied),
    }
}

/// A bus transaction initiator for `requester_id`, made from the `BTI`
/// resource `resource`.
fn bti_create(resource: &OwnedHandle, requester_id: u64) -> Result<OwnedHandle, Status> {
    let mut bti = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::bti_create(resource.raw(), requester_id, 0, &mut bti) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(bti) })
}
