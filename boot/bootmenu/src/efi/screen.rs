//! The graphics output used to show the menu.

use alloc::vec::Vec;
use bootmenu::canvas::Canvas;
use bootmenu::display::{self, Choice, ModeInfo};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, Mode, PixelFormat};
use uefi::{Handle, table};

/// A graphics output in a mode with 32-bit pixels.
pub struct Screen {
    gop: ScopedProtocol<GraphicsOutput>,
    width: usize,
    height: usize,
    pixels: Vec<BltPixel>,
}

/// Opens the graphics output of the console output handle, or else of any
/// handle that has one.
fn open_gop() -> Option<ScopedProtocol<GraphicsOutput>> {
    let console = table::system_table_raw().and_then(|st| {
        // SAFETY: the system table pointer is valid while boot services run,
        // and its console output handle is either null or a valid handle.
        unsafe { Handle::from_ptr(st.as_ref().stdout_handle) }
    });
    let open = |handle: Handle| {
        let params = OpenProtocolParams {
            handle,
            agent: boot::image_handle(),
            controller: None,
        };
        // SAFETY: GetProtocol does not take ownership of the protocol, so the
        // console driver keeps using it. The menu stops drawing before it
        // starts another image or returns.
        unsafe {
            boot::open_protocol::<GraphicsOutput>(params, OpenProtocolAttributes::GetProtocol)
        }
        .ok()
    };
    console.and_then(open).or_else(|| {
        boot::get_handle_for_protocol::<GraphicsOutput>()
            .ok()
            .and_then(open)
    })
}

fn mode_info(index: u32, info: &uefi::proto::console::gop::ModeInfo) -> ModeInfo {
    let linear_32bit = match info.pixel_format() {
        PixelFormat::Rgb | PixelFormat::Bgr => true,
        PixelFormat::Bitmask => info
            .pixel_bitmask()
            .is_some_and(|m| display::bitmask_is_32bit(m.red, m.green, m.blue, m.reserved)),
        PixelFormat::BltOnly => false,
    };
    let (width, height) = info.resolution();
    ModeInfo {
        index,
        width,
        height,
        linear_32bit,
    }
}

impl Screen {
    /// Opens the graphics output and puts it in a mode with 32-bit pixels.
    /// Returns `None` when there is no graphics output or no such mode.
    pub fn open() -> Option<Screen> {
        let mut gop = open_gop()?;
        // Modes are identified by their position in `modes`; the index of
        // the current mode is not needed, since it is only ever kept.
        let current = mode_info(u32::MAX, &gop.current_mode_info());
        let modes: Vec<Mode> = gop.modes().collect();
        let infos = (0..).zip(&modes).map(|(i, m)| mode_info(i, m.info()));
        match display::choose(current, infos) {
            Choice::Keep => {}
            Choice::Switch(index) => {
                gop.set_mode(&modes[index as usize]).ok()?;
            }
            Choice::Unusable => return None,
        }
        let (width, height) = gop.current_mode_info().resolution();
        Some(Screen {
            gop,
            width,
            height,
            pixels: Vec::new(),
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Copies `canvas`, which must have the size of the screen, to it.
    pub fn present(&mut self, canvas: &Canvas) {
        self.pixels.clear();
        for y in 0..self.height {
            let row = canvas.row(y);
            self.pixels
                .extend(row.iter().map(|c| BltPixel::new(c.r(), c.g(), c.b())));
        }
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer: &self.pixels,
            src: BltRegion::Full,
            dest: (0, 0),
            dims: (self.width, self.height),
        });
    }
}
