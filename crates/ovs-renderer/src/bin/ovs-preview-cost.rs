//! What a preview costs, in hardware and in software.
//!
//! ```text
//! ovs-preview-cost                       one surface at 1280x720, both encoders
//! ovs-preview-cost --surfaces 4          four at once
//! ovs-preview-cost --size 1920x1080 --frames 600
//! ```
//!
//! # Why this exists
//!
//! The owner's answer to Q13 says a preview may have its rate
//! reduced, or be refused with a clear status, rather than being allowed to
//! threaten the show — and that the ninth preview must fall back to software
//! rather than being refused at all. The fall back is built and proved (seven
//! `h264_nvenc` sessions on the test machine, then `libx264`).
//!
//! The guard is not, and it cannot be until somebody knows what a software
//! preview actually costs. A threshold picked without that number would be
//! invented, and an invented threshold in the one path whose whole job is to
//! protect the show is worse than no guard at all.
//!
//! So this measures it: the same surface, the same frames, through each
//! encoder, reporting processor time as a fraction of a core. Nothing here
//! talks to a GPU surface, a server or a display — it encodes pictures it makes
//! itself — so it runs on the reference server as readily as on a laptop.
//!
//! # What the number means
//!
//! **Cores.** One core fully busy is 1.00 whatever it is measured on, which is
//! the only form of this number that can be compared between two machines. A
//! percentage would need the reader to remember how many cores each had.
//!
//! The hardware figure is processor time, not encode-engine time: NVENC does
//! the work on a part of the card that draws nothing, and what shows up here is
//! only the cost of handing frames to it. That is the point — it is what the
//! *show* pays, which is the thing the guard is protecting.

#[cfg(not(unix))]
fn main() {
    eprintln!("ovs-preview-cost needs an encoder, which is only built on unix");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::run()
}

#[cfg(unix)]
mod unix {
    use std::process::ExitCode;
    use std::time::Instant;

    use ovs_renderer::stream::PreviewEncoder;
    use ovs_types::geometry::{FrameRate, SizePx};

    /// What to say when the arguments do not make sense.
    const USAGE: &str =
        "usage: ovs-preview-cost [--surfaces N] [--frames N] [--size WxH] [--bitrate KBPS]";

    pub fn run() -> ExitCode {
        let mut surfaces = 1usize;
        let mut frames = 300u32;
        let mut size = SizePx::new(1280, 720);
        let mut kbps = 6_000u32;

        let mut args = std::env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--surfaces" => surfaces = parse(args.next().as_deref(), "--surfaces"),
                "--frames" => frames = parse(args.next().as_deref(), "--frames"),
                "--bitrate" => kbps = parse(args.next().as_deref(), "--bitrate"),
                "--size" => {
                    let Some(read) = args.next().as_deref().and_then(dimensions) else {
                        eprintln!("--size wants something like 1280x720");
                        return ExitCode::from(2);
                    };
                    size = read;
                }
                "--help" | "-h" => {
                    println!("{USAGE}");
                    return ExitCode::SUCCESS;
                }
                other => {
                    eprintln!("unknown option {other}");
                    return ExitCode::from(2);
                }
            }
        }

        let rate = FrameRate::FPS_60;
        println!(
            "{surfaces} surface(s) of {}x{} at {kbps} kbit/s, {frames} frames each",
            size.width, size.height
        );
        println!("a frame at sixty is 16666 us\n");

        // The pictures are made once and shown to both encoders, so what is
        // being compared is the encoding rather than the drawing.
        let pictures: Vec<_> = (0..frames.min(120))
            .map(|frame| picture(size, frame))
            .collect();

        printf_header();
        let mut any = false;
        // The renderer's own list rather than a copy of it: this measures
        // what the server would actually reach for, in the order it would, and
        // a build with a different FFmpeg under it reports what *that* one has.
        // A second list here was already one encoder out of date — it never
        // mentioned libopenh264, which is what a licence-clean build uses.
        for wanted in ovs_renderer::stream::ENCODERS {
            let Some(row) = measure(wanted, surfaces, frames, size, rate, kbps * 1000, &pictures)
            else {
                println!("{wanted:<14} not available on this machine");
                continue;
            };
            any = true;
            let (cores, kbits) = row;
            println!(
                "{wanted:<14} {cores:<10.2} {:<14.3} {kbits:.0}",
                cores / surfaces as f64
            );
        }

        if !any {
            eprintln!("no encoder this build can use is installed");
            return ExitCode::from(1);
        }

        println!(
            "\nthe second column is what one more preview costs, which is the\n\
             number the guard owner decision Q13 asks for has to be built on"
        );
        ExitCode::SUCCESS
    }

    fn printf_header() {
        println!(
            "{:<14} {:<10} {:<14} kbit/s",
            "encoder", "cores", "a surface"
        );
    }

    /// Encodes `frames` through `surfaces` encoders and reports what it cost.
    ///
    /// `None` when this build cannot open that encoder at all, which is a
    /// machine without the hardware rather than a failure.
    fn measure(
        // `'static` because that is what the encoder list holds and what
        // `open_with` keeps for its own log line.
        wanted: &'static str,
        surfaces: usize,
        frames: u32,
        size: SizePx,
        rate: FrameRate,
        bitrate: u32,
        pictures: &[ovs_renderer::media::DecodedImage],
    ) -> Option<(f64, f64)> {
        // By name, because `open` walks the list and would hand back hardware
        // every time on a machine that has it — which is right for a preview
        // and useless for a comparison.
        let mut open = Vec::new();
        for _ in 0..surfaces {
            open.push(PreviewEncoder::open_with(wanted, size, rate, bitrate).ok()?);
        }

        let began = Instant::now();
        let cpu_began = processor_seconds();
        let mut bytes = 0u64;
        let mut stream = Vec::new();
        for frame in 0..frames {
            let picture = &pictures[frame as usize % pictures.len()];
            for encoder in &mut open {
                stream.clear();
                if encoder.encode(picture, &mut stream).is_err() {
                    return None;
                }
                bytes += stream.len() as u64;
            }
        }
        let cpu = processor_seconds() - cpu_began;
        let elapsed = began.elapsed().as_secs_f64().max(f64::EPSILON);

        // The wire rate this would be at the show's own frame rate, rather than
        // at whatever rate this loop managed: a preview's bitrate is a property
        // of the stream, not of how fast it was produced.
        let seconds = f64::from(frames) / f64::from(rate.numerator.max(1))
            * f64::from(rate.denominator.max(1));
        Some((
            cpu / elapsed,
            bytes as f64 * 8.0 / 1000.0 / seconds.max(f64::EPSILON),
        ))
    }

    /// A picture that changes, so the encoder has something to compress.
    fn picture(size: SizePx, frame: u32) -> ovs_renderer::media::DecodedImage {
        let mut pixels = vec![0_u8; size.width as usize * size.height as usize * 4];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = (index % size.width as usize) as u32;
            let y = (index / size.width as usize) as u32;
            pixel[0] = ((x + frame * 7) % 256) as u8;
            pixel[1] = ((y + frame * 3) % 256) as u8;
            pixel[2] = ((x ^ y).wrapping_add(frame) % 256) as u8;
            pixel[3] = 0xFF;
        }
        ovs_renderer::media::DecodedImage::new(pixels, size).expect("a picture")
    }

    /// `WIDTHxHEIGHT`, or nothing.
    fn dimensions(text: &str) -> Option<SizePx> {
        let (width, height) = text.split_once(['x', 'X'])?;
        Some(SizePx::new(width.parse().ok()?, height.parse().ok()?))
    }

    /// Processor time this whole process has used, in seconds.
    ///
    /// The same reading `ovs-decode-cost` takes, and the same reason: the work
    /// happens on threads this program never sees.
    fn processor_seconds() -> f64 {
        const TICKS_A_SECOND: f64 = 100.0;
        let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
            return 0.0;
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            return 0.0;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let ticks = |at: usize| {
            fields
                .get(at)
                .and_then(|field| field.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        (ticks(11) + ticks(12)) / TICKS_A_SECOND
    }

    /// Reads a number, or explains which option wanted one.
    fn parse<T: std::str::FromStr>(value: Option<&str>, what: &str) -> T {
        match value.and_then(|text| text.parse().ok()) {
            Some(parsed) => parsed,
            None => {
                eprintln!("{what} wants a number");
                std::process::exit(2);
            }
        }
    }
}
