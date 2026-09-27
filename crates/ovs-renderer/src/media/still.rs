//! Decoding a still image into pixels a texture can be made from.

use std::path::{Path, PathBuf};

use ovs_types::geometry::SizePx;

/// The largest still the renderer will decode, per side.
///
/// 16384 is the smallest maximum image dimension Vulkan requires of a
/// conformant implementation, so a larger image could not become a texture on
/// some device the server might be deployed to even where it decodes. Refusing
/// it here means one clear message instead of a device-dependent failure at
/// upload time.
pub const MAX_IMAGE_SIDE: u32 = 16_384;

/// The colour planes of a picture that is not RGBA.
///
/// Cb and Cr, tightly packed, each `size` divided by the subsampling factors.
/// The luma plane is the image's own [`pixels`](DecodedImage::pixels).
#[derive(Clone, PartialEq, Eq)]
pub struct Chroma {
    cb: Vec<u8>,
    cr: Vec<u8>,
    format: ChromaFormat,
}

/// How many bits a sample of a planar picture carries.
///
/// Two paths, and the owner's instruction of 2026-09-23 is that they stay two:
/// eight-bit material takes the eight-bit path and ten-bit material takes the
/// ten-bit one, with neither converted into the other on the processor. What
/// converges is the shader, not the memory.
///
/// # How ten bits reach the eight-bit arithmetic exactly
///
/// A ten-bit plane is uploaded as `R16_UNORM` and sampled as a fraction of
/// 65535, so the shader is handed a number in the wrong domain and multiplies
/// it by [`sample_scale`](Self::sample_scale) to put it in the right one.
///
/// The scale is `65535 / 1020` rather than the `65535 / 1023` a reader would
/// expect, and the three is the whole point. Narrow-range video is 16 to 235
/// at eight bits and 64 to 940 at ten — exactly four times — so dividing by
/// 1020 lands a ten-bit sample on precisely the eight-bit value it means, and
/// every range and matrix constant in the shader stays as it was. Dividing by
/// 1023 would be a third of an eight-bit level out at every level, which is
/// invisible and wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SampleDepth {
    /// Eight bits a sample, one byte a plane element.
    #[default]
    Eight,
    /// Ten bits a sample in the low bits of a sixteen-bit element.
    ///
    /// What a software decoder produces: `yuv420p10le`.
    TenLow,
    /// Ten bits a sample in the high bits of a sixteen-bit element.
    ///
    /// What a card produces: P010, which NVDEC hands back for HEVC Main 10
    /// and which has its ten bits at the top with six zeroes under them.
    TenHigh,
}

impl SampleDepth {
    /// How many bytes one sample takes.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::Eight => 1,
            Self::TenLow | Self::TenHigh => 2,
        }
    }

    /// What the shader multiplies a sample by to reach the eight-bit domain.
    #[must_use]
    pub fn sample_scale(self) -> f32 {
        match self {
            Self::Eight => 1.0,
            // 65535/1020: a sixteen-bit sampler reading ten bits in the low
            // end, brought onto the eight-bit scale exactly.
            Self::TenLow => 65535.0 / 1020.0,
            // The same, for ten bits sitting at the top of the sixteen.
            Self::TenHigh => 65535.0 / (1020.0 * 64.0),
        }
    }

    /// What a stored element is divided by to reach the eight-bit domain.
    ///
    /// The other end of [`Self::sample_scale`], for the readers that are not a
    /// shader. A sampler hands the shader a fraction of its own full scale and
    /// the shader multiplies; anything reading a plane on the processor has the
    /// raw number and divides once. Derived from `sample_scale` rather than
    /// written out again, so the two cannot drift apart — and they must not:
    /// [`DecodedImage::pixel`] is what the tests believe and the shader is what
    /// the wall shows.
    #[must_use]
    pub fn divisor(self) -> f32 {
        match self {
            Self::Eight => 255.0,
            Self::TenLow | Self::TenHigh => 65535.0 / self.sample_scale(),
        }
    }

    /// Whether this is one of the ten-bit layouts.
    #[must_use]
    pub const fn is_ten_bit(self) -> bool {
        matches!(self, Self::TenLow | Self::TenHigh)
    }
}

/// How a picture's colour planes are arranged and what they mean.
///
/// Four small facts that only make sense together: how far the colour planes
/// are subsampled, whether the samples use the whole range, and which matrix
/// turns them back into red, green and blue. They travel as one value because
/// taking any one of them from a different file than the others would be a
/// picture drawn in the wrong colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromaFormat {
    /// How many luma samples across share one chroma sample. 1 or 2.
    pub subsample_x: u32,
    /// How many luma samples down share one chroma sample. 1 or 2.
    pub subsample_y: u32,
    /// Whether the samples use the whole 0-255 range rather than 16-235.
    ///
    /// Video is nearly always the narrow one, and a file that says otherwise —
    /// most things off a phone — shown as though it were narrow is visibly
    /// washed out. It is one multiply in the shader and it has to be right.
    pub full_range: bool,
    /// Which coefficients turn the samples back into colour.
    pub matrix: ColourMatrix,
    /// How many bits a sample carries, and where in its element they sit.
    pub depth: SampleDepth,
}

/// Which set of coefficients turns Y, Cb and Cr back into red, green and blue.
///
/// Getting this wrong is not subtle at the ends of the scale: a BT.601 clip
/// shown as BT.709 has visibly wrong greens and skin tones, and it is the sort
/// of wrong that looks like a grade rather than like a fault.
///
/// Standard definition and a lot of older camera material is BT.601;
/// everything HD is BT.709; and BT.2020 arrived with Main 10, which most
/// ten-bit material is tagged as.
///
/// # What BT.2020 means here, and what it does not
///
/// **The coefficients, and only the coefficients.** BT.2020 is three things at
/// once — a matrix, a set of primaries and a transfer function — and this is
/// the first of them. Decoding BT.2020 samples with BT.709 numbers is a hue
/// error of several degrees in saturated reds and greens, and that is what this
/// fixes. The wider primaries are *not* converted to the wall's, and a PQ or
/// HLG transfer is not tone-mapped: material graded for a thousand nits still
/// looks like material graded for a thousand nits. That belongs to a colour
/// pipeline this product does not have, and claiming it here by naming the
/// standard would be exactly the kind of half-support that is worse than none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourMatrix {
    /// HD and everything after it.
    Bt709,
    /// Standard definition, and older material that kept its convention.
    Bt601,
    /// BT.2020 non-constant luminance, which is what Main 10 usually carries.
    ///
    /// The matrix only — see the note on the type.
    Bt2020,
}

impl Chroma {
    /// The Cb plane, tightly packed, row by row from the top.
    #[must_use]
    pub fn cb(&self) -> &[u8] {
        &self.cb
    }

    /// The Cr plane, tightly packed, row by row from the top.
    #[must_use]
    pub fn cr(&self) -> &[u8] {
        &self.cr
    }

    /// How these planes are arranged and what they mean.
    #[must_use]
    pub const fn format(&self) -> ChromaFormat {
        self.format
    }

    /// How many bits a sample carries, and where in its element they sit.
    #[must_use]
    pub const fn depth(&self) -> SampleDepth {
        self.format.depth
    }

    /// How many luma samples across share one chroma sample. 1 or 2.
    #[must_use]
    pub const fn subsample_x(&self) -> u32 {
        self.format.subsample_x
    }

    /// How many luma samples down share one chroma sample. 1 or 2.
    #[must_use]
    pub const fn subsample_y(&self) -> u32 {
        self.format.subsample_y
    }

    /// Whether the samples use the whole 0-255 range rather than 16-235.
    #[must_use]
    pub const fn full_range(&self) -> bool {
        self.format.full_range
    }

    /// Which coefficients turn these samples back into colour.
    #[must_use]
    pub const fn matrix(&self) -> ColourMatrix {
        self.format.matrix
    }

    /// The size of each chroma plane, for a picture of `size`.
    ///
    /// Rounded up, which is what every decoder does: a 1921-wide 4:2:0 picture
    /// has 961 chroma samples across, not 960.
    #[must_use]
    pub const fn plane_size(&self, size: SizePx) -> SizePx {
        SizePx::new(
            size.width.div_ceil(self.format.subsample_x),
            size.height.div_ceil(self.format.subsample_y),
        )
    }
}

/// One decoded picture: RGBA, or luma with colour planes beside it.
///
/// Straight, not premultiplied: the shader multiplies by the quad's own colour,
/// whose alpha already carries layer opacity and bus gain, so premultiplying
/// here would apply alpha twice.
///
/// # Why it is not always RGBA
///
/// It was, and converting to it was costing more than everything else in the
/// renderer put together. A 1080p RGBA frame is 8.3 MB and the same frame as
/// `yuv420p` is 3.1, and that frame is copied twice a layer a frame — into a
/// staging buffer on the processor, then onto the card. Seven layers of it was
/// 74% of a frame at sixty, measured with `ovs-upload-cost`, against 5% for the
/// same seven layers when nothing new had to be copied.
///
/// So a decoder hands over what it decoded. The shader does the conversion,
/// which it gets for a few instructions in a pass it was already running, and
/// `swscale` — about two fifths of what decoding a frame cost — goes away.
///
/// A still, a generator and the composition itself are RGBA and stay RGBA: one
/// plane, no chroma, and every path that was written for them is unchanged.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pixels: Vec<u8>,
    size: SizePx,
    chroma: Option<Chroma>,
    /// Set when `pixels` is block-compressed texture data rather than pixels.
    ///
    /// A HAP frame is already a texture, so there is nothing to decode and
    /// nothing to convert: the bytes in the file are the bytes the card
    /// samples. Carrying it here rather than in a type of its own means every
    /// path a picture already takes — the ring, the queue, the staging, the
    /// upload — takes this one too, and only the two ends know the difference.
    blocks: Option<crate::media::hap::BlockFormat>,
    /// Set when `pixels` are four bytes a pixel in blue, green, red, alpha.
    ///
    /// The same argument as `blocks`, one format along. The System screen
    /// arrives this way — it is what CEF paints and what this card's swapchain
    /// is — and a texture can be told to read it, so nothing converts it.
    bgra: bool,
}

impl std::fmt::Debug for DecodedImage {
    /// Prints the size, never the pixels.
    ///
    /// A megabyte of RGBA in a log line helps nobody and hides the line that
    /// would have.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecodedImage")
            .field("size", &self.size)
            .field("bytes", &self.pixels.len())
            .field("planar", &self.chroma.is_some())
            .field("bgra", &self.bgra)
            .finish()
    }
}

impl DecodedImage {
    /// Wraps pixels that are already RGBA.
    ///
    /// Returns `None` unless there are exactly `width * height * 4` bytes, so an
    /// image that does not describe itself correctly cannot become a texture.
    #[must_use]
    pub fn new(pixels: Vec<u8>, size: SizePx) -> Option<Self> {
        let wanted = u64::from(size.width) * u64::from(size.height) * 4;
        (u64::try_from(pixels.len()).ok()? == wanted).then_some(Self {
            pixels,
            size,
            chroma: None,
            blocks: None,
            bgra: false,
        })
    }

    /// Wraps pixels that are already BGRA, as the System screen's are.
    ///
    /// The same shape and the same check as [`Self::new`]; what differs is which
    /// format the texture is made in, and therefore that nothing converts these
    /// bytes between the browser that painted them and the panel they land on.
    #[must_use]
    pub fn bgra(pixels: Vec<u8>, size: SizePx) -> Option<Self> {
        Self::new(pixels, size).map(|image| Self {
            bgra: true,
            ..image
        })
    }

    /// Whether these bytes are BGRA rather than RGBA.
    #[must_use]
    pub const fn is_bgra(&self) -> bool {
        self.bgra
    }

    /// Wraps block-compressed texture data, as a HAP frame holds it.
    ///
    /// Returns `None` unless there are exactly as many bytes as that many
    /// blocks of that format take, so a frame that does not describe itself
    /// correctly cannot become a texture — which for compressed data matters
    /// more than for pixels, because a short one is not a short picture but a
    /// wrong one.
    #[must_use]
    pub fn compressed(
        blocks: Vec<u8>,
        size: SizePx,
        format: crate::media::hap::BlockFormat,
    ) -> Option<Self> {
        (blocks.len() == format.bytes_for(size.width, size.height)).then_some(Self {
            pixels: blocks,
            size,
            chroma: None,
            blocks: Some(format),
            bgra: false,
        })
    }

    /// What the blocks are, when this is a compressed texture.
    #[must_use]
    pub const fn block_format(&self) -> Option<crate::media::hap::BlockFormat> {
        self.blocks
    }

    /// Wraps three planes of eight-bit Y, Cb and Cr.
    ///
    /// The subsampling factors are 1 or 2, which covers 4:4:4, 4:2:2 and 4:2:0
    /// — every planar eight-bit layout a decoder in this renderer produces.
    /// Anything else is converted to one of them before it gets here.
    ///
    /// Returns `None` unless every plane is exactly the size it should be. A
    /// plane that is short by a row is a texture upload reading past the end of
    /// a buffer, which is the one mistake in this module that is not
    /// recoverable.
    #[must_use]
    pub fn planar(
        luma: Vec<u8>,
        cb: Vec<u8>,
        cr: Vec<u8>,
        size: SizePx,
        format: ChromaFormat,
    ) -> Option<Self> {
        if !matches!(format.subsample_x, 1 | 2) || !matches!(format.subsample_y, 1 | 2) {
            return None;
        }
        let chroma = Chroma { cb, cr, format };
        // A sample is one element, and an element is one byte at eight bits
        // and two at ten. Checking in samples and multiplying here means a
        // ten-bit picture with an eight-bit plane count is refused rather than
        // uploaded half-full.
        let element = format.depth.bytes() as u64;
        let luma_wanted = u64::from(size.width) * u64::from(size.height) * element;
        let plane = chroma.plane_size(size);
        let chroma_wanted = u64::from(plane.width) * u64::from(plane.height) * element;
        if u64::try_from(luma.len()).ok()? != luma_wanted
            || u64::try_from(chroma.cb.len()).ok()? != chroma_wanted
            || u64::try_from(chroma.cr.len()).ok()? != chroma_wanted
        {
            return None;
        }
        Some(Self {
            pixels: luma,
            size,
            chroma: Some(chroma),
            blocks: None,
            bgra: false,
        })
    }

    /// The first plane, tightly packed, row by row from the top.
    ///
    /// RGBA for an RGBA image, four bytes a pixel; luma for a planar one, one
    /// byte a pixel. [`chroma`](Self::chroma) says which, and everything that
    /// turns a picture into a texture has to ask.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The colour planes, for a picture that has them.
    ///
    /// `None` means RGBA.
    #[must_use]
    pub const fn chroma(&self) -> Option<&Chroma> {
        self.chroma.as_ref()
    }

    /// The colour at a point, as RGBA, whatever the picture is made of.
    ///
    /// # Not for the render path
    ///
    /// The renderer never calls this: a planar frame goes to the card as three
    /// planes and the shader converts every pixel of it at once, which is the
    /// whole point. This is for the things that have to ask about
    /// one pixel from the outside — a test checking what colour a layer came
    /// out, a diagnostic, a stand-in compositor.
    ///
    /// The coefficients are the same ones `shaders/canvas.wgsl` uses and have
    /// to stay the same. A test that agreed with this and disagreed with the
    /// wall would be worse than no test at all.
    ///
    /// Out of range, or a picture with no pixels, is transparent black.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.size.width || y >= self.size.height {
            return [0, 0, 0, 0];
        }

        let Some(chroma) = &self.chroma else {
            let at = (y as usize * self.size.width as usize + x as usize) * 4;
            return self
                .pixels
                .get(at..at + 4)
                .and_then(|slice| slice.first_chunk::<4>().copied())
                .unwrap_or([0, 0, 0, 0]);
        };

        let plane = chroma.plane_size(self.size);
        let chroma_at = (y / chroma.format.subsample_y).min(plane.height.saturating_sub(1))
            as usize
            * plane.width as usize
            + (x / chroma.format.subsample_x).min(plane.width.saturating_sub(1)) as usize;
        // An index is in samples; a sample is one or two bytes. Reading a
        // ten-bit plane a byte at a time gave a picture that was neither the
        // colour it encoded nor obviously broken — the low byte of every second
        // sample, which is noise that happens to be in range.
        let element = chroma.format.depth.bytes();
        let divisor = chroma.format.depth.divisor();
        let sample = |plane: &[u8], at: usize| {
            let at = at * element;
            let raw = match plane.get(at..at + element) {
                Some([byte]) => f32::from(*byte),
                Some([low, high]) => f32::from(u16::from_le_bytes([*low, *high])),
                _ => 0.0,
            };
            raw / divisor
        };

        let mut luma = sample(
            &self.pixels,
            y as usize * self.size.width as usize + x as usize,
        );
        let mut blue = sample(&chroma.cb, chroma_at) - 0.5;
        let mut red = sample(&chroma.cr, chroma_at) - 0.5;
        if !chroma.format.full_range {
            luma = (luma - 16.0 / 255.0) * (255.0 / 219.0);
            blue *= 255.0 / 224.0;
            red *= 255.0 / 224.0;
        }

        let rgb = match chroma.format.matrix {
            ColourMatrix::Bt709 => [
                luma + 1.5748 * red,
                luma - 0.187_324 * blue - 0.468_124 * red,
                luma + 1.8556 * blue,
            ],
            ColourMatrix::Bt601 => [
                luma + 1.402 * red,
                luma - 0.344_136 * blue - 0.714_136 * red,
                luma + 1.772 * blue,
            ],
            // Kr = 0.2627, Kb = 0.0593, so Kg = 0.678: a much greener green
            // than either of the others, which is where the hue error shows.
            ColourMatrix::Bt2020 => [
                luma + 1.474_6 * red,
                luma - 0.164_553 * blue - 0.571_353 * red,
                luma + 1.881_4 * blue,
            ],
        };
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        [byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), 255]
    }

    /// How many bytes this picture is, across every plane.
    ///
    /// What actually crosses the bus.
    #[must_use]
    pub fn byte_size(&self) -> usize {
        self.pixels.len()
            + self
                .chroma
                .as_ref()
                .map_or(0, |chroma| chroma.cb.len() + chroma.cr.len())
    }

    /// The first plane, to be written over.
    ///
    /// The length cannot change through a slice, so an image that describes
    /// itself correctly still does afterwards. It is here for the one caller
    /// that draws a new picture into the same buffer sixty times a second
    /// rather than allocating eight megabytes to throw away (`crate::pattern`),
    /// which draws RGBA and only RGBA.
    #[must_use]
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// The image's dimensions.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }
}

/// Why a still could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// The file could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Unreadable {
        /// The file that could not be read.
        path: PathBuf,
        /// What the filesystem said.
        #[source]
        source: std::io::Error,
    },

    /// The file is not an image this renderer can open.
    #[error("cannot decode {}: {reason}", path.display())]
    Undecodable {
        /// The file that could not be decoded.
        path: PathBuf,
        /// What the decoder said.
        reason: String,
    },

    /// The image is larger than any texture it could become.
    #[error("{} is {width}x{height}, larger than the {MAX_IMAGE_SIDE} pixel limit", path.display())]
    TooLarge {
        /// The file that was too large.
        path: PathBuf,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
    },
}

/// Reads and decodes one still image into RGBA.
///
/// The dimensions come from the file, never from the database: the catalogue's
/// idea of an image can be out of date and the pixels cannot.
///
/// # Errors
///
/// Returns [`MediaError`] naming the file and what went wrong with it. A single
/// unreadable image is never a reason to stop a show, so every caller is
/// expected to carry on without it.
pub fn decode_still(path: &Path) -> Result<DecodedImage, MediaError> {
    let bytes = std::fs::read(path).map_err(|source| MediaError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;

    // Read from memory with the format guessed from the content rather than
    // from the extension. A store is content-addressed, so a file's name says
    // nothing about what is in it, and trusting an extension is how a decoder
    // gets handed something it was not built to parse.
    let undecodable = |reason: String| MediaError::Undecodable {
        path: path.to_path_buf(),
        reason,
    };
    let reader = || {
        image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|source| MediaError::Undecodable {
                path: path.to_path_buf(),
                reason: source.to_string(),
            })
    };

    // The header is read before the pixels are. A declared size is cheap, and
    // refusing here means a corrupt or hostile one cannot make the process
    // allocate gigabytes before anything objects. The reader is consumed by
    // asking, so a second one is built to do the decoding.
    let (width, height) = reader()?
        .into_dimensions()
        .map_err(|source| undecodable(source.to_string()))?;
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err(MediaError::TooLarge {
            path: path.to_path_buf(),
            width,
            height,
        });
    }

    let decoded = reader()?
        .decode()
        .map_err(|source| undecodable(source.to_string()))?;

    let rgba = decoded.to_rgba8();
    let size = SizePx::new(rgba.width(), rgba.height());

    DecodedImage::new(rgba.into_raw(), size).ok_or_else(|| {
        undecodable(
            "the decoder produced a buffer that does not match its own dimensions".to_owned(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a PNG of one colour and returns its path.
    fn png(directory: &Path, name: &str, width: u32, height: u32, colour: [u8; 4]) -> PathBuf {
        let path = directory.join(name);
        let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba(colour));
        buffer.save(&path).expect("writes a png");
        path
    }

    const MATRIX: ColourMatrix = ColourMatrix::Bt709;

    /// A description of the planes `planes` builds.
    const fn format(subsample_x: u32, subsample_y: u32) -> ChromaFormat {
        ChromaFormat {
            subsample_x,
            subsample_y,
            full_range: false,
            matrix: MATRIX,
            depth: SampleDepth::Eight,
        }
    }

    /// Three planes of the right sizes for a picture of `size`.
    fn planes(size: SizePx, x: u32, y: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let luma = vec![16u8; (size.width * size.height) as usize];
        let wide = size.width.div_ceil(x);
        let tall = size.height.div_ceil(y);
        (
            luma,
            vec![128u8; (wide * tall) as usize],
            vec![128u8; (wide * tall) as usize],
        )
    }

    #[test]
    fn a_planar_picture_says_what_it_is() {
        let size = SizePx::new(1920, 1080);
        let (luma, cb, cr) = planes(size, 2, 2);

        let image = DecodedImage::planar(luma, cb, cr, size, format(2, 2)).expect("well formed");

        assert_eq!(image.size(), size);
        assert_eq!(
            image.pixels().len(),
            1920 * 1080,
            "one byte a pixel, not four"
        );
        let chroma = image.chroma().expect("planar");
        assert_eq!(chroma.plane_size(size), SizePx::new(960, 540));
        assert!(!chroma.full_range());

        // The whole reason for the type: 3.1 MB against 8.3 for the same
        // picture as RGBA, and that is what crosses the bus twice a frame.
        assert_eq!(image.byte_size(), 1920 * 1080 * 3 / 2);
    }

    #[test]
    fn a_planar_picture_reads_back_as_the_colour_it_encodes() {
        // BT.709, narrow range: red is Y'=63, Cb=102, Cr=240, which is what
        // every encoder writes for pure red and what the shader has to give
        // back. The tolerance is the round through eight-bit samples.
        let size = SizePx::new(4, 4);
        let (luma, cb, cr) = planes(size, 2, 2);
        let red = DecodedImage::planar(
            vec![63; luma.len()],
            vec![102; cb.len()],
            vec![240; cr.len()],
            size,
            format(2, 2),
        )
        .expect("well formed");

        let got = red.pixel(1, 1);
        assert!(
            got[0] > 250 && got[1] < 5 && got[2] < 5,
            "red read back as {got:?}"
        );
        assert_eq!(got[3], 255, "planar video is opaque");
    }

    #[test]
    fn a_ten_bit_picture_reads_back_as_the_same_colour_the_card_shows() {
        // The same red, written as the ten-bit numbers that mean it: 63, 102
        // and 240 four times over, because narrow range is 16..235 at eight
        // bits and 64..940 at ten. Both storage layouts and 4:2:2 as well as
        // 4:2:0 — this is the combination that was wrong, and it was wrong
        // quietly. `pixel` read one byte a sample, so a ten-bit plane gave it
        // the low byte of every second sample: a plausible colour, no panic,
        // and nothing at all to do with the picture.
        for (depth, shift) in [(SampleDepth::TenLow, 0), (SampleDepth::TenHigh, 6)] {
            for (x, y) in [(2, 2), (2, 1)] {
                let size = SizePx::new(4, 4);
                let wide = size.width.div_ceil(x) as usize;
                let tall = size.height.div_ceil(y) as usize;
                let fill = |value: u16, samples: usize| -> Vec<u8> {
                    (value << shift)
                        .to_le_bytes()
                        .iter()
                        .copied()
                        .cycle()
                        .take(samples * 2)
                        .collect()
                };

                let red = DecodedImage::planar(
                    fill(252, 16),
                    fill(408, wide * tall),
                    fill(960, wide * tall),
                    size,
                    ChromaFormat {
                        subsample_x: x,
                        subsample_y: y,
                        full_range: false,
                        matrix: MATRIX,
                        depth,
                    },
                )
                .expect("well formed");

                let got = red.pixel(1, 1);
                assert!(
                    got[0] > 250 && got[1] < 5 && got[2] < 5,
                    "{depth:?} at 4:2:{} read red back as {got:?}",
                    if y == 2 { 0 } else { 2 }
                );
            }
        }
    }

    #[test]
    fn a_ten_bit_sample_divides_by_what_the_shader_multiplies_by() {
        // Two ends of one number. If these ever disagree the tests above still
        // pass and the wall is wrong, which is the failure this pins shut.
        for depth in [SampleDepth::TenLow, SampleDepth::TenHigh] {
            let round_trip = 65535.0 / depth.divisor();
            assert!(
                (round_trip - depth.sample_scale()).abs() < f32::EPSILON,
                "{depth:?}: scale {} against divisor {}",
                depth.sample_scale(),
                depth.divisor()
            );
        }
        assert!((SampleDepth::Eight.divisor() - 255.0).abs() < f32::EPSILON);
        assert!((SampleDepth::TenLow.divisor() - 1020.0).abs() < 0.01);
        assert!((SampleDepth::TenHigh.divisor() - 1020.0 * 64.0).abs() < 0.5);
    }

    #[test]
    fn a_pixel_outside_a_picture_is_nothing_rather_than_a_panic() {
        let image = DecodedImage::new(vec![1, 2, 3, 4], SizePx::new(1, 1)).expect("well formed");
        assert_eq!(image.pixel(0, 0), [1, 2, 3, 4]);
        assert_eq!(image.pixel(1, 0), [0, 0, 0, 0]);
        assert_eq!(image.pixel(0, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn an_rgba_picture_has_no_chroma_and_never_did() {
        let size = SizePx::new(4, 3);
        let image = DecodedImage::new(vec![0u8; 4 * 3 * 4], size).expect("well formed");

        assert!(image.chroma().is_none());
        assert_eq!(image.byte_size(), 4 * 3 * 4);
    }

    #[test]
    fn an_odd_size_rounds_its_chroma_planes_up() {
        // A 1921-wide 4:2:0 picture has 961 chroma samples across, not 960, and
        // a decoder that produced 961 against an uploader that expected 960 is
        // a row read past the end of a buffer.
        let size = SizePx::new(1921, 1081);
        let (luma, cb, cr) = planes(size, 2, 2);

        let image = DecodedImage::planar(luma, cb, cr, size, format(2, 2)).expect("well formed");

        assert_eq!(
            image.chroma().expect("planar").plane_size(size),
            SizePx::new(961, 541)
        );
    }

    #[test]
    fn a_plane_of_the_wrong_size_is_refused_rather_than_uploaded() {
        let size = SizePx::new(64, 64);
        let (luma, cb, cr) = planes(size, 2, 2);

        assert!(
            DecodedImage::planar(luma.clone(), cb.clone(), vec![0; 3], size, format(2, 2))
                .is_none(),
            "a short colour plane"
        );
        assert!(
            DecodedImage::planar(vec![0; 3], cb.clone(), cr.clone(), size, format(2, 2)).is_none(),
            "a short luma plane"
        );
        assert!(
            DecodedImage::planar(luma.clone(), cb.clone(), cr.clone(), size, format(2, 1))
                .is_none(),
            "planes sized for 4:2:0 declared as 4:2:2"
        );
    }

    #[test]
    fn a_subsampling_nothing_here_produces_is_refused() {
        // 1 and 2 cover 4:4:4, 4:2:2 and 4:2:0. A decoder producing anything
        // else is converted before it gets here, and a zero would be a divide
        // by it.
        let size = SizePx::new(64, 64);
        let (luma, cb, cr) = planes(size, 2, 2);
        for (subsample_x, subsample_y) in [(0, 2), (2, 0), (4, 2), (2, 4)] {
            assert!(
                DecodedImage::planar(
                    luma.clone(),
                    cb.clone(),
                    cr.clone(),
                    size,
                    ChromaFormat {
                        subsample_x,
                        subsample_y,
                        full_range: false,
                        matrix: MATRIX,
                        depth: SampleDepth::Eight,
                    },
                )
                .is_none(),
                "{subsample_x}x{subsample_y} subsampling"
            );
        }
    }

    #[test]
    fn a_png_decodes_to_the_pixels_it_holds() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "red.png", 4, 3, [255, 0, 0, 255]);

        let image = decode_still(&path).expect("decodes");

        assert_eq!(image.size(), SizePx::new(4, 3));
        assert_eq!(image.pixels().len(), 4 * 3 * 4);
        assert_eq!(&image.pixels()[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn transparency_survives_decoding() {
        // A logo with an alpha channel is the ordinary case for a still, and
        // flattening it against something would be visible on stage.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = png(directory.path(), "ghost.png", 2, 2, [0, 255, 0, 64]);

        let image = decode_still(&path).expect("decodes");

        assert_eq!(&image.pixels()[..4], &[0, 255, 0, 64]);
    }

    #[test]
    fn the_first_pixel_is_the_top_left_one() {
        // The shader inverts V on the assumption that pixels arrive top row
        // first. If that ever stopped being true, every still would be upside
        // down and nothing else would notice.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("halves.png");
        let mut buffer = image::RgbaImage::from_pixel(1, 2, image::Rgba([0, 0, 0, 255]));
        buffer.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        buffer.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        buffer.save(&path).expect("writes");

        let image = decode_still(&path).expect("decodes");

        assert_eq!(&image.pixels()[..4], &[255, 0, 0, 255], "top row first");
        assert_eq!(&image.pixels()[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn a_jpeg_decodes_too() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("photo.jpg");
        image::RgbImage::from_pixel(8, 8, image::Rgb([10, 200, 30]))
            .save(&path)
            .expect("writes a jpeg");

        let image = decode_still(&path).expect("decodes");

        assert_eq!(image.size(), SizePx::new(8, 8));
        assert_eq!(image.pixels()[3], 255, "opaque, since JPEG has no alpha");
    }

    #[test]
    fn the_extension_does_not_decide_the_format() {
        // A content-addressed store names files by hash, so an extension is a
        // hint at best. Guessing from the content is what makes the store's
        // naming scheme and the decoder independent.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("actually-a-png.mov");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save_with_format(&path, image::ImageFormat::Png)
            .expect("writes");

        assert!(decode_still(&path).is_ok());
    }

    #[test]
    fn a_file_that_is_not_an_image_is_refused_rather_than_guessed_at() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("notes.txt");
        std::fs::write(&path, b"this is not a picture").expect("writes");

        let error = decode_still(&path).expect_err("refuses");
        assert!(matches!(error, MediaError::Undecodable { .. }), "{error}");
    }

    #[test]
    fn a_missing_file_says_so() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let error = decode_still(&directory.path().join("gone.png")).expect_err("refuses");

        assert!(matches!(error, MediaError::Unreadable { .. }), "{error}");
        assert!(
            error.to_string().contains("gone.png"),
            "the message must name the file: {error}"
        );
    }

    #[test]
    fn an_empty_file_is_refused() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("empty.png");
        std::fs::write(&path, b"").expect("writes");

        assert!(decode_still(&path).is_err());
    }

    #[test]
    fn pixels_that_do_not_match_their_dimensions_are_not_an_image() {
        assert!(DecodedImage::new(vec![0; 15], SizePx::new(2, 2)).is_none());
        assert!(DecodedImage::new(vec![0; 17], SizePx::new(2, 2)).is_none());
        assert!(DecodedImage::new(vec![0; 16], SizePx::new(2, 2)).is_some());
    }

    #[test]
    fn a_decoded_image_never_logs_its_pixels() {
        let image = DecodedImage::new(vec![7; 16], SizePx::new(2, 2)).expect("an image");
        let printed = format!("{image:?}");

        assert!(printed.contains("bytes: 16"), "{printed}");
        assert!(!printed.contains('7'), "{printed}");
    }
}
