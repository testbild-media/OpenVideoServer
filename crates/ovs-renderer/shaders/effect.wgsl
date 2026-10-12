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
    // The clock, in seconds, for the passes that move. Free-running and the
    // same for every pass in a frame: two layers carrying one effect have to
    // move together.
    seconds: f32,
}

var<immediate> effect: Effect;

const KIND_COLOUR_CORRECT: u32 = 0u;
const KIND_TINT: u32 = 1u;
const KIND_PIXELATE_ACROSS: u32 = 2u;
const KIND_PIXELATE_DOWN: u32 = 3u;
const KIND_BLUR_ACROSS: u32 = 4u;
const KIND_BLUR_DOWN: u32 = 5u;
const KIND_CROP: u32 = 6u;
const KIND_WHITE_BALANCE: u32 = 7u;
const KIND_MIRROR: u32 = 8u;
const KIND_COLORIZE: u32 = 9u;
const KIND_RGB_CORRECT: u32 = 10u;
const KIND_RGB_SPLIT: u32 = 11u;
const KIND_TV_GLITCH: u32 = 12u;

/// How often the line jumps are re-rolled, a second.
///
/// **Mine.** Twelve: fast enough to read as a fault rather than as a pattern,
/// slow enough that a jump lasts long enough to see. Every frame would be a
/// shimmer, and once a second would be a slideshow.
const GLITCH_ROLLS_A_SECOND: f32 = 12.0;

/// How long the rolling error takes to cross the picture, in seconds.
///
/// **Mine**, and the one number here anybody will argue with: slow enough to
/// watch travel, quick enough to come round while somebody is looking at it.
const GLITCH_ROLL_SECONDS: f32 = 2.8;

/// The temperature at which the white point is left alone, in kelvin.
///
/// D65, which is what Rec. 709 is mastered against, so a picture arriving here
/// is already balanced for it. Every gain below is divided by the gain at this
/// temperature, which makes the published default exactly neutral rather than
/// nearly so.
const NEUTRAL_KELVIN: f32 = 6500.0;

/// How far the ends of the tint control push the green channel.
///
/// **Mine, not measured.** Thirty per cent at either end: enough that the
/// control does something at its extremes and little enough that it stays a
/// correction rather than a colour wash — which is what the Colorize effect is
/// for. The red and blue halves move the other way at half the rate, so the
/// axis runs green to magenta rather than green to grey.
const TINT_GREEN_RANGE: f32 = 0.3;

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

/// Cuts the edges off, in composition pixels.
///
/// One pass for all four edges: the test is a rectangle and there is no
/// kernel, so there is nothing to separate into two passes the way the blur
/// and the pixelate are.
///
/// Cleared rather than blacked. Straight alpha throughout this shader, so
/// clearing is an alpha of zero and the colour behind it stops mattering --
/// what shows through is the layer underneath, which is what trimming a layer
/// is for.
///
/// The comparison is against the pixel's centre, which is what makes the count
/// exact: with `left` at four, the pixel at three sits at 3.5 and is cut, the
/// pixel at four sits at 4.5 and is kept, and four pixels have gone.
fn crop(uv: vec2<f32>, source: vec4<f32>) -> vec4<f32> {
    let left = max(effect.values.x, 0.0) * effect.texel.x;
    let right = max(effect.values.y, 0.0) * effect.texel.x;
    let top = max(effect.values.z, 0.0) * effect.texel.y;
    let bottom = max(effect.values.w, 0.0) * effect.texel.y;

    let inside = uv.x >= left && uv.x <= 1.0 - right && uv.y >= top && uv.y <= 1.0 - bottom;
    return select(vec4<f32>(0.0, 0.0, 0.0, 0.0), source, inside);
}

/// The channel gains of a blackbody at a temperature, unnormalised.
///
/// A piecewise fit to the blackbody locus rather than a colorimetric
/// conversion: within a few per cent across the range this publishes, which is
/// a long way inside what anybody can see on a wall, and it costs two
/// logarithms instead of a matrix and a table. The alternative — CIE
/// chromaticities through a Bradford adaptation — is the right answer for a
/// grade that has to match another facility's, and this does not.
fn kelvin_gain(kelvin: f32) -> vec3<f32> {
    let temperature = clamp(kelvin, 1000.0, 40000.0) / 100.0;

    var red = 255.0;
    if temperature > 66.0 {
        red = 329.698727446 * pow(temperature - 60.0, -0.1332047592);
    }

    var green = 0.0;
    if temperature <= 66.0 {
        green = 99.4708025861 * log(temperature) - 161.1195681661;
    } else {
        green = 288.1221695283 * pow(temperature - 60.0, -0.0755148492);
    }

    var blue = 255.0;
    if temperature < 66.0 {
        if temperature <= 19.0 {
            blue = 0.0;
        } else {
            blue = 138.5177312231 * log(temperature - 10.0) - 305.0447927307;
        }
    }

    return clamp(vec3<f32>(red, green, blue) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
}

/// Moves the white point: warm to cool, and green to magenta.
///
/// Runs after brightness, contrast, saturation and gamma, which is where the
/// owner put it: those three move levels and bend them, and a white point
/// applied before them would be stretched by them rather than sitting on top
/// of the picture.
///
/// **Brightness is held.** The gains are divided by their own luma, so warming
/// a picture does not also darken it — an operator reaching for temperature is
/// not asking about exposure, and two controls that both change brightness are
/// two controls nobody can set.
///
/// The alpha is untouched throughout: this moves colour, and a layer's
/// coverage is not colour.
fn white_balance(source: vec4<f32>) -> vec4<f32> {
    let kelvin = effect.values.x;
    let tint = clamp(effect.values.y, -1.0, 1.0);

    // Relative to neutral, so 6500 K is the identity by construction rather
    // than by how good the fit happens to be there.
    var gain = kelvin_gain(kelvin) / max(kelvin_gain(NEUTRAL_KELVIN), vec3<f32>(0.0001));

    // Green one way, red and blue the other at half the rate: the axis runs
    // green to magenta, and magenta is what red and blue together are.
    gain = gain * vec3<f32>(
        1.0 - tint * TINT_GREEN_RANGE * 0.5,
        1.0 + tint * TINT_GREEN_RANGE,
        1.0 - tint * TINT_GREEN_RANGE * 0.5,
    );

    let luma = dot(gain, LUMA_WEIGHTS);
    gain = gain / max(luma, 0.0001);

    return vec4<f32>(source.rgb * gain, source.a);
}

/// Folds one half of the picture onto the other, on either axis or both.
///
/// A fold is a reflection of where the picture is *read* from, not of what is
/// written: `min` keeps the low side and sends the high side to its mirror,
/// `max` does the opposite. With both axes folded a quadrant is mirrored twice,
/// which is the kaleidoscope this is for.
fn mirror(uv: vec2<f32>) -> vec4<f32> {
    var at = uv;

    let horizontal = u32(max(effect.values.x, 0.0));
    if horizontal == 1u {
        at.x = min(at.x, 1.0 - at.x);
    } else if horizontal == 2u {
        at.x = max(at.x, 1.0 - at.x);
    }

    let vertical = u32(max(effect.values.y, 0.0));
    if vertical == 1u {
        at.y = min(at.y, 1.0 - at.y);
    } else if vertical == 2u {
        at.y = max(at.y, 1.0 - at.y);
    }

    return read(at);
}

/// The fully saturated colour at a hue, in degrees.
///
/// The three channels as three triangles a third of the wheel apart, which is
/// the whole of a hue-to-RGB conversion once the value and the saturation are
/// taken out of it — and they are, here, because the picture supplies one and
/// a control supplies the other.
fn rgb_from_hue(degrees: f32) -> vec3<f32> {
    let hue = fract(degrees / 360.0) * 6.0;
    return clamp(
        vec3<f32>(
            abs(hue - 3.0) - 1.0,
            2.0 - abs(hue - 2.0),
            2.0 - abs(hue - 4.0),
        ),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

/// Throws the colour away and paints the brightness back in.
///
/// The picture's luma is the value and the controls are the hue and the
/// saturation, which is what "colour the luma in" means: saturation at zero is
/// the picture in monochrome, and that is a useful place to stop rather than a
/// dead end of the control.
///
/// `amount` mixes it back against what came in, so the published default of
/// zero changes nothing.
fn colorize(source: vec4<f32>) -> vec4<f32> {
    let saturation = clamp(effect.values.y, 0.0, 1.0);
    let amount = clamp(effect.values.z, 0.0, 1.0);

    let luma = dot(source.rgb, LUMA_WEIGHTS);
    let hue = mix(vec3<f32>(1.0), rgb_from_hue(effect.values.x), saturation);

    return vec4<f32>(mix(source.rgb, hue * luma, amount), source.a);
}

/// A black level and a white level, per channel.
///
/// Black goes to the lift, white goes to the gain, and everything between
/// follows the straight line joining them — which is what those two words mean
/// on a desk. Lift at zero and gain at one is the identity, so the published
/// defaults change nothing.
///
/// The lift arrives in the colour slot because it is a colour: a per-channel
/// offset at black. See the comment beside the pass this is built from.
fn rgb_correct(source: vec4<f32>) -> vec4<f32> {
    let lift = effect.colour.rgb;
    let gain = effect.values.rgb;
    return vec4<f32>(lift + source.rgb * (gain - lift), source.a);
}

/// Pulls the channels apart along a line.
///
/// Red one way, blue the other, green where it was. The offset is in
/// composition pixels, so a look set up on one composition is the same look
/// after the pieces are mapped onto a wall of another size.
///
/// The alpha is the middle sample's: the coverage belongs to the layer, and
/// three different coverages would put a fringe on the edge of every shape
/// that the channels themselves do not have.
fn rgb_split(uv: vec2<f32>) -> vec4<f32> {
    let offset = max(effect.values.x, 0.0);
    let angle = radians(effect.values.y);
    let step = vec2<f32>(cos(angle), sin(angle)) * offset * effect.texel;

    let red = read(uv + step);
    let green = read(uv);
    let blue = read(uv - step);

    return vec4<f32>(red.r, green.g, blue.b, green.a);
}

/// A repeatable number in `0.0..1.0` from a point.
///
/// The usual sine-and-fract hash. It is not a good random number generator and
/// does not need to be: what it has to do is look unrelated to its neighbours,
/// and it is the same on every card given the same input, which is what keeps
/// two outputs showing one composition from glitching differently.
fn hash21(point: vec2<f32>) -> f32 {
    return fract(sin(dot(point, vec2<f32>(127.1, 311.7))) * 43758.5453123);
}

/// A picture coming apart the way a tube did.
///
/// Three faults, each with its own amount and all under the master, which is
/// the control an operator rides. With the master at zero nothing is read from
/// anywhere but the pixel itself, so the published default is exactly the
/// picture that came in.
///
/// Where the time comes from: the clock a pass now carries, which is
/// free-running and shared across the frame. Two layers carrying this move
/// together, and nothing in the show reaches it.
fn tv_glitch(uv: vec2<f32>, source: vec4<f32>) -> vec4<f32> {
    let master = clamp(effect.values.x, 0.0, 1.0);
    let tear = clamp(effect.values.y, 0.0, 1.0) * master;
    let snow = clamp(effect.values.z, 0.0, 1.0) * master;
    let roll = clamp(effect.values.w, 0.0, 1.0) * master;

    let seconds = effect.seconds;
    let lines = 1.0 / max(effect.texel.y, 0.000001);
    let line = floor(uv.y * lines);

    var at = uv;

    // **Lines that jump, and only some of them.** A displacement every line
    // would be a texture; a few lines at a time is a fault. Re-rolled a dozen
    // times a second rather than every frame, so a jump lasts long enough to
    // be seen.
    let roll_index = floor(seconds * GLITCH_ROLLS_A_SECOND);
    let picked = hash21(vec2<f32>(line, roll_index));
    if picked > 1.0 - tear * 0.35 {
        let shift = hash21(vec2<f32>(line, roll_index + 7.0)) - 0.5;
        at.x = at.x + shift * tear * 0.15;
    }

    // **An error travelling down the picture**, which is what a mistimed field
    // looked like: a band that drags the lines sideways and lifts them.
    let head = fract(seconds / GLITCH_ROLL_SECONDS);
    let distance = abs(fract(uv.y - head + 0.5) - 0.5);
    let band = smoothstep(0.06, 0.0, distance) * roll;
    at.x = at.x + band * 0.04;

    var colour = read(at);

    // **Snow**, which is the only one of the three that is per pixel rather
    // than per line, and the reason this is called artefacts rather than
    // noise: it sits on top of the picture instead of replacing it.
    let grain = hash21(vec2<f32>(uv.x * lines, uv.y * lines + seconds * 60.0)) - 0.5;
    let lifted = colour.rgb + vec3<f32>(grain * snow * 0.5) + vec3<f32>(band * 0.12);

    return vec4<f32>(clamp(lifted, vec3<f32>(0.0), vec3<f32>(1.0)), source.a);
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
        case KIND_CROP: {
            return crop(input.uv, source);
        }
        case KIND_WHITE_BALANCE: {
            return white_balance(source);
        }
        case KIND_MIRROR: {
            return mirror(input.uv);
        }
        case KIND_COLORIZE: {
            return colorize(source);
        }
        case KIND_RGB_CORRECT: {
            return rgb_correct(source);
        }
        case KIND_RGB_SPLIT: {
            return rgb_split(input.uv);
        }
        case KIND_TV_GLITCH: {
            return tv_glitch(input.uv, source);
        }
        // Unreachable: the renderer only ever writes a kind it has a branch
        // for. Returning the picture untouched is the one answer that cannot
        // put something on a wall that nobody asked for.
        default: {
            return source;
        }
    }
}
