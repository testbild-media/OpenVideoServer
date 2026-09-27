// One effect pass: a full-screen triangle that reads one picture and writes
// another.
//
// WGSL for the same reason as the composition shader: naga's GLSL front end
// cannot express binding qualifiers, so a GLSL shader cannot name a texture.
//
// One module and one pipeline for every effect, with the pass's identity in a
// push constant. The alternative is a pipeline per effect and a shader per
// effect, which is four times the build for four branches the hardware takes
// uniformly — every fragment in a pass reads the same constant, so the branch
// costs nothing measurable.
//
// Coordinate convention: exactly the composition shader's. +Y is up in clip
// space here and naga emits the Vulkan conversion when it lowers this to
// SPIR-V; texture space has its origin at the top left. The two are reconciled
// once, in `vertex_main`, so a pass that samples its own pixel reads its own
// pixel.
//
// Every read is `textureSampleLevel` rather than `textureSample`. A pass with a
// loop whose length depends on a parameter is not uniform control flow, and an
// implicit-derivative sample is only allowed where it is; asking for level zero
// explicitly is both legal everywhere and exactly what a full-resolution pass
// wants.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // One triangle rather than two, so no pixel is rasterised twice along a
    // diagonal seam. The third of it that hangs off the canvas is clipped.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let corner = corners[vertex_index];

    var out: VertexOutput;
    out.position = vec4<f32>(corner, 0.0, 1.0);
    // V runs the other way from clip space, as it does in the composition
    // shader: the top of the picture is the top of the screen.
    out.uv = vec2<f32>(corner.x * 0.5 + 0.5, 0.5 - corner.y * 0.5);
    return out;
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

// What one pass is. The renderer fills this from the effect's manifest, so the
// defaults a project was published with are the only defaults there are.
struct Effect {
    // The pass's numbers, in the order its own branch reads them.
    values: vec4<f32>,
    // The pass's colour, where it has one.
    colour: vec4<f32>,
    // One composition pixel, in texture coordinates.
    texel: vec2<f32>,
    // Which branch below runs.
    kind: u32,
}

var<immediate> effect: Effect;

const KIND_COLOUR_CORRECT: u32 = 0u;
const KIND_TINT: u32 = 1u;
const KIND_PIXELATE_ACROSS: u32 = 2u;
const KIND_PIXELATE_DOWN: u32 = 3u;
const KIND_BLUR_ACROSS: u32 = 4u;
const KIND_BLUR_DOWN: u32 = 5u;

// Rec. 709, because that is what the content is graded in and what every panel
// downstream of this expects.
const LUMA_WEIGHTS: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

/// Reads one pixel of the source picture.
fn read(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(source_texture, source_sampler, uv, 0.0);
}

/// Brightness, contrast, saturation and gamma, in that order.
///
/// The order is fixed and is not a parameter. Each of these is a different
/// operation on the same pixel and they do not commute — brightening and then
/// stretching contrast is not the same picture as stretching and then
/// brightening — so an order that could vary would mean two operators with the
/// same numbers getting two different walls. This is the order a colourist
/// expects: set the level, then the contrast around it, then how colourful it
/// is, then the curve.
fn colour_correct(source: vec4<f32>) -> vec4<f32> {
    let brightness = effect.values.x;
    let contrast = effect.values.y;
    let saturation = effect.values.z;
    let gamma = max(effect.values.w, 0.0001);

    var rgb = source.rgb * brightness;
    // Around mid grey, so raising contrast opens the picture up rather than
    // making it darker: a pivot at zero would be a multiply by another name.
    rgb = (rgb - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
    let luma = dot(rgb, LUMA_WEIGHTS);
    rgb = mix(vec3<f32>(luma), rgb, saturation);
    // `1 / gamma`, which is the direction every grading control means by it:
    // above one lifts the middle of the picture. Negative values cannot be
    // raised to a fractional power, so the picture is held at black first.
    rgb = pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(1.0 / gamma));

    return vec4<f32>(rgb, source.a);
}

/// Pushes a picture towards one colour.
fn tint(source: vec4<f32>) -> vec4<f32> {
    let amount = clamp(effect.values.x, 0.0, 1.0);
    let preserve_luma = effect.values.y > 0.5;

    var towards = effect.colour.rgb;
    if preserve_luma {
        // Only the direction of the tint colour, scaled to the brightness the
        // picture already has. Luma is a linear function of the channels, so
        // mixing towards a colour of the same luma leaves the luma alone:
        // what changes is the hue, which is what "keep brightness" means.
        let source_luma = dot(source.rgb, LUMA_WEIGHTS);
        let tint_luma = dot(towards, LUMA_WEIGHTS);
        // A tint colour with no brightness of its own has no direction to
        // take: there is no scale of black that is as bright as the picture.
        // Grey of the picture's own luma is the honest answer — it keeps the
        // brightness and takes all the colour out, which is what mixing
        // towards something with no hue should do.
        if tint_luma > 0.0001 {
            towards = towards * (source_luma / tint_luma);
        } else {
            towards = vec3<f32>(source_luma);
        }
    }

    return vec4<f32>(mix(source.rgb, towards, amount), source.a);
}

/// Averages one axis of a block, in composition pixels.
///
/// Half of the pixelate: across, then down. Two passes rather than one nested
/// loop because a square block costs the square of what two lines cost — the
/// same reason the blur is separable — and because a block average is exactly
/// the product of a row average and a column average, so the two passes give
/// the block its true mean rather than an approximation of one.
///
/// Averaged weighted by alpha, and this matters wherever a layer does not cover
/// the whole canvas: the colour of a fully transparent pixel is not a colour,
/// and letting it into the mean is what gives a blurred logo a dark rim.
fn pixelate(uv: vec2<f32>, down: bool) -> vec4<f32> {
    let size = vec2<f32>(1.0, 1.0) / effect.texel;
    let block = max(floor(effect.values.x + 0.5), 1.0);

    // Which pixel this is, and where its block starts, both in composition
    // pixels so that a look survives being mapped onto a wall of another size.
    let here = select(floor(uv.x * size.x), floor(uv.y * size.y), down);
    let limit = select(size.x, size.y, down);
    let start = floor(here / block) * block;

    var weighted = vec3<f32>(0.0);
    var coverage = 0.0;
    var counted = 0.0;

    for (var step = 0.0; step < block; step = step + 1.0) {
        let line = start + step;
        // A block that runs off the edge averages the pixels that are there.
        // Clamping instead would count the last one twice and pull the block
        // towards it.
        if line >= limit {
            break;
        }

        let at = (line + 0.5) * select(effect.texel.x, effect.texel.y, down);
        let sample = read(select(vec2<f32>(at, uv.y), vec2<f32>(uv.x, at), down));
        weighted = weighted + sample.rgb * sample.a;
        coverage = coverage + sample.a;
        counted = counted + 1.0;
    }

    if coverage <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return vec4<f32>(weighted / coverage, coverage / max(counted, 1.0));
}

/// One axis of a Gaussian blur.
///
/// `amount` is applied in each of the two passes rather than once at the end,
/// which is what keeps the thing separable: the kernel each pass convolves with
/// is `(1 - amount)` of a single tap plus `amount` of a Gaussian, and the
/// two-dimensional kernel is the outer product of the two. Mixing only at the
/// end would need the picture as it was before the first pass, which is a third
/// image to carry for a difference nobody could see.
fn blur(uv: vec2<f32>, down: bool) -> vec4<f32> {
    let radius = max(effect.values.x, 0.0);
    let amount = clamp(effect.values.y, 0.0, 1.0);
    let taps = i32(floor(radius + 0.5));
    // Truncated at three standard deviations, which is where a Gaussian has
    // nothing left worth sampling. A radius of zero leaves one tap of weight
    // one, so the default really is the picture it was given.
    let sigma = max(radius / 3.0, 0.0001);

    let step = select(vec2<f32>(effect.texel.x, 0.0), vec2<f32>(0.0, effect.texel.y), down);

    var weighted = vec3<f32>(0.0);
    var coverage = 0.0;
    var total = 0.0;

    for (var tap = -taps; tap <= taps; tap = tap + 1) {
        let distance = f32(tap);
        let weight = exp(-0.5 * distance * distance / (sigma * sigma));
        let sample = read(uv + step * distance);
        weighted = weighted + sample.rgb * sample.a * weight;
        coverage = coverage + sample.a * weight;
        total = total + weight;
    }

    let source = read(uv);
    if coverage <= 0.0 {
        return vec4<f32>(source.rgb, mix(source.a, 0.0, amount));
    }

    let blurred = vec4<f32>(weighted / coverage, coverage / max(total, 0.0001));
    return vec4<f32>(
        mix(source.rgb, blurred.rgb, amount),
        mix(source.a, blurred.a, amount),
    );
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Straight alpha throughout, never premultiplied. What comes out of the
    // last pass is composited by the same shader that composites a still, and
    // that one multiplies by the quad's own colour — so a picture that arrived
    // premultiplied would be scaled by its alpha twice.
    let source = read(input.uv);

    switch effect.kind {
        case KIND_COLOUR_CORRECT: {
            return colour_correct(source);
        }
        case KIND_TINT: {
            return tint(source);
        }
        case KIND_PIXELATE_ACROSS: {
            return pixelate(input.uv, false);
        }
        case KIND_PIXELATE_DOWN: {
            return pixelate(input.uv, true);
        }
        case KIND_BLUR_ACROSS: {
            return blur(input.uv, false);
        }
        case KIND_BLUR_DOWN: {
            return blur(input.uv, true);
        }
        // Unreachable: the renderer only ever writes a kind it has a branch
        // for. Returning the picture untouched is the one answer that cannot
        // put something on a wall that nobody asked for.
        default: {
            return source;
        }
    }
}
