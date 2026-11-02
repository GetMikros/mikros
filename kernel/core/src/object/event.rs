//! Events: objects whose only state is their signals.

use super::budget::Budget;
use super::{Header, Object, ObjectType};
use crate::status::Error;
use alloc::sync::Arc;

pub struct Event {
    header: Header,
    budget: Arc<Budget>,
}

impl Event {
    /// The kernel memory an event is charged.
    const COST: u64 = size_of::<Event>() as u64;

    /// An event with every signal clear, charged to `budget`.
    pub fn new(budget: &Arc<Budget>) -> Result<Arc<Event>, Error> {
        budget.charge_kernel(Self::COST)?;
        crate::fallible::try_arc(Event {
            header: Header::new(ObjectType::Event),
            budget: budget.clone(),
        })
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Event {
    fn header(&self) -> &Header {
        &self.header
    }
}
