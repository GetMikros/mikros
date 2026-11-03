//! Collecting what the kernel is given while boot services run.

use super::Failure;
use alloc::string::String;
use alloc::vec::Vec;
use boot_protocol::Framebuffer;
use mikrosboot::display::{self, ModeInfo, Output, Pixels};
use mikrosboot::elf::{self, Kernel};
use mikrosboot::firmware::{self, RSDP_SIZE};
use mikrosboot::{cmdline, load};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::fs::FileSystem;
use uefi::proto::console::gop::{GraphicsOutput, Mode, PixelFormat};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::rng::Rng;
use uefi::table::cfg::ConfigTableEntry;
use uefi::{CString16, Handle, runtime, system, table};

const KERNEL_PATH: &str = "\\EFI\\Mikros\\kernel";
const BOOT_IMAGE_PATH: &str = "\\EFI\\Mikros\\boot.img";
const BOOT_CONF_PATH: &str = "\\EFI\\Mikros\\boot.conf";

/// The largest RSDP length that is read.
const RSDP_MAX: usize = 4096;

/// Everything collected before memory is allocated for the kernel.
pub struct Gathered {
    pub kernel_file: Vec<u8>,
    pub kernel: Kernel,
    pub layout: load::Layout,
    pub boot_image: Vec<u8>,
    pub cmdline: String,
    pub framebuffer: Option<Framebuffer>,
    pub rsdp: u64,
    pub rng_seed: Option<[u8; 32]>,
    pub boot_time: Option<u64>,
}

fn read(fs: &mut FileSystem, path: &str, step: &'static str) -> Result<Vec<u8>, Failure> {
    let name = CString16::try_from(path).map_err(|_| Failure::new(step, "bad path"))?;
    fs.read(name.as_ref())
        .map_err(|e| Failure::new(step, alloc::format!("{e}")))
}

fn exists(fs: &mut FileSystem, path: &str) -> Result<bool, Failure> {
    let step = "reading \\EFI\\Mikros\\boot.conf";
    let name = CString16::try_from(path).map_err(|_| Failure::new(step, "bad path"))?;
    fs.try_exists(name.as_ref())
        .map_err(|e| Failure::new(step, alloc::format!("{e}")))
}

fn command_line(fs: &mut FileSystem) -> Result<String, Failure> {
    let step = "finding the command line";
    let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
        .map_err(|e| Failure::new(step, alloc::format!("{e}")))?;
    let line = match image
        .load_options_as_bytes()
        .and_then(cmdline::from_load_options)
    {
        Some(line) => line,
        None if exists(fs, BOOT_CONF_PATH)? => {
            let contents = read(fs, BOOT_CONF_PATH, "reading \\EFI\\Mikros\\boot.conf")?;
            cmdline::from_boot_conf(&contents).map_err(|e| Failure::new(step, e))?
        }
        None => String::new(),
    };
    cmdline::check_len(&line).map_err(|e| Failure::new(step, e))?;
    Ok(line)
}

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
        // SAFETY: GetProtocol does not take ownership, so the console driver
        // keeps using the protocol until boot services end.
        unsafe {
            boot::open_protocol::<GraphicsOutput>(params, OpenProtocolAttributes::GetProtocol)
        }
        .ok()
    };
    display::find(console, open, || {
        boot::get_handle_for_protocol::<GraphicsOutput>().ok()
    })
}

fn pixels(info: &uefi::proto::console::gop::ModeInfo) -> Option<Pixels> {
    match info.pixel_format() {
        PixelFormat::Rgb => Some(Pixels::Rgb),
        PixelFormat::Bgr => Some(Pixels::Bgr),
        PixelFormat::Bitmask => info
            .pixel_bitmask()
            .filter(|m| display::bitmask_is_32bit(m.red, m.green, m.blue, m.reserved))
            .map(|m| Pixels::Bitmask([m.red, m.green, m.blue, m.reserved])),
        PixelFormat::BltOnly => None,
    }
}

fn mode_info(index: u32, info: &uefi::proto::console::gop::ModeInfo) -> ModeInfo {
    let (width, height) = info.resolution();
    ModeInfo {
        index,
        width,
        height,
        stride: info.stride(),
        size: None,
        linear_32bit: pixels(info).is_some(),
    }
}

/// A graphics output protocol and its modes.
struct Gop {
    gop: ScopedProtocol<GraphicsOutput>,
    modes: Vec<Mode>,
}

impl Output for Gop {
    type Error = uefi::Error;

    fn current_mode(&mut self) -> ModeInfo {
        mode_info(u32::MAX, &self.gop.current_mode_info())
    }

    fn modes(&mut self) -> Vec<ModeInfo> {
        (0..)
            .zip(&self.modes)
            .map(|(i, m)| mode_info(i, m.info()))
            .collect()
    }

    fn set_mode(&mut self, index: u32) -> uefi::Result {
        self.gop.set_mode(&self.modes[index as usize])
    }

    fn framebuffer_size(&mut self) -> u64 {
        self.gop.frame_buffer().size() as u64
    }

    fn framebuffer(&mut self) -> Option<display::Mode> {
        let info = self.gop.current_mode_info();
        let pixels = pixels(&info)?;
        let (width, height) = info.resolution();
        let mut buffer = self.gop.frame_buffer();
        let as_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Some(display::Mode {
            phys_base: buffer.as_mut_ptr() as u64,
            size: buffer.size() as u64,
            width: as_u32(width),
            height: as_u32(height),
            stride: as_u32(info.stride()),
            pixels,
        })
    }
}

/// Picks the display mode and describes its framebuffer, or gives `None`
/// when there is no usable one.
fn framebuffer(cmdline: &str) -> Result<Option<Framebuffer>, Failure> {
    let Some(gop) = open_gop() else {
        return Ok(None);
    };
    let modes = gop.modes().collect();
    display::framebuffer(&mut Gop { gop, modes }, cmdline::video_mode(cmdline))
        .map_err(|e| Failure::new("switching the display mode", alloc::format!("{e}")))
}

fn rsdp() -> Result<u64, Failure> {
    let step = "finding the ACPI RSDP";
    let address = system::with_config_table(|entries| {
        entries
            .iter()
            .find(|e| e.guid == ConfigTableEntry::ACPI2_GUID)
            .map(|e| e.address as u64)
    })
    .filter(|&a| a != 0)
    .ok_or_else(|| Failure::new(step, "the firmware has no ACPI 2.0 table"))?;
    // SAFETY: boot services identity-map memory, and the configuration table
    // entry points at the RSDP, which is at least RSDP_SIZE bytes long.
    let head = unsafe { core::slice::from_raw_parts(address as *const u8, RSDP_SIZE) };
    let length = u32::from_le_bytes([head[20], head[21], head[22], head[23]]) as usize;
    let len = if length > RSDP_SIZE && length <= RSDP_MAX {
        length
    } else {
        RSDP_SIZE
    };
    // SAFETY: as above; a longer length is only trusted up to RSDP_MAX.
    let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, len) };
    firmware::check_rsdp(bytes).map_err(|e| Failure::new(step, e))?;
    Ok(address)
}

/// The firmware's random number generator.
struct UefiRng(ScopedProtocol<Rng>);

impl firmware::Rng for UefiRng {
    fn fill(&mut self, bytes: &mut [u8]) -> bool {
        self.0.get_rng(None, bytes).is_ok()
    }
}

fn rng_seed() -> Option<[u8; 32]> {
    let rng = boot::get_handle_for_protocol::<Rng>()
        .ok()
        .and_then(|handle| boot::open_protocol_exclusive::<Rng>(handle).ok());
    firmware::rng_seed(rng.map(UefiRng).as_mut())
}

fn boot_time() -> Option<u64> {
    let t = runtime::get_time().ok()?;
    firmware::unix_seconds(firmware::Time {
        year: t.year(),
        month: t.month(),
        day: t.day(),
        hour: t.hour(),
        minute: t.minute(),
        second: t.second(),
        time_zone: t.time_zone().unwrap_or(firmware::UNSPECIFIED_TIMEZONE),
    })
}

pub fn gather() -> Result<Gathered, Failure> {
    let volume = boot::get_image_file_system(boot::image_handle())
        .map_err(|e| Failure::new("opening the boot volume", alloc::format!("{e}")))?;
    let mut fs = FileSystem::new(volume);
    let kernel_file = read(&mut fs, KERNEL_PATH, "reading \\EFI\\Mikros\\kernel")?;
    let range_start = boot_protocol::x86_64::KERNEL_RANGE_START;
    let kernel = elf::check(&kernel_file, range_start)
        .map_err(|e| Failure::new("checking the kernel", e))?;
    let layout = load::layout(&kernel);
    let boot_image = read(&mut fs, BOOT_IMAGE_PATH, "reading \\EFI\\Mikros\\boot.img")?;
    let cmdline = command_line(&mut fs)?;
    drop(fs);
    let framebuffer = framebuffer(&cmdline)?;
    Ok(Gathered {
        kernel_file,
        kernel,
        layout,
        boot_image,
        cmdline,
        framebuffer,
        rsdp: rsdp()?,
        rng_seed: rng_seed(),
        boot_time: boot_time(),
    })
}
