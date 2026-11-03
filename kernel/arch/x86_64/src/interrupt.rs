//! `Interrupt` objects for interrupt lines, which set `SIGNALED` when
//! their line is triggered.

use crate::apic::{self, DEVICE_VECTORS};
use alloc::sync::{Arc, Weak};
use kernel_core::object::budget::Budget;
use kernel_core::object::{Header, Object, ObjectType, signals};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// `options` bits of `interrupt_create`: level instead of edge triggering,
/// and active-low instead of active-high polarity.
pub const LEVEL: u32 = 1 << 0;
pub const ACTIVE_LOW: u32 = 1 << 1;

const VECTORS: usize = (DEVICE_VECTORS.end - DEVICE_VECTORS.start) as usize;

/// The line and object a device vector is given to.
struct Slot {
    gsi: u32,
    object: Weak<Interrupt>,
}

/// The device vectors, from the first, with what each is given to. A slot
/// is taken when an object is created and freed when it is destroyed.
static SLOTS: SpinLock<[Option<Slot>; VECTORS]> = SpinLock::new([const { None }; VECTORS]);

pub struct Interrupt {
    header: Header,
    budget: Arc<Budget>,
    gsi: u32,
    vector: u8,
}

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
        apic::route(
            gsi,
            interrupt.vector,
            options & LEVEL != 0,
            options & ACTIVE_LOW != 0,
        )?;
        Ok(interrupt)
    }

    /// Creates the object in a free slot.
    fn claim(budget: &Arc<Budget>, gsi: u32) -> Result<Arc<Interrupt>, Error> {
        let mut slots = SLOTS.lock();
        if slots.iter().flatten().any(|s| s.gsi == gsi) {
            return Err(Error::AlreadyExists);
        }
        let index = slots
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoResources)?;
        let interrupt = kernel_core::fallible::try_arc(Interrupt {
            header: Header::new(ObjectType::Interrupt),
            budget: budget.clone(),
            gsi,
            vector: DEVICE_VECTORS.start + index as u8,
        })?;
        slots[index] = Some(Slot {
            gsi,
            object: Arc::downgrade(&interrupt),
        });
        Ok(interrupt)
    }

    /// Clears `SIGNALED` and unmasks the line.
    pub fn ack(&self) {
        self.header.update_signals(signals::SIGNALED, 0);
        apic::mask_line(self.gsi, false);
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        apic::mask_line(self.gsi, true);
        SLOTS.lock()[usize::from(self.vector - DEVICE_VECTORS.start)] = None;
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Interrupt {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// Handles device vector `vector`: masks the line it is given to, signals
/// end of interrupt and sets `SIGNALED` on its object. A vector given to
/// nothing only gets the end of interrupt.
pub fn deliver(vector: u8) {
    let slot = SLOTS.lock()[usize::from(vector - DEVICE_VECTORS.start)]
        .as_ref()
        .map(|s| (s.gsi, s.object.clone()));
    if let Some((gsi, _)) = slot {
        apic::mask_line(gsi, true);
    }
    apic::eoi();
    // Interrupts arrive only where no other code of the kernel runs, so
    // the object outlives this reference and is not destroyed here.
    if let Some(interrupt) = slot.and_then(|(_, object)| object.upgrade()) {
        interrupt.header.update_signals(0, signals::SIGNALED);
    }
}
