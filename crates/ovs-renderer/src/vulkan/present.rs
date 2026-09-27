//! Putting a frame on a physical connector.
//!
//! The production output path. The renderer takes a
//! display in direct mode, sets the mode a show asked for on it, and presents
//! to it. There is no compositor: a compositor would insert a buffer between the
//! renderer and the panel, take the frame timing away and make the dropped-frame
//! count a guess rather than a measurement.
//!
//! # How a frame gets there
//!
//! The pieces are composed exactly as they always were — into
//! [`VulkanOffscreenBackend`]'s image, through the pipelines and render pass
//! that every measurement so far has been taken through — and the finished image
//! is blitted into the swapchain image and presented.
//!
//! That blit is a deliberate extra step. Rendering the pieces straight into the
//! swapchain image would save it, and is where this should end up; doing it that
//! way *first* would have meant a second copy of the compositing path, live on a
//! wall, with nothing to compare it against. This way the picture on the panel is
//! the picture the offscreen path produces, by construction, and the blit is one
//! full-screen copy — well under a millisecond at 2560x1440 on any GPU this
//! product runs on. It also scales, which is what lets a show authored at
//! 1920x1080 drive a panel that offers only 2560x1440.
//!
//! # What happens when a frame does not reach the panel
//!
//! A present that fails is a dropped frame and the loop carries on; the show
//! does not stop because one frame did not make it (house rule §17). A swapchain
//! that has gone out of date is rebuilt on the next pass. Only a lost device or
//! a lost surface takes the output down, and those are reported by name.

use std::sync::Arc;

use ash::vk;
use ovs_types::config::{DisplayMode, OutputIdentity};
use ovs_types::geometry::SizePx;
use ovs_types::id::OutputId;

use crate::backend::{BackendError, CompositionSource, FrameRequest, OutputBackend};
use crate::scene::Scene;

use super::display::Display;
use super::offscreen::VulkanOffscreenBackend;
use super::{VulkanContext, VulkanError};

/// How many frames may be in flight at once.
///
/// Two: one being presented while the next is recorded. A third would add a
/// frame of latency for a buffer nothing is waiting on, and latency on a media
/// server is the thing being minimised.
const FRAMES_IN_FLIGHT: usize = 2;

/// Everything one in-flight frame needs of its own.
struct FrameSync {
    /// Signalled when the swapchain image is ready to be written.
    acquired: vk::Semaphore,
    /// Signalled when this frame's blit is done and it may be presented.
    ready: vk::Semaphore,
    /// Signalled when the GPU is finished with this frame's command buffer.
    in_flight: vk::Fence,
    command_buffer: vk::CommandBuffer,
}

/// One physical output, driven directly.
pub struct VulkanDisplayBackend {
    /// Composes this output's pieces. The picture that reaches the panel is the
    /// one this produces, which is the point of composing through it.
    canvas: VulkanOffscreenBackend,
    context: Arc<VulkanContext>,
    surface_instance: ash::khr::surface::Instance,
    swapchain_device: ash::khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    extent: vk::Extent2D,
    /// Whether the offered surface formats have been logged for this output.
    said_what_was_offered: bool,
    /// The format the swapchain was actually created with.
    ///
    /// Kept so this output can say how many bits leave it without asking the
    /// driver again, and so the answer is the format the driver *gave* rather
    /// than the one that was preferred. They differ: `choose_format` asks for
    /// eight-bit BGRA and takes the first offered format when it cannot have
    /// it, and that first one is whatever the panel and driver agreed on.
    format: vk::Format,
    command_pool: vk::CommandPool,
    frames: Vec<FrameSync>,
    /// Which frame slot the next present uses.
    next: usize,
    /// True when the swapchain must be rebuilt before the next present.
    stale: bool,
    /// The connector this output is forced onto.
    connector: String,
    /// The display this output drives, so it can be given back to the kernel.
    display: vk::DisplayKHR,
    /// The display plane this output was given.
    ///
    /// **Handed back when this is dropped.** The context keeps a ledger of which
    /// planes it has given out, because the driver's own `current_display` only
    /// says so once a plane is actually driving one — and two outputs built a
    /// second apart both read every plane as free, take the same one, and put
    /// two pictures on one panel with the other dark. Not giving it back would
    /// use the card's supply up one rebuild at a time.
    plane: u32,
    /// What is plugged into it, when the driver says. Read, never matched on.
    model: Option<String>,
    gpu_pci_address: Option<String>,
    /// Whether the "this panel is not the size the show asked for" line has
    /// been said. Once per output, not once per frame.
    warned_about_scaling: bool,
    /// How many frames have reached the panel.
    ///
    /// Only the first two matter: the first queues while the mode is still being
    /// set, and the second returns once the display is actually scanning out.
    /// That second one is when this output has started.
    presented: u8,
}

impl VulkanDisplayBackend {
    /// Takes a display and prepares to present to it.
    ///
    /// `mode` is what the show asked for; `display_mode` is the mode actually
    /// set on the panel. They differ when a panel does not offer what the
    /// show wanted, in which case the frame is scaled and this says so once.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the surface or the swapchain cannot be made.
    pub fn on(
        context: Arc<VulkanContext>,
        name: impl Into<String>,
        display: &Display,
        display_mode: super::display::DisplayMode,
        mode: DisplayMode,
    ) -> Result<Self, VulkanError> {
        let plane = context.display_plane_for(display.handle)?;
        Self::on_plane(context, name, display, display_mode, mode, plane)
    }

    /// The same, on a plane the caller names.
    ///
    /// **Split out so the plane can be asked about.** A display is driven
    /// through a plane, the driver's account of which plane can drive which
    /// display is not always true, and until these were two functions there was
    /// no way to try one and see. `ovs-planes` is what that is for.
    ///
    /// The plane is this output's until it is dropped — see the ledger on
    /// [`VulkanContext`] — so a caller that names one itself must have taken it
    /// from [`VulkanContext::display_plane_for`] or be prepared for two outputs
    /// on one plane, which puts two pictures on one panel.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the surface or the swapchain cannot be made.
    pub fn on_plane(
        context: Arc<VulkanContext>,
        name: impl Into<String>,
        display: &Display,
        display_mode: super::display::DisplayMode,
        mode: DisplayMode,
        plane: u32,
    ) -> Result<Self, VulkanError> {
        let name = name.into();
        let canvas = VulkanOffscreenBackend::on(Arc::clone(&context), name.clone(), mode)?;

        // Which card this socket is on. A show names an output by **both**
        // halves, so an output that reports only its connector
        // matches nothing a show ever wrote down: the panel was driven, at
        // the mode the show asked for, while the show's own output sat in
        // `missing_outputs` and its pieces stayed dark. Read from the context
        // rather than passed in, because the context is the thing that knows.
        let gpu_pci_address = context.summary().pci_address.clone();

        let surface_instance =
            ash::khr::surface::Instance::new(context.entry(), context.instance());
        let swapchain_device =
            ash::khr::swapchain::Device::new(context.instance(), context.device());

        // **Taken from the kernel before anything is created for it.** A surface
        // made for a display the kernel still owns is undefined, and on the
        // driver this was written against it lands on whichever display the
        // driver already holds — which is why one output always worked and two
        // never did.
        context.acquire_display(display.handle);

        // From here the plane is this output's, and every failure below has to
        // give it back: a connector that refused a mode would otherwise take a
        // plane with it, and the next attempt would have one fewer to try.
        let surface = match create_surface(&context, display_mode.handle, plane, display_mode.size)
        {
            Ok(surface) => surface,
            Err(error) => {
                context.release_display_plane(plane);
                return Err(error);
            }
        };

        let mut backend = Self {
            canvas,
            context,
            surface_instance,
            swapchain_device,
            surface,
            swapchain: vk::SwapchainKHR::null(),
            images: Vec::new(),
            extent: vk::Extent2D {
                width: display_mode.size.width,
                height: display_mode.size.height,
            },
            command_pool: vk::CommandPool::null(),
            frames: Vec::new(),
            next: 0,
            stale: false,
            connector: display.connector.clone(),
            display: display.handle,
            plane,
            model: display.model.clone(),
            gpu_pci_address,
            warned_about_scaling: false,
            said_what_was_offered: false,
            // Replaced by `create_swapchain` before anything is presented.
            // `UNDEFINED` reports as an unknown depth, which is the honest
            // answer for an output that has not been brought up yet.
            format: vk::Format::UNDEFINED,
            presented: 0,
        };

        // Both may fail, and both leave the plane this output was given in the
        // ledger unless it is handed back here: `Drop` does not run on a value
        // that was never returned.
        if let Err(error) = backend
            .create_command_pool()
            .and_then(|()| backend.create_swapchain())
        {
            backend.context.release_display_plane(backend.plane);
            return Err(error);
        }

        tracing::info!(
            output = %name,
            connector = %backend.connector,
            width = display_mode.size.width,
            height = display_mode.size.height,
            refresh_mhz = display_mode.refresh_millihertz,
            images = backend.images.len(),
            // Which plane of the card this output drives through. Two outputs
            // sharing one put both pictures on one panel and leave the other
            // dark, and nothing else in a log would say so.
            plane = backend.plane,
            // What the *surface* came back as, against what was asked for. Every
            // other field here is a request; this is the answer.
            surface_width = backend.extent.width,
            surface_height = backend.extent.height,
            "presenting to a physical display"
        );

        Ok(backend)
    }

    /// Records and submits the blit that puts this frame on the panel.
    fn show(&mut self) -> Result<(), VulkanError> {
        if self.stale {
            self.rebuild_swapchain()?;
        }

        let device = self.context.device();
        let slot = self.next;
        self.next = (self.next + 1) % FRAMES_IN_FLIGHT;
        let frame = &self.frames[slot];

        // SAFETY: the fence belongs to this device and is signalled by the
        // submission below, so waiting on it is what makes reusing this slot's
        // command buffer safe.
        unsafe {
            device
                .wait_for_fences(&[frame.in_flight], true, u64::MAX)
                .map_err(call("vkWaitForFences"))?;
        }

        // SAFETY: as above.
        let acquired = unsafe {
            self.swapchain_device.acquire_next_image(
                self.swapchain,
                u64::MAX,
                frame.acquired,
                vk::Fence::null(),
            )
        };

        let index = match acquired {
            Ok((index, suboptimal)) => {
                // Suboptimal still presents. Rebuilding on the next pass keeps
                // this frame on the wall rather than skipping it to be tidy.
                self.stale = suboptimal;
                index
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.stale = true;
                return Err(VulkanError::Call {
                    operation: "vkAcquireNextImageKHR",
                    result: vk::Result::ERROR_OUT_OF_DATE_KHR,
                });
            }
            Err(result) => {
                return Err(VulkanError::Call {
                    operation: "vkAcquireNextImageKHR",
                    result,
                })
            }
        };

        // SAFETY: the fence is not in use once it has been waited on.
        unsafe {
            device
                .reset_fences(&[frame.in_flight])
                .map_err(call("vkResetFences"))?;
        }

        self.record_blit(frame.command_buffer, self.images[index as usize])?;

        let wait = [frame.acquired];
        // The blit writes the swapchain image, so it is the transfer stage that
        // must wait for the image to be available — not the top of the pipe.
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let signal = [frame.ready];
        let buffers = [frame.command_buffer];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&wait)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&buffers)
            .signal_semaphores(&signal);

        // SAFETY: every handle above belongs to this device and outlives the
        // call; the fence tells the next use of this slot when it is finished.
        unsafe {
            device
                .queue_submit(self.context.graphics_queue(), &[submit], frame.in_flight)
                .map_err(call("vkQueueSubmit"))?;
        }

        let swapchains = [self.swapchain];
        let indices = [index];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&signal)
            .swapchains(&swapchains)
            .image_indices(&indices);

        // SAFETY: as above.
        match unsafe {
            self.swapchain_device
                .queue_present(self.context.graphics_queue(), &present)
        } {
            Ok(_) => Ok(()),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.stale = true;
                Ok(())
            }
            Err(result) => Err(VulkanError::Call {
                operation: "vkQueuePresentKHR",
                result,
            }),
        }
    }

    /// Records the copy from the composed canvas into a swapchain image.
    fn record_blit(
        &self,
        command_buffer: vk::CommandBuffer,
        target: vk::Image,
    ) -> Result<(), VulkanError> {
        let device = self.context.device();
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        // SAFETY: this buffer's previous submission has completed — the fence
        // above says so — so it may be reset and recorded.
        unsafe {
            device
                .begin_command_buffer(command_buffer, &begin)
                .map_err(call("vkBeginCommandBuffer"))?;
        }

        let whole = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);

        // Whatever the swapchain image held is being overwritten, so its old
        // contents are not worth preserving: UNDEFINED is both correct and the
        // cheapest transition a driver can make.
        let to_transfer = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .image(target)
            .subresource_range(whole);

        let canvas = self.canvas.image();
        let source = self.canvas.mode().size;
        let offsets = |width: i32, height: i32| {
            [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: width,
                    y: height,
                    z: 1,
                },
            ]
        };

        let layers = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .layer_count(1);
        let region = vk::ImageBlit::default()
            .src_subresource(layers)
            .src_offsets(offsets(
                i32::try_from(source.width).unwrap_or(i32::MAX),
                i32::try_from(source.height).unwrap_or(i32::MAX),
            ))
            .dst_subresource(layers)
            .dst_offsets(offsets(
                i32::try_from(self.extent.width).unwrap_or(i32::MAX),
                i32::try_from(self.extent.height).unwrap_or(i32::MAX),
            ));

        let to_present = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::empty())
            .image(target)
            .subresource_range(whole);

        // SAFETY: the command buffer is recording, and every handle belongs to
        // this device. The canvas image is left in TRANSFER_SRC_OPTIMAL by its
        // own render pass, which is why no barrier is needed for it.
        unsafe {
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            device.cmd_blit_image(
                command_buffer,
                canvas,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                target,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
                // Linear, because this scales when a panel does not offer the
                // mode a show asked for. Nearest would alias a wall.
                vk::Filter::LINEAR,
            );
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            device
                .end_command_buffer(command_buffer)
                .map_err(call("vkEndCommandBuffer"))?;
        }

        Ok(())
    }

    fn create_command_pool(&mut self) -> Result<(), VulkanError> {
        let device = self.context.device();
        let create = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(self.context.graphics_family());

        // SAFETY: the family index came from this device's own selection.
        self.command_pool = unsafe { device.create_command_pool(&create, None) }
            .map_err(call("vkCreateCommandPool"))?;

        let allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(u32::try_from(FRAMES_IN_FLIGHT).unwrap_or(2));

        // SAFETY: the pool was just created on this device.
        let buffers = unsafe { device.allocate_command_buffers(&allocate) }
            .map_err(call("vkAllocateCommandBuffers"))?;

        for command_buffer in buffers {
            let semaphore = vk::SemaphoreCreateInfo::default();
            // Created signalled: the first frame has nothing to wait for, and
            // an unsignalled fence would hang it.
            let fence = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);

            // SAFETY: plain creation on this device.
            unsafe {
                self.frames.push(FrameSync {
                    acquired: device
                        .create_semaphore(&semaphore, None)
                        .map_err(call("vkCreateSemaphore"))?,
                    ready: device
                        .create_semaphore(&semaphore, None)
                        .map_err(call("vkCreateSemaphore"))?,
                    in_flight: device
                        .create_fence(&fence, None)
                        .map_err(call("vkCreateFence"))?,
                    command_buffer,
                });
            }
        }

        Ok(())
    }

    fn create_swapchain(&mut self) -> Result<(), VulkanError> {
        // SAFETY: the surface and the physical device belong to this instance.
        let capabilities = unsafe {
            self.surface_instance
                .get_physical_device_surface_capabilities(
                    self.context.physical_device(),
                    self.surface,
                )
        }
        .map_err(call("vkGetPhysicalDeviceSurfaceCapabilitiesKHR"))?;

        // SAFETY: as above.
        let formats = unsafe {
            self.surface_instance
                .get_physical_device_surface_formats(self.context.physical_device(), self.surface)
        }
        .map_err(call("vkGetPhysicalDeviceSurfaceFormatsKHR"))?;

        let format = choose_format(&formats);
        self.format = format.format;

        // Once an output, not once a rebuild. Whether a panel offers a ten-bit
        // surface format at all is the question that decides whether a real
        // ten-bit physical output is reachable on a given machine, and it is not
        // answerable from anywhere else: `vulkaninfo` over SSH has no display to
        // enumerate, and the driver only tells the process holding the surface.
        // So it goes in the log at bring-up, where somebody asking can read it.
        if !self.said_what_was_offered {
            self.said_what_was_offered = true;
            let ten_bit = formats
                .iter()
                .any(|offered| depth_of(offered.format) == ovs_types::depth::OutputDepth::Ten);
            tracing::info!(
                output = %self.canvas.name(),
                offered = formats.len(),
                ten_bit_offered = ten_bit,
                taken = ?format.format,
                depth = ?depth_of(format.format),
                "surface formats"
            );
        }

        if capabilities.current_extent.width != u32::MAX {
            self.extent = capabilities.current_extent;
        }

        // One more than the minimum, so the renderer is not waiting on the
        // display for an image the moment it finishes a frame — capped at what
        // the driver allows, where zero means no maximum.
        let mut count = capabilities.min_image_count + 1;
        if capabilities.max_image_count > 0 && count > capabilities.max_image_count {
            count = capabilities.max_image_count;
        }

        let create = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(self.extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            // FIFO and nothing else. It is the only mode Vulkan guarantees, and
            // it is the one this product wants: every frame presented, on the
            // vertical blank, which is what makes the dropped-frame count mean
            // something.
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(self.swapchain);

        // SAFETY: `create` borrows nothing that does not outlive the call.
        let swapchain = unsafe { self.swapchain_device.create_swapchain(&create, None) }
            .map_err(call("vkCreateSwapchainKHR"))?;

        // The old one is retired by the creation above and must be destroyed
        // after it, not before: the driver may still be reading it.
        if self.swapchain != vk::SwapchainKHR::null() {
            // SAFETY: no frame referencing it is still in flight — the caller
            // waits for idle before rebuilding.
            unsafe {
                self.swapchain_device
                    .destroy_swapchain(self.swapchain, None);
            }
        }
        self.swapchain = swapchain;

        // SAFETY: the swapchain was just created.
        self.images = unsafe { self.swapchain_device.get_swapchain_images(self.swapchain) }
            .map_err(call("vkGetSwapchainImagesKHR"))?;

        Ok(())
    }

    fn rebuild_swapchain(&mut self) -> Result<(), VulkanError> {
        // SAFETY: waiting for the device is what makes destroying the old
        // swapchain safe.
        unsafe {
            let _ = self.context.device().device_wait_idle();
        }
        self.create_swapchain()?;
        self.stale = false;
        tracing::info!(output = %self.canvas.name(), "swapchain rebuilt");
        Ok(())
    }

    /// Says once when the panel is not the size the show asked for.
    fn warn_about_scaling(&mut self) {
        if self.warned_about_scaling {
            return;
        }
        self.warned_about_scaling = true;

        let source = self.canvas.mode().size;
        if source.width == self.extent.width && source.height == self.extent.height {
            return;
        }

        tracing::warn!(
            output = %self.canvas.name(),
            connector = %self.connector,
            show = format!("{}x{}", source.width, source.height),
            panel = format!("{}x{}", self.extent.width, self.extent.height),
            "the panel does not offer the mode this output asks for; the frame is being scaled"
        );
    }
}

/// Creates a surface on one plane of one display mode.
fn create_surface(
    context: &VulkanContext,
    mode: vk::DisplayModeKHR,
    plane: u32,
    size: SizePx,
) -> Result<vk::SurfaceKHR, VulkanError> {
    let display_instance = ash::khr::display::Instance::new(context.entry(), context.instance());

    let create = vk::DisplaySurfaceCreateInfoKHR::default()
        .display_mode(mode)
        .plane_index(plane)
        .plane_stack_index(0)
        .transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
        .global_alpha(1.0)
        .alpha_mode(vk::DisplayPlaneAlphaFlagsKHR::OPAQUE)
        .image_extent(vk::Extent2D {
            width: size.width,
            height: size.height,
        });

    // SAFETY: the mode and plane came from this device's enumeration.
    unsafe { display_instance.create_display_plane_surface(&create, None) }
        .map_err(call("vkCreateDisplayPlaneSurfaceKHR"))
}

/// Picks the surface format to present in.
///
/// `B8G8R8A8_UNORM` where it exists, because it is what the compositor's image
/// blits into without a conversion, and the first offered otherwise. A driver
/// answering with `UNDEFINED` means "anything", which is the one case where
/// choosing is this code's business.
fn choose_format(formats: &[vk::SurfaceFormatKHR]) -> vk::SurfaceFormatKHR {
    const PREFERRED: [vk::Format; 2] = [vk::Format::B8G8R8A8_UNORM, vk::Format::R8G8B8A8_UNORM];

    if formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED {
        return vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_UNORM,
            color_space: formats[0].color_space,
        };
    }

    for wanted in PREFERRED {
        if let Some(found) = formats.iter().find(|format| format.format == wanted) {
            return *found;
        }
    }

    formats
        .first()
        .copied()
        .unwrap_or(vk::SurfaceFormatKHR::default())
}

/// How many bits a channel of `format` carries on the cable.
///
/// Matched rather than worked out arithmetically, because the question is not
/// how wide the pixel is: `A2B10G10R10` and `R8G8B8A8` are both thirty-two bits
/// and one of them is a ten-bit output. A format this does not recognise is
/// `Unknown` rather than eight — an unfamiliar format is exactly the case where
/// a guess would be believed and wrong.
fn depth_of(format: vk::Format) -> ovs_types::depth::OutputDepth {
    use ovs_types::depth::OutputDepth;
    match format {
        vk::Format::B8G8R8A8_UNORM
        | vk::Format::B8G8R8A8_SRGB
        | vk::Format::R8G8B8A8_UNORM
        | vk::Format::R8G8B8A8_SRGB => OutputDepth::Eight,
        vk::Format::A2B10G10R10_UNORM_PACK32
        | vk::Format::A2R10G10B10_UNORM_PACK32
        | vk::Format::R16G16B16A16_UNORM
        | vk::Format::R16G16B16A16_SFLOAT => OutputDepth::Ten,
        _ => OutputDepth::Unknown,
    }
}

/// Turns a Vulkan result into an error that names the call it came from.
fn call(operation: &'static str) -> impl Fn(vk::Result) -> VulkanError {
    move |result| VulkanError::Call { operation, result }
}

impl OutputBackend for VulkanDisplayBackend {
    fn id(&self) -> OutputId {
        self.canvas.id()
    }

    fn output_depth(&self) -> ovs_types::depth::OutputDepth {
        depth_of(self.format)
    }

    fn composition_depth(&self) -> ovs_types::depth::CompositionDepth {
        crate::backend::Compositor::composition_depth(&self.canvas)
    }

    fn upload_texture(
        &mut self,
        key: crate::scene::TextureKey,
        image: &crate::media::DecodedImage,
    ) -> Result<(), crate::backend::BackendError> {
        // Onto this output's own canvas, which is the one it composes into.
        // Only the per-output test pattern arrives this way: everything else an
        // output draws comes from the composition it is bound to.
        crate::backend::Compositor::upload_texture(&mut self.canvas, key, image)
    }

    fn drop_texture(&mut self, key: crate::scene::TextureKey) -> bool {
        crate::backend::Compositor::drop_texture(&mut self.canvas, key)
    }

    fn set_readback(&mut self, wanted: bool) {
        crate::backend::Compositor::set_readback(&mut self.canvas, wanted);
    }

    fn name(&self) -> &str {
        self.canvas.name()
    }

    fn just_started(&mut self) -> bool {
        // The second present is the first one that waited for the display: the
        // mode is set, the panel is scanning out, and from here a missed
        // interval is a missed frame rather than hardware coming up.
        const STARTED_AFTER: u8 = 2;

        if self.presented == STARTED_AFTER {
            self.presented = self.presented.saturating_add(1);
            return true;
        }
        false
    }

    fn mode(&self) -> DisplayMode {
        self.canvas.mode()
    }

    fn identity(&self) -> Option<OutputIdentity> {
        // What this output is: the socket it is forced onto. The
        // EDID fields stay empty here — the display extensions do not offer
        // them, and nothing matches on them any more.
        Some(OutputIdentity {
            gpu_pci_address: self.gpu_pci_address.clone(),
            connector: Some(self.connector.clone()),
            // Carried so that a console can name the panel an engineer is
            // standing in front of. `match_against` ignores it, and must:
            // a monitor swapped for a spare is the same output.
            model: self.model.clone(),
            ..OutputIdentity::default()
        })
    }

    fn present(&mut self, frame: FrameRequest, scene: &Scene) -> Result<(), BackendError> {
        // Compose first, into the image every measurement so far was taken
        // through, and then put that image on the panel.
        self.canvas.present(frame, scene)?;
        self.warn_about_scaling();

        let shown = self.show();
        if shown.is_ok() && self.presented < 2 {
            self.presented += 1;
        }

        shown.map_err(|error| {
            let permanent = matches!(
                &error,
                VulkanError::Call { result, .. }
                    if *result == vk::Result::ERROR_DEVICE_LOST
                        || *result == vk::Result::ERROR_SURFACE_LOST_KHR
            );
            if permanent {
                BackendError::OutputLost {
                    name: self.canvas.name().to_owned(),
                    reason: error.to_string(),
                }
            } else {
                BackendError::PresentFailed {
                    name: self.canvas.name().to_owned(),
                    reason: error.to_string(),
                }
            }
        })
    }

    fn snapshot(&self, size: SizePx, into: &mut Vec<u8>) -> bool {
        // From the composed image rather than from the swapchain: it is the same
        // picture, it is already in a layout that can be read, and reading a
        // swapchain image would mean synchronising with the display.
        self.canvas.snapshot(size, into)
    }

    fn bind_composition(&mut self, source: CompositionSource) -> Result<(), BackendError> {
        self.canvas.bind_composition(source)
    }
}

impl Drop for VulkanDisplayBackend {
    fn drop(&mut self) {
        let device = self.context.device();

        // SAFETY: nothing may be destroyed while the GPU is still using it, and
        // this is the only thing that can say when that is true.
        unsafe {
            let _ = device.device_wait_idle();

            for frame in &self.frames {
                device.destroy_semaphore(frame.acquired, None);
                device.destroy_semaphore(frame.ready, None);
                device.destroy_fence(frame.in_flight, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                device.destroy_command_pool(self.command_pool, None);
            }
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_device
                    .destroy_swapchain(self.swapchain, None);
            }
            if self.surface != vk::SurfaceKHR::null() {
                self.surface_instance.destroy_surface(self.surface, None);
            }
        }

        // **After the surface is gone, never before.** The plane is free for
        // another output only once nothing on this one is still using it, and
        // the ledger is what the next output reads.
        self.context.release_display_plane(self.plane);

        // And the display itself goes back to the kernel, which is what lets
        // this connector be taken again at another mode — or by something else
        // entirely.
        self.context.release_display(self.display);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(format: vk::Format) -> vk::SurfaceFormatKHR {
        vk::SurfaceFormatKHR {
            format,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        }
    }

    #[test]
    fn the_preferred_format_is_taken_when_it_is_offered() {
        let offered = [
            format(vk::Format::R5G6B5_UNORM_PACK16),
            format(vk::Format::B8G8R8A8_UNORM),
        ];
        assert_eq!(choose_format(&offered).format, vk::Format::B8G8R8A8_UNORM);
    }

    #[test]
    fn the_second_choice_is_taken_when_the_first_is_not_offered() {
        let offered = [
            format(vk::Format::R5G6B5_UNORM_PACK16),
            format(vk::Format::R8G8B8A8_UNORM),
        ];
        assert_eq!(choose_format(&offered).format, vk::Format::R8G8B8A8_UNORM);
    }

    #[test]
    fn a_driver_that_offers_nothing_in_particular_is_given_one() {
        // A single UNDEFINED entry is the driver saying "anything you like",
        // which is the one case where this code has to decide.
        let offered = [format(vk::Format::UNDEFINED)];
        assert_eq!(choose_format(&offered).format, vk::Format::B8G8R8A8_UNORM);
    }

    #[test]
    fn something_is_always_chosen_rather_than_nothing() {
        // A panel offering only one odd format is still a panel, and refusing
        // it would leave a wall dark over a preference.
        let offered = [format(vk::Format::R5G6B5_UNORM_PACK16)];
        assert_eq!(
            choose_format(&offered).format,
            vk::Format::R5G6B5_UNORM_PACK16
        );
    }
}
