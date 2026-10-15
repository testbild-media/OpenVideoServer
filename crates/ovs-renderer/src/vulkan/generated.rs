//! Drawing a generator on the card instead of on the processor.
//!
//! Measured on PC05 on 2026-10-15, one source at a time, the wall at 1920x1080
//! at 59.94: a 3840x2160 clip decoded on the card cost the render thread **6
//! microseconds** a pass and `ovs.noise` cost **25,532**. A generator was the
//! only thing the wall was waiting for, because it was the only thing still
//! making every pixel on the processor and then copying eight megabytes of
//! them across the bus. Sharing the rows out between cores took it to about
//! twelve milliseconds; this removes the work and the copy together.
//!
//! One pipeline for every generator and every size. The size is dynamic state
//! rather than baked into the pipeline, because a layer is whatever size the
//! show says and a pipeline per size would be a pipeline per layer.
//!
//! # What this does not own
//!
//! The image it draws into. That is the layer's ordinary media texture, which
//! the composition already knows how to sample — a generator drawn here lands
//! in exactly the place an uploaded one did, so nothing downstream of it
//! changes. All this needs of that texture is that it was made able to be an
//! attachment, and a framebuffer pointing at it.

use ash::vk;

use crate::vulkan::VulkanError;

/// What one generator pass is told.
///
/// Laid out to match `Generated` in `shaders/generator.wgsl`: four floats,
/// four floats, two unsigned and two more. 48 bytes, which is inside the 128
/// every Vulkan implementation guarantees for immediates.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GeneratedPush {
    /// The kind's own numbers, in the order its branch reads them.
    pub values: [f32; 4],
    /// The kind's colour, where it has one.
    pub colour: [f32; 4],
    /// Its second colour: a gradient's far end, a grid's or a shape's bed.
    pub colour_b: [f32; 4],
    /// The noise field's seed: one 64-bit number, low half first.
    pub field: [u32; 2],
    /// How big the picture being drawn is.
    ///
    /// Stated rather than asked of the target, because most kinds read no
    /// texture at all and are handed the one-by-one white default: asking that
    /// its size would have made every gradient a single colour and every grid
    /// one line, on a machine where it is impossible to see that it is wrong.
    pub size: [u32; 2],
    /// Which branch of the shader runs.
    pub kind: u32,
    /// Bit 0: the three channels get independent fields.
    pub flags: u32,
}

impl GeneratedPush {
    /// Value noise.
    pub const NOISE: u32 = 0;
    /// A flat fill, which is also what a strobe is on the tick it is lit.
    pub const SOLID: u32 = 1;
    /// The service pattern: drifting bars, a sweep, and a caption.
    pub const PATTERN: u32 = 2;
    /// A linear gradient between two colours.
    pub const GRADIENT: u32 = 3;
    /// Ruled lines at a spacing.
    pub const GRID: u32 = 4;
    /// One figure, centred and turned, over a bed.
    pub const SHAPE: u32 = 5;
    /// The three channels get independent noise fields.
    pub const COLOURED: u32 = 1;

    /// The bytes to push, in the layout the shader declares.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `Self` is `repr(C)` and holds nothing but 32-bit numbers, so
        // every byte of it is initialised and reading it as bytes is defined.
        unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        }
    }
}

/// The one pipeline every generator is drawn by.
pub struct GeneratedPipeline {
    /// The pass a generator's texture is drawn in.
    ///
    /// Public because a framebuffer has to name it, and the framebuffers live
    /// with the textures rather than here.
    pub render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    shader: vk::ShaderModule,
}

impl GeneratedPipeline {
    /// Builds the render pass, the layout and the pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming the Vulkan call that failed.
    pub fn new(
        device: &ash::Device,
        spirv: &[u8],
        set_layout: vk::DescriptorSetLayout,
    ) -> Result<Self, VulkanError> {
        let render_pass = Self::create_render_pass(device)?;
        let shader = match Self::create_shader(device, spirv) {
            Ok(shader) => shader,
            Err(error) => {
                // SAFETY: created just above and not yet used.
                unsafe { device.destroy_render_pass(render_pass, None) };
                return Err(error);
            }
        };

        // One set, of the shape every texture here already has. Most kinds
        // read no picture at all and are handed the white default — a set must
        // be bound for a layout that declares one, and binding a texture the
        // shader never reads costs a bind and nothing else.
        //
        // The immediates are declared for both stages. A range that covers a
        // stage which does not use it is legal and costs nothing, while one
        // that misses a stage which does is a pipeline the driver may refuse.
        let set_layouts = [set_layout];
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(u32::try_from(std::mem::size_of::<GeneratedPush>()).unwrap_or(128))];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&ranges);
        // SAFETY: every borrowed slice outlives the call.
        let layout = match unsafe { device.create_pipeline_layout(&layout_info, None) } {
            Ok(layout) => layout,
            Err(result) => {
                // SAFETY: both were created above and nothing refers to them.
                unsafe {
                    device.destroy_shader_module(shader, None);
                    device.destroy_render_pass(render_pass, None);
                }
                return Err(VulkanError::Call {
                    operation: "vkCreatePipelineLayout for a generator",
                    result,
                });
            }
        };

        match Self::create_pipeline(device, render_pass, layout, shader) {
            Ok(pipeline) => Ok(Self {
                render_pass,
                layout,
                pipeline,
                shader,
            }),
            Err(error) => {
                // SAFETY: as above; nothing was handed out.
                unsafe {
                    device.destroy_pipeline_layout(layout, None);
                    device.destroy_shader_module(shader, None);
                    device.destroy_render_pass(render_pass, None);
                }
                Err(error)
            }
        }
    }

    /// Records one generator pass into `framebuffer`.
    ///
    /// # Safety
    ///
    /// Recording must be in progress on `command_buffer`, no render pass may
    /// be open, and `framebuffer` must name an image of `extent` made against
    /// [`Self::render_pass`].
    pub unsafe fn draw(
        &self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        framebuffer: vk::Framebuffer,
        extent: vk::Extent2D,
        set: vk::DescriptorSet,
        push: &GeneratedPush,
    ) {
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        // No clear value: the pass does not load and the triangle covers every
        // pixel, so there is nothing to clear and nothing to preserve.
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(framebuffer)
            .render_area(area);

        let viewports = [vk::Viewport::default()
            .width(extent.width as f32)
            .height(extent.height as f32)
            .max_depth(1.0)];
        let scissors = [area];

        // SAFETY: the caller promises recording is open and the handles match;
        // every handle here was made on this device.
        unsafe {
            device.cmd_begin_render_pass(command_buffer, &begin, vk::SubpassContents::INLINE);
            device.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            );
            device.cmd_set_viewport(command_buffer, 0, &viewports);
            device.cmd_set_scissor(command_buffer, 0, &scissors);
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[set],
                &[],
            );
            device.cmd_push_constants(
                command_buffer,
                self.layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                push.bytes(),
            );
            // One triangle, covering the target.
            device.cmd_draw(command_buffer, 3, 1, 0, 0);
            device.cmd_end_render_pass(command_buffer);
        }
    }

    /// Frees everything this made.
    ///
    /// # Safety
    ///
    /// The device must be idle, and nothing may refer to these handles.
    pub unsafe fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller promises the device is idle.
        unsafe {
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_shader_module(self.shader, None);
            device.destroy_render_pass(self.render_pass, None);
        }
        self.pipeline = vk::Pipeline::null();
        self.layout = vk::PipelineLayout::null();
        self.shader = vk::ShaderModule::null();
        self.render_pass = vk::RenderPass::null();
    }

    /// The pass a generator is drawn in.
    ///
    /// Nothing is loaded — the triangle writes every pixel, so reading the old
    /// contents in would be a full-size read for nothing — and it ends ready
    /// to be sampled, which is the only thing that happens to it next.
    fn create_render_pass(device: &ash::Device) -> Result<vk::RenderPass, VulkanError> {
        let attachments = [vk::AttachmentDescription::default()
            .format(vk::Format::R8G8B8A8_UNORM)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];

        let colour_references = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpasses = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&colour_references)];

        // Both transitions said out loud, for the reason the composition pass
        // gives at length: an implicit outgoing dependency makes the colour
        // writes visible to nothing, and the transition happens inside the
        // pass where no later barrier can reach it. The picture written here
        // is sampled by the composition in the same submit.
        let dependencies = [
            vk::SubpassDependency::default()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .dst_subpass(0)
                .src_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                .src_access_mask(vk::AccessFlags::SHADER_READ)
                .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
            vk::SubpassDependency::default()
                .src_subpass(0)
                .dst_subpass(vk::SUBPASS_EXTERNAL)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags::SHADER_READ),
        ];

        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);

        // SAFETY: every borrowed slice outlives the call.
        unsafe { device.create_render_pass(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateRenderPass for a generator",
            result,
        })
    }

    /// Compiles the shader module the build script produced.
    fn create_shader(device: &ash::Device, spirv: &[u8]) -> Result<vk::ShaderModule, VulkanError> {
        // SPIR-V is a sequence of 32-bit words and the loader wants it as one.
        let mut words = Vec::with_capacity(spirv.len() / 4);
        for word in spirv.as_chunks::<4>().0 {
            words.push(u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
        }
        let info = vk::ShaderModuleCreateInfo::default().code(&words);
        // SAFETY: `words` outlives the call and holds whole words.
        unsafe { device.create_shader_module(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateShaderModule for a generator",
            result,
        })
    }

    /// Builds the pipeline, with the size left as dynamic state.
    fn create_pipeline(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        layout: vk::PipelineLayout,
        shader: vk::ShaderModule,
    ) -> Result<vk::Pipeline, VulkanError> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(shader)
                .name(c"vertex_main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(shader)
                .name(c"fragment_main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);

        // One viewport and one scissor, both set when the pass is recorded.
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);

        let rasteriser = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);

        // No blending and every channel written: this fills an image from
        // nothing, so what comes out of the fragment shader is what the image
        // holds. That is also what keeps alpha straight, since a blend would
        // premultiply it — and a strobe's clear half is alpha and nothing else.
        let attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .blend_enable(false)];
        let blend_state =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);

        let infos = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport_state)
            .dynamic_state(&dynamic_state)
            .rasterization_state(&rasteriser)
            .multisample_state(&multisample)
            .color_blend_state(&blend_state)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0)];

        // SAFETY: every borrowed slice outlives the call, and the render pass
        // and shader module are live.
        let created =
            unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &infos, None) }
                .map_err(|(_, result)| VulkanError::Call {
                    operation: "vkCreateGraphicsPipelines for a generator",
                    result,
                })?;

        created.first().copied().ok_or(VulkanError::Call {
            operation: "vkCreateGraphicsPipelines returned no generator pipeline",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_immediates_are_the_shape_the_shader_declares() {
        // 72 bytes: three colours worth of floats, a seed, a size and two more. If
        // this grows past 128 it stops being pushable on hardware that offers
        // the guaranteed minimum, and the failure would be a pipeline that
        // will not create on somebody else's machine rather than here.
        assert_eq!(std::mem::size_of::<GeneratedPush>(), 72);
        assert!(std::mem::size_of::<GeneratedPush>() <= 128);
        assert_eq!(std::mem::align_of::<GeneratedPush>(), 4);
    }

    #[test]
    fn the_bytes_are_the_fields_in_order() {
        let push = GeneratedPush {
            values: [1.0, 2.0, 0.0, 0.0],
            colour: [0.0, 0.0, 0.0, 1.0],
            colour_b: [0.0, 0.0, 0.0, 1.0],
            field: [0xDEAD_BEEF, 0x0102_0304],
            size: [1920, 1080],
            kind: GeneratedPush::NOISE,
            flags: GeneratedPush::COLOURED,
        };
        let bytes = push.bytes();
        assert_eq!(bytes.len(), 72);
        assert_eq!(&bytes[0..4], &1.0_f32.to_ne_bytes());
        assert_eq!(&bytes[48..52], &0xDEAD_BEEF_u32.to_ne_bytes());
        assert_eq!(&bytes[56..60], &1920_u32.to_ne_bytes());
        assert_eq!(&bytes[64..68], &GeneratedPush::NOISE.to_ne_bytes());
        assert_eq!(&bytes[68..72], &GeneratedPush::COLOURED.to_ne_bytes());
    }
}
