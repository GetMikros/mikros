//! Composing with Vulkan on the CPU driver. Each output is a linear image
//! whose memory the compositor exports as a memory object, which the
//! display imports, so that nothing copies a frame before the display
//! driver. A client's buffer is imported, through the export token or
//! memory object that the client gave, as a Vulkan buffer of its memory. A
//! frame clears the output, or only the rectangles of its damage on an
//! image that keeps the rest, to the background colour and draws a quad for
//! each layer that touches them, whose fragment shader
//! (shaders/compose.wgsl) reads the layer's pixels from its buffer and puts
//! them, by their alpha, over the haze or a colour inside the layer's
//! material areas and over the frame elsewhere. The values of the layers
//! and the rectangles of their areas go to the shaders in buffers of host
//! memory, and the buffers that each layer reads are pushed as its
//! descriptors.

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

/// A client's buffer, imported.
pub struct ClientBuffer {
    buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
    pub(super) stride: u32,
    pub(super) offset: u32,
    /// Whether its fourth byte is alpha.
    pub(super) alpha: bool,
}

/// Pixels that a layer shows: a buffer, the size of the pixels, the
/// pixels of a row and the first pixel, and whether they have alpha.
#[derive(Clone, Copy)]
pub struct Pixels {
    pub(super) buffer: vk::Buffer,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) row: u32,
    pub(super) first: u32,
    pub(super) alpha: bool,
}

impl ClientBuffer {
    pub fn pixels(&self) -> Pixels {
        Pixels {
            buffer: self.buffer,
            width: self.width,
            height: self.height,
            row: self.stride / BYTES_PER_PIXEL,
            first: self.offset / BYTES_PER_PIXEL,
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
    first: u32,
    row: u32,
    alpha: u32,
    haze: u32,
    flat: u32,
    translucent: u32,
    fade: f32,
    haze_first: u32,
    haze_count: u32,
    blur_first: u32,
    blur_count: u32,
    _padding: [u32; 3],
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
    /// when `alpha`. The import takes the handle; a failed one closes it.
    pub fn import(
        &self,
        handle: OwnedHandle,
        token: bool,
        (width, height, stride, offset): (u32, u32, u32, u32),
        alpha: bool,
    ) -> Result<ClientBuffer, Error> {
        let handle_type = if token { TOKEN } else { MEMORY_OBJECT };
        let size = u64::from(offset) + u64::from(stride) * u64::from(height);
        if stride < width * BYTES_PER_PIXEL
            || stride % BYTES_PER_PIXEL != 0
            || offset % BYTES_PER_PIXEL != 0
        {
            return Err(Error::NoMemory);
        }
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle_type);
        let info = vk::BufferCreateInfo::default()
            .push_next(&mut external)
            .size(size)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER);
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
        let memory_type = self
            .memory_type(
                needs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE,
            )
            .or_else(|_| {
                self.memory_type(needs.memory_type_bits, vk::MemoryPropertyFlags::empty())
            });
        let mut info = vk::MemoryAllocateInfo::default()
            .allocation_size(needs.size.max(size))
            .memory_type_index(memory_type?);
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
            alpha,
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
                first: p.first,
                row: p.row,
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
                _padding: [0; 3],
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
            let whole_area = vk::Rect2D::from(extent);
            let areas: Vec<vk::Rect2D> = match damage {
                None => vec![whole_area],
                Some(rects) => clip(rects, target),
            };
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
                    let pixels = whole(layer.pixels.buffer);
                    let infos = [&pixels, &shared[0], &shared[1], &shared[2], &shared[3]];
                    let writes: Vec<vk::WriteDescriptorSet> = infos
                        .iter()
                        .enumerate()
                        .map(|(binding, info)| {
                            vk::WriteDescriptorSet::default()
                                .dst_binding(binding as u32)
                                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                                .buffer_info(&info[..])
                        })
                        .collect();
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
