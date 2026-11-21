//! The compositor's graphics pipeline: the shaders that the build script
//! compiles from shaders/compose.wgsl, and the layouts of what they read.

use super::render::{Error, FORMAT};
use ash::vk;

/// The SPIR-V of the shaders, which the build script compiles.
const VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compose.vert.spv"));
const FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compose.frag.spv"));

/// The descriptor set layout of the layer's image and the four storage
/// buffers that the shaders read, pushed for each layer, the pipeline layout and the pipeline,
/// which draws triangle strips into an output with premultiplied alpha.
pub(super) fn pipeline(
    device: &ash::Device,
) -> Result<(vk::DescriptorSetLayout, vk::PipelineLayout, vk::Pipeline), Error> {
    let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
    let bindings: Vec<_> = (0..5)
        .map(|binding| {
            let kind = match binding {
                0 => vk::DescriptorType::SAMPLED_IMAGE,
                _ => vk::DescriptorType::STORAGE_BUFFER,
            };
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(kind)
                .descriptor_count(1)
                .stage_flags(stages)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default()
        .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR)
        .bindings(&bindings);
    // SAFETY: the create info and what it points to live for the call.
    let set_layout = unsafe { device.create_descriptor_set_layout(&info, None)? };
    let set_layouts = [set_layout];
    let info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
    // SAFETY: as above.
    let layout = unsafe { device.create_pipeline_layout(&info, None)? };
    let module = |bytes: &[u8]| {
        let words = ash::util::read_spv(&mut std::io::Cursor::new(bytes))
            .expect("the build script's SPIR-V");
        let info = vk::ShaderModuleCreateInfo::default().code(&words);
        // SAFETY: the words are SPIR-V that naga validated.
        unsafe { device.create_shader_module(&info, None) }
    };
    let vertex = module(VERTEX)?;
    let fragment = module(FRAGMENT)?;
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vertex)
            .name(c"vertex"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(fragment)
            .name(c"fragment"),
    ];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let blend = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)];
    let blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
    let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
    let formats = [FORMAT];
    let mut rendering =
        vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .push_next(&mut rendering)
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blending)
        .dynamic_state(&dynamic)
        .layout(layout);
    // SAFETY: the create info and what it points to live for the call.
    let pipelines =
        unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) };
    // SAFETY: the modules are no longer needed once the pipeline is made.
    unsafe {
        device.destroy_shader_module(vertex, None);
        device.destroy_shader_module(fragment, None);
    }
    let pipeline = pipelines.map_err(|(_, e)| e)?[0];
    Ok((set_layout, layout, pipeline))
}
