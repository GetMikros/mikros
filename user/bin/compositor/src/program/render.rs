//! Composing with Vulkan on the CPU driver. Each output is a linear image
//! whose memory the compositor exports as a memory object, which the
//! display imports, so that nothing copies a frame before the display
//! driver. A client's buffer is imported, through the export token or
//! memory object that the client gave, as a Vulkan buffer of its memory,
//! and a frame copies the rows of each shown buffer into the output
//! after clearing it to the background colour.

use ash::vk;
use std::ffi::c_void;
use std::os::mikros::io::OwnedHandle;

/// The values of VK_MIKROS_external_memory_handle, as Mesa's
/// vulkan_mikros.h defines them.
const IMPORT_MEMORY_HANDLE_INFO: vk::StructureType = vk::StructureType::from_raw(1_009_000_000);
const MEMORY_GET_HANDLE_INFO: vk::StructureType = vk::StructureType::from_raw(1_009_000_002);
const TOKEN: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::from_raw(0x1000_0000);
const MEMORY_OBJECT: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::from_raw(0x2000_0000);
const MEMORY_EXTENSION: &std::ffi::CStr = c"VK_MIKROS_external_memory_handle";

#[repr(C)]
struct ImportMemoryHandleInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    handle_type: vk::ExternalMemoryHandleTypeFlags,
    handle: u32,
}

#[repr(C)]
struct MemoryGetHandleInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    memory: vk::DeviceMemory,
    handle_type: vk::ExternalMemoryHandleTypeFlags,
}

type GetMemoryHandle =
    unsafe extern "system" fn(vk::Device, *const MemoryGetHandleInfo, *mut u32) -> vk::Result;

/// The format of outputs and of the buffers that clients may give: 32
/// bits a pixel, blue first in memory.
pub const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;
const BYTES_PER_PIXEL: u32 = 4;

/// An output image and its memory.
pub struct Target {
    pub image: vk::Image,
    memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The offset of its first row in its memory.
    pub offset: u64,
}

/// A client's buffer, imported.
pub struct ClientBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
    stride: u32,
    offset: u32,
}

/// A buffer to copy into an output at a position.
pub struct Layer<'a> {
    pub buffer: &'a ClientBuffer,
    pub x: i32,
    pub y: i32,
}

pub struct Renderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    commands: vk::CommandBuffer,
    fence: vk::Fence,
    memory_types: vk::PhysicalDeviceMemoryProperties,
    get_memory_handle: GetMemoryHandle,
}

/// Why rendering failed.
#[derive(Debug)]
pub enum Error {
    Vulkan(vk::Result),
    /// No physical device of the CPU driver with the Mikros memory
    /// extension, or no queue that can copy.
    NoDevice,
    /// No memory type for an image or a buffer.
    NoMemory,
}

impl From<vk::Result> for Error {
    fn from(result: vk::Result) -> Error {
        Error::Vulkan(result)
    }
}

impl Renderer {
    pub fn new() -> Result<Renderer, Error> {
        let entry = ash::Entry::linked();
        let app = vk::ApplicationInfo::default()
            .application_name(c"compositor")
            .api_version(vk::API_VERSION_1_3);
        let info = vk::InstanceCreateInfo::default().application_info(&app);
        // SAFETY: the create info and what it points to live for the call.
        let instance = unsafe { entry.create_instance(&info, None)? };
        // SAFETY: the instance is valid.
        let physicals = unsafe { instance.enumerate_physical_devices()? };
        let physical = physicals
            .into_iter()
            .find(|&p| {
                // SAFETY: the physical device belongs to the instance.
                let properties = unsafe { instance.get_physical_device_properties(p) };
                // SAFETY: as above.
                let extensions = unsafe { instance.enumerate_device_extension_properties(p) }
                    .unwrap_or_default();
                properties.device_type == vk::PhysicalDeviceType::CPU
                    && extensions
                        .iter()
                        .any(|e| e.extension_name_as_c_str() == Ok(MEMORY_EXTENSION))
            })
            .ok_or(Error::NoDevice)?;
        // SAFETY: as above.
        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let family = families
            .iter()
            .position(|f| {
                f.queue_flags.contains(vk::QueueFlags::TRANSFER)
                    || f.queue_flags.contains(vk::QueueFlags::GRAPHICS)
            })
            .ok_or(Error::NoDevice)? as u32;
        let priorities = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let extensions = [MEMORY_EXTENSION.as_ptr()];
        let info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extensions);
        // SAFETY: the physical device and the create info are valid.
        let device = unsafe { instance.create_device(physical, &info, None)? };
        // SAFETY: the device was made with this queue family.
        let queue = unsafe { device.get_device_queue(family, 0) };
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: the device is valid.
        let pool = unsafe { device.create_command_pool(&pool_info, None)? };
        let info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool belongs to the device.
        let commands = unsafe { device.allocate_command_buffers(&info)? }[0];
        // SAFETY: the device is valid.
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None)? };
        // SAFETY: as above.
        let memory_types = unsafe { instance.get_physical_device_memory_properties(physical) };
        // SAFETY: the name is a valid C string, and the device enabled the
        // extension whose command it is.
        let function = unsafe {
            instance.get_device_proc_addr(device.handle(), c"vkGetMemoryHandleMIKROS".as_ptr())
        }
        .ok_or(Error::NoDevice)?;
        // SAFETY: the loader returned the command of that name, whose
        // signature GetMemoryHandle is.
        let get_memory_handle = unsafe {
            std::mem::transmute::<unsafe extern "system" fn(), GetMemoryHandle>(function)
        };
        Ok(Renderer {
            _entry: entry,
            instance,
            device,
            queue,
            pool,
            commands,
            fence,
            memory_types,
            get_memory_handle,
        })
    }

    /// A memory type of `bits` that has `flags`.
    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32, Error> {
        let count = self.memory_types.memory_type_count as usize;
        self.memory_types.memory_types[..count]
            .iter()
            .enumerate()
            .find(|(i, t)| bits & (1 << i) != 0 && t.property_flags.contains(flags))
            .map(|(i, _)| i as u32)
            .ok_or(Error::NoMemory)
    }

    /// A new output of `width` by `height` pixels, and a handle of the
    /// memory object that holds it, for the display to import.
    pub fn target(&self, width: u32, height: u32) -> Result<(Target, OwnedHandle), Error> {
        let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(MEMORY_OBJECT);
        let info = vk::ImageCreateInfo::default()
            .push_next(&mut external)
            .image_type(vk::ImageType::TYPE_2D)
            .format(FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: the create info and what it points to live for the call.
        let image = unsafe { self.device.create_image(&info, None)? };
        // SAFETY: the image belongs to the device.
        let needs = unsafe { self.device.get_image_memory_requirements(image) };
        let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(MEMORY_OBJECT);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let info = vk::MemoryAllocateInfo::default()
            .push_next(&mut export)
            .push_next(&mut dedicated)
            .allocation_size(needs.size)
            .memory_type_index(
                self.memory_type(needs.memory_type_bits, vk::MemoryPropertyFlags::empty())?,
            );
        // SAFETY: as above.
        let memory = unsafe { self.device.allocate_memory(&info, None)? };
        // SAFETY: the memory was made for the image.
        unsafe { self.device.bind_image_memory(image, memory, 0)? };
        let get = MemoryGetHandleInfo {
            s_type: MEMORY_GET_HANDLE_INFO,
            p_next: std::ptr::null(),
            memory,
            handle_type: MEMORY_OBJECT,
        };
        let mut handle = 0;
        // SAFETY: the info and the out pointer are valid for the call.
        let result = unsafe { (self.get_memory_handle)(self.device.handle(), &get, &mut handle) };
        result.result()?;
        let subresource = vk::ImageSubresource::default().aspect_mask(vk::ImageAspectFlags::COLOR);
        // SAFETY: the image is linear and bound.
        let layout = unsafe { self.device.get_image_subresource_layout(image, subresource) };
        let target = Target {
            image,
            memory,
            width,
            height,
            stride: layout.row_pitch as u32,
            offset: layout.offset,
        };
        // SAFETY: the driver gave us the new handle.
        Ok((target, unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    /// Imports `handle`, an export token when `token` and otherwise a
    /// memory object, as a buffer of `width` by `height` pixels with
    /// `stride` bytes a row from `offset`. The import takes the handle;
    /// a failed one closes it.
    pub fn import(
        &self,
        handle: OwnedHandle,
        token: bool,
        (width, height, stride, offset): (u32, u32, u32, u32),
    ) -> Result<ClientBuffer, Error> {
        let handle_type = if token { TOKEN } else { MEMORY_OBJECT };
        let size = u64::from(offset) + u64::from(stride) * u64::from(height);
        if stride < width * BYTES_PER_PIXEL || stride % BYTES_PER_PIXEL != 0 {
            return Err(Error::NoMemory);
        }
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle_type);
        let info = vk::BufferCreateInfo::default()
            .push_next(&mut external)
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC);
        // SAFETY: the create info and what it points to live for the call.
        let buffer = unsafe { self.device.create_buffer(&info, None)? };
        // SAFETY: the buffer belongs to the device.
        let needs = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let raw = handle.into_raw_handle();
        let import = ImportMemoryHandleInfo {
            s_type: IMPORT_MEMORY_HANDLE_INFO,
            p_next: std::ptr::null(),
            handle_type,
            handle: raw,
        };
        let mut info = vk::MemoryAllocateInfo::default()
            .allocation_size(needs.size.max(size))
            .memory_type_index(
                self.memory_type(needs.memory_type_bits, vk::MemoryPropertyFlags::empty())?,
            );
        info.p_next = (&raw const import).cast();
        // SAFETY: the info and the import structure live for the call; on
        // success the driver owns the handle.
        let memory = match unsafe { self.device.allocate_memory(&info, None) } {
            Ok(memory) => memory,
            Err(e) => {
                // SAFETY: a failed import leaves the handle with us.
                drop(unsafe { OwnedHandle::from_raw_handle(raw) });
                // SAFETY: the buffer is unused.
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err(e.into());
            }
        };
        // SAFETY: the memory holds the buffer's size.
        unsafe { self.device.bind_buffer_memory(buffer, memory, 0)? };
        Ok(ClientBuffer {
            buffer,
            memory,
            width,
            height,
            stride,
            offset,
        })
    }

    pub fn forget(&self, buffer: ClientBuffer) {
        // SAFETY: no frame being drawn uses the buffer: frames are drawn
        // to completion.
        unsafe {
            self.device.destroy_buffer(buffer.buffer, None);
            self.device.free_memory(buffer.memory, None);
        }
    }

    /// Draws a frame into `target`: the background `colour`, and then the
    /// rows of each layer, clipped to the output. Returns once the frame
    /// is drawn.
    pub fn draw(&self, target: &Target, colour: [f32; 4], layers: &[Layer]) -> Result<(), Error> {
        let d = &self.device;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let barrier = |old, new| {
            vk::ImageMemoryBarrier::default()
                .image(target.image)
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::MEMORY_READ)
                .subresource_range(range)
        };
        let regions: Vec<vk::BufferImageCopy> = layers
            .iter()
            .filter_map(|layer| copy_region(target, layer))
            .collect();
        // SAFETY: the command buffer, the target and the layers' buffers
        // belong to the device, and the previous frame is complete.
        unsafe {
            d.reset_command_buffer(self.commands, vk::CommandBufferResetFlags::empty())?;
            d.begin_command_buffer(self.commands, &vk::CommandBufferBeginInfo::default())?;
            let to_transfer = barrier(
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            d.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            let clear = vk::ClearColorValue { float32: colour };
            d.cmd_clear_color_image(
                self.commands,
                target.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &clear,
                &[range],
            );
            for (layer, region) in layers.iter().zip(&regions) {
                d.cmd_copy_buffer_to_image(
                    self.commands,
                    layer.buffer.buffer,
                    target.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    std::slice::from_ref(region),
                );
            }
            let to_general = barrier(
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::GENERAL,
            );
            d.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_general],
            );
            d.end_command_buffer(self.commands)?;
            let commands = [self.commands];
            let submit = vk::SubmitInfo::default().command_buffers(&commands);
            d.queue_submit(self.queue, &[submit], self.fence)?;
            d.wait_for_fences(&[self.fence], true, u64::MAX)?;
            d.reset_fences(&[self.fence])?;
        }
        Ok(())
    }
}

/// The copy of `layer` into `target`, clipped to it, or None when none of
/// it is on the output.
fn copy_region(target: &Target, layer: &Layer) -> Option<vk::BufferImageCopy> {
    let buffer = layer.buffer;
    let (x0, y0) = (layer.x.max(0), layer.y.max(0));
    let x1 = (layer.x + buffer.width as i32).min(target.width as i32);
    let y1 = (layer.y + buffer.height as i32).min(target.height as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // The first pixel of the buffer that the output shows.
    let (skip_x, skip_y) = ((x0 - layer.x) as u64, (y0 - layer.y) as u64);
    let offset = u64::from(buffer.offset)
        + skip_y * u64::from(buffer.stride)
        + skip_x * u64::from(BYTES_PER_PIXEL);
    Some(
        vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .buffer_row_length(buffer.stride / BYTES_PER_PIXEL)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_offset(vk::Offset3D { x: x0, y: y0, z: 0 })
            .image_extent(vk::Extent3D {
                width: (x1 - x0) as u32,
                height: (y1 - y0) as u32,
                depth: 1,
            }),
    )
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: nothing runs on the device any more.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

impl Target {
    pub fn destroy(self, renderer: &Renderer) {
        // SAFETY: no frame being drawn uses the target.
        unsafe {
            renderer.device.destroy_image(self.image, None);
            renderer.device.free_memory(self.memory, None);
        }
    }
}
