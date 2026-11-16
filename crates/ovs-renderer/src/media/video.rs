//! Decoding a clip, one frame at a time.
//!
//! The renderer asks for "the frame at index *n*" sixty times a second and this
//! answers with pixels. Everything above it — the playhead, the ring, the
//! layer — is written against [`FrameSource`] and knows nothing about codecs
//!; this is the first implementation of that trait that opens a real
//! file.
//!
//! # Why FFmpeg
//!
//! The server plays what it is given — H.264, HEVC, AV1, ProRes, MPEG-1/2, HAP
//! and the rest — and there is exactly one library that decodes all
//! of them. FFmpeg's decoder is also the one every tool in the building already
//! uses, which matters more than it sounds: a clip that plays in the editor and
//! not on the server is a support call, and "we wrote our own" is the wrong
//! answer to it.
//!
//! # Random access, not streaming
//!
//! A media server does not play a file from start to finish. It jumps to a
//! frame because a slot was triggered, plays backwards because an operator asked
//! it to, and holds still on one frame while a fader moves. So this is built
//! around *seek and decode* rather than a pipeline: give it an index and it
//! answers, wherever the last one was.
//!
//! That is cheap on all-intra media: every ProRes frame is a keyframe, so a
//! seek lands exactly where it was asked to and costs one frame's decode. On a
//! long-GOP codec the same call decodes from the last keyframe forward, which
//! can be half a second of pictures nobody asked for.
//!
//! The server plays long-GOP media now, so that is a cost to *avoid
//! paying* rather than a reason to keep such media out of a show. Avoiding it
//! is not done here — it is done by not asking. `SEEK_AFTER` keeps an ordinary
//! forward step out of the seek path, and the player above keeps its lead
//! inside its budget so that a clip which is simply playing is never mistaken
//! for one being scrubbed; before it did, a 1080p60 H.264 clip held one frame
//! in seven while ProRes held none, and nothing about the codec was the reason.
//! When a seek is genuinely asked for it costs what the codec costs, and on a
//! long-GOP clip played backwards that is a stutter the operator was told about
//! rather than one the renderer hides.
//!
//! # Unix only, for now
//!
//! The FFmpeg libraries are a build dependency of this module, and the machine
//! this product runs on is Debian. A Windows development machine builds the rest
//! of the renderer without them and without this module; the tests that matter
//! run where the screens are.

// Three things here are unsafe, each explained where it is written: the colour
// converter's `Send`, the card's shared CUDA context, and copying a picture the
// card decoded into host memory. The last two are FFmpeg's hardware API, which
// `ffmpeg-next` does not wrap.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::{Mutex, PoisonError};

use ffmpeg::ffi;
use ffmpeg_next as ffmpeg;
use ovs_types::geometry::SizePx;

use crate::media::source::{Frame, FrameSource};
use crate::media::still::{
    ChromaFormat, ColourMatrix, DecodedImage, MediaError, SampleDepth, MAX_IMAGE_SIDE,
};
use crate::media::timeline::SourceLength;

/// What a decoded frame is turned into before it is uploaded.
///
/// # Why this is not always RGBA
///
/// It was, and it was most of what the renderer spent its time on. A 1080p RGBA
/// frame is 8.3 MB and the same frame as 4:2:0 is 3.1, and that frame is copied
/// twice a layer a frame — into a staging buffer, then onto the card. Seven
/// layers of RGBA is 74% of a frame at sixty; the same seven with nothing new
/// to copy is 5%.
///
/// So a frame whose codec produced planar eight-bit Y'CbCr is handed over as it
/// is, with no conversion at all, and the shader does the colour. A frame in
/// some other planar layout is converted to the eight-bit planar form with the
/// same chroma subsampling, which keeps what the codec kept and is still far
/// less to move than RGBA. Only a frame that carries alpha, or one in a layout
/// nothing here recognises, goes to RGBA — correct, and the expensive case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Four bytes a pixel, ready to draw, and what alpha needs.
    Rgba,
    /// Three planes of eight bits, with this chroma subsampling.
    Planar {
        format: ffmpeg::format::Pixel,
        subsample_x: u32,
        subsample_y: u32,
    },
    /// Two planes, the second carrying Cb and Cr together, two bytes a sample.
    ///
    /// What a card hands back: NVDEC decodes to NV12 and there is no option to
    /// ask it for anything else. It used to go through `swscale` to reach the
    /// three-plane form, which walks every pixel of every frame to move bytes
    /// that are already there.
    ///
    /// # NV12 is no longer split at all
    ///
    /// It was, in the copy that was happening anyway: one strided write into
    /// two destinations instead of one memcpy into one. The note here said
    /// that a two-plane texture would save that second stream, cost a fourth
    /// shader path and a second chroma layout everywhere, and was *worth
    /// reconsidering if the copy ever showed up in a measurement*.
    ///
    /// It showed up. The render thread was measured spending about half its
    /// time in `memcpy` moving frames, and the wall was running at 28.5 frames
    /// a second against the 59.94 it is set to with a single clip on it.
    ///
    /// The fourth shader path turned out not to be needed: a Vulkan component
    /// mapping lives on the *view* rather than on the image, so one `R8G8`
    /// chroma image is bound twice — once as it lies, where `.r` is Cb, and
    /// once with green swizzled into red, where `.r` is Cr. The shader is
    /// untouched and the sampler does the work for nothing
    /// ([`TextureLayout::Nv12`]). What is left of the cost is one more layout,
    /// which is this arm and [`DecodedImage::interleaved`].
    ///
    /// So NV12 — eight-bit, 4:2:0, Cb first — is copied out row by row and
    /// goes up as it is. The other interleaved layouts still split, because
    /// there is one two-component texture layout and NV12 is its shape:
    /// ten-bit P010 would need `R16G16`, 4:2:2 NV16 a differently sized plane,
    /// and NV21 the other swizzle. Each is a layout, not a flag, and none of
    /// them is what a card in this building produces.
    ///
    /// [`TextureLayout::Nv12`]: crate::vulkan::TextureLayout::Nv12
    Interleaved {
        /// How many luma samples across share one chroma sample.
        subsample_x: u32,
        /// How many luma samples down share one chroma sample.
        subsample_y: u32,
        /// Whether Cb comes first in each pair, as in NV12. NV21 is the other
        /// way round and is otherwise identical.
        blue_first: bool,
    },
}

impl Target {
    /// What a frame in `format` should become.
    ///
    /// Matched rather than read out of `FFmpeg`'s descriptor table, because the
    /// fallback has to be RGBA: a layout this does not recognise is converted
    /// and drawn correctly, just expensively, and a format carrying alpha that
    /// was mistaken for one that does not would lose the alpha silently.
    /// Whether this target is one of the ten-bit ones.
    const fn is_ten_bit(self) -> bool {
        matches!(
            self,
            Self::Planar {
                format: ffmpeg::format::Pixel::YUV420P10LE
                    | ffmpeg::format::Pixel::YUV422P10LE
                    | ffmpeg::format::Pixel::YUV444P10LE,
                ..
            }
        )
    }

    fn for_format(format: ffmpeg::format::Pixel) -> Self {
        use ffmpeg::format::Pixel;

        let planar = |format, subsample_x, subsample_y| Self::Planar {
            format,
            subsample_x,
            subsample_y,
        };

        match format {
            // Already what is wanted: no conversion runs at all, which is the
            // case nearly every H.264, HEVC and AV1 clip falls into.
            Pixel::YUV420P | Pixel::YUVJ420P => planar(Pixel::YUV420P, 2, 2),
            Pixel::YUV422P | Pixel::YUVJ422P => planar(Pixel::YUV422P, 2, 1),
            Pixel::YUV444P | Pixel::YUVJ444P => planar(Pixel::YUV444P, 1, 1),

            // HEVC Main 10 on the processor: three planes of sixteen-bit
            // elements with the ten bits at the **bottom**. Kept rather than
            // converted (owner, 2026-09-23) — accepting Main 10 and then
            // walking every pixel of every frame to throw ten bits away would
            // be a profile accepted rather than supported.
            Pixel::YUV420P10LE => planar(Pixel::YUV420P10LE, 2, 2),
            Pixel::YUV422P10LE => planar(Pixel::YUV422P10LE, 2, 1),
            Pixel::YUV444P10LE => planar(Pixel::YUV444P10LE, 1, 1),

            // Interleaved chroma, which is what every hardware decoder
            // produces. Split rather than converted: the samples are already
            // eight-bit and already at the right subsampling, and `swscale`
            // would walk every pixel to move bytes that are in the right
            // order.
            Pixel::NV12 => Self::Interleaved {
                subsample_x: 2,
                subsample_y: 2,
                blue_first: true,
            },
            Pixel::NV21 => Self::Interleaved {
                subsample_x: 2,
                subsample_y: 2,
                blue_first: false,
            },

            // What a card hands back for HEVC Main 10. NV12's shape with
            // sixteen-bit elements, and the ten bits at the **top** of each
            // one with six zeroes under them — which is the half of this that
            // must not be confused with `yuv420p10le` below, where they are at
            // the bottom. Sixty-four times apart; `depth_of` is what keeps
            // them apart, and the shader is told which it has.
            Pixel::P010LE => Self::Interleaved {
                subsample_x: 2,
                subsample_y: 2,
                blue_first: true,
            },
            Pixel::NV16 => Self::Interleaved {
                subsample_x: 2,
                subsample_y: 1,
                blue_first: true,
            },

            // What the texture path still does not hold: twelve bits, and
            // ten bits stored big-endian.
            //
            // Converted, to the planar form with the same chroma subsampling
            // rather than to RGBA: the chroma is not what is being given up.
            // Ten-bit little-endian is no longer here — it goes up as it is.
            //
            // The big-endian arms are theory rather than practice. FFmpeg
            // decodes to the machine's own order, so a ten-bit stream on this
            // hardware arrives as `...10LE` and takes the native path above;
            // these exist so a byte order this renderer has never seen is
            // converted rather than drawn as noise.
            Pixel::YUV420P10BE | Pixel::YUV420P12LE | Pixel::YUV420P12BE => {
                planar(Pixel::YUV420P, 2, 2)
            }
            Pixel::YUV422P10BE | Pixel::YUV422P12LE | Pixel::YUV422P12BE => {
                planar(Pixel::YUV422P, 2, 1)
            }
            Pixel::YUV444P10BE
            | Pixel::YUV444P12LE
            | Pixel::YUV444P12BE
            | Pixel::GBRP
            | Pixel::GBRP10LE
            | Pixel::GBRP12LE => planar(Pixel::YUV444P, 1, 1),

            // Alpha, or something unrecognised. Both are correct this way and
            // only one of them is a surprise.
            _ => Self::Rgba,
        }
    }
}

/// How many threads one clip's decoder may use.
///
/// FFmpeg's own default is one per core, which on a fourteen-core machine means
/// one clip fanning out to fourteen. That is the right answer for a program
/// decoding one file as fast as it can, and the wrong one here: the renderer
/// already gives every layer its own decoding thread, so the fan-out multiplies
/// rather than divides. Three layers under the default is forty-two threads on
/// fourteen cores, each with its own pool of frame buffers.
///
/// Four is not a compromise. It is better on both counts at once, which is what
/// oversubscription looks like from the outside. Measured with
/// `ovs-decode-cost` on the test machine, three 1080p60 ProRes clips at about
/// three hundred megabits, nine hundred passes at sixty a second, the wall on a
/// generator so nothing else was decoding — three runs each:
///
/// | threads a clip | passes that held a stale frame | resident, a clip |
/// |---|---|---|
/// | the default, fourteen | 714–729 of 900 | 178–197 MB |
/// | two | 547–601 | 71–86 MB |
/// | **four** | **185–207** | **80–85 MB** |
/// | eight | 266–304 | 103–109 MB |
///
/// One clip on its own holds no frame at any of these, so nothing is given up
/// at the light end to buy the heavy one.
///
/// The number is measured on a machine with fourteen cores, and the reference
/// server has six. Nobody has run this there. `ovs-decode-cost` is
/// in the tree so that the same table can be produced on that machine in a few
/// minutes, without a display, a card or a server.
///
/// # What this does not fix, and may cost
///
/// The wall's frame rate is limited by something else: every frame a decoder
/// succeeds in producing has to be copied onto the card, and that copy is inside
/// the submit the render thread waits for — about 8.7 milliseconds a layer at
/// 1080p. Measured on the wall, three layers of the *easy* clip — the
/// one three decoders keep up with almost perfectly — run at forty frames a
/// second, while three layers of the heavy one, whose decoders cannot keep up
/// and therefore ask for far fewer copies, run at fifty-nine.
///
/// So this cap makes decoders deliver more frames, and more delivered frames is
/// more uploading. It may lower the wall's rate while making what the wall shows
/// fresher. **That trade has not been measured**, because measuring it means
/// replacing the binary on a machine running a show. It is kept because half the
/// memory and a decoder that delivers the frame it was asked for are both real,
/// and because the rate it might cost is a rate that was being held by showing
/// stale pictures.
///
/// # On the card
///
/// The same four, measured rather than inherited when the card's decoders
/// moved to FFmpeg's NVDEC accelerator (2026-11-16, the WSL on the development
/// machine, its 1080p H.265 clip through `ovs-decode-cost --threads N`):
///
/// | threads | seven players on two cores | the frame after a jump |
/// |---|---|---|
/// | one | 0 held, 0.66 cores | 83.5 ms average, 201.4 worst |
/// | two | 0 held, 0.66 cores | 74.7 ms average, 173.8 worst |
/// | **four** | **0 held, 0.67 cores** | **55.8 ms average, 125.8 worst** |
///
/// The engine is the card's either way; the threads read the bitstream and set
/// each picture up for it, and that is what a jump waits on.
///
/// **More is not free on the card, and too many is refused.** FFmpeg asks
/// NVDEC for a surface for every reference picture the stream may keep, five
/// more, and one for every frame thread (7.1.1, `ff_nvdec_frame_params` and
/// `ff_decode_get_hw_frames_ctx`), and NVDEC refuses a decoder much past 32.
/// The committed H.264 fixture is the hard case: it does not say how far it
/// reorders, so FFmpeg assumes the most its level allows and counts about
/// eighteen reference pictures. It was decoded on the card at eight threads and
/// refused at twelve; the 1080p H.265 clip was refused at twenty-four. FFmpeg
/// then decodes on the processor — correctly, and
/// [`VideoSource::decoder_name`] says so — but a show sized for the card does
/// not fit there. Four keeps every H.265 stream on the card, whose reference
/// buffer is at most seventeen pictures.
const DECODE_THREADS: usize = 4;

/// How many frames a player keeps ahead of a clip decoding on the card.
///
/// A loop point is a seek, and a seek on the card costs about 224 ms on
/// average and 313 at its worst — measured with `ovs-decode-cost --seeks`.
/// Twenty-four frames is four hundred milliseconds at sixty a second, which
/// covers the worst one with room left.
///
/// Sixteen was tried first, on the average rather than the worst, and left one
/// or two stale frames at each loop: eight loops of seven layers held 63. At
/// twenty-four the same run holds **none**. Thirty-two buys nothing.
///
/// The cost is memory, and it is smaller than it looks: about 24 MB a layer
/// above what the decoder holds anyway, against the 141 MB a software decoder's
/// own buffers take. Only a clip actually decoding on a card pays it.
///
/// # Measured on the `*_cuvid` decoders, and kept
///
/// Every figure above is the `*_cuvid` decoders', which rebuilt their decoder
/// at every seek. The NVDEC accelerator that replaced them keeps it, and
/// measured side by side in the WSL (2026-11-16, 1080p H.265, four threads,
/// `ovs-decode-cost --seeks 30`) a jump costs half and a loop a ninth:
///
/// | | the frame after a jump | the first frame of a loop |
/// |---|---|---|
/// | `hevc_cuvid` | 110.3 ms average, 213.8 worst | 22.9 ms average, 25.0 worst |
/// | `hevc_nvdec` | 54.8 ms average, 116.7 worst | 2.6 ms average, 2.9 worst |
///
/// So this could be shallower now. It is not changed, because the number it
/// was sized against is the reference server's worst jump and that has not
/// been measured on the new path there; a lead that is too deep costs memory,
/// and one that is too shallow costs the stale frames at every loop that
/// sixteen did.
const HARDWARE_FRAMES_AHEAD: u32 = 24;

/// What [`VideoSource::decoder_name`] says when the processor did the work.
const PROCESSOR: &str = "the processor";

/// Which decoder can hand a codec to the card, if any.
///
/// FFmpeg's own decoder for the codec, with its NVDEC accelerator: the picture
/// is decoded by a fixed-function block that draws nothing, lands in the card's
/// memory, and is copied back as NV12 — or P010 at ten bits — for the one frame
/// that is kept ([`download`]). The name is the accelerator's, so a log line
/// says which path a clip took. Measured on the test machine with the `*_cuvid`
/// decoders this replaced, 360 frames of 1080p60 (the block that decodes is the
/// same one):
///
/// | clip | software | on the card |
/// |---|---|---|
/// | H.264, grain-heavy | 5.99 CPU-seconds | **0.39** |
/// | HEVC, the same | 7.23 | **0.27** |
/// | H.264, easy | 1.58 | **0.29** |
///
/// A core a layer against a fifteenth of one, which is the difference between
/// seven layers fitting on the reference server and not.
///
/// # What is deliberately not on this list
///
/// **ProRes and HAP**, because no card decodes them — they stay on the
/// processor, which is what `None` here means and why the caller must cope with
/// it rather than refuse the clip.
///
/// **MJPEG.** `mjpeg_cuvid` exists, and every JPEG frame is a keyframe decoded
/// in about the time it takes to ask: there is nothing to win, and the cuvid
/// path has its own chroma quirks on 4:2:2 material. Software is the safer
/// answer where it is also the cheap one.
///
/// # Why not the `*_cuvid` decoders any more
///
/// They were what this opened until 0.3.11, and they leaked. A `*_cuvid`
/// decoder is a separate decoder with its own CUDA context, made for every clip
/// it opens, and it throws its NVDEC decoder away and builds a new one at every
/// seek — which a loop point is. Measured on the reference server's own driver
/// (550.163) on 2026-11-15, by counting the 2 MiB mappings of `/dev/nvidiactl`
/// in the renderer while the owner played: **one left behind for every H.265
/// clip opened** (34 opens, 34 mappings), **and about one for every twelve
/// loops** (36 loops, 3). A week of shows had taken the renderer to 6.5 GB
/// resident with nothing playing. The WSL's driver does not do it, which is why
/// nothing here saw it.
///
/// The accelerator decodes into one context for the whole process
/// ([`cuda_device`]) and keeps its decoder across a seek, so neither of those
/// happens on this path at all.
///
/// AV1 is asked for by name because FFmpeg's own AV1 decoder is the one that
/// carries the accelerator, and `libdav1d`, where a build has it, is found
/// first when FFmpeg is asked for "a decoder for AV1".
fn hardware_decoder(id: ffmpeg::codec::Id) -> Option<(ffmpeg::Codec, &'static str)> {
    use ffmpeg::codec::Id;

    let (decoder, name): (&str, &'static str) = match id {
        Id::H264 => ("h264", "h264_nvdec"),
        Id::HEVC => ("hevc", "hevc_nvdec"),
        Id::AV1 => ("av1", "av1_nvdec"),
        Id::VP9 => ("vp9", "vp9_nvdec"),
        Id::VP8 => ("vp8", "vp8_nvdec"),
        Id::MPEG1VIDEO => ("mpeg1video", "mpeg1_nvdec"),
        Id::MPEG2VIDEO => ("mpeg2video", "mpeg2_nvdec"),
        Id::MPEG4 => ("mpeg4", "mpeg4_nvdec"),
        Id::VC1 => ("vc1", "vc1_nvdec"),
        _ => return None,
    };
    let codec = ffmpeg::decoder::find_by_name(decoder)?;
    decodes_on_cuda(codec).then_some((codec, name))
}

/// Whether a decoder in this build can decode into a CUDA device.
///
/// Asked of the build rather than assumed, because an FFmpeg configured with
/// `--disable-everything` has no accelerator at all unless it is named — every
/// package up to 0.3.11 shipped that way. A decoder without one plays on the
/// processor, which is a slower machine rather than a broken one.
fn decodes_on_cuda(codec: ffmpeg::Codec) -> bool {
    let method = ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32;
    (0..)
        .map_while(|index| {
            // SAFETY: `codec` is one of FFmpeg's static decoder descriptions,
            // which live for the whole process. `avcodec_get_hw_config` reads
            // its table and returns null past the end, which ends the walk;
            // what it returns otherwise is static too.
            unsafe { ffi::avcodec_get_hw_config(codec.as_ptr(), index).as_ref() }
        })
        .any(|config| {
            config.device_type == ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA
                && config.methods & method != 0
        })
}

/// The card's CUDA context, made once and shared by every clip decoding on it.
///
/// A reference to an `AVHWDeviceContext`, which FFmpeg counts. Every decoder
/// opened on the card takes a reference of its own and gives it back when it
/// is freed; this one is never given back, so the context lives as long as the
/// renderer does. That is the point of it: a context made and destroyed with
/// every clip is exactly what the `*_cuvid` decoders did, and on the reference
/// server's driver it was not all given back (see [`hardware_decoder`]).
struct CudaDevice(NonNull<ffi::AVBufferRef>);

// SAFETY: the reference is made once, in `cuda_device`, and never written
// through or freed afterwards. All anything does with it is `av_buffer_ref`,
// whose count is atomic, and FFmpeg's CUDA device is built to be used by
// several decoders on several threads — each pushes the context onto its own
// thread for the length of a call.
unsafe impl Send for CudaDevice {}
// SAFETY: as for `Send`: nothing reads it but `av_buffer_ref`.
unsafe impl Sync for CudaDevice {}

/// The shared CUDA context, made the first time a clip asks for it.
///
/// A failure is not remembered. A machine with no NVIDIA driver answers fast
/// every time it is asked, and one whose CUDA failed to start — which the hall
/// machine's did until the afternoon of 2026-10-14 — should get the card back
/// with the next clip once it works, rather than at the next restart.
fn cuda_device() -> Result<&'static CudaDevice, ffmpeg::Error> {
    static DEVICE: Mutex<Option<&'static CudaDevice>> = Mutex::new(None);

    let mut held = DEVICE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(device) = *held {
        return Ok(device);
    }
    let mut made: *mut ffi::AVBufferRef = ptr::null_mut();
    // SAFETY: `made` is a valid place for FFmpeg to write the new reference
    // into; a null device name means the first card, and no options and no
    // flags are what FFmpeg's own `-hwaccel cuda` passes.
    let code = unsafe {
        ffi::av_hwdevice_ctx_create(
            &raw mut made,
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    if code < 0 {
        return Err(ffmpeg::Error::from(code));
    }
    let made = NonNull::new(made).ok_or(ffmpeg::Error::Bug)?;
    let device: &'static CudaDevice = Box::leak(Box::new(CudaDevice(made)));
    *held = Some(device);
    drop(held);
    Ok(device)
}

/// Copies a picture the card decoded into host memory, into `into`.
///
/// The one copy off the card this path makes, and only for a frame that is
/// kept: a seek decodes forward from a keyframe and every frame it passes stays
/// on the card. The `*_cuvid` decoders copied every frame they decoded, wanted
/// or not.
///
/// `into` is kept between frames and reused while the size and format hold,
/// for the reason [`VideoSource::rgba`] is: a fresh picture for every frame is
/// an allocation on the hot path.
fn download(
    path: &Path,
    from: &ffmpeg::frame::Video,
    into: &mut ffmpeg::frame::Video,
) -> Result<(), MediaError> {
    // SAFETY: `from` is a frame the decoder handed back in the CUDA format, so
    // FFmpeg guarantees it carries a live reference to the `AVHWFramesContext`
    // its picture came from, which is what is read. `into` is owned by this
    // source and referenced by nothing else, so freeing and reallocating its
    // buffers cannot pull memory out from under anybody, and when it is reused
    // it has exactly the format and size the copy will write.
    let code = unsafe {
        let source = from.as_ptr();
        let frames = (*source).hw_frames_ctx;
        if frames.is_null() {
            return Err(MediaError::Undecodable {
                path: path.to_path_buf(),
                reason: "the card returned a picture that does not say where it is".to_owned(),
            });
        }
        let software = (*(*frames).data.cast::<ffi::AVHWFramesContext>()).sw_format as i32;
        let destination = into.as_mut_ptr();
        let fits = !(*destination).buf[0].is_null()
            && (*destination).format == software
            && (*destination).width == (*source).width
            && (*destination).height == (*source).height;
        if !fits {
            ffi::av_frame_unref(destination);
            (*destination).format = software;
            (*destination).width = (*source).width;
            (*destination).height = (*source).height;
            let code = ffi::av_frame_get_buffer(destination, 0);
            if code < 0 {
                return Err(undecodable(path, &ffmpeg::Error::from(code)));
            }
        }
        ffi::av_hwframe_transfer_data(destination, source, 0)
    };
    if code < 0 {
        return Err(undecodable(path, &ffmpeg::Error::from(code)));
    }
    Ok(())
}

/// Whether one clip may be decoded on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hardware {
    /// The card if this machine has one and the build allows it.
    IfAllowed,
    /// The processor, whatever the machine has.
    Never,
}

/// Whether this build may decode on the card at all.
///
/// `OVS_HARDWARE_DECODE=off` forces software, which is what a comparison needs
/// and what somebody diagnosing a picture fault reaches for first: a clip that
/// looks wrong on the card and right off it is a different problem from one
/// that looks wrong both ways.
fn hardware_decode_allowed() -> bool {
    !std::env::var("OVS_HARDWARE_DECODE").is_ok_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        value == "off" || value == "0" || value == "false" || value == "no"
    })
}

/// How far ahead a sequential decode may run before seeking instead.
///
/// With all-intra media a seek costs one frame's decode wherever it lands, and
/// running forward costs one for every frame skipped — so the threshold is low
/// on purpose. It is not zero because the common case by far is "the next
/// frame", and seeking for that would throw away the decoder's state sixty
/// times a second.
const SEEK_AFTER: u32 = 2;

/// A colour converter that can be moved to the thread that decodes.
///
/// An `SwsContext` is an ordinary owned object: FFmpeg documents one as usable
/// from a single thread at a time, and this one is never shared — it is made by
/// the [`VideoSource`] that owns it, used only through `&mut self`, and freed
/// with it. The wrapper exists because the binding does not say so and
/// [`FrameSource`] must be `Send`: decoding happens on a worker thread, which is
/// the whole reason the render path never waits for a frame.
struct Scaler(ffmpeg::software::scaling::Context);

// SAFETY: exclusive ownership and no aliasing. The pointer inside is created in
// `VideoSource::open`, reachable only through `&mut VideoSource`, and destroyed
// by its own `Drop`. Nothing else in this process holds it, so moving the
// `VideoSource` to another thread moves the only handle there is.
unsafe impl Send for Scaler {}

/// One clip, open and ready to be asked for frames.
pub struct VideoSource {
    path: PathBuf,
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    /// Which decoder was opened, by FFmpeg's name for it.
    ///
    /// Kept because the fall back to the processor is silent by design — a
    /// machine with no card plays the show — and a silent fallback is one
    /// nobody notices until the cores run out. A test that means to compare the
    /// two paths can check it actually got two.
    decoder_name: &'static str,
    /// The colour converter, for a clip whose frames need converting.
    ///
    /// `None` for one that does not, which is most of them: a codec that
    /// produced planar eight-bit Y'CbCr is handed over as it is, and the
    /// conversion that used to run here for every frame of every layer does
    /// not exist.
    scaler: Option<Scaler>,
    /// The layout of the pictures as they reach [`convert`](Self::convert):
    /// what the processor decoded, or what a picture on the card was copied
    /// back as.
    ///
    /// Read off a decoded picture, never off the decoder, because before the
    /// first picture the decoder only knows what the file says. A clip on the
    /// card says `yuv420p` and arrives as NV12; the two are not the same bytes,
    /// and the converter and the shader have to be told which they have.
    /// `target`, `depth` and `scaler` all follow from it.
    format: ffmpeg::format::Pixel,
    /// What a frame is turned into before it goes up.
    target: Target,
    /// How many bits a sample carries, and where in its element they sit.
    ///
    /// Eight for everything this renderer has played until now. Ten-bit
    /// material keeps its ten bits from here to the card (owner, 2026-09-23):
    /// accepting HEVC Main 10 and then converting every frame down to eight on
    /// the processor would be the per-frame conversion the rest of this path
    /// exists to avoid, and a profile that is accepted rather than supported.
    depth: SampleDepth,
    /// Whether the samples run 0–255 rather than 16–235.
    full_range: bool,
    /// Which coefficients turn them back into colour.
    matrix: ColourMatrix,
    stream_index: usize,
    /// The stream's time base, for turning a presentation stamp into an index.
    time_base: ffmpeg::Rational,
    /// The clip's own frame rate, which is not the show's.
    rate: ffmpeg::Rational,
    /// The first presentation stamp, which is not always zero.
    start_pts: i64,
    size: SizePx,
    length: SourceLength,
    /// What came out last, so that asking twice costs nothing.
    ///
    /// A fader held still asks for the same frame for as long as it is held.
    last: Option<Frame>,
    /// Which index the decoder will produce next, when that is known.
    ///
    /// `None` after a seek, until a frame comes out and says where it landed.
    next_index: Option<u32>,
    /// Where the colour converter writes, when there is one, kept between
    /// frames.
    ///
    /// A fresh picture here costs eight megabytes at 1080p, asked for and given
    /// back sixty times a second for as long as the clip plays, and three layers
    /// doing it is most of a gigabyte a second through the allocator. That is a
    /// per-frame allocation on the hot path, which house rule §5 does not allow,
    /// and the converter will fill a picture it is given — so it is given the
    /// same one every time.
    ///
    /// What made this worth looking at was a soak: the renderer held about half
    /// a gigabyte with one clip playing and about seven and a half with three.
    /// Removing this allocation was not measured against that figure on its own,
    /// and the number that turned out to explain most of it is `DECODE_THREADS`.
    rgba: ffmpeg::frame::Video,
    /// Where the decoder writes, kept between frames.
    ///
    /// Reused for the same reason `rgba` is: a fresh one per frame is an
    /// allocation on the hot path, and this one is written sixty times a second
    /// per layer whether the frame is kept or not.
    decoded: ffmpeg::frame::Video,
    /// Where a picture the card decoded is copied back to, kept between
    /// frames for the same reason.
    downloaded: ffmpeg::frame::Video,
}

impl std::fmt::Debug for VideoSource {
    /// Prints what it is, never what is in it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VideoSource")
            .field("path", &self.path)
            .field("size", &self.size)
            .field("frames", &self.length.frame_count)
            .finish()
    }
}

impl VideoSource {
    /// Opens a clip and decodes its first picture.
    ///
    /// The first picture is what says how the clip decodes: whether the card
    /// took it, and what layout its pictures come in. Neither can be known
    /// before — the card's decoder is set up when the first picture's header
    /// is read, not when the decoder is opened — and the player asks for both
    /// the moment it is made. So it is decoded here, on the media worker's
    /// thread where opening happens anyway, and kept: asking for frame 0
    /// afterwards costs nothing.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when the file cannot be opened, holds no video,
    /// uses a codec this build has no decoder for, or is larger than any texture
    /// it could become.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        Self::open_with_threads(path, DECODE_THREADS)
    }

    /// Which decoder this clip opened, by FFmpeg's name for it.
    ///
    /// [`PROCESSOR`] when it is the codec's own software decoder, and the
    /// decoder's name — `h264_cuvid` and the like — when it is the card's.
    #[must_use]
    pub const fn decoder_name(&self) -> &'static str {
        self.decoder_name
    }

    /// How many bits a sample of this clip carries, and where in its element.
    ///
    /// Read off the decoder's own format, so a card handing back P010 and a
    /// processor handing back `yuv420p10le` are told apart — they are sixty-four
    /// times apart and there is no arithmetic that makes them the same.
    #[must_use]
    pub const fn depth(&self) -> SampleDepth {
        self.depth
    }

    /// What this source converts every frame through, if anything.
    ///
    /// `None` is the answer that matters. A conversion here is `swscale` walking
    /// every pixel of every frame on the processor, and for ten-bit material it
    /// would also be throwing two bits away — which is the thing the native
    /// ten-bit path exists not to do. It is a method rather than a comment so a
    /// test can assert it and a measurement can print it.
    #[must_use]
    pub fn conversion(&self) -> Option<String> {
        self.scaler.as_ref().map(|_| {
            let wanted = match self.target {
                Target::Rgba => ffmpeg::format::Pixel::RGBA,
                Target::Planar { format, .. } => format,
                Target::Interleaved { .. } => self.format,
            };
            format!("{:?} -> {:?}", self.format, wanted)
        })
    }

    /// Whether the samples of this clip use the whole range rather than 16-235.
    #[must_use]
    pub const fn full_range(&self) -> bool {
        self.full_range
    }

    /// Which coefficients turn this clip's samples back into colour.
    #[must_use]
    pub const fn matrix(&self) -> ColourMatrix {
        self.matrix
    }

    /// Opens a clip without letting the card near it.
    ///
    /// Per clip, where `OVS_HARDWARE_DECODE=off` is per process. Two callers
    /// want it: a test that decodes the same file both ways and compares the
    /// pictures, and somebody with a clip that looks wrong on a wall — a clip
    /// that looks wrong on the card and right off it is a different fault from
    /// one that looks wrong both ways, and that is worth being able to ask
    /// without restarting a show.
    ///
    /// # Errors
    ///
    /// The same as [`open`](VideoSource::open).
    pub fn open_on_processor(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        Self::open_with(path, DECODE_THREADS, Hardware::Never)
    }

    /// Opens a clip with a given number of decoding threads.
    ///
    /// Separate from [`open`](VideoSource::open) so that the cost of the choice
    /// can be measured — `ovs-decode-cost` varies it — rather than argued
    /// about. Zero means FFmpeg's own default, which is one thread per core.
    ///
    /// # Errors
    ///
    /// The same as [`open`](VideoSource::open).
    pub fn open_with_threads(path: impl AsRef<Path>, threads: usize) -> Result<Self, MediaError> {
        Self::open_with(path, threads, Hardware::IfAllowed)
    }

    /// The one that does the work, with both choices made explicit.
    fn open_with(
        path: impl AsRef<Path>,
        threads: usize,
        hardware: Hardware,
    ) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        initialise();

        let input = ffmpeg::format::input(&path).map_err(|error| undecodable(&path, &error))?;
        Self::with_input(path, input, threads, hardware)
    }

    /// Opens a clip whose container is already open, as [`open`](Self::open)
    /// would.
    ///
    /// For [`super::open_clip`], which opens the file once to find out whether
    /// it is HAP and should not make the decoder open it a second time.
    pub(super) fn from_input(
        path: PathBuf,
        input: ffmpeg::format::context::Input,
    ) -> Result<Self, MediaError> {
        Self::with_input(path, input, DECODE_THREADS, Hardware::IfAllowed)
    }

    fn with_input(
        path: PathBuf,
        input: ffmpeg::format::context::Input,
        threads: usize,
        hardware: Hardware,
    ) -> Result<Self, MediaError> {
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| MediaError::Undecodable {
                path: path.clone(),
                reason: "there is no video in this file".to_owned(),
            })?;

        let stream_index = stream.index();
        let time_base = stream.time_base();
        let start_pts = match stream.start_time() {
            // `AV_NOPTS_VALUE`. A container that does not say starts at zero,
            // which is what every frame stamp is then measured against.
            i64::MIN => 0,
            start => start,
        };

        // The rate the clip was authored at. It is not the rate the show runs
        // at and must not be confused with it: a 25 fps clip on a 60 Hz output
        // is an ordinary thing, and the playhead above this does that work.
        let rate = rate_of(&stream);
        let frames = frame_count(&stream, rate);

        let parameters = stream.parameters();

        // The card first, and the processor when there is no card, no driver,
        // no decoder for this codec, or no room for another session. Falling
        // back rather than failing is the whole point: the same binary has to
        // run on a machine with an RTX in it and on one without.
        let (decoder, decoder_name) =
            Self::open_decoder(&path, parameters.clone(), threads, hardware)?;

        let size = SizePx::new(decoder.width(), decoder.height());
        if size.width > MAX_IMAGE_SIDE || size.height > MAX_IMAGE_SIDE {
            return Err(MediaError::TooLarge {
                path,
                width: size.width,
                height: size.height,
            });
        }
        if size.width == 0 || size.height == 0 {
            return Err(MediaError::Undecodable {
                path,
                reason: "this clip has no picture size".to_owned(),
            });
        }

        // What the file says about its own colour, with a fallback that is the
        // convention rather than a guess: standard definition is BT.601 and
        // everything larger is BT.709, which is what every tool assumes of a
        // file that does not say.
        let full_range = matches!(decoder.color_range(), ffmpeg::color::Range::JPEG)
            || matches!(
                decoder.format(),
                ffmpeg::format::Pixel::YUVJ420P
                    | ffmpeg::format::Pixel::YUVJ422P
                    | ffmpeg::format::Pixel::YUVJ444P
            );
        let matrix = match decoder.color_space() {
            ffmpeg::color::Space::BT709 => ColourMatrix::Bt709,
            ffmpeg::color::Space::BT470BG | ffmpeg::color::Space::SMPTE170M => ColourMatrix::Bt601,
            // What Main 10 is usually tagged as, in both its forms. Constant
            // luminance is the rarer one and its matrix is not this — but a
            // file carrying it is vanishingly rare, and BT.2020's coefficients
            // are far closer to it than BT.709's.
            ffmpeg::color::Space::BT2020NCL | ffmpeg::color::Space::BT2020CL => {
                ColourMatrix::Bt2020
            }
            _ if size.height <= 576 => ColourMatrix::Bt601,
            _ => ColourMatrix::Bt709,
        };

        let mut source = Self {
            path,
            input,
            decoder,
            decoder_name,
            // All four are decided by the first picture, in `start`.
            scaler: None,
            format: ffmpeg::format::Pixel::None,
            target: Target::Rgba,
            depth: SampleDepth::Eight,
            full_range,
            matrix,
            stream_index,
            time_base,
            rate,
            start_pts,
            size,
            length: SourceLength {
                duration_ms: duration_ms(frames, rate),
                frame_count: frames,
            },
            last: None,
            next_index: None,
            rgba: ffmpeg::frame::Video::empty(),
            decoded: ffmpeg::frame::Video::empty(),
            downloaded: ffmpeg::frame::Video::empty(),
        };

        if let Err(error) = source.start() {
            if source.decoder_name == PROCESSOR {
                return Err(error);
            }
            // The card opened the clip and then could not decode it — a
            // profile NVDEC has no engine for, on a decoder that has nothing
            // to fall back to inside FFmpeg (AV1's own decoder decodes on a
            // card or not at all). The processor gets the clip from the top.
            tracing::info!(
                clip = %source.path.display(),
                decoder = source.decoder_name,
                "this clip decodes on the processor: the card could not decode its first picture ({error})"
            );
            source.decoder = Self::open_codec(None, parameters, threads, None)
                .map_err(|error| undecodable(&source.path, &error))?;
            source.decoder_name = PROCESSOR;
            source.seek(0)?;
            source.start()?;
        }
        Ok(source)
    }

    /// Decodes the first picture and decides from it how every picture after
    /// it is handled.
    ///
    /// Also where a clip that opened on the card and was decoded on the
    /// processor anyway is found out. FFmpeg does that by itself when the
    /// card's decoder cannot be set up — no engine for the profile, no session
    /// left — and says nothing; a picture that is not in the CUDA format is the
    /// only sign of it, and [`decoder_name`](Self::decoder_name) has to say so,
    /// because it decides how far ahead the player decodes.
    fn start(&mut self) -> Result<(), MediaError> {
        let Some(index) = self.pull()? else {
            return Err(MediaError::Undecodable {
                path: self.path.clone(),
                reason: "the clip has no picture in it".to_owned(),
            });
        };
        if self.decoder_name != PROCESSOR && self.decoded.format() != ffmpeg::format::Pixel::CUDA {
            tracing::info!(
                clip = %self.path.display(),
                decoder = self.decoder_name,
                "this clip decodes on the processor: the card would not take it"
            );
            self.decoder_name = PROCESSOR;
        }
        self.next_index = Some(index.saturating_add(1));
        let frame = Frame::new(index, self.convert()?);
        self.last = Some(frame);
        Ok(())
    }

    /// Decides what pictures in `format` are turned into, and makes the
    /// converter for them when they need one.
    ///
    /// At the clip's own size, whatever the conversion. Scaling to an output's
    /// size belongs to the compositor, which knows about mapping and rotation;
    /// doing it here would mean decoding the same clip twice for two walls.
    fn adopt(&mut self, format: ffmpeg::format::Pixel, size: SizePx) -> Result<(), MediaError> {
        let target = Target::for_format(format);
        let wanted = match target {
            Target::Rgba => ffmpeg::format::Pixel::RGBA,
            Target::Planar { format, .. } => format,
            // Handed over as it is and split on the way out, so there is
            // nothing for a converter to do.
            Target::Interleaved { .. } => format,
        };
        // Nothing at all when the decoder already produces what is wanted,
        // which is the ordinary case for H.264, HEVC and AV1. An `SwsContext`
        // asked to convert a format to itself still walks every pixel.
        self.scaler = if format == wanted {
            None
        } else {
            Some(Scaler(
                ffmpeg::software::scaling::Context::get(
                    format,
                    size.width,
                    size.height,
                    wanted,
                    size.width,
                    size.height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )
                .map_err(|error| undecodable(&self.path, &error))?,
            ))
        };
        self.format = format;
        self.target = target;
        self.depth = depth_of(format, target);
        Ok(())
    }

    /// The clip's own picture size.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }

    /// The clip's own frame rate, as an exact rational.
    #[must_use]
    pub fn rate(&self) -> ovs_types::geometry::FrameRate {
        ovs_types::geometry::FrameRate {
            numerator: self.rate.numerator().unsigned_abs(),
            denominator: self.rate.denominator().unsigned_abs().max(1),
        }
    }

    /// Turns a presentation stamp into a frame number.
    fn index_of(&self, pts: i64) -> u32 {
        let elapsed = pts.saturating_sub(self.start_pts);
        // seconds = pts * time_base; index = seconds * rate. In one step, in
        // wide arithmetic, so that a long clip at a fine time base cannot
        // overflow its way to the wrong frame.
        let numerator = i128::from(elapsed)
            * i128::from(self.time_base.numerator())
            * i128::from(self.rate.numerator());
        let denominator =
            i128::from(self.time_base.denominator()) * i128::from(self.rate.denominator().max(1));
        if denominator == 0 {
            return 0;
        }

        // Rounded, not truncated: a stamp a tick short of the next frame is
        // that frame, and every container rounds somewhere.
        let index = (numerator + denominator / 2) / denominator;
        u32::try_from(index.max(0)).unwrap_or(u32::MAX)
    }

    /// Puts the reader at the frame `index`, or just before it.
    fn seek(&mut self, index: u32) -> Result<(), MediaError> {
        let micros = i128::from(index) * 1_000_000 * i128::from(self.rate.denominator().max(1))
            / i128::from(self.rate.numerator().max(1));
        let offset =
            i128::from(self.start_pts) * i128::from(self.time_base.numerator()) * 1_000_000
                / i128::from(self.time_base.denominator().max(1));
        let target = i64::try_from(micros + offset).unwrap_or(i64::MAX);

        // Backwards, to the keyframe at or before the target. With all-intra
        // media that is the frame itself; with anything else the decode below
        // walks forward to it.
        self.input
            .seek(target, ..target)
            .map_err(|error| undecodable(&self.path, &error))?;
        // `avcodec_flush_buffers`. On the `*_cuvid` decoders it was the
        // expensive half of a seek on the card — 224 ms against 64 on the
        // processor, measured on the test machine — because it destroyed the
        // NVDEC decoder and made a new one. The accelerator keeps its decoder
        // and forgets only the pictures, which is why a loop on the card now
        // costs a ninth of what it did (`HARDWARE_FRAMES_AHEAD`). Draining the
        // decoder's output instead was tried and is wrong — it leaves the
        // packets already sent in the decoder's input queue, so a backward
        // seek to frame 5 came back with frame 26.
        self.decoder.flush();
        self.next_index = None;
        Ok(())
    }

    /// Decodes the next frame into [`decoded`](Self::decoded), if the clip has
    /// one left, and says which frame it is.
    ///
    /// The picture is **not** turned into an image here. A seek lands on the
    /// keyframe at or before the frame that was asked for, so decoding forward
    /// to it throws away every frame in between — up to a whole GOP — and
    /// converting those meant copying three planes of each, about three
    /// megabytes a frame at 1080p, to drop them. The caller converts the one it
    /// keeps.
    fn pull(&mut self) -> Result<Option<u32>, MediaError> {
        let mut ended = false;

        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    let stamp = self.decoded.pts().or_else(|| self.decoded.timestamp());
                    // A frame with no stamp is the one after the last one that
                    // had one, which is what a decoder that drops stamps means.
                    let index = stamp.map_or_else(
                        || self.next_index.unwrap_or(0),
                        |stamp| self.index_of(stamp),
                    );
                    return Ok(Some(index));
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                // Anything else means "not yet": the decoder wants another
                // packet before it can answer. A genuine failure comes back
                // from `send` below, or as the end of the file.
                Err(_) => {}
            }

            if ended {
                return Ok(None);
            }

            // One packet, then ask again. Sending a burst was tried on the
            // card, where the decode engine sits idle between a frame and the
            // next packet: it made every count worse — one layer went from six
            // stale frames in twenty seconds to nine hundred — because this
            // pump is asked for a *particular* frame rather than for the next
            // one, and running the file ahead of the playhead is work thrown
            // away at every seek. The engine's idle time is real and is not
            // won back here.
            self.send(&mut ended)?;
        }
    }

    /// Hands the decoder one more packet, or tells it the file is over.
    ///
    /// Returns whether there is any point sending another. Three reasons there
    /// might not be: the file has run out, in which case `ended` is set; the
    /// packet belonged to another stream, which is not a reason to stop; or the
    /// decoder is full.
    ///
    /// **A full decoder is not a broken one.** `avcodec_send_packet` answers
    /// `EAGAIN` to mean "take a frame out before putting another in", and it is
    /// the ordinary answer once the pump is keeping a decoder fed rather than
    /// handing it one packet at a time. Reading it as a failure killed four
    /// workers out of five the first time this pump sent a burst — silently,
    /// because a worker that fails says so once and stops.
    fn send(&mut self, ended: &mut bool) -> Result<bool, MediaError> {
        let Some((stream, packet)) = self.input.packets().next() else {
            // Everything has been read; let the decoder hand back what it is
            // still holding before saying the clip is over.
            *ended = true;
            let _ = self.decoder.send_eof();
            return Ok(false);
        };

        if stream.index() != self.stream_index {
            return Ok(true);
        }

        match self.decoder.send_packet(&packet) {
            Ok(()) => Ok(true),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => Ok(false),
            Err(error) => Err(undecodable(&self.path, &error)),
        }
    }

    /// Turns one decoded picture into something a texture can be made from.
    ///
    /// Converts first if the clip needs it, then copies the planes out tightly
    /// packed. Most clips need no conversion at all, and for those this is the
    /// copy and nothing else.
    fn convert(&mut self) -> Result<DecodedImage, MediaError> {
        let on_card = self.decoded.format() == ffmpeg::format::Pixel::CUDA;
        if on_card {
            download(&self.path, &self.decoded, &mut self.downloaded)?;
        }
        // Once, at the first picture, and again only if a stream changes
        // layout partway through — which is legal, and is otherwise a picture
        // drawn through the wrong converter.
        let (format, size) = {
            let decoded = if on_card {
                &self.downloaded
            } else {
                &self.decoded
            };
            (
                decoded.format(),
                SizePx::new(decoded.width(), decoded.height()),
            )
        };
        if format != self.format {
            self.adopt(format, size)?;
        }
        // Borrowed again, field by field, so the converter below can be
        // borrowed mutably beside it.
        let decoded = if on_card {
            &self.downloaded
        } else {
            &self.decoded
        };

        let picture = match &mut self.scaler {
            Some(scaler) => {
                scaler
                    .0
                    .run(decoded, &mut self.rgba)
                    .map_err(|error| undecodable(&self.path, &error))?;
                &self.rgba
            }
            None => decoded,
        };

        let size = SizePx::new(picture.width(), picture.height());
        match self.target {
            Target::Rgba => {
                let pixels = Self::plane(&self.path, picture, 0, size.width * 4, size.height)?;
                DecodedImage::new(pixels, size)
            }
            Target::Planar {
                subsample_x,
                subsample_y,
                ..
            } => {
                let wide = size.width.div_ceil(subsample_x);
                let tall = size.height.div_ceil(subsample_y);
                // A row is as many bytes as it has samples, times how wide a
                // sample is: one byte at eight bits and two at ten.
                let element = self.depth.bytes() as u32;
                let luma = Self::plane(&self.path, picture, 0, size.width * element, size.height)?;
                let cb = Self::plane(&self.path, picture, 1, wide * element, tall)?;
                let cr = Self::plane(&self.path, picture, 2, wide * element, tall)?;
                DecodedImage::planar(
                    luma,
                    cb,
                    cr,
                    size,
                    ChromaFormat {
                        subsample_x,
                        subsample_y,
                        full_range: self.full_range,
                        matrix: self.matrix,
                        depth: self.depth,
                    },
                )
            }
            Target::Interleaved {
                subsample_x,
                subsample_y,
                blue_first,
            } => {
                let wide = size.width.div_ceil(subsample_x);
                let tall = size.height.div_ceil(subsample_y);
                let element = self.depth.bytes();
                let luma = Self::plane(
                    &self.path,
                    picture,
                    0,
                    size.width * element as u32,
                    size.height,
                )?;
                let format = ChromaFormat {
                    subsample_x,
                    subsample_y,
                    full_range: self.full_range,
                    matrix: self.matrix,
                    depth: self.depth,
                };
                // NV12 goes up as it is: one row-wise copy, and the card
                // samples both components out of one two-component image.
                // Everything else interleaved is split here, because NV12 is
                // the shape of that image — see `DecodedImage::interleaved`.
                if blue_first && element == 1 && subsample_x == 2 && subsample_y == 2 {
                    let pairs =
                        Self::plane(&self.path, picture, 1, wide * 2 * element as u32, tall)?;
                    DecodedImage::interleaved(luma, pairs, size, format)
                } else {
                    let (cb, cr) =
                        Self::split(&self.path, picture, wide, tall, blue_first, element)?;
                    DecodedImage::planar(luma, cb, cr, size, format)
                }
            }
        }
        .ok_or_else(|| MediaError::Undecodable {
            path: self.path.clone(),
            reason: "the decoder returned a picture that does not describe itself".to_owned(),
        })
    }

    /// Opens the best decoder this machine has for a stream.
    ///
    /// Tries the card, and falls back to the processor with a line in the log
    /// saying which was taken and why. The log line is at info because it
    /// changes what the machine can hold — a show that fits seven layers on
    /// the card and four without it is two different machines, and somebody
    /// reading a log after a bad night is entitled to know which one they had.
    fn open_decoder(
        path: &Path,
        parameters: ffmpeg::codec::Parameters,
        threads: usize,
        hardware: Hardware,
    ) -> Result<(ffmpeg::decoder::Video, &'static str), MediaError> {
        let id = parameters.id();

        if hardware == Hardware::IfAllowed && hardware_decode_allowed() {
            if let Some((codec, name)) = hardware_decoder(id) {
                let opened = cuda_device().and_then(|device| {
                    Self::open_codec(Some(codec), parameters.clone(), threads, Some(device))
                });
                match opened {
                    Ok(decoder) => {
                        tracing::debug!(clip = %path.display(), decoder = name, "decoding on the card");
                        return Ok((decoder, name));
                    }
                    Err(error) => {
                        // Not a fault. A machine with no NVIDIA driver, or one
                        // whose card is out of decode sessions, plays the clip
                        // on the processor and the show carries on.
                        tracing::info!(
                            clip = %path.display(),
                            "this clip decodes on the processor: the card would not take it ({error})"
                        );
                    }
                }
            }
        }

        Self::open_codec(None, parameters, threads, None)
            .map(|decoder| (decoder, PROCESSOR))
            .map_err(|error| undecodable(path, &error))
    }

    /// Opens one decoder, either a named one or the codec's own, on the card
    /// when it is given one.
    fn open_codec(
        codec: Option<ffmpeg::Codec>,
        parameters: ffmpeg::codec::Parameters,
        threads: usize,
        card: Option<&CudaDevice>,
    ) -> Result<ffmpeg::decoder::Video, ffmpeg::Error> {
        let mut context = match codec {
            // `new_with_codec` fixes which decoder will be opened;
            // `set_parameters` then fills in the size, the extradata and the
            // rest without touching that choice.
            Some(codec) => {
                let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);
                context.set_parameters(parameters)?;
                context
            }
            None => ffmpeg::codec::context::Context::from_parameters(parameters)?,
        };

        // A device is all the accelerator needs. FFmpeg's own choice of
        // format, given one, is the card's whenever the card can take the
        // stream and the processor's when it cannot — which is the fall-back
        // this renderer wants, so no `get_format` of its own is written.
        if let Some(card) = card {
            // SAFETY: `context` is allocated and not yet opened, which is
            // when FFmpeg reads `hw_device_ctx`, and nothing has set it.
            // `av_buffer_ref` makes a new reference that the context owns from
            // here on: `avcodec_free_context` gives it back when the decoder
            // is dropped, and the shared one stays where it is.
            unsafe {
                let reference = ffi::av_buffer_ref(card.0.as_ptr());
                if reference.is_null() {
                    return Err(ffmpeg::Error::Other {
                        errno: ffmpeg::error::ENOMEM,
                    });
                }
                (*context.as_mut_ptr()).hw_device_ctx = reference;
            }
        }

        // Kept on the card as well; the measurement is under `DECODE_THREADS`.
        // Except FFmpeg's own default of one a core, which on the card is the
        // one choice that can be refused: sixteen threads and a deep reference
        // buffer ask NVDEC for more surfaces than it has. Only a measurement
        // asks for the default, and it should measure what the renderer does.
        let threads = if card.is_some() && threads == 0 {
            DECODE_THREADS
        } else {
            threads
        };
        context.set_threading(ffmpeg::threading::Config {
            kind: ffmpeg::threading::Type::Frame,
            count: threads,
        });
        // `LOW_DELAY` is deliberately **not** set.
        //
        // It was, with the reasoning that a media server wants the frame it
        // asked for now while a transcoder wants throughput. That reasoning
        // sounds right and the measurement says it is wrong on both counts.
        //
        // One 3840x2160 VP9 clip on the test machine, driven at 120 passes a
        // second — four times its own rate, which is what a beat-locked clip
        // reaches on a fast track:
        //
        // | | frames held of 400 | after a jump |
        // |---|---|---|
        // | with `LOW_DELAY` | 175 | 526 ms average, 1040 ms worst |
        // | without | 5 | 316 ms average, 477 ms worst |
        //
        // Held frames are what an operator sees as a clip that stutters. The
        // flag stops the decoder using its own parallelism, so it is slower at
        // producing the next frame *and* slower at producing the first one
        // after a seek — there is no trade being made here, only a loss.
        //
        // What made the reasoning wrong is that this renderer does not ask for
        // one frame at a time. It reads a ring, filled up to twenty-four
        // frames ahead on the card's decoders (`HARDWARE_FRAMES_AHEAD`), and a
        // pipeline is exactly what fills a ring. The flag was solving a
        // problem the ring already solved, and paying for it twice.
        //
        // H.264 barely notices either way, which is why this went unseen: every
        // clip measured before this one was H.264.
        context.decoder().video()
    }

    /// Copies the interleaved chroma plane out as two tightly packed planes.
    ///
    /// One pass over the bytes, the same pass `plane` would make, writing to
    /// two destinations instead of one. `samples` is how many chroma samples
    /// there are across, so the plane's rows are twice that many bytes.
    fn split(
        path: &Path,
        picture: &ffmpeg::frame::Video,
        samples: u32,
        rows: u32,
        blue_first: bool,
        element: usize,
    ) -> Result<(Vec<u8>, Vec<u8>), MediaError> {
        let samples = samples as usize;
        let rows = rows as usize;
        let stride = picture.stride(1);
        let data = picture.data(1);

        // A chroma sample is one element wide and the two colours alternate,
        // so a row is `samples * 2 * element` bytes however wide an element
        // is. NV12 splits bytes; P010 splits pairs of them, and the pairs are
        // copied whole rather than interpreted — this does not care that they
        // are little-endian, only that they stay together.
        //
        // # Why this is written out rather than done with a slice iterator
        //
        // It was, and it cost half the ten-bit throughput on the card. The
        // obvious form — `chunks(pair)`, `split_at`, two `extend_from_slice`
        // calls — is two bounds-checked calls of two bytes each per chroma
        // sample, which at 1080p is about a million calls a frame and at 4K
        // four. Measured on the test server: P010 at 1080p managed about 106
        // frames a second that way against 220 for NV12, while the *software*
        // ten-bit path — three planes, one memcpy each, no de-interleave at all
        // — sat at 240 with almost nothing held. The bytes were never the
        // problem; the per-sample call was.
        //
        // So: the rows are allocated once, sized, and written into by index,
        // with the element width resolved to a constant before the loop rather
        // than tested inside it. `as_chunks` gives fixed-size arrays, so there
        // is no length check per sample either.
        let row_bytes = samples * element;
        let pair = 2 * element;
        let mut cb = vec![0u8; row_bytes * rows];
        let mut cr = vec![0u8; row_bytes * rows];
        for row in 0..rows {
            let start = row * stride;
            let end = start + samples * pair;
            let Some(line) = data.get(start..end) else {
                return Err(MediaError::Undecodable {
                    path: path.to_path_buf(),
                    reason: format!(
                        "the decoder returned {} bytes of chroma, short of a picture",
                        data.len()
                    ),
                });
            };
            let out = row * row_bytes;
            let (blue_row, red_row) =
                (&mut cb[out..out + row_bytes], &mut cr[out..out + row_bytes]);
            // Which of the two the *first* element of each pair belongs to. NV12
            // and P010 put Cb first; NV21 is the other way round and is
            // otherwise identical, so this is the whole of the difference.
            let (first_row, second_row) = if blue_first {
                (blue_row, red_row)
            } else {
                (red_row, blue_row)
            };
            if element == 2 {
                for (index, sample) in line.as_chunks::<4>().0.iter().enumerate() {
                    let at = index * 2;
                    first_row[at] = sample[0];
                    first_row[at + 1] = sample[1];
                    second_row[at] = sample[2];
                    second_row[at + 1] = sample[3];
                }
            } else {
                for (index, sample) in line.as_chunks::<2>().0.iter().enumerate() {
                    first_row[index] = sample[0];
                    second_row[index] = sample[1];
                }
            }
        }
        Ok((cb, cr))
    }

    /// Copies one plane out, tightly packed, row by row from the top.
    ///
    /// A decoder's rows are padded to whatever suited it and a texture's are
    /// not. Copying the whole buffer would show the padding as a slanting
    /// picture, which is the classic way to get this wrong.
    fn plane(
        path: &Path,
        picture: &ffmpeg::frame::Video,
        index: usize,
        row_bytes: u32,
        rows: u32,
    ) -> Result<Vec<u8>, MediaError> {
        let row_bytes = row_bytes as usize;
        let rows = rows as usize;
        let stride = picture.stride(index);
        let data = picture.data(index);

        let mut out = Vec::with_capacity(row_bytes * rows);
        for row in 0..rows {
            let start = row * stride;
            let end = start + row_bytes;
            let Some(line) = data.get(start..end) else {
                return Err(MediaError::Undecodable {
                    path: path.to_path_buf(),
                    reason: format!(
                        "the decoder returned {} bytes for plane {index}, short of a picture",
                        data.len()
                    ),
                });
            };
            out.extend_from_slice(line);
        }
        Ok(out)
    }
}

impl FrameSource for VideoSource {
    fn length(&self) -> SourceLength {
        self.length
    }

    /// Deeper on the card, because a jump there costs three and a half times
    /// what it costs on the processor and a loop may not show a still frame.
    fn decode_depth(&self) -> ovs_types::depth::DecodeDepth {
        if self.depth.is_ten_bit() {
            ovs_types::depth::DecodeDepth::Ten
        } else {
            ovs_types::depth::DecodeDepth::Eight
        }
    }

    fn lead_frames(&self) -> u32 {
        if self.decoder_name == PROCESSOR {
            crate::media::player::FRAMES_AHEAD
        } else {
            HARDWARE_FRAMES_AHEAD
        }
    }

    fn frame(&mut self, index: u32) -> Result<Frame, MediaError> {
        let wanted = index.min(self.length.frame_count.saturating_sub(1));

        if let Some(last) = &self.last {
            if last.index == wanted {
                return Ok(last.clone());
            }
        }

        let sequential = self
            .next_index
            .is_some_and(|next| wanted >= next && wanted - next <= SEEK_AFTER);
        if !sequential {
            self.seek(wanted)?;
        }

        loop {
            let Some(index) = self.pull()? else {
                // The clip ended before the frame that was asked for: the
                // container's frame count was optimistic, or the file is short.
                // Holding the last good frame is what a late decoder does
                // everywhere else in this renderer; only a clip that has never
                // produced one is a failure.
                return self.last.clone().ok_or_else(|| MediaError::Undecodable {
                    path: self.path.clone(),
                    reason: format!("the clip ended before frame {wanted}"),
                });
            };

            self.next_index = Some(index.saturating_add(1));

            if index >= wanted {
                let frame = Frame::new(index, self.convert()?);
                self.last = Some(frame.clone());
                return Ok(frame);
            }
        }
    }
}

/// Where a sample's bits are, given what the decoder produced and what it is
/// being turned into.
///
/// Read off the decoder's own format rather than off the target, because the
/// two disagree in exactly one case and it is the important one: a card hands
/// back P010, whose ten bits are at the *top* of each sixteen-bit element,
/// while a software decoder hands back `yuv420p10le`, whose ten bits are at
/// the bottom. Both become three planes of sixteen-bit elements; the shader
/// has to be told which, or every ten-bit clip off the card is sixty-four
/// times too dark.
fn depth_of(format: ffmpeg::format::Pixel, target: Target) -> SampleDepth {
    use ffmpeg::format::Pixel;
    match format {
        Pixel::P010LE | Pixel::P010BE => SampleDepth::TenHigh,
        _ if target.is_ten_bit() => SampleDepth::TenLow,
        _ => SampleDepth::Eight,
    }
}

/// Starts FFmpeg once per process, and quietens it.
pub(super) fn initialise() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // The error is not worth propagating: every call after a failure fails
        // the same way with a message that says what is actually wrong.
        let _ = ffmpeg::init();
        // FFmpeg writes to stderr by default, at a level that describes every
        // stream in every file. The renderer's own log says what failed and
        // which file it was.
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    });
}

/// Turns an FFmpeg error into one of this renderer's.
fn undecodable(path: &Path, error: &ffmpeg::Error) -> MediaError {
    MediaError::Undecodable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

/// The rate a stream was authored at.
///
/// `avg_frame_rate` is what the container measured and `r_frame_rate` is what it
/// claims; the first is right for the fixed-rate media a show is built from and
/// the second is the fallback when a container says nothing.
pub(super) fn rate_of(stream: &ffmpeg::format::stream::Stream<'_>) -> ffmpeg::Rational {
    let average = stream.avg_frame_rate();
    if average.numerator() > 0 && average.denominator() > 0 {
        return average;
    }

    let claimed = stream.rate();
    if claimed.numerator() > 0 && claimed.denominator() > 0 {
        return claimed;
    }

    // A clip that will not say. 25 is a rate, not a guess at this clip's rate,
    // and the playhead above resamples whatever comes out to the show's rate
    // anyway.
    ffmpeg::Rational::new(25, 1)
}

/// How many frames a stream holds.
pub(super) fn frame_count(
    stream: &ffmpeg::format::stream::Stream<'_>,
    rate: ffmpeg::Rational,
) -> u32 {
    let counted = stream.frames();
    if counted > 0 {
        return u32::try_from(counted).unwrap_or(u32::MAX);
    }

    // No count in the container, which is ordinary for a stream that was
    // written as it was recorded. The duration and the rate give it.
    let duration = stream.duration();
    if duration > 0 {
        let time_base = stream.time_base();
        let frames = i128::from(duration)
            * i128::from(time_base.numerator())
            * i128::from(rate.numerator())
            / (i128::from(time_base.denominator().max(1)) * i128::from(rate.denominator().max(1)));
        return u32::try_from(frames.max(0)).unwrap_or(u32::MAX);
    }

    // A clip whose length nothing knows is one frame long until proven
    // otherwise: the scheduler clamps to it, so the picture is right and the
    // pass is short, rather than the other way round.
    1
}

/// One pass at the clip's own rate, in milliseconds.
pub(super) fn duration_ms(frames: u32, rate: ffmpeg::Rational) -> u64 {
    let numerator = u128::from(frames) * 1000 * rate.denominator().unsigned_abs() as u128;
    let denominator = u128::from(rate.numerator().unsigned_abs().max(1));
    u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
}
