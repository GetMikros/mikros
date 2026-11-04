//! `Interrupt` objects, which set `SIGNALED` when their interrupt line is
//! triggered or a device sends their message.

use crate::apic::{self, DEVICE_VECTORS};
use crate::iommu::Remapping;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::object::budget::Budget;
use kernel_core::object::{Header, Object, ObjectType, signals};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// `options` bits of `interrupt_create`: level instead of edge triggering,
/// and active-low instead of active-high polarity.
pub const LEVEL: u32 = 1 << 0;
pub const ACTIVE_LOW: u32 = 1 << 1;

const VECTORS: usize = (DEVICE_VECTORS.end - DEVICE_VECTORS.start) as usize;

/// The address range of messages to the local APICs.
const MSI_BASE: u64 = 0xFEE0_0000;

/// The line, if any, and object a device vector is given to.
struct Slot {
    gsi: Option<u32>,
    object: Weak<Interrupt>,
}

/// The most objects `interrupt_create_msi` makes at once.
pub const MSI_MAX: usize = 32;

/// The address and data a device writes to send a message.
#[derive(Clone, Copy, Default)]
pub struct Message {
    pub address: u64,
    pub data: u32,
}

/// The device vectors, from the first, with what each is given to. A slot
/// is taken when an object is created and freed when it is destroyed.
static SLOTS: SpinLock<[Option<Slot>; VECTORS]> = SpinLock::new([const { None }; VECTORS]);

pub struct Interrupt {
    header: Header,
    budget: Arc<Budget>,
    /// The line, for an object that is not message-signaled.
    gsi: Option<u32>,
    vector: u8,
    /// The requester ID whose interrupts the vector's remapping entry
    /// accepts, or `NO_SOURCE` when it has none.
    source: AtomicU64,
}

const NO_SOURCE: u64 = u64::MAX;

impl Interrupt {
    /// The kernel memory an interrupt object is charged.
    const COST: u64 = size_of::<Interrupt>() as u64;

    /// An object for global system interrupt `gsi`, charged to `budget`,
    /// with a device vector that the line is routed to as `options` says.
    /// Fails with `InvalidArgs` for unknown options, `AlreadyExists` while
    /// another object exists for `gsi`, `NoResources` when no vector is
    /// free and `NotFound` when no I/O APIC handles `gsi`.
    pub fn new(budget: &Arc<Budget>, gsi: u32, options: u32) -> Result<Arc<Interrupt>, Error> {
        if options & !(LEVEL | ACTIVE_LOW) != 0 {
            return Err(Error::InvalidArgs);
        }
        budget.charge_kernel(Self::COST)?;
        let interrupt =
            Self::claim(budget, gsi).inspect_err(|_| budget.release_kernel(Self::COST))?;
        // From here on, dropping the object releases the charge and the
        // vector.
        let source = apic::route(
            gsi,
            interrupt.vector,
            options & LEVEL != 0,
            options & ACTIVE_LOW != 0,
        )?;
        interrupt.set_source(source);
        Ok(interrupt)
    }

    /// Creates the object in a free slot.
    fn claim(budget: &Arc<Budget>, gsi: u32) -> Result<Arc<Interrupt>, Error> {
        let mut slots = SLOTS.lock();
        if slots.iter().flatten().any(|s| s.gsi == Some(gsi)) {
            return Err(Error::AlreadyExists);
        }
        let index = slots
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoResources)?;
        let interrupt = Self::make(budget, Some(gsi), index)?;
        slots[index] = Some(Slot {
            gsi: Some(gsi),
            object: Arc::downgrade(&interrupt),
        });
        Ok(interrupt)
    }

    fn make(budget: &Arc<Budget>, gsi: Option<u32>, index: usize) -> Result<Arc<Interrupt>, Error> {
        kernel_core::fallible::try_arc(Interrupt {
            header: Header::new(ObjectType::Interrupt),
            budget: budget.clone(),
            gsi,
            vector: DEVICE_VECTORS.start + index as u8,
            source: AtomicU64::new(NO_SOURCE),
        })
    }

    fn set_source(&self, source: Option<u64>) {
        self.source
            .store(source.unwrap_or(NO_SOURCE), Ordering::Relaxed);
    }

    /// `count` message-signaled objects for the device with requester ID
    /// `source`, each charged to `budget`, with the messages that trigger
    /// them, which only that device may send when interrupts are
    /// remapped. Fails with `NoResources`, taking nothing, when fewer than
    /// `count` vectors are free, and with `NotSupported` without remapping
    /// when the boot processor's ID does not fit a message.
    pub fn new_msi(
        budget: &Arc<Budget>,
        source: u64,
        count: usize,
    ) -> Result<Vec<(Arc<Interrupt>, Message)>, Error> {
        debug_assert!((1..=MSI_MAX).contains(&count));
        let remapping = crate::iommu::remapping();
        let id = apic::boot_id();
        if remapping == Remapping::Off && id > 0xFF {
            return Err(Error::NotSupported);
        }
        budget.charge_kernel(count as u64 * Self::COST)?;
        let mut objects = Vec::new();
        if objects.try_reserve(count).is_err() {
            budget.release_kernel(count as u64 * Self::COST);
            return Err(Error::NoMemory);
        }
        {
            let mut slots = SLOTS.lock();
            let free: Vec<usize> = (0..VECTORS)
                .filter(|&i| slots[i].is_none())
                .take(count)
                .collect();
            if free.len() < count {
                drop(slots);
                budget.release_kernel(count as u64 * Self::COST);
                return Err(Error::NoResources);
            }
            for (made, &index) in free.iter().enumerate() {
                match Self::make(budget, None, index) {
                    Ok(interrupt) => {
                        slots[index] = Some(Slot {
                            gsi: None,
                            object: Arc::downgrade(&interrupt),
                        });
                        objects.push(interrupt);
                    }
                    Err(e) => {
                        // The objects made release their own charges.
                        drop(slots);
                        budget.release_kernel((count - made) as u64 * Self::COST);
                        drop(objects);
                        return Err(e);
                    }
                }
            }
        }
        let mut out = Vec::new();
        out.try_reserve(count).map_err(|_| Error::NoMemory)?;
        for interrupt in objects {
            let index = interrupt.index();
            let message = match remapping {
                Remapping::Vtd => {
                    crate::iommu::remap(interrupt.vector, source, Some(false))?;
                    interrupt.set_source(Some(source));
                    // The remappable format: the index, with its top bit
                    // apart, and the format bit; the data is not used.
                    let address = MSI_BASE
                        | u64::from(index & 0x7FFF) << 5
                        | 1 << 4
                        | u64::from(index >> 15) << 2;
                    Message { address, data: 0 }
                }
                // On AMD-Vi the vector selects the entry in the device's
                // table, which delivers it.
                Remapping::Amd | Remapping::Off => {
                    if remapping == Remapping::Amd {
                        crate::iommu::remap(interrupt.vector, source, Some(false))?;
                        interrupt.set_source(Some(source));
                    }
                    // Fixed delivery and edge triggering to the boot
                    // processor.
                    Message {
                        address: MSI_BASE | u64::from(id & 0xFF) << 12,
                        data: u32::from(interrupt.vector),
                    }
                }
            };
            out.push((interrupt, message));
        }
        Ok(out)
    }

    /// The remapping entry of the object, which follows its vector.
    fn index(&self) -> u16 {
        u16::from(self.vector - DEVICE_VECTORS.start)
    }

    /// Clears `SIGNALED` and unmasks the line, if the object has one.
    pub fn ack(&self) {
        self.header.update_signals(signals::SIGNALED, 0);
        if let Some(gsi) = self.gsi {
            apic::mask_line(gsi, false);
        }
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        if let Some(gsi) = self.gsi {
            apic::mask_line(gsi, true);
        }
        // When the unit does not confirm, the entry stays, but its vector
        // reaches no object until it is given out again.
        let source = self.source.load(Ordering::Relaxed);
        if source != NO_SOURCE {
            let _ = crate::iommu::remap(self.vector, source, None);
        }
        SLOTS.lock()[usize::from(self.vector - DEVICE_VECTORS.start)] = None;
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Interrupt {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// Handles device vector `vector`: masks the line it is given to, if
/// any, signals end of interrupt and sets `SIGNALED` on its object, which
/// counts messages that arrive while it is set once. A vector given to
/// nothing only gets the end of interrupt.
pub fn deliver(vector: u8) {
    let slot = SLOTS.lock()[usize::from(vector - DEVICE_VECTORS.start)]
        .as_ref()
        .map(|s| (s.gsi, s.object.clone()));
    if let Some((Some(gsi), _)) = slot {
        apic::mask_line(gsi, true);
    }
    apic::eoi();
    // Interrupts arrive only where no other code of the kernel runs, so
    // the object outlives this reference and is not destroyed here.
    if let Some(interrupt) = slot.and_then(|(_, object)| object.upgrade()) {
        interrupt.header.update_signals(0, signals::SIGNALED);
    }
}
