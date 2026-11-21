//! Composing with Vulkan on the CPU driver. Each output is a linear image
//! whose memory the compositor exports as a memory object, which the
//! display imports, so that nothing copies a frame before the display
//! driver. A client's buffer is imported, through the export token or
//! memory object that the client gave, as a Vulkan buffer of its memory,
//! and every layer's pixels are an image that the shader samples: a frame
//! first copies the part of each layer's buffer that it draws into the
//! layer's image. A frame clears the output, or only the rectangles of its
//! damage on an image that keeps the rest, to the background colour and
//! draws a quad for each layer that touches them, whose fragment shader
//! (shaders/compose.wgsl) reads the layer's pixels from its image and puts
//! them, by their alpha, over the haze or a colour inside the layer's
//! material areas and over the frame elsewhere. The values of the layers
//! and the rectangles of their areas go to the shaders in buffers of host
//! memory, and the image that each layer reads is pushed as its
//! descriptor.

use super::host::HostBuffer;
use crate::server::Rect;
use ash::vk;
use std::cell::RefCell;
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

/// An output image, its view and its memory.
pub struct Target {
    pub image: vk::Image,
    view: vk::ImageView,
    memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The offset of its first row in its memory.
    pub offset: u64,
}

/// A client's buffer, imported, and the image that shows it.
pub struct ClientBuffer {
    buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
    picture: Picture,
    /// Whether the image is bound to the buffer's memory, in which case
    /// there is no buffer of its own and nothing to copy.
    direct: bool,
    pub width: u32,
    pub height: u32,
    pub(super) stride: u32,
    pub(super) offset: u32,
    /// Whether its fourth byte is alpha.
    pub(super) alpha: bool,
}

/// The layout of a client's buffer in memory.
struct Shape {
    width: u32,
    height: u32,
    stride: u32,
    offset: u32,
    alpha: bool,
}

/// An image that a layer samples, with the memory it owns.
pub(super) struct Picture {
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    pub(super) memory: vk::DeviceMemory,
}

/// Where the pixels of a layer's image come from: a buffer, the pixels of
/// its rows and the byte of its first row.
#[derive(Clone, Copy)]
pub(super) struct Source {
    pub(super) buffer: vk::Buffer,
    pub(super) row: u32,
    pub(super) first: u32,
}

/// Pixels that a layer shows: an image, its size, the buffer that each
/// frame copies the drawn part of it from, and whether they have alpha.
#[derive(Clone, Copy)]
pub struct Pixels {
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    /// The layout that shaders sample the image in.
    pub(super) layout: vk::ImageLayout,
    /// What each frame copies from, unless the image is the buffer's
    /// memory itself.
    pub(super) source: Option<Source>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) alpha: bool,
}

impl ClientBuffer {
    pub fn pixels(&self) -> Pixels {
        Pixels {
            image: self.picture.image,
            view: self.picture.view,
            layout: if self.direct {
                vk::ImageLayout::GENERAL
            } else {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            },
            source: (!self.direct).then_some(Source {
                buffer: self.buffer,
                row: self.stride / BYTES_PER_PIXEL,
                first: self.offset,
            }),
            width: self.width,
            height: self.height,
            alpha: self.alpha,
        }
    }
}

/// What a layer's haze area shows beneath it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Under {
    Haze,
    Flat,
}

/// Pixels to draw at a position, with the areas of their materials in
/// the layer's coordinates.
pub struct Layer<'a> {
    pub pixels: Pixels,
    pub x: i32,
    pub y: i32,
    pub under: Under,
    pub haze: &'a [Rect],
    pub blur: &'a [Rect],
}

/// The output's haze while a frame is drawn: the new one, the old one,
/// and how much of the new one shows.
pub struct Haze<'a> {
    pub new: &'a HostBuffer,
    pub old: &'a HostBuffer,
    pub fade: f32,
}

/// The theme's colours of a frame: the flat one under haze, XRGB8888, and
/// the translucent one under blur, premultiplied ARGB8888.
pub struct Look {
    pub flat: u32,
    pub translucent: u32,
}

/// The values of a layer for the shaders, laid out as `Layer` in
/// shaders/compose.wgsl.
#[repr(C)]
#[derive(Clone, Copy)]
struct LayerValues {
    rect: [i32; 4],
    output: [u32; 2],
    alpha: u32,
    haze: u32,
    flat: u32,
    translucent: u32,
    fade: f32,
    haze_first: u32,
    haze_count: u32,
    blur_first: u32,
    blur_count: u32,
    _padding: u32,
}

pub struct Renderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    pub(super) device: ash::Device,
    push: ash::khr::push_descriptor::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    commands: vk::CommandBuffer,
    fence: vk::Fence,
    memory_types: vk::PhysicalDeviceMemoryProperties,
    get_memory_handle: GetMemoryHandle,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    /// The values of the last frame's layers and its areas' rectangles,
    /// kept to be filled again by the next one.
    frame: RefCell<Option<(HostBuffer, HostBuffer)>>,
}

/// Why rendering failed.
#[derive(Debug)]
pub enum Error {
    Vulkan(vk::Result),
    /// No physical device of the CPU driver with the Mikros memory
    /// extension and push descriptors, no queue that can draw, or no
    /// linear images to draw into.
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
        let wanted = [MEMORY_EXTENSION, vk::KHR_PUSH_DESCRIPTOR_NAME];
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
                // SAFETY: as above.
                let format = unsafe { instance.get_physical_device_format_properties(p, FORMAT) };
                let target = vk::FormatFeatureFlags::COLOR_ATTACHMENT
                    | vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND;
                properties.device_type == vk::PhysicalDeviceType::CPU
                    && format.linear_tiling_features.contains(target)
                    && wanted.iter().all(|&name| {
                        extensions
                            .iter()
                            .any(|e| e.extension_name_as_c_str() == Ok(name))
                    })
            })
            .ok_or(Error::NoDevice)?;
        // SAFETY: as above.
        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let family = families
            .iter()
            .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .ok_or(Error::NoDevice)? as u32;
        let priorities = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let extensions = wanted.map(|name| name.as_ptr());
        let mut features = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true);
        let info = vk::DeviceCreateInfo::default()
            .push_next(&mut features)
            .queue_create_infos(&queues)
            .enabled_extension_names(&extensions);
        // SAFETY: the physical device and the create info are valid.
        let device = unsafe { instance.create_device(physical, &info, None)? };
        let push = ash::khr::push_descriptor::Device::new(&instance, &device);
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
        let (set_layout, layout, pipeline) = super::pipeline::pipeline(&device)?;
        Ok(Renderer {
            _entry: entry,
            instance,
            device,
            push,
            queue,
            pool,
            commands,
            fence,
            memory_types,
            get_memory_handle,
            set_layout,
            layout,
            pipeline,
            frame: RefCell::new(None),
        })
    }

    /// A memory type of `bits` that has `flags`.
    pub(super) fn memory_type(
        &self,
        bits: u32,
        flags: vk::MemoryPropertyFlags,
    ) -> Result<u32, Error> {
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
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
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
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(FORMAT)
            .subresource_range(range);
        // SAFETY: the image is bound to memory.
        let view = unsafe { self.device.create_image_view(&info, None)? };
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
            view,
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
    /// `stride` bytes a row from `offset`, whose fourth byte is alpha
    /// when `alpha`. The memory is bound to a linear image that the shader
    /// samples in place when the driver lays out its rows as the buffer
    /// does, and otherwise to a buffer that each frame copies from. The
    /// import takes the handle; a failed one closes it.
    pub fn import(
        &self,
        handle: OwnedHandle,
        token: bool,
        (width, height, stride, offset): (u32, u32, u32, u32),
        alpha: bool,
    ) -> Result<ClientBuffer, Error> {
        let handle_type = if token { TOKEN } else { MEMORY_OBJECT };
        if stride < width * BYTES_PER_PIXEL
            || stride % BYTES_PER_PIXEL != 0
            || offset % BYTES_PER_PIXEL != 0
        {
            return Err(Error::NoMemory);
        }
        let shape = Shape {
            width,
            height,
            stride,
            offset,
            alpha,
        };
        let raw = handle.into_raw_handle();
        let linear = self.linear(handle_type, &shape);
        mikros_rt::log!(
            Info,
            "compositor: a buffer of {width} by {height} pixels with {stride} bytes a row at {offset} is {}",
            if linear.is_some() {
                "sampled in place"
            } else {
                "copied for each frame"
            }
        );
        match linear {
            Some((image, needs)) => self.import_image(raw, handle_type, image, needs, shape),
            None => self.import_buffer(raw, handle_type, shape),
        }
    }

    /// A linear image of a buffer of `shape`, and what it needs, when its
    /// rows lie where the buffer's do in memory that has room for it.
    fn linear(
        &self,
        handle_type: vk::ExternalMemoryHandleTypeFlags,
        shape: &Shape,
    ) -> Option<(vk::Image, vk::MemoryRequirements)> {
        let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(handle_type);
        let info = vk::ImageCreateInfo::default()
            .push_next(&mut external)
            .image_type(vk::ImageType::TYPE_2D)
            .format(FORMAT)
            .extent(vk::Extent3D {
                width: shape.width,
                height: shape.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::PREINITIALIZED);
        // SAFETY: the create info and what it points to live for the call.
        let image = unsafe { self.device.create_image(&info, None) }.ok()?;
        let subresource = vk::ImageSubresource::default().aspect_mask(vk::ImageAspectFlags::COLOR);
        // SAFETY: the image is linear and belongs to the device.
        let layout = unsafe { self.device.get_image_subresource_layout(image, subresource) };
        // SAFETY: as above.
        let needs = unsafe { self.device.get_image_memory_requirements(image) };
        let fits = layout.offset == 0
            && layout.row_pitch == u64::from(shape.stride)
            && needs.alignment != 0
            && u64::from(shape.offset) % needs.alignment == 0
            && needs.size <= u64::from(shape.stride) * u64::from(shape.height);
        if fits {
            Some((image, needs))
        } else {
            // SAFETY: the image is unused.
            unsafe { self.device.destroy_image(image, None) };
            None
        }
    }

    /// Imports `raw` as memory of `handle_type` and `size` bytes, of a type
    /// in `type_bits`; the memory takes the handle, and a failure closes it.
    fn allocate(
        &self,
        raw: u32,
        handle_type: vk::ExternalMemoryHandleTypeFlags,
        size: u64,
        type_bits: u32,
    ) -> Result<vk::DeviceMemory, Error> {
        let import = ImportMemoryHandleInfo {
            s_type: IMPORT_MEMORY_HANDLE_INFO,
            p_next: std::ptr::null(),
            handle_type,
            handle: raw,
        };
        let memory_type = self
            .memory_type(type_bits, vk::MemoryPropertyFlags::HOST_VISIBLE)
            .or_else(|_| self.memory_type(type_bits, vk::MemoryPropertyFlags::empty()));
        let mut info = vk::MemoryAllocateInfo::default().allocation_size(size);
        let result = memory_type.and_then(|index| {
            info = info.memory_type_index(index);
            info.p_next = (&raw const import).cast();
            // SAFETY: the info and the import structure live for the call;
            // on success the driver owns the handle.
            Ok(unsafe { self.device.allocate_memory(&info, None)? })
        });
        if result.is_err() {
            // SAFETY: a failed import leaves the handle with us.
            drop(unsafe { OwnedHandle::from_raw_handle(raw) });
        }
        result
    }

    /// The image that `linear` made, bound to the imported memory at the
    /// buffer's offset, for the shader to sample in place.
    fn import_image(
        &self,
        raw: u32,
        handle_type: vk::ExternalMemoryHandleTypeFlags,
        image: vk::Image,
        needs: vk::MemoryRequirements,
        shape: Shape,
    ) -> Result<ClientBuffer, Error> {
        let size = u64::from(shape.offset) + needs.size;
        let memory = match self.allocate(raw, handle_type, size, needs.memory_type_bits) {
            Ok(memory) => memory,
            Err(e) => {
                // SAFETY: the image is unused.
                unsafe { self.device.destroy_image(image, None) };
                return Err(e);
            }
        };
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(FORMAT)
            .subresource_range(range);
        // SAFETY: the memory has room for the image from the offset, which
        // `linear` checked to suit its alignment.
        let view = unsafe {
            self.device
                .bind_image_memory(image, memory, u64::from(shape.offset))
                .and_then(|()| self.device.create_image_view(&info, None))
        };
        let made = view
            .map_err(Error::from)
            .and_then(|view| self.make_general(image).map(|()| view));
        let view = match made {
            Ok(view) => view,
            Err(e) => {
                // SAFETY: nothing uses the image or its memory.
                unsafe {
                    self.device.destroy_image(image, None);
                    self.device.free_memory(memory, None);
                }
                return Err(e);
            }
        };
        Ok(ClientBuffer {
            buffer: vk::Buffer::null(),
            memory,
            picture: Picture {
                image,
                view,
                memory: vk::DeviceMemory::null(),
            },
            direct: true,
            width: shape.width,
            height: shape.height,
            stride: shape.stride,
            offset: shape.offset,
            alpha: shape.alpha,
        })
    }

    /// Moves `image`, whose contents a client wrote, to the layout that
    /// shaders sample it in, and waits for that.
    fn make_general(&self, image: vk::Image) -> Result<(), Error> {
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let barrier = vk::ImageMemoryBarrier::default()
            .image(image)
            .old_layout(vk::ImageLayout::PREINITIALIZED)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::HOST_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .subresource_range(range);
        let d = &self.device;
        // SAFETY: the command buffer is not recording, no frame is being
        // drawn, and the image belongs to the device.
        unsafe {
            d.reset_command_buffer(self.commands, vk::CommandBufferResetFlags::empty())?;
            d.begin_command_buffer(self.commands, &vk::CommandBufferBeginInfo::default())?;
            d.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::HOST,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
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

    /// The buffer of `shape` as a Vulkan buffer of the imported memory,
    /// with an image of its own for the shader.
    fn import_buffer(
        &self,
        raw: u32,
        handle_type: vk::ExternalMemoryHandleTypeFlags,
        shape: Shape,
    ) -> Result<ClientBuffer, Error> {
        let size = u64::from(shape.offset) + u64::from(shape.stride) * u64::from(shape.height);
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle_type);
        let info = vk::BufferCreateInfo::default()
            .push_next(&mut external)
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC);
        // SAFETY: the create info and what it points to live for the call.
        let buffer = match unsafe { self.device.create_buffer(&info, None) } {
            Ok(buffer) => buffer,
            Err(e) => {
                // SAFETY: the import has not taken the handle.
                drop(unsafe { OwnedHandle::from_raw_handle(raw) });
                return Err(e.into());
            }
        };
        // SAFETY: the buffer belongs to the device.
        let needs = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory = match self.allocate(
            raw,
            handle_type,
            needs.size.max(size),
            needs.memory_type_bits,
        ) {
            Ok(memory) => memory,
            Err(e) => {
                // SAFETY: the buffer is unused.
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err(e);
            }
        };
        // SAFETY: the memory holds the buffer's size.
        let bound = unsafe { self.device.bind_buffer_memory(buffer, memory, 0) };
        let picture = bound
            .map_err(Error::from)
            .and_then(|()| self.picture(shape.width, shape.height));
        let picture = match picture {
            Ok(picture) => picture,
            Err(e) => {
                // SAFETY: nothing uses the buffer or its memory.
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                    self.device.free_memory(memory, None);
                }
                return Err(e);
            }
        };
        Ok(ClientBuffer {
            buffer,
            memory,
            picture,
            direct: false,
            width: shape.width,
            height: shape.height,
            stride: shape.stride,
            offset: shape.offset,
            alpha: shape.alpha,
        })
    }

    pub fn forget(&self, buffer: ClientBuffer) {
        self.forget_picture(buffer.picture);
        // SAFETY: no frame being drawn uses the buffer: frames are drawn
        // to completion.
        unsafe {
            self.device.destroy_buffer(buffer.buffer, None);
            self.device.free_memory(buffer.memory, None);
        }
    }

    /// An image of `width` by `height` pixels in memory of its own, which
    /// shaders sample and copies fill.
    pub(super) fn picture(&self, width: u32, height: u32) -> Result<Picture, Error> {
        let info = vk::ImageCreateInfo::default()
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
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: the create info lives for the call.
        let image = unsafe { self.device.create_image(&info, None)? };
        // SAFETY: the image belongs to the device.
        let needs = unsafe { self.device.get_image_memory_requirements(image) };
        let made = self
            .memory_type(needs.memory_type_bits, vk::MemoryPropertyFlags::empty())
            .and_then(|index| {
                let info = vk::MemoryAllocateInfo::default()
                    .allocation_size(needs.size)
                    .memory_type_index(index);
                // SAFETY: the info lives for the call.
                Ok(unsafe { self.device.allocate_memory(&info, None)? })
            });
        let memory = match made {
            Ok(memory) => memory,
            Err(e) => {
                // SAFETY: the image is unused.
                unsafe { self.device.destroy_image(image, None) };
                return Err(e);
            }
        };
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(FORMAT)
            .subresource_range(range);
        // SAFETY: the memory was made for the image.
        let view = unsafe {
            self.device
                .bind_image_memory(image, memory, 0)
                .and_then(|()| self.device.create_image_view(&info, None))
        };
        match view {
            Ok(view) => Ok(Picture {
                image,
                view,
                memory,
            }),
            Err(e) => {
                // SAFETY: the image and its memory are unused.
                unsafe {
                    self.device.destroy_image(image, None);
                    self.device.free_memory(memory, None);
                }
                Err(e.into())
            }
        }
    }

    pub(super) fn forget_picture(&self, picture: Picture) {
        // SAFETY: no frame being drawn uses the image: frames are drawn to
        // completion.
        unsafe {
            self.device.destroy_image_view(picture.view, None);
            self.device.destroy_image(picture.image, None);
            self.device.free_memory(picture.memory, None);
        }
    }

    /// Records the copies that fill the images of `layers` with what a
    /// frame of `areas` draws of them, and the barriers around them: each
    /// image is written only where it is sampled, so that what it held
    /// before is of no use.
    fn copy_pixels(&self, layers: &[Layer], areas: &[vk::Rect2D]) {
        let mut copies: Vec<(Pixels, Vec<vk::BufferImageCopy>)> = Vec::new();
        for layer in layers {
            let p = layer.pixels;
            let Some(source) = p.source else {
                continue;
            };
            let (x0, y0) = (layer.x, layer.y);
            let regions: Vec<vk::BufferImageCopy> = areas
                .iter()
                .filter_map(|area| {
                    let left = area.offset.x.max(x0);
                    let top = area.offset.y.max(y0);
                    let right = (area.offset.x + area.extent.width as i32).min(x0 + p.width as i32);
                    let bottom =
                        (area.offset.y + area.extent.height as i32).min(y0 + p.height as i32);
                    (right > left && bottom > top).then(|| {
                        let (x, y) = ((left - x0) as u32, (top - y0) as u32);
                        let at = u64::from(source.first)
                            + (u64::from(y) * u64::from(source.row) + u64::from(x))
                                * u64::from(BYTES_PER_PIXEL);
                        vk::BufferImageCopy::default()
                            .buffer_offset(at)
                            .buffer_row_length(source.row)
                            .image_subresource(
                                vk::ImageSubresourceLayers::default()
                                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                                    .layer_count(1),
                            )
                            .image_offset(vk::Offset3D {
                                x: x as i32,
                                y: y as i32,
                                z: 0,
                            })
                            .image_extent(vk::Extent3D {
                                width: (right - left) as u32,
                                height: (bottom - top) as u32,
                                depth: 1,
                            })
                    })
                })
                .collect();
            if regions.is_empty() {
                continue;
            }
            match copies.iter_mut().find(|(q, _)| q.image == p.image) {
                Some((_, all)) => all.extend(regions),
                None => copies.push((p, regions)),
            }
        }
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let barrier = |image, old, new, src, dst| {
            vk::ImageMemoryBarrier::default()
                .image(image)
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(src)
                .dst_access_mask(dst)
                .subresource_range(range)
        };
        let to_copy: Vec<_> = copies
            .iter()
            .map(|(p, _)| {
                barrier(
                    p.image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::SHADER_READ,
                    vk::AccessFlags::TRANSFER_WRITE,
                )
            })
            .collect();
        let to_sample: Vec<_> = copies
            .iter()
            .map(|(p, _)| {
                barrier(
                    p.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::SHADER_READ,
                )
            })
            .collect();
        // SAFETY: the command buffer is recording, and the images and
        // buffers belong to the device and outlive the frame; the host's
        // writes to the buffers are visible to the device.
        unsafe {
            self.device.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &to_copy,
            );
            for (p, regions) in &copies {
                let Some(source) = p.source else {
                    continue;
                };
                self.device.cmd_copy_buffer_to_image(
                    self.commands,
                    source.buffer,
                    p.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    regions,
                );
            }
            self.device.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &to_sample,
            );
        }
    }

    /// The buffers of the frame's layer values and rectangles, at least
    /// `layers` and `rects` bytes long.
    fn frame_buffers(&self, layers: u64, rects: u64) -> Result<(), Error> {
        let mut frame = self.frame.borrow_mut();
        if let Some((l, r)) = frame.as_ref()
            && l.size >= layers
            && r.size >= rects
        {
            return Ok(());
        }
        if let Some((l, r)) = frame.take() {
            self.forget_host(l);
            self.forget_host(r);
        }
        let l = self.host_buffer(layers.next_power_of_two())?;
        match self.host_buffer(rects.next_power_of_two()) {
            Ok(r) => *frame = Some((l, r)),
            Err(e) => {
                self.forget_host(l);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Draws a frame into `target`: the background `colour`, and then each
    /// layer, clipped to the output, with `haze` beneath the haze areas
    /// of layers that show it. Returns once the frame is drawn.
    pub fn draw(
        &self,
        target: &Target,
        colour: [f32; 4],
        layers: &[Layer],
        haze: Option<&Haze>,
        look: &Look,
        damage: Option<&[Rect]>,
    ) -> Result<(), Error> {
        let mut values = Vec::with_capacity(layers.len());
        let mut rects: Vec<[i32; 4]> = Vec::new();
        for layer in layers {
            let mut area = |area: &[Rect]| {
                let first = rects.len() as u32;
                rects.extend(area.iter().map(|r| [r.x, r.y, r.width, r.height]));
                (first, area.len() as u32)
            };
            let (haze_first, haze_count) = area(layer.haze);
            let (blur_first, blur_count) = area(layer.blur);
            let p = layer.pixels;
            values.push(LayerValues {
                rect: [layer.x, layer.y, p.width as i32, p.height as i32],
                output: [target.width, target.height],
                alpha: u32::from(p.alpha),
                haze: match (layer.under, haze) {
                    _ if layer.haze.is_empty() => 0,
                    (Under::Haze, Some(_)) => 1,
                    _ => 2,
                },
                flat: look.flat,
                translucent: look.translucent,
                fade: haze.map_or(1.0, |h| h.fade),
                haze_first,
                haze_count,
                blur_first,
                blur_count,
                _padding: 0,
            });
        }
        self.frame_buffers(
            std::mem::size_of_val(values.as_slice()) as u64,
            std::mem::size_of_val(rects.as_slice()) as u64,
        )?;
        let frame = self.frame.borrow();
        let (layer_buffer, rect_buffer) = frame.as_ref().expect("frame buffers");
        layer_buffer.write(0, &values);
        rect_buffer.write(0, &rects);
        let d = &self.device;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let barrier = |old, new, src, dst| {
            vk::ImageMemoryBarrier::default()
                .image(target.image)
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(src)
                .dst_access_mask(dst)
                .subresource_range(range)
        };
        let whole = |buffer| {
            [vk::DescriptorBufferInfo::default()
                .buffer(buffer)
                .range(vk::WHOLE_SIZE)]
        };
        let (haze_new, haze_old) = match haze {
            Some(h) => (h.new.buffer, h.old.buffer),
            None => (rect_buffer.buffer, rect_buffer.buffer),
        };
        let shared = [
            whole(haze_new),
            whole(haze_old),
            whole(rect_buffer.buffer),
            whole(layer_buffer.buffer),
        ];
        let extent = vk::Extent2D {
            width: target.width,
            height: target.height,
        };
        let areas: Vec<vk::Rect2D> = match damage {
            None => vec![vk::Rect2D::from(extent)],
            Some(rects) => clip(rects, target),
        };
        // SAFETY: the command buffer, the target, the pipeline and every
        // buffer that the descriptors name belong to the device, the host
        // wrote the frame's values before the submission, and the previous
        // frame is complete.
        unsafe {
            d.reset_command_buffer(self.commands, vk::CommandBufferResetFlags::empty())?;
            d.begin_command_buffer(self.commands, &vk::CommandBufferBeginInfo::default())?;
            // A frame of the whole output need not keep what the image
            // holds; one of its damage draws over it.
            let old = match damage {
                None => vk::ImageLayout::UNDEFINED,
                Some(_) => vk::ImageLayout::GENERAL,
            };
            self.copy_pixels(layers, &areas);
            let to_draw = barrier(
                old,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );
            d.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_draw],
            );
            let attachments = [vk::RenderingAttachmentInfo::default()
                .image_view(target.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(match damage {
                    None => vk::AttachmentLoadOp::CLEAR,
                    Some(_) => vk::AttachmentLoadOp::LOAD,
                })
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue { float32: colour },
                })];
            let rendering = vk::RenderingInfo::default()
                .render_area(extent.into())
                .layer_count(1)
                .color_attachments(&attachments);
            d.cmd_begin_rendering(self.commands, &rendering);
            d.cmd_bind_pipeline(
                self.commands,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            );
            let viewport = vk::Viewport::default()
                .width(target.width as f32)
                .height(target.height as f32)
                .max_depth(1.0);
            d.cmd_set_viewport(self.commands, 0, &[viewport]);
            for area in &areas {
                d.cmd_set_scissor(self.commands, 0, std::slice::from_ref(area));
                if damage.is_some() {
                    let clear = vk::ClearAttachment::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .color_attachment(0)
                        .clear_value(vk::ClearValue {
                            color: vk::ClearColorValue { float32: colour },
                        });
                    let rect = vk::ClearRect::default().rect(*area).layer_count(1);
                    d.cmd_clear_attachments(self.commands, &[clear], &[rect]);
                }
                for (index, layer) in layers.iter().enumerate() {
                    if !overlaps(layer, area) {
                        continue;
                    }
                    let image = [vk::DescriptorImageInfo::default()
                        .image_view(layer.pixels.view)
                        .image_layout(layer.pixels.layout)];
                    let mut writes = vec![
                        vk::WriteDescriptorSet::default()
                            .dst_binding(0)
                            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                            .image_info(&image),
                    ];
                    for (i, info) in shared.iter().enumerate() {
                        writes.push(
                            vk::WriteDescriptorSet::default()
                                .dst_binding(i as u32 + 1)
                                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                                .buffer_info(&info[..]),
                        );
                    }
                    self.push.cmd_push_descriptor_set(
                        self.commands,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.layout,
                        0,
                        &writes,
                    );
                    d.cmd_draw(self.commands, 4, 1, 0, index as u32);
                }
            }
            d.cmd_end_rendering(self.commands);
            let to_show = barrier(
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::GENERAL,
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                vk::AccessFlags::MEMORY_READ,
            );
            d.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_show],
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

/// The rectangles of `rects` within `target`, as Vulkan's, without the
/// empty ones; more than eight become the one rectangle that covers them.
fn clip(rects: &[Rect], target: &Target) -> Vec<vk::Rect2D> {
    let (width, height) = (target.width as i32, target.height as i32);
    let mut areas: Vec<(i32, i32, i32, i32)> = rects
        .iter()
        .filter_map(|r| {
            let (x0, y0) = (r.x.max(0), r.y.max(0));
            let x1 = r.x.saturating_add(r.width).min(width);
            let y1 = r.y.saturating_add(r.height).min(height);
            (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
        })
        .collect();
    if areas.len() > 8 {
        let cover = areas.iter().fold(areas[0], |a, b| {
            (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
        });
        areas = vec![cover];
    }
    areas
        .into_iter()
        .map(|(x0, y0, x1, y1)| vk::Rect2D {
            offset: vk::Offset2D { x: x0, y: y0 },
            extent: vk::Extent2D {
                width: (x1 - x0) as u32,
                height: (y1 - y0) as u32,
            },
        })
        .collect()
}

/// Whether `layer` covers any of `area`.
fn overlaps(layer: &Layer, area: &vk::Rect2D) -> bool {
    let (x0, y0) = (area.offset.x, area.offset.y);
    let (x1, y1) = (
        x0 + area.extent.width as i32,
        y0 + area.extent.height as i32,
    );
    layer.x < x1
        && layer.y < y1
        && layer.x + layer.pixels.width as i32 > x0
        && layer.y + layer.pixels.height as i32 > y0
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: nothing runs on the device any more.
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some((l, r)) = self.frame.borrow_mut().take() {
                self.forget_host(l);
                self.forget_host(r);
            }
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.layout, None);
            self.device
                .destroy_descriptor_set_layout(self.set_layout, None);
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
            renderer.device.destroy_image_view(self.view, None);
            renderer.device.destroy_image(self.image, None);
            renderer.device.free_memory(self.memory, None);
        }
    }
}
