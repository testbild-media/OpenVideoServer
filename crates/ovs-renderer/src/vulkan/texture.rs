//! GPU textures.
//!
//! The machinery three different things need: a still image in a slot, a decoded
//! video frame, and the composition itself when pieces sample it onto an output.
//! Building it once here is why none of those three has to build it again.
//!
//! # Upload path
//!
//! Pixels go into a host-visible staging buffer, then a copy on the GPU moves
//! them into device-local memory with an optimal tiling. Sampling directly from
//! host memory would work and would be much slower every frame thereafter; the
//! copy is paid once per upload.
//!
//! The staging buffer is kept and reused rather than allocated per upload, so a
//! video texture replaced sixty times a second allocates nothing after the
//! first frame (house rule §5).
//!
//! # One image or three
//!
//! A still, a generator and the composition are RGBA: one image, four bytes a
//! pixel. A decoded video frame is what its codec produced, which for nearly
//! everything is three planes of eight-bit Y, Cb and Cr — and at 4:2:0 that is
//! 3.1 MB for a 1080p frame against 8.3 for the same frame as RGBA.
//!
//! That ratio is the whole reason this module knows about planes. The frame is
//! copied twice a layer a frame, into the staging buffer and then onto the
//! card, and seven layers of RGBA is 74% of a frame at sixty. The
//! conversion the copy was paying for costs a few instructions in a fragment
//! shader that was already sampling.
//!
//! Three images rather than one interleaved one, because that is what a decoder
//! hands over: separate, tightly packed planes, at their own sizes. The sampler
//! does the chroma upsampling for free — a half-size plane sampled with
//! normalised coordinates is bilinear chroma interpolation and nobody had to
//! write it.

#![allow(
    unsafe_code,
    reason = "every Vulkan entry point is unsafe; this module is the boundary"
)]

use ash::vk;

use crate::media::{ColourMatrix, DecodedImage, SampleDepth};
use crate::vulkan::{VulkanContext, VulkanError};

/// Finds a memory type satisfying `requirements` with the wanted properties.
pub(crate) fn find_memory_type(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    wanted: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..memory_properties.memory_type_count).find(|index| {
        let supported = type_bits & (1 << index) != 0;
        let properties = memory_properties.memory_types[*index as usize].property_flags;
        supported && properties.contains(wanted)
    })
}

/// How the samples in a texture's planes are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureLayout {
    /// One image, four bytes a pixel, ready to draw.
    Rgba,
    /// The same, with blue and red the other way round in memory.
    ///
    /// **No shader change and no conversion.** A Vulkan format describes how
    /// bytes are arranged, and a sampler hands the shader components in RGBA
    /// order whichever arrangement it read them from — so this differs from
    /// [`Self::Rgba`] in exactly one place, the format in `planes`.
    ///
    /// It exists because the System screen arrives BGRA, which is what CEF
    /// paints, and because the swapchain this driver offers is BGRA too. The
    /// alternative was a pass over every pixel of every frame to put the bytes
    /// back the way the card was going to read them anyway.
    Bgra,
    /// One image the card already knows how to sample, block-compressed.
    ///
    /// A HAP frame, handed over as it arrived. There is no decoding and no
    /// conversion anywhere on this path: the bytes in the file are the bytes
    /// in the image, which is a quarter of the bus traffic of the same frame
    /// as RGBA and none of the processor work.
    ///
    /// Only the two formats the card samples as colour. `YCoCg_DXT5` is also
    /// DXT5 blocks, but its three channels are luma and two chroma
    /// differences with a scale in the alpha — sampling it as colour gives a
    /// green picture, so it needs a shader branch it does not yet have and is
    /// refused rather than shown wrongly.
    Blocks(crate::media::hap::BlockFormat),
    /// Three images of Y, Cb and Cr, converted in the shader.
    Planar {
        /// How many luma samples across share one chroma sample. 1 or 2.
        subsample_x: u32,
        /// How many luma samples down share one chroma sample. 1 or 2.
        subsample_y: u32,
        /// Whether the samples use 0–255 rather than 16–235.
        full_range: bool,
        /// Which coefficients turn them back into colour.
        matrix: ColourMatrix,
        /// How many bits a sample carries, and where in its element.
        ///
        /// Eight-bit planes are `R8_UNORM` and ten-bit ones `R16_UNORM`, and
        /// the two stay apart all the way to the card: the owner's instruction
        /// of 2026-09-23 is that eight-bit material takes the eight-bit path
        /// and is not widened into a sixteen-bit one for the sake of having
        /// one path. What converges is the shader.
        depth: SampleDepth,
    },
    /// Two images: luma, and chroma with Cb and Cr interleaved.
    ///
    /// **What a card's decoder produces.** NVDEC hands back NV12 — one eight-bit
    /// luma plane and one half-size plane of Cb and Cr side by side — and a
    /// picture that is already in the card's memory must not be taken apart on
    /// the processor to fit a three-plane texture. That would be the copy this
    /// whole path exists to avoid, with a deinterleave on top of it.
    ///
    /// The shader is untouched. It samples Cb from one image and Cr from
    /// another, so this layout gives it **two views of the same image**: one as
    /// it is, where `.r` is Cb, and one with the green component swizzled into
    /// red, where `.r` is Cr. A view costs nothing and a swizzle is free in the
    /// sampler.
    ///
    /// Always 4:2:0 and always eight-bit, because that is what NV12 is. Ten-bit
    /// material from a card is P010 and is a different layout, not a flag on
    /// this one.
    Nv12 {
        /// Whether the samples use 0–255 rather than 16–235.
        full_range: bool,
        /// Which coefficients turn them back into colour.
        matrix: ColourMatrix,
    },
}

impl TextureLayout {
    /// What the shader multiplies a sample by before converting it.
    ///
    /// One for everything that is already in the domain the colour constants
    /// are written in, which is every eight-bit texture and every block one.
    /// The two ten-bit layouts are the only things that are not, and they are
    /// not by different amounts — see [`SampleDepth`].
    #[must_use]
    pub fn sample_scale(self) -> f32 {
        match self {
            Self::Planar { depth, .. } => depth.sample_scale(),
            // NV12 is eight-bit by definition, so its samples are already in the
            // domain the colour constants are written in.
            Self::Nv12 { .. } | Self::Rgba | Self::Bgra | Self::Blocks(_) => 1.0,
        }
    }

    /// What the shader is told about this layout, as a per-instance number.
    ///
    /// A number rather than four flags because it travels as one vertex
    /// attribute beside the quad's colour and rectangle, and because the
    /// shader's job is to pick a matrix rather than to reason about a bitfield.
    /// Zero is RGBA, which is the case that does nothing.
    #[must_use]
    pub const fn shader_code(self) -> f32 {
        match self {
            // All three of these are already colour by the time the card
            // samples them, so the shader does what it does for RGBA: nothing.
            // BGRA included — the sampler has already put the components in
            // order by then.
            Self::Rgba | Self::Bgra | Self::Blocks(_) => 0.0,
            // NV12 is Y'CbCr like the planar layouts and converts the same
            // way: how the chroma reached the card is the texture's business,
            // not the shader's.
            Self::Nv12 {
                full_range: false,
                matrix: ColourMatrix::Bt709,
            }
            | Self::Planar {
                full_range: false,
                matrix: ColourMatrix::Bt709,
                ..
            } => 1.0,
            Self::Nv12 {
                full_range: true,
                matrix: ColourMatrix::Bt709,
            }
            | Self::Planar {
                full_range: true,
                matrix: ColourMatrix::Bt709,
                ..
            } => 2.0,
            Self::Nv12 {
                full_range: false,
                matrix: ColourMatrix::Bt601,
            }
            | Self::Planar {
                full_range: false,
                matrix: ColourMatrix::Bt601,
                ..
            } => 3.0,
            Self::Nv12 {
                full_range: true,
                matrix: ColourMatrix::Bt601,
            }
            | Self::Planar {
                full_range: true,
                matrix: ColourMatrix::Bt601,
                ..
            } => 4.0,
            Self::Nv12 {
                full_range: false,
                matrix: ColourMatrix::Bt2020,
            }
            | Self::Planar {
                full_range: false,
                matrix: ColourMatrix::Bt2020,
                ..
            } => 5.0,
            Self::Nv12 {
                full_range: true,
                matrix: ColourMatrix::Bt2020,
            }
            | Self::Planar {
                full_range: true,
                matrix: ColourMatrix::Bt2020,
                ..
            } => 6.0,
        }
    }

    /// The layout a decoded picture wants a texture in.
    #[must_use]
    pub fn of(image: &DecodedImage) -> Self {
        if let Some(format) = image.block_format() {
            return Self::Blocks(format);
        }
        if image.is_bgra() {
            return Self::Bgra;
        }
        if let Some(chroma) = image.chroma().filter(|chroma| chroma.is_interleaved()) {
            // `DecodedImage::interleaved` accepts nothing but NV12, so there is
            // no shape of interleaved picture this layout cannot sample.
            return Self::Nv12 {
                full_range: chroma.full_range(),
                matrix: chroma.matrix(),
            };
        }
        image.chroma().map_or(Self::Rgba, |chroma| Self::Planar {
            subsample_x: chroma.subsample_x(),
            subsample_y: chroma.subsample_y(),
            full_range: chroma.full_range(),
            matrix: chroma.matrix(),
            depth: chroma.depth(),
        })
    }

    /// The size and format of each plane, for a picture of `width` by `height`.
    fn planes(self, width: u32, height: u32) -> Vec<(u32, u32, vk::Format)> {
        match self {
            Self::Rgba => vec![(width, height, vk::Format::R8G8B8A8_UNORM)],
            Self::Bgra => vec![(width, height, vk::Format::B8G8R8A8_UNORM)],
            // In texels, not blocks: Vulkan sizes a compressed image the way it
            // sizes any other, and works out the blocks itself.
            Self::Blocks(format) => vec![(width, height, block_format(format))],
            // Luma at full size, then one half-size plane holding both
            // chroma components side by side. Two images for three bindings:
            // `chroma_views` makes the third a swizzled view of the second.
            Self::Nv12 { .. } => vec![
                (width, height, vk::Format::R8_UNORM),
                (
                    width.div_ceil(2).max(1),
                    height.div_ceil(2).max(1),
                    vk::Format::R8G8_UNORM,
                ),
            ],
            Self::Planar {
                subsample_x,
                subsample_y,
                depth,
                ..
            } => {
                let wide = width.div_ceil(subsample_x.max(1)).max(1);
                let tall = height.div_ceil(subsample_y.max(1)).max(1);
                // One element a sample either way; what changes is how wide an
                // element is. Sixteen bits normalised, not ten: `R16_UNORM` is
                // supported everywhere, and the sampler's extra six bits are
                // taken out again by the scale the shader applies.
                let format = if depth.is_ten_bit() {
                    vk::Format::R16_UNORM
                } else {
                    vk::Format::R8_UNORM
                };
                vec![
                    (width, height, format),
                    (wide, tall, format),
                    (wide, tall, format),
                ]
            }
        }
    }
}

/// What Vulkan calls each of the block formats this reads.
const fn block_format(format: crate::media::hap::BlockFormat) -> vk::Format {
    use crate::media::hap::BlockFormat;
    match format {
        BlockFormat::Dxt1 => vk::Format::BC1_RGB_UNORM_BLOCK,
        // YCoCg is DXT5 blocks too. It is refused before it reaches here, by
        // whatever opened the clip; naming it as BC3 keeps this total rather
        // than leaving a panic in a frame path.
        BlockFormat::Dxt5 | BlockFormat::YCoCgDxt5 => vk::Format::BC3_UNORM_BLOCK,
    }
}

/// One image of a texture: the whole of an RGBA one, or a plane of a planar one.
struct Plane {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
    /// Where this plane's bytes start in the shared staging buffer.
    ///
    /// One buffer for all three, because one mapping and one allocation is
    /// cheaper than three and because the copies are recorded together anyway.
    offset: u64,
    bytes: u64,
}

/// A sampled image on the GPU.
pub struct Texture {
    planes: Vec<Plane>,
    /// A second view of the chroma plane, where Cr is in the red component.
    ///
    /// Only for [`TextureLayout::Nv12`], whose one chroma image holds both
    /// components side by side while the shader samples them from two bindings.
    /// A view with the green component swizzled into red is what lets the
    /// second binding read Cr without a shader branch, a second image or a pass
    /// over the picture — the sampler does it, and it costs nothing.
    chroma_as_cr: Option<vk::ImageView>,
    layout: TextureLayout,
    sampler: vk::Sampler,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_mapped: *mut u8,
    width: u32,
    height: u32,
}

/// Where a plane's bytes begin, rounded up so every copy is legally aligned.
///
/// `vkCmdCopyBufferToImage` wants an offset that is a multiple of four and of
/// the format's texel size. Sixteen satisfies both for every format here and
/// costs at most forty-five bytes across three planes.
const fn aligned(offset: u64) -> u64 {
    offset.div_ceil(16) * 16
}

// SAFETY: `staging_mapped` points into host-coherent memory owned by this
// texture for as long as it lives, and nothing else maps or frees it. A texture
// is only used from one thread at a time, which is what `Send` promises.
unsafe impl Send for Texture {}

impl Texture {
    /// Creates an RGBA texture of the given size, with nothing in it yet.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming the Vulkan call that failed.
    pub fn new(context: &VulkanContext, width: u32, height: u32) -> Result<Self, VulkanError> {
        Self::with_layout(context, width, height, TextureLayout::Rgba)
    }

    /// Creates a texture of the given size and layout, with nothing in it yet.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming the Vulkan call that failed.
    pub fn with_layout(
        context: &VulkanContext,
        width: u32,
        height: u32,
        layout: TextureLayout,
    ) -> Result<Self, VulkanError> {
        let device = context.device();
        let width = width.max(1);
        let height = height.max(1);

        // SAFETY: the physical device belongs to this context's instance.
        let memory_properties = unsafe {
            context
                .instance_ref()
                .get_physical_device_memory_properties(context.physical_device())
        };

        let mut planes = Vec::new();
        let mut offset = 0u64;
        for (plane_width, plane_height, format) in layout.planes(width, height) {
            // A compressed plane is measured in whole blocks, and a frame is
            // not obliged to be a multiple of four: 1920x1074 is 480 by 269
            // blocks, and sizing it by texels would come up short by a row.
            let bytes = if let TextureLayout::Blocks(blocks) = layout {
                blocks.bytes_for(plane_width, plane_height) as u64
            } else {
                u64::from(plane_width) * u64::from(plane_height) * texel_bytes(format)?
            };
            match Self::create_plane(
                device,
                &memory_properties,
                plane_width,
                plane_height,
                format,
                offset,
                bytes,
            ) {
                Ok(plane) => planes.push(plane),
                Err(error) => {
                    // Whatever was made before the failure is this function's
                    // to clean up: a half-built texture nobody holds is a leak
                    // for as long as the renderer runs.
                    for plane in &mut planes {
                        plane.destroy(device);
                    }
                    return Err(error);
                }
            }
            offset = aligned(offset + bytes);
        }

        let sampler = Self::create_sampler(device)?;
        let staging_bytes = offset.max(1);
        let (staging, staging_memory, staging_mapped) =
            Self::create_staging(context, &memory_properties, staging_bytes)?;

        let chroma_as_cr = match (layout, planes.get(1)) {
            (TextureLayout::Nv12 { .. }, Some(chroma)) => {
                match Self::swizzled_view(device, chroma.image) {
                    Ok(view) => Some(view),
                    Err(error) => {
                        // Nothing is half-built: the planes and the staging go
                        // back before the error does.
                        for plane in &planes {
                            // SAFETY: created above on this device and not in use.
                            unsafe {
                                device.destroy_image_view(plane.view, None);
                                device.destroy_image(plane.image, None);
                                device.free_memory(plane.memory, None);
                            }
                        }
                        return Err(error);
                    }
                }
            }
            _ => None,
        };

        Ok(Self {
            planes,
            chroma_as_cr,
            layout,
            sampler,
            staging,
            staging_memory,
            staging_mapped,
            width,
            height,
        })
    }

    /// Creates one plane's image, memory and view.
    fn create_plane(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        width: u32,
        height: u32,
        format: vk::Format,
        offset: u64,
        bytes: u64,
    ) -> Result<Plane, VulkanError> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        // SAFETY: `image_info` is fully initialised and outlives the call.
        let image = unsafe { device.create_image(&image_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateImage",
                result,
            }
        })?;

        // SAFETY: `image` was just created on this device.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory_type = find_memory_type(
            memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or(VulkanError::Call {
            operation: "finding device-local memory for a texture",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;

        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: `allocate` is fully initialised.
        let memory = unsafe { device.allocate_memory(&allocate, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateMemory",
                result,
            }
        })?;
        // SAFETY: allocated for exactly this image's requirements.
        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindImageMemory",
                result,
            }
        })?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        // SAFETY: `image` is bound and `view_info` is fully initialised.
        let view = unsafe { device.create_image_view(&view_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateImageView",
                result,
            }
        })?;

        Ok(Plane {
            image,
            memory,
            view,
            width,
            height,
            offset,
            bytes,
        })
    }

    /// A view of a two-component chroma image with green read as red.
    ///
    /// The shader asks its Cr binding for `.r`, and in NV12 Cr is the second
    /// component. A component mapping is part of the view rather than of the
    /// image, so one image can be bound twice and read differently each time.
    fn swizzled_view(device: &ash::Device, image: vk::Image) -> Result<vk::ImageView, VulkanError> {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8_UNORM)
            .components(
                vk::ComponentMapping::default()
                    .r(vk::ComponentSwizzle::G)
                    .g(vk::ComponentSwizzle::G)
                    .b(vk::ComponentSwizzle::G)
                    .a(vk::ComponentSwizzle::ONE),
            )
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        // SAFETY: `image` is bound and `view_info` is fully initialised.
        unsafe { device.create_image_view(&view_info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateImageView for the Cr half of an NV12 plane",
            result,
        })
    }

    fn create_staging(
        context: &VulkanContext,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        bytes: u64,
    ) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8), VulkanError> {
        let device = context.device();

        let info = vk::BufferCreateInfo::default()
            .size(bytes)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `info` is fully initialised.
        let buffer =
            unsafe { device.create_buffer(&info, None) }.map_err(|result| VulkanError::Call {
                operation: "vkCreateBuffer for a texture upload",
                result,
            })?;

        // SAFETY: `buffer` was just created on this device.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        // The card's own memory first, where the card lets the processor write
        // into it. A staging buffer there is written once across the bus by
        // `stage_image` and then copied inside the card, which is memory it
        // already has; one in host memory is written once in host memory and
        // then *read* across the bus by the copy, which is the same trip taken
        // later and more slowly.
        //
        // Measured on the test machine, seven 1080p layers: the card-side copy
        // was 5885 us of a 6756 us composition with the buffer in host memory,
        // against 871 us to draw the same frame with nothing to copy. It is the
        // largest single thing in a frame.
        //
        // Not every card offers it. Without resizable BAR the window is a
        // couple of hundred megabytes and a show's worth of layers will not
        // fit, so this falls back rather than failing — and it falls back on a
        // failed *allocation* as well as on a missing memory type, because a
        // window that exists and is full looks like the first and behaves like
        // the second.
        let wanted = [
            vk::MemoryPropertyFlags::DEVICE_LOCAL
                | vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ];

        let mut memory = None;
        for properties in wanted {
            let Some(memory_type) =
                find_memory_type(memory_properties, requirements.memory_type_bits, properties)
            else {
                continue;
            };
            let allocate = vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type);
            // SAFETY: `allocate` is fully initialised.
            if let Ok(allocated) = unsafe { device.allocate_memory(&allocate, None) } {
                tracing::trace!(
                    memory_type,
                    ?properties,
                    "a texture upload buffer was allocated"
                );
                memory = Some(allocated);
                break;
            }
        }
        let Some(memory) = memory else {
            return Err(VulkanError::Call {
                operation: "finding host-visible memory for a texture upload",
                result: vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
            });
        };
        // SAFETY: allocated for exactly this buffer's requirements.
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindBufferMemory",
                result,
            }
        })?;

        // Mapped once at offset zero, which satisfies the alignment rule an
        // arbitrary offset would not, and kept mapped so a texture replaced
        // every frame does no driver round trip per frame.
        // SAFETY: the memory is host-visible and host-coherent, and nothing else
        // maps it.
        let mapped =
            unsafe { device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) }
                .map_err(|result| VulkanError::Call {
                    operation: "vkMapMemory for a texture upload",
                    result,
                })?;

        Ok((buffer, memory, mapped.cast::<u8>()))
    }

    /// Returns the texture's size in pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Returns how this texture's samples are arranged.
    #[must_use]
    pub const fn layout(&self) -> TextureLayout {
        self.layout
    }

    /// Returns the first plane's view, for binding.
    ///
    /// RGBA for an RGBA texture, luma for a planar one.
    #[must_use]
    pub fn view(&self) -> vk::ImageView {
        self.planes
            .first()
            .map_or(vk::ImageView::null(), |plane| plane.view)
    }

    /// Returns the Cb and Cr views, for binding.
    ///
    /// An RGBA texture has neither, and answers with its own view twice: a
    /// descriptor set must name an image for every binding it declares, and
    /// the shader is told by its instance not to sample these. Binding the
    /// same image twice is free and leaving a binding empty is undefined.
    #[must_use]
    pub fn chroma_views(&self) -> (vk::ImageView, vk::ImageView) {
        match (self.planes.get(1), self.planes.get(2), self.chroma_as_cr) {
            // NV12: one image, read twice. The swizzled view puts Cr where the
            // shader looks for it.
            (Some(chroma), None, Some(as_cr)) => (chroma.view, as_cr),
            (Some(cb), Some(cr), _) => (cb.view, cr.view),
            _ => (self.view(), self.view()),
        }
    }

    /// Returns the sampler, for binding.
    #[must_use]
    pub const fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// Copies pixels into the staging buffer, ready to be uploaded.
    ///
    /// `pixels` is tightly packed RGBA, four bytes per pixel, `width * height`
    /// pixels. For a planar texture, use [`stage_image`](Self::stage_image).
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] when the slice is not the right length or
    /// the texture is not RGBA, rather than reading past the end of it or
    /// leaving the texture half written.
    pub fn stage(&mut self, pixels: &[u8]) -> Result<(), VulkanError> {
        // Either way round: both are one plane of four bytes a pixel, and which
        // way round the bytes are is the texture's business, not the caller's.
        if !matches!(self.layout, TextureLayout::Rgba | TextureLayout::Bgra) {
            return Err(VulkanError::Call {
                operation: "RGBA pixels staged into a planar texture",
                result: vk::Result::ERROR_VALIDATION_FAILED_EXT,
            });
        }
        self.stage_plane(0, pixels)
    }

    /// Copies a decoded picture into the staging buffer, plane by plane.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] when the picture does not match this
    /// texture's layout or size. A picture that disagrees is never partly
    /// written: the check happens before the first byte moves.
    pub fn stage_image(&mut self, image: &DecodedImage) -> Result<(), VulkanError> {
        if TextureLayout::of(image) != self.layout {
            return Err(VulkanError::Call {
                operation: "a picture staged into a texture of another layout",
                result: vk::Result::ERROR_VALIDATION_FAILED_EXT,
            });
        }
        let Some(chroma) = image.chroma() else {
            return self.stage_plane(0, image.pixels());
        };
        // Two planes or three, depending on how the decoder handed the colour
        // over. Checked together either way, so a picture whose last plane is
        // short cannot leave the luma plane written and the colour from the
        // frame before it.
        let shaped = if chroma.is_interleaved() {
            self.planes.len() == 2 && self.fits(0, image.pixels()) && self.fits(1, chroma.pairs())
        } else {
            self.planes.len() == 3
                && self.fits(0, image.pixels())
                && self.fits(1, chroma.cb())
                && self.fits(2, chroma.cr())
        };
        if !shaped {
            return Err(VulkanError::Call {
                operation: "a planar upload is not the size of the texture",
                result: vk::Result::ERROR_VALIDATION_FAILED_EXT,
            });
        }
        self.stage_plane(0, image.pixels())?;
        if chroma.is_interleaved() {
            // One copy, row by row, instead of a strided write into two
            // destinations. The bytes go up exactly as the card produced them.
            return self.stage_plane(1, chroma.pairs());
        }
        self.stage_plane(1, chroma.cb())?;
        self.stage_plane(2, chroma.cr())
    }

    /// Whether `bytes` is exactly what plane `index` holds.
    fn fits(&self, index: usize, bytes: &[u8]) -> bool {
        self.planes
            .get(index)
            .is_some_and(|plane| u64::try_from(bytes.len()).ok() == Some(plane.bytes))
    }

    /// Copies one plane's bytes to its place in the staging buffer.
    fn stage_plane(&mut self, index: usize, bytes: &[u8]) -> Result<(), VulkanError> {
        let Some(plane) = self.planes.get(index) else {
            return Err(VulkanError::Call {
                operation: "staging a plane this texture does not have",
                result: vk::Result::ERROR_VALIDATION_FAILED_EXT,
            });
        };
        if u64::try_from(bytes.len()).ok() != Some(plane.bytes) {
            return Err(VulkanError::Call {
                operation: "a texture upload is not the size of the texture",
                result: vk::Result::ERROR_VALIDATION_FAILED_EXT,
            });
        }

        // SAFETY: the mapping covers `staging_bytes`, and this plane's offset
        // and length were laid out inside it when the texture was made. The
        // source and destination cannot overlap: one is host memory the caller
        // owns and the other is a Vulkan mapping.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.staging_mapped.add(plane.offset as usize),
                bytes.len(),
            );
        }
        Ok(())
    }

    /// Creates the sampler every image in the renderer is read through.
    ///
    /// One definition, because the filtering is a property of how this renderer
    /// draws rather than of any one image: a composition sampled one way and a
    /// still sampled another would look different on the same wall for no
    /// reason an operator could act on.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] when the sampler cannot be created.
    pub(crate) fn create_sampler(device: &ash::Device) -> Result<vk::Sampler, VulkanError> {
        let sampler_info = vk::SamplerCreateInfo::default()
            // Linear, because a piece almost never maps one composition pixel to
            // one output pixel and nearest sampling would alias visibly on an
            // LED wall.
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            // Clamp rather than repeat: a piece sampling slightly past the edge
            // of its source rectangle should find the edge, not the opposite
            // side of the composition.
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK);
        // SAFETY: `sampler_info` is fully initialised.
        unsafe { device.create_sampler(&sampler_info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateSampler",
            result,
        })
    }

    /// Records the copy from the staging buffer into every plane.
    ///
    /// Leaves each image in `SHADER_READ_ONLY_OPTIMAL`, ready to sample.
    ///
    /// One barrier covering all the planes at each end rather than a pair per
    /// plane: they are written by the same transfer and read by the same
    /// fragment shader, so three of each would be three times the work to say
    /// the same thing.
    /// `families` is the pair the image passes between when the copy runs on a
    /// queue of its own: `(transfer, graphics)`. `None` when they are the same
    /// queue, and then nothing is transferred and no acquire is owed.
    pub(crate) fn record_upload(
        &self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        families: Option<(u32, u32)>,
    ) {
        let (release_from, release_to) =
            families.unwrap_or((vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED));
        let whole_image = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);

        let to_transfer: Vec<vk::ImageMemoryBarrier<'_>> = self
            .planes
            .iter()
            .map(|plane| {
                vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(plane.image)
                    .subresource_range(whole_image)
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            })
            .collect();

        let to_sampled: Vec<vk::ImageMemoryBarrier<'_>> = self
            .planes
            .iter()
            .map(|plane| {
                vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(release_from)
                    .dst_queue_family_index(release_to)
                    .image(plane.image)
                    .subresource_range(whole_image)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    // Nothing on this queue reads it; the acquire on the other
                    // one says what does.
                    .dst_access_mask(if families.is_some() {
                        vk::AccessFlags::empty()
                    } else {
                        vk::AccessFlags::SHADER_READ
                    })
            })
            .collect();

        // SAFETY: recording is in progress on `command_buffer`, and every handle
        // belongs to this texture and is live.
        unsafe {
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &to_transfer,
            );
            for plane in &self.planes {
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(plane.offset)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: plane.width,
                        height: plane.height,
                        depth: 1,
                    });
                device.cmd_copy_buffer_to_image(
                    command_buffer,
                    self.staging,
                    plane.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            }
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                if families.is_some() {
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE
                } else {
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                },
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &to_sampled,
            );
        }
    }

    /// Takes ownership of this texture's planes on the queue that draws.
    ///
    /// The other half of the release in [`record_upload`](Self::record_upload),
    /// and only needed when the copy ran on a queue of its own. Both halves
    /// must name the same two families or the image is undefined on arrival.
    pub(crate) fn record_acquire(
        &self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
        (from, to): (u32, u32),
    ) {
        let whole_image = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);

        let acquire: Vec<vk::ImageMemoryBarrier<'_>> = self
            .planes
            .iter()
            .map(|plane| {
                vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(from)
                    .dst_queue_family_index(to)
                    .image(plane.image)
                    .subresource_range(whole_image)
                    // Nothing on this queue wrote it; the release said what did.
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)
            })
            .collect();

        // SAFETY: recording is in progress on `command_buffer`, and every
        // handle belongs to this texture and is live.
        unsafe {
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &acquire,
            );
        }
    }

    /// Releases every Vulkan object, in reverse order of creation.
    ///
    /// Doing it twice is harmless, and that is not defensive programming: a
    /// texture that could not be finished leaves an entry behind, and the
    /// alternative to this is remembering at every such place whether the
    /// thing in hand has already been given back.
    ///
    /// There is no `Drop` for this, because releasing a Vulkan object needs
    /// the device and a `Drop` cannot be handed one. That is exactly why every
    /// path that builds one has to end in this call — including the paths that
    /// fail.
    pub(crate) fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller has waited for the device to be idle, so nothing
        // below is in use.
        unsafe {
            if self.staging_memory != vk::DeviceMemory::null() {
                device.unmap_memory(self.staging_memory);
            }
            if self.staging != vk::Buffer::null() {
                device.destroy_buffer(self.staging, None);
            }
            if self.staging_memory != vk::DeviceMemory::null() {
                device.free_memory(self.staging_memory, None);
            }
            if self.sampler != vk::Sampler::null() {
                device.destroy_sampler(self.sampler, None);
            }
        }
        self.staging = vk::Buffer::null();
        self.staging_memory = vk::DeviceMemory::null();
        self.staging_mapped = std::ptr::null_mut();
        self.sampler = vk::Sampler::null();
        // Before the planes, because it is a view of one of their images and a
        // view outliving its image is the one ordering mistake here that the
        // validation layers would not forgive.
        if let Some(view) = self.chroma_as_cr.take() {
            // SAFETY: created by `swizzled_view` on this device, and the caller
            // has waited for the device to be idle.
            unsafe {
                device.destroy_image_view(view, None);
            }
        }
        for plane in &mut self.planes {
            plane.destroy(device);
        }
        self.planes.clear();
    }
}

impl Plane {
    /// Releases this plane's view, image and memory.
    fn destroy(&mut self, device: &ash::Device) {
        // SAFETY: the caller has waited for the device to be idle. A null
        // handle is one this plane has already given back.
        unsafe {
            if self.view != vk::ImageView::null() {
                device.destroy_image_view(self.view, None);
            }
            if self.image != vk::Image::null() {
                device.destroy_image(self.image, None);
            }
            if self.memory != vk::DeviceMemory::null() {
                device.free_memory(self.memory, None);
            }
        }
        self.view = vk::ImageView::null();
        self.image = vk::Image::null();
        self.memory = vk::DeviceMemory::null();
    }
}

/// Returns the pixels of a one-by-one opaque white texture.
///
/// Bound for quads that have no image of their own, so the shader is the same
/// for textured and untextured quads: it always samples and multiplies, and
/// white is the identity. One code path instead of a branch per fragment.
#[must_use]
pub const fn white_pixel() -> [u8; 4] {
    [255, 255, 255, 255]
}

/// How many bytes one texel of an uncompressed format takes.
///
/// **A format this does not know is an error.** It used to be one byte, as a
/// fallback, and on 2026-09-26 that cost an evening: `B8G8R8A8_UNORM` was added
/// to [`TextureLayout::planes`] with the BGRA layout and not here, so a 1920x1080
/// BGRA texture was allocated a quarter of its size. Every upload into it was
/// refused — and the texture was still made, still bound to a descriptor and
/// still sampled, so the card read past the end of it and stopped answering.
/// `vkWaitForFences` then timed out on every output and on the composition pass:
/// two black panels, from a missing match arm.
///
/// A wrong size is worse than no texture. So this returns nothing for a format
/// nobody has written down, and the texture is not created at all.
///
/// # Errors
///
/// [`VulkanError::Call`] naming the format, so the next person to add a layout
/// reads which line they did not write.
fn texel_bytes(format: vk::Format) -> Result<u64, VulkanError> {
    match format {
        vk::Format::R8G8B8A8_UNORM | vk::Format::B8G8R8A8_UNORM => Ok(4),
        // Two components of one byte: the chroma plane of an NV12 picture,
        // where a texel is one Cb beside one Cr.
        vk::Format::R16_UNORM | vk::Format::R8G8_UNORM => Ok(2),
        vk::Format::R8_UNORM => Ok(1),
        _ => Err(VulkanError::Call {
            operation: "a texture format whose size nobody has written down",
            result: vk::Result::ERROR_FORMAT_NOT_SUPPORTED,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every format a layout can ask for has a size written down.
    ///
    /// **The test that would have caught it.** A layout added with a new format,
    /// and the table not told, allocated a texture a quarter of its size — and
    /// the failure was not a refused upload but a card that stopped answering,
    /// three layers away and an hour later. This walks every layout this
    /// renderer can produce and asks the same question the allocator asks.
    #[test]
    fn every_format_a_layout_asks_for_has_a_size() {
        let mut layouts = vec![TextureLayout::Rgba, TextureLayout::Bgra];
        for depth in [
            SampleDepth::Eight,
            SampleDepth::TenLow,
            SampleDepth::TenHigh,
        ] {
            for matrix in [
                ColourMatrix::Bt601,
                ColourMatrix::Bt709,
                ColourMatrix::Bt2020,
            ] {
                for full_range in [false, true] {
                    layouts.push(TextureLayout::Planar {
                        subsample_x: 2,
                        subsample_y: 2,
                        full_range,
                        matrix,
                        depth,
                    });
                }
            }
        }

        for layout in layouts {
            for (width, height, format) in layout.planes(1920, 1080) {
                assert!(
                    texel_bytes(format).is_ok(),
                    "{layout:?} asks for {format:?} at {width}x{height}, and nothing knows \
                     how big one of those is"
                );
            }
        }
    }

    #[test]
    fn a_bgra_plane_is_four_bytes_a_pixel_like_an_rgba_one() {
        // The two differ in which byte is blue and in nothing else. This is the
        // arithmetic that was wrong: a quarter of the size, which the allocator
        // accepted and the card did not.
        let rgba = TextureLayout::Rgba.planes(1920, 1080);
        let bgra = TextureLayout::Bgra.planes(1920, 1080);
        assert_eq!(rgba.len(), 1);
        assert_eq!(bgra.len(), 1);

        let bytes = |plane: &(u32, u32, vk::Format)| {
            u64::from(plane.0) * u64::from(plane.1) * texel_bytes(plane.2).expect("a known format")
        };
        assert_eq!(bytes(&bgra[0]), bytes(&rgba[0]));
        assert_eq!(bytes(&bgra[0]), 1920 * 1080 * 4);
    }

    fn context() -> Option<VulkanContext> {
        match VulkanContext::new("ovs-renderer texture tests") {
            Ok(context) => Some(context),
            Err(VulkanError::NoLoader(reason)) => {
                println!("skipped: no Vulkan loader ({reason})");
                None
            }
            Err(VulkanError::NoDevice(reason)) => {
                println!("skipped: no usable device ({reason})");
                None
            }
            Err(error) => panic!("Vulkan is present but a context failed: {error}"),
        }
    }

    #[test]
    fn a_texture_reports_the_size_it_was_created_with() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(context) = context() else {
            return;
        };
        let mut texture = Texture::new(&context, 64, 32).expect("creates");
        assert_eq!(texture.size(), (64, 32));
        assert_ne!(texture.view(), vk::ImageView::null());
        assert_ne!(texture.sampler(), vk::Sampler::null());
        texture.destroy(context.device());
    }

    #[test]
    fn a_system_screen_frame_fits_the_texture_it_is_staged_into() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(context) = context() else {
            return;
        };

        // The size the System screen actually runs at on the reference machine,
        // and the format CEF paints in.
        let (width, height) = (1920, 1080);
        let pixels = vec![0x40_u8; width as usize * height as usize * 4];
        let frame =
            crate::media::DecodedImage::bgra(pixels, ovs_types::geometry::SizePx { width, height })
                .expect("a frame that describes itself");
        assert_eq!(TextureLayout::of(&frame), TextureLayout::Bgra);

        let mut texture = Texture::with_layout(&context, width, height, TextureLayout::Bgra)
            .expect("a BGRA texture");
        assert_eq!(texture.size(), (width, height));

        // **The call that failed.** With the format's size missing from the
        // table the texture was a quarter of this, and every frame was refused —
        // after the texture had been made, bound and sampled, which is what
        // stopped the card.
        texture
            .stage_image(&frame)
            .expect("a 1920x1080 BGRA frame fits a 1920x1080 BGRA texture");

        texture.destroy(context.device());
    }

    #[test]
    fn a_zero_sized_texture_becomes_one_pixel_rather_than_failing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Vulkan refuses a zero extent. A one-pixel texture is a better answer
        // than a renderer that will not start because a still had no size yet.
        let Some(context) = context() else {
            return;
        };
        let mut texture = Texture::new(&context, 0, 0).expect("creates");
        assert_eq!(texture.size(), (1, 1));
        texture.destroy(context.device());
    }

    #[test]
    fn staging_accepts_exactly_the_right_number_of_bytes() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(context) = context() else {
            return;
        };
        let mut texture = Texture::new(&context, 4, 4).expect("creates");

        assert!(texture.stage(&[0_u8; 4 * 4 * 4]).is_ok());
        texture.destroy(context.device());
    }

    #[test]
    fn a_wrongly_sized_upload_is_refused_rather_than_read_past() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Reading past the end of the slice would be a buffer overrun, and
        // writing a short one would leave half a frame of the previous image.
        let Some(context) = context() else {
            return;
        };
        let mut texture = Texture::new(&context, 4, 4).expect("creates");

        assert!(texture.stage(&[0_u8; 8]).is_err(), "too short");
        assert!(texture.stage(&[0_u8; 1_000]).is_err(), "too long");
        texture.destroy(context.device());
    }

    #[test]
    fn many_textures_can_exist_at_once() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A show has a texture per active layer plus the composition. A leak
        // per texture would show up here.
        let Some(context) = context() else {
            return;
        };
        let mut textures: Vec<Texture> = (0..32)
            .map(|_| Texture::new(&context, 16, 16).expect("creates"))
            .collect();
        for texture in &mut textures {
            texture.destroy(context.device());
        }
    }

    #[test]
    fn the_white_pixel_is_the_identity_for_a_multiply() {
        assert_eq!(white_pixel(), [255, 255, 255, 255]);
    }
}
