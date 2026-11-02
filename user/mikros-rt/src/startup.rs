//! Receiving and checking the kernel's startup message.

use crate::sys::{self, Handle};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

/// The largest message, in bytes and in handles.
const BYTES_MAX: usize = 65536;
const HANDLES_MAX: usize = 64;

const HEADER_SIZE: usize = 0x68;
const ENTRY_SIZE: usize = 24;
const FRAMEBUFFER_PRESENT: u32 = 1 << 0;
const BOOT_TIME_VALID: u32 = 1 << 1;
/// The handles every startup message carries, before the framebuffer.
const FIXED_HANDLES: usize = 7;

/// The boot framebuffer.
#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    pub memory: Handle,
    pub phys_base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
}

/// A range of the boot memory map.
#[derive(Clone, Copy, Debug)]
pub struct MemoryRange {
    pub phys_start: u64,
    pub page_count: u64,
    pub kind: u32,
}

/// What the kernel hands the initial process.
pub struct Startup {
    pub process: Handle,
    pub thread: Handle,
    pub root_resource: Handle,
    pub boot_image: Handle,
    pub log: Handle,
    pub budget: Handle,
    pub clock_page: Handle,
    pub framebuffer: Option<Framebuffer>,
    pub firmware_table_phys: u64,
    pub boot_image_size: u64,
    pub arch: u32,
    pub boot_time: Option<i64>,
    pub cmdline: &'static [u8],
    memory_map: &'static [u8],
}

impl Startup {
    /// The ranges of the boot memory map.
    pub fn memory_map(&self) -> impl Iterator<Item = MemoryRange> + '_ {
        self.memory_map
            .as_chunks::<ENTRY_SIZE>()
            .0
            .iter()
            .map(|e| MemoryRange {
                phys_start: u64_at(e, 0),
                page_count: u64_at(e, 8),
                kind: u32_at(e, 16),
            })
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// The bytes at `offset` of `len` bytes, if they lie inside `bytes`.
fn range(bytes: &[u8], offset: u32, len: usize) -> Option<&[u8]> {
    let start = offset as usize;
    bytes.get(start..start.checked_add(len)?)
}

/// The startup state in a message of `bytes` and `handles`, if the
/// message is a complete version 1 startup message.
fn decode(bytes: &'static [u8], handles: &[Handle]) -> Option<Startup> {
    if bytes.len() < HEADER_SIZE || u32_at(bytes, 0x00) != 1 {
        return None;
    }
    let flags = u32_at(bytes, 0x04);
    let has_framebuffer = flags & FRAMEBUFFER_PRESENT != 0;
    if handles.len() != FIXED_HANDLES + usize::from(has_framebuffer) {
        return None;
    }
    let cmdline = range(bytes, u32_at(bytes, 0x48), u32_at(bytes, 0x4C) as usize)?;
    let map_offset = u32_at(bytes, 0x50);
    let map_len = (u32_at(bytes, 0x54) as usize).checked_mul(ENTRY_SIZE)?;
    if !map_offset.is_multiple_of(8) {
        return None;
    }
    let memory_map = range(bytes, map_offset, map_len)?;
    let framebuffer = has_framebuffer.then(|| Framebuffer {
        memory: handles[FIXED_HANDLES],
        phys_base: u64_at(bytes, 0x10),
        size: u64_at(bytes, 0x18),
        width: u32_at(bytes, 0x20),
        height: u32_at(bytes, 0x24),
        stride: u32_at(bytes, 0x28),
        format: u32_at(bytes, 0x2C),
        red_mask: u32_at(bytes, 0x30),
        green_mask: u32_at(bytes, 0x34),
        blue_mask: u32_at(bytes, 0x38),
        reserved_mask: u32_at(bytes, 0x3C),
    });
    Some(Startup {
        process: handles[0],
        thread: handles[1],
        root_resource: handles[2],
        boot_image: handles[3],
        log: handles[4],
        budget: handles[5],
        clock_page: handles[6],
        framebuffer,
        firmware_table_phys: u64_at(bytes, 0x08),
        boot_image_size: u64_at(bytes, 0x40),
        arch: u32_at(bytes, 0x58),
        boot_time: (flags & BOOT_TIME_VALID != 0).then(|| u64_at(bytes, 0x60) as i64),
        cmdline,
        memory_map,
    })
}

/// The buffers the startup message is read into, used once by the first
/// thread before anything else runs.
struct Buffers {
    bytes: UnsafeCell<[u8; BYTES_MAX]>,
    handles: UnsafeCell<[Handle; HANDLES_MAX]>,
    startup: UnsafeCell<MaybeUninit<Startup>>,
}

// SAFETY: only `receive` touches the buffers, once, before the program
// can start other threads.
unsafe impl Sync for Buffers {}

static BUFFERS: Buffers = Buffers {
    bytes: UnsafeCell::new([0; BYTES_MAX]),
    handles: UnsafeCell::new([0; HANDLES_MAX]),
    startup: UnsafeCell::new(MaybeUninit::uninit()),
};

/// Reads the startup message from `bootstrap` and closes it. Returns the
/// startup state, with the log handle in use, or closes the received
/// handles and returns `None` when the message is not a startup message.
pub(crate) fn receive(bootstrap: Handle) -> Option<&'static Startup> {
    // SAFETY: this runs once, on the first thread, so nothing else
    // references the buffers.
    let (bytes, handles, startup) = unsafe {
        (
            &mut *BUFFERS.bytes.get(),
            &mut *BUFFERS.handles.get(),
            &mut *BUFFERS.startup.get(),
        )
    };
    let (mut len, mut count) = (0u64, 0u64);
    // SAFETY: the pointers and capacities describe the buffers above.
    let status = unsafe {
        sys::channel_read(
            bootstrap,
            bytes.as_mut_ptr(),
            BYTES_MAX as u64,
            handles.as_mut_ptr(),
            HANDLES_MAX as u64,
            &mut len,
            &mut count,
        )
    };
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(bootstrap) };
    if status != crate::OK {
        return None;
    }
    let received = &handles[..count as usize];
    let bytes: &'static [u8] = &bytes[..len as usize];
    let Some(decoded) = decode(bytes, received) else {
        for &h in received {
            // SAFETY: closing takes no pointers.
            unsafe { sys::handle_close(h) };
        }
        return None;
    };
    crate::log::set_handle(decoded.log);
    Some(startup.write(decoded))
}
