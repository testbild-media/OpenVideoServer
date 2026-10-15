// Generators, drawn by the card instead of by the processor.
//
// Measured on PC05 on 2026-10-15, one source at a time, the wall at 1920x1080
// at 59.94, reading the render thread's own `playback_us`: a 3840x2160 clip
// decoded on the card cost **6 microseconds** a pass and `ovs.noise` cost
// **25,532**. A generator was the only thing the wall was waiting for, because
// it was the only thing still making every pixel on the processor — two
// million of them, and then copying eight megabytes of them onto the card.
//
// Sharing the rows out between cores took that to about twelve milliseconds.
// This removes it: the picture is made where it is going to be looked at, and
// nothing crosses the bus at all.
//
// # It has to draw what the processor draws
//
// The same generator still draws on the processor — for the export path, for a
// machine with no usable device, and for the tests that decide what these
// pictures *are*. Two implementations of one picture is exactly the kind of
// pair that drifts, so the arithmetic here is the arithmetic in
// `media/generator.rs`, in the same order, including a 64-bit hash that WGSL
// has no type for and which is spelled out below in 32-bit halves. A test
// draws both and compares them.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // One triangle rather than two, so no pixel is rasterised twice along a
    // diagonal seam. The third of it that hangs off the target is clipped.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VertexOutput;
    out.position = vec4<f32>(corners[vertex_index], 0.0, 1.0);
    return out;
}

// What one generator is. Filled from the generator's own parameters, so the
// defaults a show was published with are the only defaults there are.
struct Generated {
    // The kind's own numbers, in the order its branch reads them.
    values: vec4<f32>,
    // The kind's colour, where it has one.
    colour: vec4<f32>,
    // Its second colour: a gradient's far end, a grid's or a shape's bed.
    colour_b: vec4<f32>,
    // The noise field's seed: one 64-bit number, low half first.
    field: vec2<u32>,
    // How big the picture being drawn is. Stated, because most kinds read no
    // texture and are handed the one-by-one white default.
    size: vec2<u32>,
    // Which branch below runs.
    kind: u32,
    // Bit 0: the three channels get independent fields.
    flags: u32,
}

var<immediate> generated: Generated;

// The one picture a generator may read: the service pattern's bars with its
// caption in a strip underneath. Read with `textureLoad` and no sampler, so
// what comes back is the texel and not a filtered guess at it — the bars are a
// measurement tool and a soft edge in them would be a lie about the surface.
//
// The layout's other bindings (the sampler and the two chroma images a video
// texture has) are not declared here. A set may hold more than a shader reads.
@group(0) @binding(0) var source: texture_2d<f32>;

const KIND_NOISE: u32 = 0u;
const KIND_SOLID: u32 = 1u;
const KIND_PATTERN: u32 = 2u;
const KIND_TEST_PATTERN: u32 = 3u;
const KIND_GRADIENT: u32 = 4u;
const KIND_GRID: u32 = 5u;
const KIND_SHAPE: u32 = 6u;

const FLAG_COLOURED: u32 = 1u;
/// The commissioning pattern's bars across the top.
const FLAG_BARS: u32 = 2u;

/// Which figure a shape draws. `media::generator::Figure`, in the same order.
const FIGURE_ELLIPSE: u32 = 0u;
const FIGURE_RECTANGLE: u32 = 1u;
const FIGURE_TRIANGLE: u32 = 2u;

/// The eight bars, left to right, as a commissioning pattern has them.
///
/// `media::generator::BARS`, and in eight-bit values rather than fractions
/// because that is how they are written there — 255 and 0, not 1.0 and 0.0,
/// so the two cannot round apart.
fn bar_colour(index: i32) -> vec3<f32> {
    var bars = array<vec3<f32>, 8>(
        vec3<f32>(255.0, 255.0, 255.0),
        vec3<f32>(255.0, 255.0, 0.0),
        vec3<f32>(0.0, 255.0, 255.0),
        vec3<f32>(0.0, 255.0, 0.0),
        vec3<f32>(255.0, 0.0, 255.0),
        vec3<f32>(255.0, 0.0, 0.0),
        vec3<f32>(0.0, 0.0, 255.0),
        vec3<f32>(0.0, 0.0, 0.0),
    );
    return bars[clamp(index, 0, 7)] / 255.0;
}

// `0x9E3779B97F4A7C15`, `0xC2B2AE3D27D4EB4F`, `0x165667B19E3779F9` and
// `0xFF51AFD7ED558CCD`, each as its low and high half. The same four constants
// `lattice` uses on the processor; a number that differed in one bit would
// give a picture that is still noise and still wrong.
const MIX_X: vec2<u32> = vec2<u32>(0x7F4A7C15u, 0x9E3779B9u);
const MIX_Y: vec2<u32> = vec2<u32>(0x27D4EB4Fu, 0xC2B2AE3Du);
const MIX_CHANNEL: vec2<u32> = vec2<u32>(0x9E3779F9u, 0x165667B1u);
const MIX_FINAL: vec2<u32> = vec2<u32>(0xED558CCDu, 0xFF51AFD7u);

// The top 24 bits of a 64-bit hash, as a fraction. 24 bits is more resolution
// than eight-bit output can show, and is exactly representable in an f32, so
// this conversion is the one place the two implementations cannot disagree.
const HASH_SCALE: f32 = 16777216.0;

/// One 32-bit multiply, keeping both halves of the 64-bit answer.
///
/// WGSL has no 64-bit integer and no widening multiply, so this is done in
/// 16-bit limbs. The carry out of the low half is the part that is easy to
/// drop and impossible to see afterwards.
fn mul_wide(a: u32, b: u32) -> vec2<u32> {
    let a_low = a & 0xFFFFu;
    let a_high = a >> 16u;
    let b_low = b & 0xFFFFu;
    let b_high = b >> 16u;

    let low_low = a_low * b_low;
    let low_high = a_low * b_high;
    let high_low = a_high * b_low;
    let high_high = a_high * b_high;

    let middle = (low_low >> 16u) + (low_high & 0xFFFFu) + (high_low & 0xFFFFu);
    let low = (middle << 16u) | (low_low & 0xFFFFu);
    let high = high_high + (low_high >> 16u) + (high_low >> 16u) + (middle >> 16u);
    return vec2<u32>(low, high);
}

/// A 64-bit multiply that wraps, as Rust's `wrapping_mul` does.
///
/// The top half of the answer is thrown away, which is what makes the two ends
/// of the high word enough: everything above bit 63 is gone either way.
fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let low = mul_wide(a.x, b.x);
    return vec2<u32>(low.x, low.y + a.x * b.y + a.y * b.x);
}

/// A 64-bit add that wraps.
fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let low = a.x + b.x;
    // The carry, without a comparison that a compiler is free to reorder: the
    // sum is smaller than either term exactly when it wrapped.
    let carry = select(0u, 1u, low < a.x);
    return vec2<u32>(low, a.y + b.y + carry);
}

/// A 64-bit shift right by 33, which is the only width this needs.
fn shift64_33(value: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(value.y >> 1u, 0u);
}

/// The value at one lattice point, in `0.0..=1.0`.
///
/// `media::generator::lattice`, spelled out. A hash rather than a stored
/// table: a table large enough not to repeat across a wall would be a megabyte
/// to carry, and this is the same picture every time the show is opened
/// without carrying anything at all.
fn lattice(x: u32, y: u32, channel: vec2<u32>) -> f32 {
    var value = mul64(vec2<u32>(x, 0u), MIX_X)
        ^ mul64(vec2<u32>(y, 0u), MIX_Y)
        ^ mul64(channel, MIX_CHANNEL);
    value = value ^ shift64_33(value);
    value = mul64(value, MIX_FINAL);
    value = value ^ shift64_33(value);

    // `value >> 40`, which lives entirely in the high word.
    return f32(value.y >> 8u) / HASH_SCALE;
}

/// Smoothstep, with zero gradient at both ends.
///
/// What makes neighbouring cells meet without the visible creases linear
/// interpolation leaves.
fn fade(t: f32) -> f32 {
    return t * t * (3.0 - 2.0 * t);
}

/// Value noise at a point, for one field.
fn value_noise(x: f32, y: f32, channel: vec2<u32>) -> f32 {
    let cell_x = floor(x);
    let cell_y = floor(y);
    let left = u32(cell_x);
    let top = u32(cell_y);
    let fraction_x = fade(x - cell_x);
    let fraction_y = fade(y - cell_y);

    let top_edge = mix(
        lattice(left, top, channel),
        lattice(left + 1u, top, channel),
        fraction_x,
    );
    let bottom_edge = mix(
        lattice(left, top + 1u, channel),
        lattice(left + 1u, top + 1u, channel),
        fraction_x,
    );
    return mix(top_edge, bottom_edge, fraction_y);
}

fn noise(pixel: vec2<f32>) -> vec4<f32> {
    let scale = max(generated.values.x, 1.0);
    let amount = clamp(generated.values.y, 0.0, 1.0);
    let x = pixel.x / scale;
    let y = pixel.y / scale;

    // Three independent fields when colouring, one otherwise — and when it is
    // one, it is computed once rather than three times and copied, which is
    // what the processor does too.
    var rgb: vec3<f32>;
    if (generated.flags & FLAG_COLOURED) != 0u {
        rgb = vec3<f32>(
            value_noise(x, y, add64(generated.field, vec2<u32>(1u, 0u))),
            value_noise(x, y, add64(generated.field, vec2<u32>(2u, 0u))),
            value_noise(x, y, add64(generated.field, vec2<u32>(3u, 0u))),
        );
    } else {
        rgb = vec3<f32>(value_noise(x, y, generated.field));
    }

    return vec4<f32>(vec3<f32>(0.5) + (rgb - vec3<f32>(0.5)) * amount, 1.0);
}

/// The sweep's brightness across its trail.
///
/// `pattern::sweep_alpha`'s ramp, point for point. The processor samples it
/// into a 256-step table once a frame and reads the table per pixel; a card
/// has no reason to keep a table, so it walks the nine points. Both end up on
/// the same curve.
fn sweep_alpha(fraction: f32) -> f32 {
    if fraction <= 0.0 {
        return 0.0;
    }
    if fraction >= 1.0 {
        return 1.0;
    }
    var at = array<f32, 9>(0.00, 0.20, 0.40, 0.58, 0.72, 0.84, 0.92, 0.97, 1.00);
    var height = array<f32, 9>(0.00, 0.02, 0.06, 0.14, 0.28, 0.48, 0.70, 0.88, 1.00);

    for (var step = 1; step < 9; step = step + 1) {
        if fraction <= at[step] {
            let span = at[step] - at[step - 1];
            let along = select(0.0, (fraction - at[step - 1]) / span, span > 0.0);
            return mix(height[step - 1], height[step], along);
        }
    }
    return 1.0;
}

/// The service pattern: drifting bars, a sweep over them, and the caption.
///
/// Everything that does not move was drawn once on the processor and is in
/// `source`; what happens here every pass is a read at an offset, a blend and
/// a second read. That used to be four milliseconds of the render thread, and
/// the whole of it was moving eight megabytes about.
fn pattern(pixel: vec2<f32>) -> vec4<f32> {
    let side = i32(generated.colour.x);
    // The bars are the surface; the caption is in a strip under them, which is
    // why the source is taller than the picture.
    let size = vec2<i32>(generated.size);
    let here = vec2<i32>(pixel);

    // The drift is a read at an offset that wraps, which is the whole of what
    // the processor's two copies a row were doing.
    let shift = vec2<i32>(i32(generated.values.x), i32(generated.values.y));
    let taken = (here + size - shift) % size;
    var colour = textureLoad(source, taken, 0);

    // The sweep runs along the diagonal: how far a pixel is along it is
    // (x + y) / sqrt(2), and the trail is what turns that into a brightness.
    //
    // **The window is the processor's window, to the pixel.** It walks each
    // row from `first` to `last` and those are truncated, so the pixel exactly
    // on the leading edge is outside — which at second zero is the top-left
    // corner, the one pixel a test reads first. Testing the distance alone
    // instead made that corner white against the processor's grey.
    let start = generated.values.z;
    let trail = generated.values.w;
    if trail > 0.0 {
        let edge = start + trail;
        let first = floor(max(start * 1.4142135 - pixel.y, 0.0));
        let last = floor(min(max(edge * 1.4142135 - pixel.y, 0.0), f32(size.x)));
        if pixel.x >= first && pixel.x < last {
            let along = ((pixel.x + pixel.y) / 1.4142135 - start) / trail;
            // **The processor's arithmetic, not an equivalent of it.** It
            // samples the ramp into a 256-step table once a frame, truncates
            // each entry to a 256th, and blends with a shift rather than a
            // divide. Doing it in floats here instead was five levels out in
            // the steep part of the ramp - invisible, and five levels is five
            // levels. This is cheaper to copy than to argue about.
            let step = min(floor(along * 256.0), 256.0);
            let alpha = floor(sweep_alpha(step / 256.0) * 256.0);
            let was = colour.rgb * 255.0;
            colour = vec4<f32>(
                (was + floor((vec3<f32>(255.0) - was) * alpha / 256.0)) / 255.0,
                colour.a,
            );
        }
    }

    // And the caption, over the top, where the processor puts it: a square in
    // the middle, blended by its own alpha.
    if side > 0 {
        let left = (size.x - min(side, size.x)) / 2;
        let top = (size.y - min(side, size.y)) / 2;
        let inside = here - vec2<i32>(left, top);
        if inside.x >= 0 && inside.y >= 0 && inside.x < side && inside.y < side {
            let over = textureLoad(source, vec2<i32>(inside.x, size.y + inside.y), 0);
            colour = vec4<f32>(
                mix(colour.rgb, over.rgb, over.a),
                colour.a,
            );
        }
    }

    return colour;
}

/// The commissioning pattern: a grid, bars, an edge and a cross.
///
/// `media::generator::draw_test_pattern`, arm for arm and in the same order,
/// because each of its three parts answers a different question an operator is
/// asking and a pixel belongs to exactly one of them.
fn test_pattern(pixel: vec2<f32>) -> vec4<f32> {
    let size = vec2<i32>(generated.size);
    let here = vec2<i32>(pixel);
    let grid = max(i32(generated.values.x), 2);
    let bars = (generated.flags & FLAG_BARS) != 0u;
    let bar_height = select(0, size.y / 6, bars);
    // In proportion to the canvas rather than one pixel wide: a single pixel
    // on a 1920-wide wall disappears in the console's preview, which is where
    // somebody checks it from before walking over to look.
    let stroke = max(max(size.x, size.y) / 240, 1);

    let on_grid = here.x % grid == 0 || here.y % grid == 0;
    let on_edge = here.x < stroke || here.y < stroke
        || here.x + stroke >= size.x || here.y + stroke >= size.y;
    let on_cross = abs(here.x - size.x / 2) <= stroke || abs(here.y - size.y / 2) <= stroke;

    var rgb: vec3<f32>;
    if here.y < bar_height {
        rgb = bar_colour(here.x * 8 / max(size.x, 1));
    } else if on_edge || on_cross {
        rgb = vec3<f32>(1.0, 0.0, 0.0);
    } else if on_grid {
        rgb = vec3<f32>(128.0 / 255.0);
    } else {
        rgb = vec3<f32>(16.0 / 255.0);
    }
    return vec4<f32>(rgb, 1.0);
}

/// A linear gradient between two colours.
///
/// `media::generator::draw_gradient`. The ramp is normalised across the span
/// of the canvas's own pixel centres along the angle, so the furthest pixels
/// each way land exactly on the two declared colours whatever the shape of the
/// composition - a gradient whose corners fell short of its own end colour
/// would be an operator setting a colour and never seeing it.
fn gradient(pixel: vec2<f32>) -> vec4<f32> {
    let size = vec2<f32>(generated.size);
    let angle = radians(generated.values.x);
    let turn = vec2<f32>(cos(angle), sin(angle));
    let half = size / 2.0;

    var span = abs((size.x - 1.0) * turn.x) + abs((size.y - 1.0) * turn.y);
    if span <= 0.0 {
        span = 1.0;
    }
    let along = dot(pixel + vec2<f32>(0.5) - half, turn);
    let position = clamp(along / span + 0.5, 0.0, 1.0);
    return mix(generated.colour, generated.colour_b, position);
}

/// Ruled lines at a spacing.
///
/// `media::generator::draw_grid`. Measured from the picture's own origin
/// rather than its centre, so a grid on a composition and the same grid on a
/// piece cut out of it line up.
fn grid(pixel: vec2<f32>) -> vec4<f32> {
    let spacing = max(i32(generated.values.x), 1);
    let line = max(i32(generated.values.y), 1);
    let here = vec2<i32>(pixel);
    let on_line = here.x % spacing < line || here.y % spacing < line;
    return select(generated.colour_b, generated.colour, on_line);
}

/// How far a point is from the boundary of an upright ellipse at the origin.
fn ellipse_distance(point: vec2<f32>, half: vec2<f32>) -> f32 {
    let scaled = point / half;
    let equation = dot(scaled, scaled) - 1.0;
    let slope = 2.0 * length(scaled / half);
    if slope > 0.0 {
        return equation / slope;
    }
    // The exact centre, where the equation has no slope at all.
    return -min(half.x, half.y);
}

/// How far a point is from the boundary of an upright rectangle at the origin.
fn rectangle_distance(point: vec2<f32>, half: vec2<f32>) -> f32 {
    let corner = abs(point) - half;
    return length(max(corner, vec2<f32>(0.0))) + min(max(corner.x, corner.y), 0.0);
}

/// How far a point is from a line segment.
fn distance_to_segment(point: vec2<f32>, start: vec2<f32>, finish: vec2<f32>) -> f32 {
    let along = finish - start;
    let length_squared = dot(along, along);
    if length_squared <= 0.0 {
        return distance(point, start);
    }
    let how_far = clamp(dot(point - start, along) / length_squared, 0.0, 1.0);
    return distance(point, start + along * how_far);
}

/// How far a point is from the boundary of an upright isosceles triangle.
///
/// To each of the three edges and the sign from which side of them the point
/// is on, rather than from the three edge *lines*: the nearer line would round
/// the corners off by up to a pixel, and the corner is the whole of what makes
/// a triangle read as one.
fn triangle_distance(point: vec2<f32>, half: vec2<f32>) -> f32 {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(0.0, -half.y),
        vec2<f32>(half.x, half.y),
        vec2<f32>(-half.x, half.y),
    );

    var nearest = 1e30;
    var inside = true;
    for (var edge = 0; edge < 3; edge = edge + 1) {
        let start = corners[edge];
        let finish = corners[(edge + 1) % 3];
        nearest = min(nearest, distance_to_segment(point, start, finish));
        let along = finish - start;
        let offset = point - start;
        inside = inside && (along.x * offset.y - along.y * offset.x >= 0.0);
    }
    return select(nearest, -nearest, inside);
}

/// Mixes a shape's colour over a bed by how much of the pixel it covers.
///
/// `media::generator::over`. Each colour is scaled by its own alpha for the
/// mix and taken back out afterwards, because what a generator hands over is
/// straight alpha. Mixing the four channels as written would drag a
/// half-covered edge towards the bed's *colour* as well as its transparency,
/// which after the compositor premultiplies is a dark rim around everything an
/// operator meant to float over the layer below.
fn over(bed: vec4<f32>, shape: vec4<f32>, coverage: f32) -> vec4<f32> {
    let alpha = mix(bed.a, shape.a, coverage);
    if alpha <= 0.0 {
        return vec4<f32>(mix(bed.rgb, shape.rgb, coverage), 0.0);
    }
    return vec4<f32>(
        mix(bed.rgb * bed.a, shape.rgb * shape.a, coverage) / alpha,
        alpha,
    );
}

/// One figure, centred and turned, over a bed.
fn shape(pixel: vec2<f32>) -> vec4<f32> {
    let half = vec2<f32>(generated.values.x, generated.values.y) / 2.0;
    // A size that is not finite and above zero encloses nothing, so the bed is
    // the whole of the answer - and asking an ellipse how far a point is from a
    // boundary of no radius would be a division by nothing.
    if !(half.x > 0.0 && half.y > 0.0) {
        return generated.colour_b;
    }

    let size = vec2<f32>(generated.size);
    let angle = radians(generated.values.z);
    let turn = vec2<f32>(cos(angle), sin(angle));
    let offset = pixel + vec2<f32>(0.5) - size / 2.0;

    // The pixel is turned back into the shape's frame rather than the shape
    // into the picture's: one rotation either way, and in the shape's own
    // frame every boundary is arithmetic.
    let local = vec2<f32>(
        offset.x * turn.x + offset.y * turn.y,
        offset.y * turn.x - offset.x * turn.y,
    );

    var away: f32;
    let figure = u32(generated.values.w);
    if figure == FIGURE_RECTANGLE {
        away = rectangle_distance(local, half);
    } else if figure == FIGURE_TRIANGLE {
        away = triangle_distance(local, half);
    } else {
        away = ellipse_distance(local, half);
    }

    // How much of the pixel the shape covers, estimated from the distance to
    // the boundary: a pixel half a pixel inside is covered, one half a pixel
    // outside is not, and the band between is the ramp. Deciding inside or out
    // per pixel puts a staircase on every edge, and an LED wall is the least
    // forgiving display there is for one.
    let coverage = clamp(0.5 - away, 0.0, 1.0);
    if coverage <= 0.0 {
        return generated.colour_b;
    }
    return over(generated.colour_b, generated.colour, coverage);
}

@fragment
fn fragment_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // The pixel's own index. `position` is the centre of the pixel, so the
    // floor of it is the integer the processor's loop counts with — and the
    // noise field is a function of that index, not of a texture coordinate, so
    // this has to be exact rather than close.
    let pixel = floor(in.position.xy);

    switch generated.kind {
        case KIND_NOISE: {
            return noise(pixel);
        }
        case KIND_PATTERN: {
            return pattern(pixel);
        }
        case KIND_TEST_PATTERN: {
            return test_pattern(pixel);
        }
        case KIND_GRADIENT: {
            return gradient(pixel);
        }
        case KIND_GRID: {
            return grid(pixel);
        }
        case KIND_SHAPE: {
            return shape(pixel);
        }
        // A flat fill, which also covers the strobe: the renderer decides on
        // each tick whether the lamp is lit and sends the colour or a clear
        // one, exactly as it does for the processor's version.
        default: {
            return generated.colour;
        }
    }
}
