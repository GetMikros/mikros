//! The pointer devices of the console.

use alloc::vec::Vec;
use bootmenu::input::Event;
use bootmenu::pointer::Tracker;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::ProtocolPointer;
use uefi::proto::console::pointer::{AbsolutePointer, Pointer};
use uefi::{Handle, table};

/// The console input handle, which also carries the console's pointers.
pub fn console_in() -> Option<Handle> {
    table::system_table_raw().and_then(|st| {
        // SAFETY: the system table pointer is valid while boot services run,
        // and its console input handle is either null or a valid handle.
        unsafe { Handle::from_ptr(st.as_ref().stdin_handle) }
    })
}

/// Opens protocol `P` on the console input handle without taking it over.
pub fn open_on_console<P: ProtocolPointer + ?Sized>() -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams {
        handle: console_in()?,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: GetProtocol does not take ownership of the protocol, so the
    // console keeps using it. It is closed when the returned value is
    // dropped, before an entry is started.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

/// The relative and absolute pointers of the console, with the pointer
/// position they drive.
pub struct Pointers {
    relative: Option<ScopedProtocol<Pointer>>,
    absolute: Option<ScopedProtocol<AbsolutePointer>>,
    tracker: Tracker,
}

impl Pointers {
    pub fn open(width: usize, height: usize) -> Pointers {
        Pointers {
            relative: open_on_console::<Pointer>(),
            absolute: open_on_console::<AbsolutePointer>(),
            tracker: Tracker::new(width as i32, height as i32),
        }
    }

    /// The events signaled when a pointer has new input.
    pub fn wait_events(&self) -> Vec<uefi::Event> {
        let relative = self
            .relative
            .as_ref()
            .and_then(|p| p.wait_for_input_event().ok());
        let absolute = self
            .absolute
            .as_ref()
            .and_then(|p| p.wait_for_input_event().ok());
        relative.into_iter().chain(absolute).collect()
    }

    /// Reads the pointers and returns the resulting menu events.
    pub fn read(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(pointer) = &mut self.relative
            && let Ok(Some(state)) = pointer.read_state()
        {
            let mode = pointer.mode();
            events.extend(self.tracker.relative(
                state.relative_movement_x,
                state.relative_movement_y,
                (mode.resolution_x, mode.resolution_y),
                bool::from(state.left_button),
            ));
        }
        if let Some(pointer) = &mut self.absolute
            && let Ok(Some(state)) = pointer.read_state()
        {
            let mode = pointer.mode();
            events.extend(self.tracker.absolute(
                (state.current_x, state.current_y),
                (mode.absolute_min_x, mode.absolute_max_x),
                (mode.absolute_min_y, mode.absolute_max_y),
                state.active_buttons & 1 != 0,
            ));
        }
        events
    }
}
