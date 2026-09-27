//! An output whose picture goes on the network instead of out of a socket.
//!
//! Everything a wall does, and then one more step. It is a wrapper: the output
//! underneath composes the show's pieces into a canvas of its own size, exactly
//! as a wall does, and this reads that canvas back and hands it to a
//! [`Sender`]. So a stream is mapped, masked, test-patterned and previewed by
//! the same code as a wall, and the only thing this file adds is where the
//! finished picture goes.
//!
//! Which is the point of doing it this way. The owner's requirement was
//! "ansonsten sind alle Funktionen identisch", and a second composition path
//! written to be identical would drift apart from the first one the week after.
//!
//! # The readback is not optional here
//!
//! A preview is a tap for somebody watching, and the render loop switches it off
//! when nobody is. For a network output the readback *is* the output, so
//! [`set_readback`](OutputBackend::set_readback) is refused rather than
//! forwarded: an operator switching previews off must not switch a wall's feed
//! off with them.

use ovs_types::config::DisplayMode;
use ovs_types::geometry::SizePx;
use ovs_types::id::OutputId;

use super::library::Ndi;
use super::sender::{Picture, Sender, SenderReport};
use crate::backend::{BackendError, CompositionSource, FrameRequest, OutputBackend};
use crate::scene::{Scene, TextureKey};

/// An output published on the network.
pub struct NdiOutput {
    /// The canvas the pieces are composed into.
    inner: Box<dyn OutputBackend>,
    /// The show's identifier for this output, not the canvas's own.
    ///
    /// The canvas invents one when it is created, and pieces are assigned to
    /// outputs by the show's id. Handing back the canvas's would mean no piece
    /// ever matched and the stream would publish black.
    id: OutputId,
    /// What an operator calls it here.
    name: String,
    sender: Sender,
    /// The buffer the next readback goes into.
    ///
    /// Swapped with the sender rather than copied: `offer` takes this one and
    /// hands back whichever frame it displaced, so after the first two passes
    /// nothing on this path allocates and nothing copies a frame.
    spare: Option<Vec<u8>>,
    /// Whether the last pass got a picture out of the canvas.
    ///
    /// Said once when it changes. A canvas that cannot be read back is a stream
    /// publishing nothing, and the two states look identical from outside.
    reading: bool,
}

impl std::fmt::Debug for NdiOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NdiOutput")
            .field("name", &self.name)
            .field("stream", &self.sender.name())
            .finish()
    }
}

impl NdiOutput {
    /// Wraps a canvas and publishes what it draws.
    ///
    /// `stream` is the part in brackets: the runtime puts this machine's name in
    /// front of it.
    #[must_use]
    pub fn new(
        ndi: &'static Ndi,
        id: OutputId,
        name: &str,
        stream: &str,
        mut inner: Box<dyn OutputBackend>,
    ) -> Self {
        // Turned on once and never off again — see the note on the module.
        inner.set_readback(true);
        Self {
            inner,
            id,
            name: name.to_owned(),
            sender: Sender::start(ndi, stream),
            spare: None,
            reading: true,
        }
    }

    /// What this output is called on the network, before the machine name.
    #[must_use]
    pub fn stream(&self) -> &str {
        self.sender.name()
    }

    /// What the sender has been doing.
    #[must_use]
    pub fn report(&self) -> SenderReport {
        self.sender.report()
    }

    /// Reads the finished picture back and hands it over.
    fn publish(&mut self) {
        let size = self.inner.mode().size;
        let rate = self.inner.mode().refresh;
        let mut pixels = self.spare.take().unwrap_or_default();

        if !self.inner.snapshot(size, &mut pixels) {
            // Not a fault of the show's and nothing for the walls to care
            // about, so the loop is not told. Said once, because a canvas that
            // cannot be read is a stream that publishes nothing and there would
            // otherwise be nothing anywhere to connect the two.
            if self.reading {
                self.reading = false;
                tracing::warn!(
                    output = %self.name,
                    stream = %self.sender.name(),
                    "this output's canvas cannot be read back, so nothing is being published"
                );
            }
            self.spare = Some(pixels);
            return;
        }
        if !self.reading {
            self.reading = true;
            tracing::info!(
                output = %self.name,
                stream = %self.sender.name(),
                "this output's canvas can be read again"
            );
        }

        // Straight through as a fraction, because both ends keep it as one.
        // 60000/1001 stays 60000/1001: rounding it to a single number would
        // publish 60 for material that is not, and a receiver locking to that
        // would drift by a frame every half minute.
        self.spare = self.sender.offer(Picture {
            size,
            rate_numerator: rate.numerator,
            rate_denominator: rate.denominator,
            pixels,
        });
    }
}

impl OutputBackend for NdiOutput {
    fn id(&self) -> OutputId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn mode(&self) -> DisplayMode {
        self.inner.mode()
    }

    fn identity(&self) -> Option<ovs_types::config::OutputIdentity> {
        // Attached to nothing. A stream has no socket, no EDID and no cable, and
        // answering otherwise would put it in the planner's way.
        None
    }

    fn output_depth(&self) -> ovs_types::depth::OutputDepth {
        self.inner.output_depth()
    }

    fn composition_depth(&self) -> ovs_types::depth::CompositionDepth {
        self.inner.composition_depth()
    }

    fn present(&mut self, frame: FrameRequest, scene: &Scene) -> Result<(), BackendError> {
        // Composed first and published second. A pass that could not draw
        // publishes nothing rather than the frame before it drawn again: the
        // canvas still holds the last good picture, and sending it again would
        // tell a receiver that a stalled output is running.
        self.inner.present(frame, scene)?;
        self.publish();
        Ok(())
    }

    fn just_started(&mut self) -> bool {
        self.inner.just_started()
    }

    fn snapshot(&self, size: SizePx, into: &mut Vec<u8>) -> bool {
        // A network output has previews like any other: an operator watching
        // this output in a client is watching the same canvas the stream is
        // published from, which is the honest answer to "what is it sending".
        self.inner.snapshot(size, into)
    }

    fn bind_composition(&mut self, source: CompositionSource) -> Result<(), BackendError> {
        self.inner.bind_composition(source)
    }

    fn upload_texture(
        &mut self,
        key: TextureKey,
        image: &crate::media::DecodedImage,
    ) -> Result<(), BackendError> {
        self.inner.upload_texture(key, image)
    }

    fn drop_texture(&mut self, key: TextureKey) -> bool {
        self.inner.drop_texture(key)
    }

    fn published_as(&self) -> Option<String> {
        // What the runtime says the network calls it, which is `None` for the
        // moment between this output being made and the sender being created.
        self.sender.announced()
    }

    fn set_readback(&mut self, _wanted: bool) {
        // Refused, not forwarded. See the note on the module: for this output the
        // readback is the output, and previews being switched off must not take
        // a wall's feed with them.
    }
}
