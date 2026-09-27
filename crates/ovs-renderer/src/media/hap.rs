//! HAP: a frame that is already a texture.
//!
//! Every other clip this renderer plays arrives as pixels — the decoder hands
//! back a picture, the picture is copied into a staging buffer, and the card
//! turns it into a texture. HAP skips all of that. What is stored in the file
//! *is* a block-compressed texture, the same bytes the card samples, and the
//! whole of playing it is reading them out and handing them over.
//!
//! The owner chose it for alpha material on 2026-09-23, and the reason is this
//! table for one 3840x2160 frame:
//!
//! | | over the bus | processor work |
//! |---|---|---|
//! | RGBA from a decoder | 33 MB | colour conversion, a copy per plane |
//! | HAP Alpha, as stored | **8.3 MB** | a Snappy decompression, or none |
//!
//! # Why this does not use FFmpeg's decoder
//!
//! FFmpeg has one, and it hands back `rgba` — it decompresses the blocks on
//! the processor, which throws away the entire point. So FFmpeg demuxes the
//! file and nothing else: the packets it produces are HAP frames, and this
//! reads them.
//!
//! # The format
//!
//! A frame is one section. A section is a three-byte little-endian size and a
//! type byte; a size of zero means the real size is the next four bytes. The
//! type byte's **high** nibble is how the payload is compressed and its **low**
//! nibble is what texture it is.
//!
//! That way round, and it is worth saying which way because guessing it wrong
//! does not fail — plain Hap is `0xBB`, so both readings agree on the one
//! format anybody tests with, and the mistake only shows on alpha material as
//! a picture that is garbled rather than absent.
//!
//! Read off files this build's own FFmpeg wrote, rather than off the
//! specification, because the specification is not what will be in an
//! operator's folder:
//!
//! ```text
//! hap        00 00 00 bb  0f 08 00 00   Snappy, RGB_DXT1
//! hap_alpha  00 00 00 be  55 0a 00 00   Snappy, RGBA_DXT5
//! hap_q      00 00 00 bf  02 0f 00 00   Snappy, YCoCg_DXT5
//! ```
//!
//! A "complex" frame (compressor `0xC`) is a section holding a table of chunks
//! and their own compressors, so that an encoder can split a frame across
//! threads. It is read here because encoders do produce it, and its chunks are
//! concatenated in order to make the one texture.

use super::snappy;

/// What a HAP frame could not be read as.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HapError {
    /// The frame is shorter than a section header.
    #[error("a HAP frame needs at least four bytes and this one has {0}")]
    TooShort(usize),
    /// The section says it is longer than the frame that holds it.
    #[error("a HAP section says it holds {wanted} bytes and the frame has {has}")]
    Overruns {
        /// What the header said.
        wanted: usize,
        /// What there was.
        has: usize,
    },
    /// A texture format this build does not publish.
    #[error("this build does not read HAP texture format {0:#04x}")]
    UnknownTexture(u8),
    /// A compressor this build does not know.
    #[error("this build does not read HAP compressor {0:#04x}")]
    UnknownCompressor(u8),
    /// The payload would not decompress.
    #[error("a HAP frame would not decompress: {0}")]
    Snappy(#[from] snappy::SnappyError),
    /// The decoded texture is not the size the stream said it would be.
    #[error(
        "a HAP frame holds {has} bytes and a {width}x{height} {format:?} texture needs {wanted}"
    )]
    WrongSize {
        /// What came out.
        has: usize,
        /// What the format needs.
        wanted: usize,
        /// The frame's width.
        width: u32,
        /// The frame's height.
        height: u32,
        /// The texture format.
        format: BlockFormat,
    },
}

/// The block-compressed texture a HAP frame holds.
///
/// One of three, because HAP has three and this reads all of them. What a
/// show should *contain* is a different question and a narrower answer — the
/// owner's ingest policy is Hap Alpha for anything with alpha — but a file an
/// operator already has is a file this should be able to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockFormat {
    /// `RGB_DXT1`, four bits a pixel, no alpha. Plain Hap.
    Dxt1,
    /// `RGBA_DXT5`, eight bits a pixel. Hap Alpha, and the one the owner chose.
    Dxt5,
    /// `YCoCg_DXT5`, eight bits a pixel, and not RGB until a shader says so.
    ///
    /// Hap Q. The blocks are DXT5 and the card samples them as such, but the
    /// three channels are luma and two chroma differences with a scale in the
    /// alpha channel. Sampling it as if it were colour gives a green picture,
    /// which is what makes this worth a variant of its own rather than a flag.
    YCoCgDxt5,
}

impl BlockFormat {
    /// How many bytes one 4x4 block takes.
    #[must_use]
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::Dxt1 => 8,
            Self::Dxt5 | Self::YCoCgDxt5 => 16,
        }
    }

    /// How many bytes a texture of this size takes, blocks rounded up.
    #[must_use]
    pub const fn bytes_for(self, width: u32, height: u32) -> usize {
        let across = width.div_ceil(4) as usize;
        let down = height.div_ceil(4) as usize;
        across * down * self.block_bytes()
    }

    /// Whether the card may sample this as colour without a shader step.
    #[must_use]
    pub const fn is_colour(self) -> bool {
        matches!(self, Self::Dxt1 | Self::Dxt5)
    }
}

/// One frame, decompressed into the blocks the card samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HapFrame {
    /// What the blocks are.
    pub format: BlockFormat,
    /// The blocks themselves, ready to be copied to the card as they are.
    pub blocks: Vec<u8>,
}

/// Reads one HAP frame, decompressing it into `blocks`.
///
/// `blocks` is reused between frames rather than allocated per frame: a show
/// plays sixty of these a second and the buffer is the same size every time.
///
/// # Errors
///
/// Returns [`HapError`] for a frame this build cannot read — a texture format
/// it does not publish, a compressor it does not know, a section that overruns
/// the frame, or a payload that does not decompress to the size the stream's
/// dimensions call for.
pub fn read_frame(
    frame: &[u8],
    width: u32,
    height: u32,
    blocks: &mut Vec<u8>,
) -> Result<BlockFormat, HapError> {
    let (format, compressor, payload) = section(frame)?;

    match compressor {
        // Stored as it is. The copy is unavoidable — the caller keeps the
        // buffer and the packet does not outlive the pass — but it is one
        // memcpy of already-compressed bytes and nothing else.
        COMPRESSOR_NONE => {
            blocks.clear();
            blocks.extend_from_slice(payload);
        }
        COMPRESSOR_SNAPPY => snappy::decompress(payload, blocks)?,
        COMPRESSOR_COMPLEX => complex(payload, blocks)?,
        other => return Err(HapError::UnknownCompressor(other)),
    }

    let wanted = format.bytes_for(width, height);
    if blocks.len() == wanted {
        Ok(format)
    } else {
        Err(HapError::WrongSize {
            has: blocks.len(),
            wanted,
            width,
            height,
            format,
        })
    }
}

/// Stored as it is.
const COMPRESSOR_NONE: u8 = 0xA;
/// One Snappy block.
const COMPRESSOR_SNAPPY: u8 = 0xB;
/// A table of chunks, each with its own compressor.
const COMPRESSOR_COMPLEX: u8 = 0xC;

/// A section's texture format, its compressor and its payload.
fn section(frame: &[u8]) -> Result<(BlockFormat, u8, &[u8]), HapError> {
    if frame.len() < 4 {
        return Err(HapError::TooShort(frame.len()));
    }

    let short =
        usize::from(frame[0]) | (usize::from(frame[1]) << 8) | (usize::from(frame[2]) << 16);
    let kind = frame[3];

    // A three-byte size of zero means the real one is the next four bytes.
    // Every frame this build's own encoder writes takes that path, so it is
    // the ordinary case rather than the exception.
    let (size, body): (usize, usize) = if short == 0 {
        if frame.len() < 8 {
            return Err(HapError::TooShort(frame.len()));
        }
        let long = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]) as usize;
        (long, 8)
    } else {
        (short, 4)
    };

    let end = body.checked_add(size).ok_or(HapError::Overruns {
        wanted: size,
        has: frame.len(),
    })?;
    let payload = frame.get(body..end).ok_or(HapError::Overruns {
        wanted: size,
        has: frame.len().saturating_sub(body),
    })?;

    let format = match kind & 0x0F {
        0xB => BlockFormat::Dxt1,
        0xE => BlockFormat::Dxt5,
        0xF => BlockFormat::YCoCgDxt5,
        other => return Err(HapError::UnknownTexture(other)),
    };

    Ok((format, kind >> 4, payload))
}

/// Reads a chunked frame, concatenating its chunks in order.
///
/// An encoder splits a frame into chunks so that several threads can compress
/// it at once. Each chunk carries its own compressor, so a frame may be part
/// Snappy and part stored — which happens whenever a chunk compressed to more
/// than it started as.
fn complex(payload: &[u8], blocks: &mut Vec<u8>) -> Result<(), HapError> {
    blocks.clear();

    // The decode instructions container, then the chunk payloads after it.
    let (_, _, table) = section(payload)?;
    let table_end = payload.len() - table.len();

    let mut compressors: Vec<u8> = Vec::new();
    let mut sizes: Vec<usize> = Vec::new();
    let mut at = 0;
    while at < table.len() {
        let (_, _, inner) = section(&table[at..])?;
        let header = table[at..].len() - inner.len();
        let kind = table[at + 3];
        match kind {
            // Chunk second-stage compressor table: one byte a chunk.
            0x02 => compressors.extend_from_slice(inner),
            // Chunk size table: four little-endian bytes a chunk.
            0x03 => {
                for four in inner.as_chunks::<4>().0 {
                    sizes.push(u32::from_le_bytes(*four) as usize);
                }
            }
            // Chunk counts and offset tables are read past: the sizes are
            // enough to walk the payloads in order, and order is all that
            // matters for putting one texture back together.
            _ => {}
        }
        at += header + inner.len();
    }

    let mut scratch = Vec::new();
    let mut from = table_end;
    for (index, size) in sizes.iter().enumerate() {
        let chunk = payload.get(from..from + size).ok_or(HapError::Overruns {
            wanted: *size,
            has: payload.len().saturating_sub(from),
        })?;
        from += size;

        match compressors.get(index).copied().unwrap_or(COMPRESSOR_NONE) {
            COMPRESSOR_SNAPPY => {
                snappy::decompress(chunk, &mut scratch)?;
                blocks.extend_from_slice(&scratch);
            }
            COMPRESSOR_NONE => blocks.extend_from_slice(chunk),
            other => return Err(HapError::UnknownCompressor(other)),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame header: a long size, then a type byte.
    fn framed(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 0, 0, kind];
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn the_type_byte_says_which_texture_and_which_compressor() {
        // The three this build's own FFmpeg writes, read off real files rather
        // than off the specification. Getting this wrong does not fail — it
        // puts a green or a garbled picture on the wall, which is worse.
        //
        // It was wrong here first: the nibbles were read the other way round,
        // and every synthetic test below passed anyway because plain Hap is
        // `0xBB` and reads the same either way. The file an encoder actually
        // wrote is what caught it.
        let blocks = vec![0u8; BlockFormat::Dxt5.bytes_for(8, 8)];
        let frame = framed(0xAE, &blocks);
        let mut out = Vec::new();
        assert_eq!(
            read_frame(&frame, 8, 8, &mut out).expect("reads"),
            BlockFormat::Dxt5
        );
        assert_eq!(out, blocks, "stored frames are handed over as they are");

        let dxt1 = vec![0u8; BlockFormat::Dxt1.bytes_for(8, 8)];
        assert_eq!(
            read_frame(&framed(0xAB, &dxt1), 8, 8, &mut out).expect("reads"),
            BlockFormat::Dxt1
        );

        let ycocg = vec![0u8; BlockFormat::YCoCgDxt5.bytes_for(8, 8)];
        assert_eq!(
            read_frame(&framed(0xAF, &ycocg), 8, 8, &mut out).expect("reads"),
            BlockFormat::YCoCgDxt5
        );
    }

    #[test]
    fn a_texture_is_as_many_blocks_as_it_takes_to_cover_the_frame() {
        // Blocks are 4x4 and a frame is not obliged to be a multiple of four.
        // A 1920x1074 clip — which is a real one, an operator's — is 480 by
        // 269 blocks, not 268 and a bit.
        assert_eq!(BlockFormat::Dxt5.bytes_for(1920, 1074), 480 * 269 * 16);
        assert_eq!(BlockFormat::Dxt1.bytes_for(1920, 1080), 480 * 270 * 8);
        assert_eq!(BlockFormat::Dxt5.bytes_for(1, 1), 16, "one block, not none");
    }

    #[test]
    fn a_snappy_frame_is_decompressed() {
        // One literal run holding the blocks, which is what Snappy produces
        // for data it cannot compress — and a texture of noise is exactly
        // that.
        let blocks = vec![0x5Au8; BlockFormat::Dxt5.bytes_for(4, 4)];
        let mut compressed = vec![blocks.len() as u8];
        compressed.push(((blocks.len() - 1) as u8) << 2);
        compressed.extend_from_slice(&blocks);

        let mut out = Vec::new();
        assert_eq!(
            read_frame(&framed(0xBE, &compressed), 4, 4, &mut out).expect("reads"),
            BlockFormat::Dxt5
        );
        assert_eq!(out, blocks);
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_refused_rather_than_uploaded() {
        // The failure that matters: a texture short of what the stream's
        // dimensions call for would be uploaded with whatever the buffer held
        // after it, which on a wall is a band of somebody else's frame.
        let blocks = vec![0u8; 16];
        let mut out = Vec::new();
        assert!(matches!(
            read_frame(&framed(0xAE, &blocks), 64, 64, &mut out),
            Err(HapError::WrongSize { .. })
        ));
    }

    #[test]
    fn a_section_that_overruns_its_frame_is_refused() {
        // Says it holds a kilobyte and holds four bytes. A reader that
        // believed it would hand the card whatever was next in memory.
        let mut frame = vec![0, 0, 0, 0xAE];
        frame.extend_from_slice(&1024u32.to_le_bytes());
        frame.extend_from_slice(&[1, 2, 3, 4]);
        let mut out = Vec::new();
        assert!(matches!(
            read_frame(&frame, 4, 4, &mut out),
            Err(HapError::Overruns { .. })
        ));
    }

    #[test]
    fn a_texture_format_this_build_does_not_read_is_named_rather_than_guessed() {
        let mut out = Vec::new();
        assert!(matches!(
            read_frame(&framed(0xA1, &[0u8; 16]), 4, 4, &mut out),
            Err(HapError::UnknownTexture(0x1))
        ));
    }

    /// One HAP file written by this machine's FFmpeg, or nothing.
    ///
    /// Skipped rather than failed where there is no FFmpeg: the unit tests
    /// above cover the format, and this covers the part they cannot — that
    /// what a real encoder writes is what this reads.
    fn encoded(format: &str, size: u32) -> Option<Vec<u8>> {
        let directory = std::env::temp_dir().join(format!("ovs-hap-{format}-{size}"));
        let _ = std::fs::create_dir_all(&directory);
        let movie = directory.join("clip.mov");

        let made = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={size}x{size}:rate=1:duration=1"),
                "-c:v",
                "hap",
                "-format",
                format,
            ])
            .arg(&movie)
            .status();
        if !matches!(made, Ok(status) if status.success()) {
            println!("skipped: no FFmpeg to write a HAP file with");
            return None;
        }

        // The packets as stored, which is what the renderer will be handed:
        // FFmpeg demuxes and this reads what comes out, with no decoder
        // anywhere between the two.
        let raw = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&movie)
            .args(["-c", "copy", "-f", "rawvideo", "-"])
            .output()
            .ok()?;
        let _ = std::fs::remove_dir_all(&directory);
        raw.status.success().then_some(raw.stdout)
    }

    #[test]
    fn a_file_a_real_encoder_wrote_reads_as_the_texture_it_says_it_is() {
        // The whole point, end to end: the bytes in the file become the bytes
        // the card samples, with no decoder and no colour conversion.
        for (format, wanted) in [
            ("hap", BlockFormat::Dxt1),
            ("hap_alpha", BlockFormat::Dxt5),
            ("hap_q", BlockFormat::YCoCgDxt5),
        ] {
            let Some(frame) = encoded(format, 128) else {
                return;
            };

            let mut blocks = Vec::new();
            let read = read_frame(&frame, 128, 128, &mut blocks)
                .unwrap_or_else(|error| panic!("{format} would not read: {error}"));

            assert_eq!(read, wanted, "{format}");
            assert_eq!(
                blocks.len(),
                wanted.bytes_for(128, 128),
                "{format}: 32 by 32 blocks"
            );
            // Not all one value. A parser that produced zeroes would pass
            // every size check above and put a black rectangle on the wall.
            assert!(
                blocks.iter().any(|byte| *byte != blocks[0]),
                "{format}: the blocks hold a picture"
            );
        }
    }

    #[test]
    fn a_frame_too_short_to_hold_a_header_is_refused() {
        let mut out = Vec::new();
        assert!(matches!(
            read_frame(&[0, 0, 0], 4, 4, &mut out),
            Err(HapError::TooShort(3))
        ));
        assert!(matches!(
            read_frame(&[0, 0, 0, 0xAE, 1], 4, 4, &mut out),
            Err(HapError::TooShort(5))
        ));
    }
}
