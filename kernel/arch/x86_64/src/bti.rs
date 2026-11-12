//! `Bti` objects, which name a device to the IOMMU, and `Pmt` objects,
//! which keep pages pinned for one.

use crate::iommu;
use crate::log::log;
use crate::memory_object::MemoryObject;
use crate::publish::Work;
use alloc::sync::Arc;
use alloc::vec::Vec;
use kernel_core::object::budget::Budget;
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// `options` bits of `bti_pin`: the device may read the pages, and write
/// them.
pub const DEVICE_READ: u32 = 1 << 0;
pub const DEVICE_WRITE: u32 = 1 << 1;

pub struct Bti {
    header: Header,
    budget: Arc<Budget>,
    rid: u64,
}

impl Bti {
    const COST: u64 = size_of::<Bti>() as u64;

    /// A `Bti` for the device with requester ID `rid`, charged to
    /// `budget`. Fails with `OutOfRange` when DMA is isolated and no
    /// IOMMU covers the device.
    pub fn new(budget: &Arc<Budget>, rid: u64) -> Result<Arc<Bti>, Error> {
        if iommu::unknown(rid) {
            return Err(Error::OutOfRange);
        }
        budget.charge_kernel(Self::COST)?;
        kernel_core::fallible::try_arc(Bti {
            header: Header::new(ObjectType::Bti),
            budget: budget.clone(),
            rid,
        })
    }

    /// The requester ID of the device.
    pub fn requester_id(&self) -> u64 {
        self.rid
    }

    /// Lets the device reach the `size` bytes from `offset` of `mem`, a
    /// contiguous or pinned object, as `options` says, and returns the
    /// `Pmt` that keeps them pinned, charged to `budget`. Fails with
    /// `WrongType` for other objects and `InvalidArgs` for unaligned,
    /// empty or outside ranges and bad options.
    pub fn pin(
        &self,
        budget: &Arc<Budget>,
        mem: Arc<MemoryObject>,
        offset: u64,
        size: u64,
        options: u32,
    ) -> Result<Arc<Pmt>, Error> {
        if !mem.is_fixed() {
            return Err(Error::WrongType);
        }
        let access = DEVICE_READ | DEVICE_WRITE;
        if options & !access != 0 || options & access == 0 {
            return Err(Error::InvalidArgs);
        }
        let aligned = offset.is_multiple_of(PAGE_SIZE) && size.is_multiple_of(PAGE_SIZE);
        if !aligned || size == 0 || mem.check_range(offset, size).is_err() {
            return Err(Error::InvalidArgs);
        }
        let mut frames = Vec::new();
        for first in (offset / PAGE_SIZE..(offset + size) / PAGE_SIZE).step_by(512) {
            let count = ((offset + size) / PAGE_SIZE - first).min(512);
            let part = mem.physical_pages(first, count)?;
            frames
                .try_reserve(part.len())
                .map_err(|_| Error::NoMemory)?;
            frames.extend(part);
        }
        budget.charge_kernel(Pmt::cost(frames.len()))?;
        let pin = Pin {
            rid: self.rid,
            _mem: mem,
            frames,
            read: options & DEVICE_READ != 0,
            write: options & DEVICE_WRITE != 0,
        };
        let pmt = kernel_core::fallible::try_arc(Pmt {
            header: Header::new(ObjectType::Pmt),
            budget: budget.clone(),
            charged: Pmt::cost(pin.frames.len()),
            pin: SpinLock::new(kernel_core::lock_order::BTI_PIN, None),
        });
        let pmt = match pmt {
            Ok(pmt) => pmt,
            Err(e) => {
                budget.release_kernel(Pmt::cost(pin.frames.len()));
                return Err(e);
            }
        };
        let pin = pin.apply()?;
        *pmt.pin.lock() = Some(pin);
        Ok(pmt)
    }

    /// Takes back the pins of every `Pmt` of the device that was closed
    /// without `pmt_unpin`.
    pub fn release_quarantine(&self) -> Result<(), Error> {
        let mut released = Vec::new();
        {
            let mut quarantine = QUARANTINE.lock();
            let mut i = 0;
            while i < quarantine.len() {
                if quarantine[i].0.rid == self.rid {
                    released.push(quarantine.swap_remove(i));
                } else {
                    i += 1;
                }
            }
        }
        // Every pin is tried; one whose release the IOMMU did not confirm
        // keeps its charge too.
        let mut result = Ok(());
        for (pin, charge) in released {
            if let Err(e) = pin.remove() {
                core::mem::forget(charge);
                result = result.and(Err(e));
            }
        }
        result
    }
}

impl Drop for Bti {
    fn drop(&mut self) {
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Bti {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// Pages a device may reach, with the object that holds them.
struct Pin {
    rid: u64,
    /// Held so that the frames stay the object's until the pin goes.
    _mem: Arc<MemoryObject>,
    frames: Vec<u64>,
    read: bool,
    write: bool,
}

impl Pin {
    /// Lets the device reach the pages and returns the pin, or takes back
    /// what it gave when that fails. A pin whose access the IOMMU did not
    /// confirm taken back is kept forever.
    fn apply(self) -> Result<Pin, Error> {
        let mut work = Work::new(crate::sched::preempt_point);
        let mut failed = None;
        for (i, &phys) in self.frames.iter().enumerate() {
            work.item();
            if let Err(e) = iommu::map(self.rid, phys, self.read, self.write) {
                failed = Some((i, e));
                break;
            }
        }
        let (mapped, e) = match failed {
            Some(failure) => failure,
            None => match iommu::sync(self.rid) {
                Ok(()) => {
                    #[cfg(feature = "diag")]
                    crate::diag::PINNED.fetch_add(
                        self.frames.len() as u64,
                        core::sync::atomic::Ordering::Relaxed,
                    );
                    return Ok(self);
                }
                Err(e) => (self.frames.len(), e),
            },
        };
        self.unmap(&self.frames[..mapped]);
        if let Err(sync) = iommu::sync(self.rid) {
            keep_forever(self, sync);
        }
        Err(e)
    }

    fn unmap(&self, frames: &[u64]) {
        let mut work = Work::new(crate::sched::preempt_point);
        for &phys in frames {
            work.item();
            iommu::unmap(self.rid, phys, self.read, self.write);
        }
    }

    /// Takes the device's access back, and returns once the IOMMU no
    /// longer uses it. The object goes with the pin, unless the IOMMU did
    /// not confirm it: then the pin is kept forever.
    fn remove(self) -> Result<(), Error> {
        #[cfg(feature = "diag")]
        crate::diag::PINNED.fetch_sub(
            self.frames.len() as u64,
            core::sync::atomic::Ordering::Relaxed,
        );
        self.unmap(&self.frames);
        iommu::sync(self.rid).inspect_err(|&e| keep_forever(self, e))
    }
}

/// Keeps `pin`, whose access the IOMMU did not confirm taken back, with
/// its object forever, so that the device can never reach pages handed
/// out again.
fn keep_forever(pin: Pin, e: Error) {
    log!(
        Warning,
        "iommu: device {:#06x} keeps {} pages forever: {e:?}",
        pin.rid,
        pin.frames.len()
    );
    core::mem::forget(pin);
}

/// The pins of `Pmt` objects closed without `pmt_unpin`, with the budget
/// charge of each, kept until `bti_release_quarantine`.
static QUARANTINE: SpinLock<Vec<(Pin, Charge)>> =
    SpinLock::new(kernel_core::lock_order::BTI_QUARANTINE, Vec::new());

/// Kernel memory charged to a budget, released when dropped.
struct Charge(Arc<Budget>, u64);

impl Drop for Charge {
    fn drop(&mut self) {
        self.0.release_kernel(self.1);
    }
}

pub struct Pmt {
    header: Header,
    budget: Arc<Budget>,
    charged: u64,
    /// The pin until `pmt_unpin` takes it.
    pin: SpinLock<Option<Pin>>,
}

impl Pmt {
    /// The kernel memory of a `Pmt` of `pages` pages, with its share of
    /// the quarantine.
    fn cost(pages: usize) -> u64 {
        (size_of::<Pmt>() + size_of::<(Pin, Charge)>() + pages * size_of::<u64>()) as u64
    }

    /// Takes the device's access to the pages back, and returns once the
    /// IOMMU no longer uses it.
    pub fn unpin(&self) -> Result<(), Error> {
        let pin = self.pin.lock().take();
        match pin {
            Some(pin) => pin.remove(),
            None => Ok(()),
        }
    }
}

impl Drop for Pmt {
    fn drop(&mut self) {
        let charge = Charge(self.budget.clone(), self.charged);
        let Some(pin) = self.pin.lock().take() else {
            return;
        };
        // The pages stay reachable, and charged, until the quarantine is
        // released. Room was charged with the `Pmt`.
        let mut quarantine = QUARANTINE.lock();
        if quarantine.try_reserve(1).is_ok() {
            quarantine.push((pin, charge));
        } else {
            // Without room the pin is kept forever rather than taken back
            // under the device.
            core::mem::forget(pin);
            core::mem::forget(charge);
        }
    }
}

impl Object for Pmt {
    fn header(&self) -> &Header {
        &self.header
    }
}
