//! The framebuffer console that shows the kernel log.

use crate::paging::Policy;
use crate::space;
use boot_protocol::{BootInfo, flags};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::console::{CELL_HEIGHT, CELL_WIDTH, Console, Layout, Surface};

/// The mapped framebuffer.
struct Framebuffer(*mut u32);

impl Surface for Framebuffer {
    fn set(&mut self, index: usize, value: u32) {
        // SAFETY: the console only addresses pixels inside the layout it
        // was made with, which the mapping covers.
        unsafe { self.0.add(index).write_volatile(value) };
    }
}

struct State {
    framebuffer: Framebuffer,
    console: Console<'static>,
}

struct Shared(UnsafeCell<Option<State>>);

// SAFETY: the state is written once by `init` before `READY` is set, and
// after that only by the holder of `BUSY`.
unsafe impl Sync for Shared {}

static STATE: Shared = Shared(UnsafeCell::new(None));
/// Set once the console is drawn on.
static READY: AtomicBool = AtomicBool::new(false);
/// Set while a log line is drawn.
static BUSY: AtomicBool = AtomicBool::new(false);

/// Maps the framebuffer, if there is one, clears it, and draws the log
/// records written so far.
pub fn init(info: &BootInfo) {
    if info.flags & flags::FRAMEBUFFER_PRESENT == 0 {
        return;
    }
    let fb = &info.framebuffer;
    let layout = Layout {
        width: fb.width as usize,
        height: fb.height as usize,
        stride: fb.stride as usize,
        masks: [fb.red_mask, fb.green_mask, fb.blue_mask],
    };
    let base = space::map_physical(fb.phys_base, fb.size, Policy::WriteCombining);
    let cells = (layout.width / CELL_WIDTH) * (layout.height / CELL_HEIGHT);
    let mut state = State {
        framebuffer: Framebuffer(base as *mut u32),
        console: Console::new(layout, alloc::vec![b' '; cells].leak()),
    };
    state.console.clear(&mut state.framebuffer);
    let mut after = 0;
    while let Some(record) = crate::log::next_after(after) {
        let line = kernel_core::log::line(&record, b"\n");
        state.console.write(line.as_bytes(), &mut state.framebuffer);
        after = record.sequence;
    }
    // SAFETY: READY is not set yet, so nothing else uses the state.
    unsafe { *STATE.0.get() = Some(state) };
    READY.store(true, Ordering::Release);
}

/// Draws `line`. A line that arrives
/// while another one is drawn, which only an interrupt handler can
/// cause, is not drawn.
pub fn write(line: &[u8]) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    if BUSY.swap(true, Ordering::Acquire) {
        return;
    }
    // SAFETY: holding BUSY gives exclusive use of the state, which READY
    // says is set.
    if let Some(state) = unsafe { (*STATE.0.get()).as_mut() } {
        state.console.write(line, &mut state.framebuffer);
    }
    BUSY.store(false, Ordering::Release);
}
