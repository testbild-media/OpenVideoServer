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
    // The noise field's seed: one 64-bit number, low half first.
    field: vec2<u32>,
    // Which branch below runs.
    kind: u32,
    // Bit 0: the three channels get independent fields.
    flags: u32,
}

var<immediate> generated: Generated;

const KIND_NOISE: u32 = 0u;
const KIND_SOLID: u32 = 1u;

const FLAG_COLOURED: u32 = 1u;

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
        // A flat fill, which also covers the strobe: the renderer decides on
        // each tick whether the lamp is lit and sends the colour or a clear
        // one, exactly as it does for the processor's version.
        default: {
            return generated.colour;
        }
    }
}
