//! The boot framebuffer: its description from the device's properties,
//! and writing rows of XRGB8888 pixels into it in its own format.

use mikros_display::pixels::Converter;
use mikros_rt::hardware::Mapping;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::driver::Property;
use mikros_rt::sys;

pub const PERM_READ: u32 = 1 << 0;
pub const PERM_WRITE: u32 = 1 << 1;
const PAGE_SIZE: u64 = 4096;

/// The framebuffer as its device's properties describe it.
#[derive(Clone, Copy, Debug)]
pub struct Description {
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per line in memory.
    pub stride: u32,
    pub format: u32,
    pub masks: [u32; 4],
}

impl Description {
    /// The description in `properties`: the sizes and the format in
    /// decimal, the masks as `0x` and lowercase hexadecimal digits.
    pub fn from(properties: &[Property]) -> Option<Description> {
        let get = |key: &str| {
            properties
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.value.as_str())
        };
        let decimal = |key: &str| get(key)?.parse::<u64>().ok();
        let mask = |key: &str| {
            let digits = get(key)?.strip_prefix("0x")?;
            let lower = digits
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            lower.then(|| u32::from_str_radix(digits, 16).ok())?
        };
        let d = Description {
            size: decimal("size")?,
            width: u32::try_from(decimal("width")?).ok()?,
            height: u32::try_from(decimal("height")?).ok()?,
            stride: u32::try_from(decimal("stride")?).ok()?,
            format: u32::try_from(decimal("format")?).ok()?,
            masks: [
                mask("red_mask")?,
                mask("green_mask")?,
                mask("blue_mask")?,
                mask("reserved_mask")?,
            ],
        };
        // Every pixel is 4 bytes, and the visible lines fit in the size.
        let needed = u64::from(d.stride) * u64::from(d.height) * 4;
        (d.width > 0 && d.height > 0 && d.stride >= d.width && needed <= d.size).then_some(d)
    }
}

/// The mapped framebuffer, which the driver only writes.
pub struct Framebuffer {
    pub description: Description,
    converter: Converter,
    mapping: Mapping,
    _memory: OwnedHandle,
}

impl Framebuffer {
    /// Maps `memory`, the framebuffer's memory object, into `process`.
    pub fn map(
        memory: OwnedHandle,
        description: Description,
        process: sys::Handle,
    ) -> Option<Framebuffer> {
        let [red, green, blue, _] = description.masks;
        let converter = Converter::new(description.format, red, green, blue)?;
        let len = description.size.next_multiple_of(PAGE_SIZE);
        let mapping = Mapping::new(process, PERM_READ | PERM_WRITE, memory.raw(), 0, len).ok()?;
        Some(Framebuffer {
            description,
            converter,
            mapping,
            _memory: memory,
        })
    }

    /// Unmaps the framebuffer, and then closes its memory object.
    pub fn unmap(self) {
        drop(self);
    }

    /// Writes `pixels`, XRGB8888 values, into line `y` from pixel `x`,
    /// leaving out what lies beyond the visible width.
    pub fn write_row(&self, x: u32, y: u32, pixels: impl Iterator<Item = u32>) {
        let d = &self.description;
        if y >= d.height {
            return;
        }
        // The line and pixel lie within the mapping, which
        // `Description::from` checked against the size.
        let line = u64::from(y) * u64::from(d.stride) * 4;
        for (at, pixel) in (x..d.width).zip(pixels) {
            let value = self.converter.convert(pixel);
            self.mapping.write32(line + u64::from(at) * 4, value);
        }
    }
}
