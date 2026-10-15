//! The service test pattern: what a wall shows while somebody is lining it up.
//!
//! Drawn here rather than shipped as a file, because a picture that was scaled
//! to fit would hide the very thing it is there to show. The pattern is always
//! exactly the surface it is drawn for, and it says out loud which surface that
//! is and what that surface is running at — so an engineer at the back of a
//! room reads "OUTPUT 2, 1920x1080p59.94" off the wall instead of taking
//! somebody's word for it.
//!
//! # What is in it, and why
//!
//! * **Colour bars**, in the classic arrangement, with a black-level ramp along
//!   the bottom. Wrong gamma, a stuck channel and a processor quietly crushing
//!   blacks all show up here and nowhere else.
//! * **The whole picture drifts**, slowly and diagonally. A frozen output and a
//!   live one showing a still look identical; a drifting pattern tells them
//!   apart from across a room.
//! * **A sweep** crosses it every five seconds. It is one bright diagonal edge
//!   with a fading trail, which is what tearing, frame doubling and a cable
//!   that cannot carry the rate all look wrong on.
//! * **A caption**, in the middle, naming the surface and its mode.
//!
//! # It is drawn every frame, and that costs more than it looks
//!
//! Drawing it every frame is the point: an animated pattern proves the path is
//! live. What it costs is **not** "a fill and one upload". Measured on the
//! Debian machine on 2026-09-26, one 2560x1440 output at 60 Hz, a 1280x720
//! composition of two layers with no effects and no media:
//!
//! ```text
//!                      compose   output render   fps
//!   pattern OUTPUTS     15.0 ms       7.7 ms     24-27
//!   pattern OFF          3.0 ms       2.0 ms     60.0
//! ```
//!
//! Turned off and straight back on again, three times, with the same numbers
//! each way: it is the pattern and nothing else. The frame budget at 60 Hz is
//! 16.7 ms and the pattern alone is 12 ms of it, so every frame misses its
//! vblank and waits for the next — which is why the fall is to 25 rather than to
//! 40, and why `dropped_frames` climbs by about 35 a second.
//!
//! Where it goes: `drift` copies the whole surface twice a row (30 MB of memory
//! traffic at this size), `sweep` blends per pixel over most of it, and the
//! finished picture is uploaded whole — 14.7 MB across the bus every frame. All
//! of it on the CPU, on the render thread, which is also why neither processor
//! looks busy while this happens: 10% CPU, 37% GPU, and 25 fps.
//!
//! **This belongs on the GPU**, as a fragment shader over the output's own
//! surface: the bars, the drift and the sweep are each a few lines of one, and
//! the caption is a small texture that changes only when the mode does. Nothing
//! would cross the bus per frame and the cost would be a pass over pixels the
//! card is drawing anyway.
//!
//! Until then it is honest to say what it is: a commissioning tool that halves
//! the frame rate of the thing it is commissioning. An engineer lining up a wall
//! is not reading the frame counter, but an operator who left it on is — and the
//! number they see is the tool's, not the show's.

// Visible to the crate rather than to this module alone: the clock and the
// countdown generators write with the same strokes, and a second font table for
// the same thirty-eight characters would be two sets of letters that could
// drift apart.
pub(crate) mod font;
pub mod pieces;

use ovs_types::geometry::{FrameRate, SizePx};

use crate::media::DecodedImage;

/// How long the sweep takes to cross, in seconds.
const SWEEP_SECONDS: f32 = 5.0;

/// The sweep's fading trail, at 1080p, in pixels.
///
/// Scaled with the surface so a 4K wall shows the same picture rather than the
/// same number of pixels.
const SWEEP_TRAIL_1080: f32 = 960.0;

/// How far the pattern drifts each second at 1080p, in pixels.
const DRIFT_X_1080: f32 = 100.0;
/// And downward — negative, because it rises.
const DRIFT_Y_1080: f32 = 40.0;

/// The largest surface this will draw.
///
/// The same ceiling the generators have: the cost of a pattern is a write
/// proportional to its area, and this is drawn on the render thread.
pub const MAX_PATTERN_SIDE: u32 = 8192;

/// One line of the caption.
struct Caption<'a> {
    text: &'a str,
    /// Cap height, as a fraction of the caption's own box.
    height: f32,
    /// Stroke width, as a fraction of the cap height.
    weight: f32,
    /// Extra space after each letter, as a fraction of the cap height.
    tracking: f32,
}

/// A drawn caption, kept so it is not drawn again every frame.
struct Label {
    pixels: Vec<u8>,
    side: u32,
}

/// The test pattern for one surface.
///
/// Holds everything that does not change — the bars, the caption — so that a
/// frame is a copy, a blend and nothing else.
pub struct ServicePattern {
    size: SizePx,
    title: String,
    format: String,
    /// One still copy of the bars, the size of the surface.
    tile: Vec<u8>,
    label: Label,
    /// The frame handed out, written in place so nothing is allocated per frame.
    image: DecodedImage,
}

impl std::fmt::Debug for ServicePattern {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServicePattern")
            .field("size", &self.size)
            .field("title", &self.title)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl ServicePattern {
    /// Draws the parts that do not move, for one surface.
    ///
    /// `title` is what the surface is called on the wall — `COMPOSITION`, or
    /// `OUTPUT 2`. The rate is the surface's own, so the caption is what the
    /// renderer is actually doing rather than what anybody assumed.
    #[must_use]
    pub fn new(size: SizePx, rate: FrameRate, title: &str) -> Self {
        let size = SizePx::new(
            size.width.clamp(1, MAX_PATTERN_SIDE),
            size.height.clamp(1, MAX_PATTERN_SIDE),
        );
        let format = format_mode(size, rate);
        let tile = draw_bars(size);
        let label = draw_label(size, title, &format);

        let pixels = vec![0; size.width as usize * size.height as usize * 4];
        // Cannot fail: the buffer was made from the size it is being given.
        let image = DecodedImage::new(pixels, size).expect("a buffer the size it was made for");

        Self {
            size,
            title: title.to_owned(),
            format,
            tile,
            label,
            image,
        }
    }

    /// Whether this pattern is still the right one for a surface.
    ///
    /// A wall that changed mode gets a new pattern; one that did not keeps the
    /// bars it has already drawn.
    #[must_use]
    pub fn matches(&self, size: SizePx, rate: FrameRate, title: &str) -> bool {
        self.size == size && self.title == title && self.format == format_mode(size, rate)
    }

    /// The surface this was drawn for.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }

    /// What the caption says the surface is running at.
    #[must_use]
    pub fn format(&self) -> &str {
        &self.format
    }

    /// What the caption calls the surface.
    ///
    /// As it was handed over, not as it was drawn: the drawing folds it to the
    /// letters the stroke font has, and this is what an operator typed.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Draws the frame for a moment in time.
    ///
    /// `seconds` is elapsed time, not a frame number: the pattern's movement is
    /// a property of the clock and must look the same at 50 Hz as at 60.
    pub fn draw(&mut self, seconds: f32) -> &DecodedImage {
        self.drift(seconds);
        self.sweep(seconds);
        self.caption();
        &self.image
    }

    /// Copies the bars in, shifted by where they have drifted to.
    fn drift(&mut self, seconds: f32) {
        let width = self.size.width as usize;
        let height = self.size.height as usize;
        let stride = width * 4;

        let scale_x = self.size.width as f32 / 1920.0;
        let scale_y = self.size.height as f32 / 1080.0;
        let shift_x = wrap(DRIFT_X_1080 * scale_x * seconds, width);
        // Upward, which is what the drawing does, and the direction that reads
        // as "running" rather than "falling".
        let shift_y = wrap(-DRIFT_Y_1080 * scale_y * seconds, height);

        let split = shift_x * 4;
        // Split the borrow by hand: the rows are read from the tile while they
        // are written into the image, and both live on `self`.
        let Self { tile, image, .. } = self;
        let pixels = image.pixels_mut();

        // **In one thread, and that is the measured answer.** Eight megabytes
        // of copying for a 1920x1080 picture every pass, and every row is
        // independent, so this looks exactly like work to share out. It was,
        // and on PC05 it was *slower*: 4,158 us a pass in one thread against
        // 5,427 shared out. Copying is limited by how fast memory will go, not
        // by how many cores ask it to, and the asking is not free. The noise
        // field next door is arithmetic and goes twice as fast across the
        // pool; this is not, and does not.
        for (row, line) in pixels.chunks_mut(stride).enumerate() {
            let source = (row + height - shift_y) % height;
            let from = source * stride;

            // Two copies per row, because the picture wraps: the right-hand
            // end of the bars comes round to the left.
            let (left, right) = tile[from..from + stride].split_at(stride - split);
            line[..split].copy_from_slice(right);
            line[split..stride].copy_from_slice(left);
        }
    }

    /// Blends the sweep over what is there.
    fn sweep(&mut self, seconds: f32) {
        let width = self.size.width as f32;
        let height = self.size.height as f32;
        let trail = SWEEP_TRAIL_1080 * ((width + height) / 3000.0);

        // The leading edge runs from the top-left corner to the bottom-right,
        // measured along the diagonal it is perpendicular to, and then keeps
        // going until the trail has left the picture too.
        let reach = (width + height) / std::f32::consts::SQRT_2;
        let speed = reach / SWEEP_SECONDS;
        let cycle = SWEEP_SECONDS + trail / speed;
        let edge = speed * (seconds % cycle);
        let start = edge - trail;
        if start >= reach {
            return;
        }

        // The ramp, as a table, worked out once per frame instead of once per
        // pixel. It is a handful of line segments and this is two million
        // pixels: the difference is the difference between a commissioning tool
        // that costs frames and one that does not.
        let mut ramp = [0_u16; RAMP_STEPS + 1];
        for (step, entry) in ramp.iter_mut().enumerate() {
            *entry = (sweep_alpha(step as f32 / RAMP_STEPS as f32) * 256.0) as u16;
        }

        let stride = self.size.width as usize * 4;
        let columns = self.size.width as usize;
        let pixels = self.image.pixels_mut();

        // How far along the trail one step to the right goes. The distance
        // along the sweep's own axis is (x + y) / √2, so within a row it is a
        // straight line and can be walked rather than recomputed.
        let per_pixel = (1.0 / std::f32::consts::SQRT_2) / trail * RAMP_STEPS as f32;

        // In one thread too, for the same reason the drift is: this touches a
        // diagonal band of the picture rather than all of it, so there is less
        // work here than in the copy that was measured as not worth sharing.
        {
            for (index, line) in pixels.chunks_mut(stride).enumerate() {
                let row = index as f32;
                let first = (start * std::f32::consts::SQRT_2 - row).max(0.0) as usize;
                let last = ((edge * std::f32::consts::SQRT_2 - row).max(0.0) as usize).min(columns);
                if first >= last {
                    continue;
                }

                let mut along = (((first as f32 + row) / std::f32::consts::SQRT_2 - start) / trail)
                    * RAMP_STEPS as f32;

                for x in first..last {
                    let step = (along as usize).min(RAMP_STEPS);
                    along += per_pixel;

                    let alpha = ramp[step];
                    if alpha == 0 {
                        continue;
                    }

                    let at = x * 4;
                    for channel in 0..3 {
                        let was = u16::from(line[at + channel]);
                        // 256ths, so the divide is a shift.
                        line[at + channel] = (was + (((255 - was) * alpha) >> 8)) as u8;
                    }
                }
            }
        }
    }

    /// Blends the caption into the middle.
    fn caption(&mut self) {
        let side = self.label.side as usize;
        if side == 0 {
            return;
        }

        let width = self.size.width as usize;
        let height = self.size.height as usize;
        let left = (width - side.min(width)) / 2;
        let top = (height - side.min(height)) / 2;
        let stride = width * 4;
        let label_stride = side * 4;
        let pixels = self.image.pixels_mut();

        for y in 0..side.min(height) {
            for x in 0..side.min(width) {
                let from = y * label_stride + x * 4;
                let alpha = u16::from(self.label.pixels[from + 3]);
                if alpha == 0 {
                    continue;
                }

                let at = (top + y) * stride + (left + x) * 4;
                for channel in 0..3 {
                    let over = u16::from(self.label.pixels[from + channel]);
                    let under = u16::from(pixels[at + channel]);
                    pixels[at + channel] = ((under * (255 - alpha) + over * alpha) / 255) as u8;
                }
            }
        }
    }
}

/// How a surface's mode is written on the wall: `1920x1080p59.94`.
///
/// Whole rates lose their decimals and broadcast rates keep two, which is how
/// they are written everywhere else in the building. Nothing is rounded to a
/// "nice" number: 59.94 and 60 are different modes and a test card that blurs
/// them is worse than one that says nothing.
#[must_use]
pub fn format_mode(size: SizePx, rate: FrameRate) -> String {
    let hertz = rate.as_f64();
    let written = if (hertz - hertz.round()).abs() < 0.001 {
        format!("{}", hertz.round() as i64)
    } else {
        format!("{hertz:.2}")
    };

    format!("{}x{}p{written}", size.width, size.height)
}

/// Where a drift has got to, wrapped into the picture.
fn wrap(distance: f32, span: usize) -> usize {
    if span == 0 {
        return 0;
    }
    let span_f = span as f32;
    let wrapped = distance % span_f;
    let positive = if wrapped < 0.0 {
        wrapped + span_f
    } else {
        wrapped
    };
    (positive as usize).min(span - 1)
}

/// How finely the sweep's ramp is sampled.
const RAMP_STEPS: usize = 256;

/// The sweep's brightness across its trail, from the drawing's own ramp.
fn sweep_alpha(fraction: f32) -> f32 {
    const RAMP: [(f32, f32); 9] = [
        (0.00, 0.00),
        (0.20, 0.02),
        (0.40, 0.06),
        (0.58, 0.14),
        (0.72, 0.28),
        (0.84, 0.48),
        (0.92, 0.70),
        (0.97, 0.88),
        (1.00, 1.00),
    ];

    if fraction <= 0.0 {
        return 0.0;
    }
    if fraction >= 1.0 {
        return 1.0;
    }

    for pair in RAMP.windows(2) {
        let (from, low) = pair[0];
        let (to, high) = pair[1];
        if fraction <= to {
            let across = (fraction - from) / (to - from);
            return low + (high - low) * across;
        }
    }

    1.0
}

/// Draws the bars, once.
fn draw_bars(size: SizePx) -> Vec<u8> {
    /// The seven top bars, at 75%.
    const TOP: [[u8; 3]; 7] = [
        [0xBF, 0xBF, 0xBF],
        [0xBF, 0xBF, 0x00],
        [0x00, 0xBF, 0xBF],
        [0x00, 0xBF, 0x00],
        [0xBF, 0x00, 0xBF],
        [0xBF, 0x00, 0x00],
        [0x00, 0x00, 0xBF],
    ];
    /// The reversed strip under them, which is what a chroma error skews.
    const MIDDLE: [[u8; 3]; 7] = [
        [0x00, 0x00, 0xBF],
        [0x00, 0x00, 0x00],
        [0xBF, 0x00, 0xBF],
        [0x00, 0x00, 0x00],
        [0x00, 0xBF, 0xBF],
        [0x00, 0x00, 0x00],
        [0xBF, 0xBF, 0xBF],
    ];

    let width = size.width as usize;
    let height = size.height as usize;
    let mut pixels = vec![0_u8; width * height * 4];

    let upper = (height as f32 * 0.67) as usize;
    let middle = upper + (height as f32 * 0.08) as usize;
    let bar = width as f32 / 7.0;

    for y in 0..height {
        for x in 0..width {
            let colour = if y < upper {
                TOP[((x as f32 / bar) as usize).min(6)]
            } else if y < middle {
                MIDDLE[((x as f32 / bar) as usize).min(6)]
            } else {
                bottom_colour(x as f32 / width as f32)
            };

            let at = (y * width + x) * 4;
            pixels[at] = colour[0];
            pixels[at + 1] = colour[1];
            pixels[at + 2] = colour[2];
            pixels[at + 3] = 0xFF;
        }
    }

    pixels
}

/// The bottom strip: the two reference patches, the black-level ramp and the
/// white and black ends.
///
/// The three steps in the middle are just above, at, and just below black. A
/// processor that crushes blacks shows two of them as one, which is the fault
/// this strip exists to catch.
fn bottom_colour(across: f32) -> [u8; 3] {
    match across {
        x if x < 0.18 => [0x00, 0x21, 0x4C],
        x if x < 0.36 => [0xFF, 0xFF, 0xFF],
        x if x < 0.54 => [0x32, 0x00, 0x6A],
        x if x < 0.62 => [0x11, 0x11, 0x11],
        x if x < 0.675 => [0x05, 0x05, 0x05],
        x if x < 0.73 => [0x11, 0x11, 0x11],
        x if x < 0.785 => [0x1D, 0x1D, 0x1D],
        x if x < 0.82 => [0x11, 0x11, 0x11],
        x if x < 0.91 => [0xFF, 0xFF, 0xFF],
        _ => [0x00, 0x00, 0x00],
    }
}

/// Draws the caption disc, once.
fn draw_label(size: SizePx, title: &str, format: &str) -> Label {
    let side = size.width.min(size.height) / 2;
    if side < 8 {
        // A surface too small to write on. The bars are still worth drawing.
        return Label {
            pixels: Vec::new(),
            side: 0,
        };
    }

    let span = side as usize;
    let mut pixels = vec![0_u8; span * span * 4];
    let centre = side as f32 / 2.0;
    let radius = centre - 1.0;
    let border = (side as f32 * 0.005).max(2.0);

    for y in 0..span {
        for x in 0..span {
            let distance =
                ((x as f32 + 0.5 - centre).powi(2) + (y as f32 + 0.5 - centre).powi(2)).sqrt();
            let at = (y * span + x) * 4;

            if distance > radius {
                continue;
            }

            // A ring, and a dark disc inside it. Both soften over the last
            // pixel so the edge does not crawl when the pattern drifts under
            // it.
            let (colour, alpha) = if distance > radius - border {
                (255.0, 0.75 * (radius - distance).min(1.0))
            } else {
                (0.0, 0.75 * (radius - border - distance).clamp(0.0, 1.0))
            };

            pixels[at] = colour as u8;
            pixels[at + 1] = colour as u8;
            pixels[at + 2] = colour as u8;
            pixels[at + 3] = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }

    // **The title folded to what the strokes can write, and only the title.**
    // An operator typed it, so it may be lower case or carry an umlaut. The
    // format line must *not* go through this: `1920x1080p59.94` would come out
    // `1920X1080P59.94`, and the lower-case `x` and `p` are the convention.
    //
    // Before the widths below are computed, because `ß` becomes two letters and
    // the shrink-to-fit is measured from the length.
    let title = font::writable(title);

    // The lines, in the proportions the drawing gives them: a quiet label, the
    // name of the surface, and what it is running at.
    //
    // How much of the disc there is at each height: the name sits across the
    // middle and gets nearly the full diameter, the other two are closer to the
    // curve and get less of it.
    let vmin = size.width.min(size.height) as f32 / 100.0;
    let wide = 0.86 * side as f32;
    let narrow = 0.76 * side as f32;

    let named = Caption {
        text: &title,
        height: vmin * 7.0,
        weight: 0.13,
        tracking: 0.08,
    };
    // Held here because the captions borrow from it.
    // The mode line's height is the floor: a name smaller than what it is
    // running at puts the least important line on the card in the largest type.
    let wrapped = wrap_title(&title, &named, wide, vmin * 4.0);

    let mut lines: Vec<(Caption<'_>, f32)> = Vec::with_capacity(wrapped.len() + 2);
    lines.push((
        Caption {
            text: "TESTPATTERN GENERATOR",
            height: vmin * 2.6,
            weight: 0.09,
            tracking: 0.10,
        },
        narrow,
    ));
    for text in &wrapped {
        lines.push((Caption { text, ..named }, wide));
    }
    lines.push((
        Caption {
            text: format,
            height: vmin * 4.0,
            weight: 0.10,
            tracking: 0.04,
        },
        narrow,
    ));

    // Nothing is allowed out of the disc. A line that still does not fit is set
    // smaller rather than clipped: COMPOSITION is half again as long as
    // OUTPUT 1, and an operator reading a wall from the back of a room should
    // get the whole word.
    let lines: Vec<Caption<'_>> = lines
        .into_iter()
        .map(|(line, across)| {
            let width = line_width(&line);
            let height = if width > across {
                line.height * across / width
            } else {
                line.height
            };
            Caption { height, ..line }
        })
        .collect();

    let gaps: f32 = lines.iter().map(|line| line.height * 1.45).sum();
    let mut baseline = centre - gaps / 2.0;
    for line in &lines {
        baseline += line.height * 1.45;
        write_line(
            &mut pixels,
            span,
            line,
            centre,
            baseline - line.height * 0.3,
        );
    }

    Label { pixels, side }
}

/// How wide a line is when it is set at its own height.
fn line_width(line: &Caption<'_>) -> f32 {
    let advance = (font::ADVANCE + line.tracking) * line.height;
    (advance * line.text.chars().count() as f32 - line.tracking * line.height).max(f32::EPSILON)
}

/// Breaks a name that would otherwise be set smaller than the mode under it.
///
/// **Not simply "does not fit".** The name is set at 7 vmin on purpose and most
/// names are wider than the disc at that size; shrinking them is the ordinary
/// case and the right one. `floor` is the question that matters — the height of
/// the line below, which the name must not end up smaller than, because that
/// would put the least important thing on the card in the largest letters. The
/// first version of this asked only about fitting and broke `INTERNAL DP` in
/// half.
///
/// On a 1280x720 card `UPSTAGE LEFT LED PROCESSOR 2 (BUHNE)` came out at 12
/// pixels against the mode's 29; on two lines it is 25, and the name is what
/// somebody on a ladder is being given over the radio.
///
/// Two lines at most, broken at the space nearest the middle rather than by
/// filling the first line, so that for an ordinary name they come out about the
/// same length.
///
/// **What it does not promise is an even break.** A name that is one enormous
/// word and one short one — `A SUPERCALIFRAGILISTIC…` — splits into `A` over the
/// rest, because that really is very slightly larger than one line and the code
/// takes whatever is larger. Refusing it would need a threshold for "even
/// enough", and there is nothing here to justify a number with. What is
/// guaranteed is the thing that matters: a break never makes the name smaller
/// than leaving it alone.
///
/// A name with no space in it is left whole and set smaller, and so is one that
/// gains nothing by being broken. Splitting inside a word would look like the
/// text had been corrupted, which is worse than small.
fn wrap_title(title: &str, line: &Caption<'_>, across: f32, floor: f32) -> Vec<String> {
    let whole = || vec![title.to_owned()];
    let fitted = |width: f32| {
        if width > across {
            line.height * across / width
        } else {
            line.height
        }
    };
    let as_wide_as = |text: &str| {
        line_width(&Caption {
            text,
            height: line.height,
            weight: line.weight,
            tracking: line.tracking,
        })
    };

    let one = fitted(line_width(line));
    if one >= floor {
        return whole();
    }

    let characters: Vec<char> = title.chars().collect();
    let middle = characters.len() / 2;
    let Some(at) = characters
        .iter()
        .enumerate()
        .filter(|(_, character)| **character == ' ')
        .min_by_key(|(at, _)| at.abs_diff(middle))
        .map(|(at, _)| at)
    else {
        return whole();
    };

    let split: Vec<String> = vec![
        characters[..at].iter().collect(),
        characters[at + 1..].iter().collect(),
    ];
    // Measured rather than assumed: a break that leaves one half nearly as long
    // as the whole buys nothing, and two small lines are worse than one.
    let two = fitted(
        split
            .iter()
            .map(|half| as_wide_as(half))
            .fold(0.0, f32::max),
    );
    if two > one {
        split
    } else {
        whole()
    }
}

/// Writes one line, centred on `centre_x`, with its baseline at `baseline`.
fn write_line(pixels: &mut [u8], span: usize, line: &Caption<'_>, centre_x: f32, baseline: f32) {
    let advance = (font::ADVANCE + line.tracking) * line.height;
    let mut pen = centre_x - line_width(line) / 2.0;
    let stroke = line.weight * line.height;

    for character in line.text.chars() {
        for polyline in font::glyph(character) {
            for pair in polyline.windows(2) {
                let from = (
                    pen + pair[0].0 * line.height,
                    baseline - line.height + pair[0].1 * line.height,
                );
                let to = (
                    pen + pair[1].0 * line.height,
                    baseline - line.height + pair[1].1 * line.height,
                );
                stroke_segment(pixels, span, from, to, stroke);
            }
        }
        pen += advance;
    }
}

/// Draws one straight stroke, softened at its edges.
fn stroke_segment(pixels: &mut [u8], span: usize, from: (f32, f32), to: (f32, f32), width: f32) {
    let half = width / 2.0;
    let left = ((from.0.min(to.0) - half - 1.0).floor().max(0.0)) as usize;
    let right = ((from.0.max(to.0) + half + 1.0).ceil().min(span as f32)) as usize;
    let top = ((from.1.min(to.1) - half - 1.0).floor().max(0.0)) as usize;
    let bottom = ((from.1.max(to.1) + half + 1.0).ceil().min(span as f32)) as usize;

    for y in top..bottom {
        for x in left..right {
            let distance = distance_to_segment((x as f32 + 0.5, y as f32 + 0.5), from, to);
            let alpha = (half + 0.5 - distance).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }

            // White over whatever the disc put there, and never less opaque
            // than it already was: letters cross the ring, and a letter that
            // punched a hole in it would look like damage.
            let at = (y * span + x) * 4;
            for channel in 0..3 {
                let under = f32::from(pixels[at + channel]);
                pixels[at + channel] = (under + (255.0 - under) * alpha) as u8;
            }
            let was = f32::from(pixels[at + 3]);
            pixels[at + 3] = was.max(alpha * 255.0) as u8;
        }
    }
}

/// How far a point is from a line segment.
fn distance_to_segment(point: (f32, f32), from: (f32, f32), to: (f32, f32)) -> f32 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx * dx + dy * dy;
    let along = if length <= f32::EPSILON {
        0.0
    } else {
        (((point.0 - from.0) * dx + (point.1 - from.1) * dy) / length).clamp(0.0, 1.0)
    };

    let nearest = (from.0 + along * dx, from.1 + along * dy);
    ((point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(image: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * image.size().width + x) * 4) as usize;
        let pixels = image.pixels();
        [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
    }

    #[test]
    fn a_mode_is_written_the_way_it_is_spoken() {
        assert_eq!(
            format_mode(SizePx::new(1920, 1080), FrameRate::FPS_59_94),
            "1920x1080p59.94"
        );
        assert_eq!(
            format_mode(SizePx::new(1920, 1080), FrameRate::FPS_60),
            "1920x1080p60",
            "a whole rate keeps no decimals"
        );
        assert_eq!(
            format_mode(SizePx::new(3840, 2160), FrameRate::FPS_50),
            "3840x2160p50"
        );
    }

    #[test]
    fn every_row_of_the_drift_comes_from_the_row_above_the_one_before_it() {
        // The drift is a rotation of the tile and nothing else: eight megabytes
        // of copying a pass, now shared out between threads. A band told the
        // wrong first row would put a block of rows from the wrong place into
        // the middle of the picture — a seam, which appears only on a machine
        // with the right number of cores and arrives as a photograph.
        //
        // The tile is replaced by one whose rows are each marked with their own
        // index, so where a row came from is readable rather than guessed at.
        // Only rows matter here: the sideways part of the drift happens inside
        // a row and no band can affect it.
        let size = SizePx::new(64, 90);
        let mut pattern = ServicePattern::new(size, FrameRate::FPS_60, "SEAM");
        let (width, height) = (size.width as usize, size.height as usize);
        let stride = width * 4;
        for (row, line) in pattern.tile.chunks_mut(stride).enumerate() {
            line.fill(u8::try_from(row).expect("under 256 rows"));
        }

        for seconds in [0.0_f32, 0.37, 1.9, 4.5, 17.25] {
            pattern.drift(seconds);
            let pixels = pattern.image.pixels();
            let first = pixels[0];
            for row in 0..height {
                let wanted =
                    u8::try_from((usize::from(first) + row) % height).expect("under 256 rows");
                let at = row * stride;
                assert!(
                    pixels[at..at + stride].iter().all(|&byte| byte == wanted),
                    "at {seconds}s, row {row} should have come from tile row {wanted}"
                );
            }
        }
    }

    #[test]
    fn the_pattern_is_exactly_the_surface_it_was_asked_for() {
        // The whole reason it is drawn rather than shipped: a pattern scaled to
        // fit would hide the mis-set mode it exists to reveal.
        let mut pattern = ServicePattern::new(SizePx::new(320, 200), FrameRate::FPS_60, "OUTPUT 1");
        let image = pattern.draw(0.0);

        assert_eq!(image.size(), SizePx::new(320, 200));
        assert_eq!(image.pixels().len(), 320 * 200 * 4);
    }

    #[test]
    fn the_bars_are_in_it() {
        // At the start of a pass the picture has not drifted and the sweep is
        // still in the corner, so every bar is where it was drawn.
        let mut pattern =
            ServicePattern::new(SizePx::new(1920, 1080), FrameRate::FPS_60, "OUTPUT 1");
        let image = pattern.draw(0.0);

        let grey = pixel(image, 100, 100);
        assert_eq!(
            [grey[0], grey[1], grey[2]],
            [0xBF, 0xBF, 0xBF],
            "the first bar"
        );
        assert_eq!(grey[3], 0xFF, "opaque, like everything else it draws");

        let blue = pixel(image, 1820, 100);
        assert_eq!(
            [blue[0], blue[1], blue[2]],
            [0x00, 0x00, 0xBF],
            "the last bar"
        );

        // And the white patch in the black-level strip along the bottom, which
        // is what tells a crushed picture from a correct one.
        let white = pixel(image, 1700, 1060);
        assert_eq!([white[0], white[1], white[2]], [0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn it_moves() {
        // A frozen output and a live one showing a still look identical. This
        // is the thing that tells them apart, so it is worth a test.
        let mut pattern = ServicePattern::new(SizePx::new(640, 360), FrameRate::FPS_60, "OUTPUT 1");
        let first: Vec<u8> = pattern.draw(0.0).pixels().to_vec();
        let later = pattern.draw(0.5).pixels();

        assert_ne!(first.as_slice(), later, "the picture is the same as it was");
    }

    #[test]
    fn the_caption_is_drawn_over_the_middle() {
        // Something dark and something white, in the middle of a picture that
        // is otherwise bars: the disc and the lettering.
        let mut pattern = ServicePattern::new(SizePx::new(960, 540), FrameRate::FPS_60, "OUTPUT 1");
        let image = pattern.draw(3.0);

        let middle = pixel(image, 480, 270);
        assert!(
            middle[0] < 0x60 || middle[0] > 0xF0,
            "the middle is either the dark disc or a letter on it: {middle:?}"
        );
    }

    #[test]
    fn a_surface_that_changed_gets_a_new_pattern() {
        let pattern = ServicePattern::new(SizePx::new(1920, 1080), FrameRate::FPS_60, "OUTPUT 1");

        assert!(pattern.matches(SizePx::new(1920, 1080), FrameRate::FPS_60, "OUTPUT 1"));
        assert!(
            !pattern.matches(SizePx::new(1920, 1080), FrameRate::FPS_59_94, "OUTPUT 1"),
            "59.94 is not 60, and the caption would be lying"
        );
        assert!(
            !pattern.matches(SizePx::new(1280, 720), FrameRate::FPS_60, "OUTPUT 1"),
            "a different size is a different pattern"
        );
        assert!(
            !pattern.matches(SizePx::new(1920, 1080), FrameRate::FPS_60, "COMPOSITION"),
            "and so is a different name"
        );
    }

    #[test]
    fn a_long_name_goes_on_two_lines_rather_than_shrinking_away() {
        // A 1280x720 card: the disc is 360 across and the name gets 0.86 of it.
        let across = 0.86 * 360.0;
        // A `fn` rather than a closure: a closure returning a `Caption` borrowed
        // from its own argument cannot have that lifetime inferred, and the
        // error it gives says nothing about why.
        fn line_of(text: &str) -> Caption<'_> {
            Caption {
                text,
                height: 7.2 * 7.0,
                weight: 0.13,
                tracking: 0.08,
            }
        }

        // The line below it, which is the height it must not end up under.
        let floor = 7.2 * 4.0;

        // **Wider than the disc and still left on one line**, because shrunk to
        // fit it is still bigger than the mode. The first version of this broke
        // it in half.
        assert_eq!(
            wrap_title("INTERNAL DP", &line_of("INTERNAL DP"), across, floor),
            vec!["INTERNAL DP".to_owned()],
            "a name that only needed shrinking was broken anyway"
        );

        // Long enough that shrinking takes it under the mode, and broken at the
        // space nearest the middle — not greedily, which would leave one long
        // line over one short word.
        let long = "UPSTAGE LEFT LED PROCESSOR 2 (BUHNE)";
        let split = wrap_title(long, &line_of(long), across, floor);
        assert_eq!(split.len(), 2, "a long name stayed on one line: {split:?}");
        assert_eq!(
            split.join(" "),
            long,
            "the break lost or added something: {split:?}"
        );
        let shorter = split[0].chars().count().min(split[1].chars().count());
        let longer = split[0].chars().count().max(split[1].chars().count());
        assert!(
            longer - shorter <= 6,
            "the two lines are lopsided: {split:?}"
        );

        // **And it does get bigger for it**, which is the whole point.
        let one = across / line_width(&line_of(long));
        let two = across
            / line_width(&Caption {
                text: &split[0],
                ..line_of(long)
            });
        assert!(
            two > one * 1.6,
            "two lines bought almost nothing: {one} against {two}"
        );

        // One long word is set smaller rather than broken in half.
        let word = "SUPERCALIFRAGILISTICEXPIALIDOCIOUS";
        assert_eq!(
            wrap_title(word, &line_of(word), across, floor),
            vec![word.to_owned()]
        );

        // **The one thing a break always promises.** A name that is one
        // enormous word and one short one splits unevenly — `A` over the rest —
        // and is allowed to, because refusing it needs a threshold for "even
        // enough" that nothing here justifies. What must never happen is a break
        // that makes the name smaller than leaving it whole.
        for name in [
            "A SUPERCALIFRAGILISTICEXPIALIDOCIOUSLYLONGWALLNAME",
            "UPSTAGE LEFT LED PROCESSOR 2 (BUHNE)",
            "INTERNAL DP",
            "STAGE RIGHT",
            "",
        ] {
            let broken = wrap_title(name, &line_of(name), across, floor);
            let whole = across / line_width(&line_of(name));
            let after = broken
                .iter()
                .map(|half| across / line_width(&line_of(half)))
                .fold(f32::INFINITY, f32::min);
            assert!(
                after >= whole,
                "breaking {name:?} into {broken:?} made it smaller"
            );
        }
    }

    #[test]
    fn a_surface_too_small_to_write_on_still_gets_bars() {
        // A 16x16 preview surface, or a mis-configured wall. Refusing to draw
        // anything would be worse than drawing what fits.
        let mut pattern = ServicePattern::new(SizePx::new(16, 16), FrameRate::FPS_60, "OUTPUT 1");
        let image = pattern.draw(0.0);

        assert_eq!(image.size(), SizePx::new(16, 16));
        assert_eq!(pixel(image, 0, 0)[3], 0xFF);
    }
}

/// A way to look at the thing, for whoever adjusts it next.
///
/// The pattern is a picture, and a picture is judged by looking at it. This
/// writes one of each to a directory when `OVS_PATTERN_PNG` names one, and does
/// nothing at all otherwise:
///
/// ```text
/// OVS_PATTERN_PNG=/tmp cargo test -p ovs-renderer --lib pattern
/// ```
#[cfg(test)]
mod preview {
    use super::*;

    #[test]
    fn write_one_of_each_when_asked_to() {
        let path = std::env::var("OVS_PATTERN_PNG").unwrap_or_default();
        if path.is_empty() {
            return;
        }
        for (name, title, size, rate) in [
            (
                "composition",
                "COMPOSITION",
                SizePx::new(1920, 1080),
                FrameRate::FPS_60,
            ),
            (
                "output",
                "OUTPUT 1",
                SizePx::new(1280, 720),
                FrameRate::FPS_59_94,
            ),
            // What an operator actually types, which is the case that was broken
            // until 2026-09-26: every lower-case letter but x, p and i drew
            // nothing, so `Internal DP` came out as `I    DP`.
            (
                "named",
                "Internal DP",
                SizePx::new(1920, 1080),
                FrameRate::FPS_60,
            ),
            // And a long one with an umlaut, which is what the shrink-to-fit is
            // for. Judged by looking: a caption pushed out of its disc is obvious
            // in a picture and invisible in an assertion.
            (
                "long-name",
                "Upstage Left LED Processor 2 (Bühne)",
                SizePx::new(1280, 720),
                FrameRate::FPS_60,
            ),
        ] {
            let mut pattern = ServicePattern::new(size, rate, title);
            let image = pattern.draw(1.7);
            image::save_buffer(
                std::path::Path::new(&format!("{path}/{name}.png")),
                image.pixels(),
                image.size().width,
                image.size().height,
                image::ColorType::Rgba8,
            )
            .expect("writes");
        }
    }
}
