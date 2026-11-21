//! Buffers of the compositor's own in host memory, which stay mapped and
//! which the shaders read: the pixels of the wallpaper and of its haze,
//! and the values of each frame. The wallpaper's pixels are read from the
//! client's buffer, whose memory the renderer imports as host-visible.

use super::render::{ClientBuffer, Error, Pixels, Renderer};
use ash::vk;

/// A buffer of the compositor's own in host memory, mapped: the pixels of
/// a wallpaper or a haze, or the values of a frame.
pub struct HostBuffer {
    pub(super) buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pub(super) size: u64,
    mapped: *mut u8,
    pub width: u32,
    pub height: u32,
}

impl HostBuffer {
    /// Its pixels, opaque rows of its width.
    pub fn pixels(&self) -> Pixels {
        Pixels {
            buffer: self.buffer,
            width: self.width,
            height: self.height,
            row: self.width,
            first: 0,
            alpha: false,
        }
    }
}

impl HostBuffer {
    /// Copies `values` into the buffer from byte `at`.
    pub(super) fn write<T: Copy>(&self, at: usize, values: &[T]) {
        let bytes = std::mem::size_of_val(values);
        assert!(
            at as u64 + bytes as u64 <= self.size,
            "a write beyond the buffer"
        );
        // SAFETY: the mapping is at least `size` bytes long, and no frame
        // being drawn reads the buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr().cast::<u8>(), self.mapped.add(at), bytes);
        }
    }
}

impl Renderer {
    /// The pixels of `source`, rows of its width without padding.
    pub fn read(&self, source: &ClientBuffer) -> Result<Vec<u32>, Error> {
        let (width, height) = (source.width as usize, source.height as usize);
        let mut pixels = Vec::with_capacity(width * height);
        // SAFETY: the memory is host-visible, as `import` chose it, and
        // holds the buffer's rows; nothing else maps it.
        unsafe {
            let base = self.device.map_memory(
                source.memory,
                0,
                vk::WHOLE_SIZE,
                vk::MemoryMapFlags::empty(),
            )?;
            for y in 0..height {
                let row = base
                    .cast::<u8>()
                    .add(source.offset as usize + y * source.stride as usize)
                    .cast::<u32>();
                pixels.extend_from_slice(std::slice::from_raw_parts(row, width));
            }
            self.device.unmap_memory(source.memory);
        }
        Ok(pixels)
    }

    /// A buffer of host memory of at least `size` bytes, mapped.
    pub(super) fn host_buffer(&self, size: u64) -> Result<HostBuffer, Error> {
        let d = &self.device;
        let size = size.max(16);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER);
        // SAFETY: the create info lives for the call.
        let buffer = unsafe { d.create_buffer(&info, None)? };
        // SAFETY: the buffer belongs to the device.
        let needs = unsafe { d.get_buffer_memory_requirements(buffer) };
        let flags = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let memory = self
            .memory_type(needs.memory_type_bits, flags)
            .and_then(|index| {
                let info = vk::MemoryAllocateInfo::default()
                    .allocation_size(needs.size)
                    .memory_type_index(index);
                // SAFETY: the info lives for the call.
                Ok(unsafe { d.allocate_memory(&info, None)? })
            });
        let memory = match memory {
            Ok(memory) => memory,
            Err(e) => {
                // SAFETY: the buffer is unused.
                unsafe { d.destroy_buffer(buffer, None) };
                return Err(e);
            }
        };
        // SAFETY: the memory was made for the buffer, is host-visible, and
        // stays mapped until `forget_host`.
        let mapped = unsafe {
            d.bind_buffer_memory(buffer, memory, 0)
                .and_then(|()| d.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()))
        };
        match mapped {
            Ok(mapped) => Ok(HostBuffer {
                buffer,
                memory,
                size,
                mapped: mapped.cast(),
                width: 0,
                height: 0,
            }),
            Err(e) => {
                // SAFETY: the buffer and the memory are unused.
                unsafe {
                    d.destroy_buffer(buffer, None);
                    d.free_memory(memory, None);
                }
                Err(e.into())
            }
        }
    }

    /// A buffer of host memory that holds `pixels`, rows of `width` by
    /// `height` pixels.
    pub fn host_pixels(
        &self,
        pixels: &[u32],
        width: u32,
        height: u32,
    ) -> Result<HostBuffer, Error> {
        let mut buffer = self.host_buffer(std::mem::size_of_val(pixels) as u64)?;
        buffer.write(0, pixels);
        buffer.width = width;
        buffer.height = height;
        Ok(buffer)
    }

    pub fn forget_host(&self, buffer: HostBuffer) {
        // SAFETY: no frame being drawn uses the buffer, and its memory is
        // mapped.
        unsafe {
            self.device.unmap_memory(buffer.memory);
            self.device.destroy_buffer(buffer.buffer, None);
            self.device.free_memory(buffer.memory, None);
        }
    }
}
