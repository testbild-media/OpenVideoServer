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

// The second picture, for the one pass that has one.
//
// A set of the same shape as the source's, bound at every pass whether or not
// it reads this: a pipeline layout is fixed and a set that is never sampled
// costs a bind. What a pass with no picture of its own gets is the white
// default texture, which is also what a mask that is still being decoded gets
// — and white is a mask that does nothing, so an effect waiting for its
// picture leaves the layer alone instead of blanking it.
@group(1) @binding(0) var mask_texture: texture_2d<f32>;
@group(1) @binding(1) var mask_sampler: sampler;

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
    // How long one frame is on the wall, in seconds, for the pass that fades
    // something over time: a fade worked out per frame would be a different
    // fade at 50 Hz and at 60.
    frame: f32,
    // Which frame this is, counted by the compositor and wrapping. Not the
    // clock: what reads this needs a number that is different every frame
    // even if the clock were to stand still.
    tick: u32,
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
const KIND_MASK: u32 = 13u;
const KIND_INVERT: u32 = 14u;
const KIND_EDGES: u32 = 15u;
const KIND_ECHO: u32 = 16u;
const KIND_SHAKE: u32 = 17u;

/// How far a frame of an echo has to fade before it counts as gone, in halvings.
///
/// Eight, so gone is below one part in 256 — under the last step an eight-bit
/// picture has. That is what lets the length an operator sets be the time a
/// frame takes to disappear, rather than a time constant whose end nobody can
/// see.
const ECHO_GONE_HALVINGS: f32 = 8.0;

/// How far past the threshold an edge takes to come fully in, in the same
/// units as the threshold.
///
/// **Mine.** A hard cut at the threshold makes a line that crawls as the
/// picture moves, because a pixel either side of the boundary flips on and off
/// from one frame to the next; a short ramp keeps the line still. Small enough
/// that the threshold still means what an operator thinks it means.
const EDGE_SOFTNESS: f32 = 0.08;

/// How often the line jumps are re-rolled, a second.
///
/// **Mine.** Twelve: fast enough to read as a fault rather than as a pattern,
/// slow enough that a jump lasts long enough to see. Every frame would be a
/// shimmer, and once a second would be a slideshow.
const GLITCH_ROLLS_A_SECOND: f32 = 12.0;

/// How often the picture is thrown off its vertical hold, a second.
///
/// **Mine.** Three: slow enough that each displacement is a place the picture
/// sits rather than a flicker, fast enough that it never looks like a cut.
/// Deliberately slower than the line jumps above, so the two faults read as two
/// faults rather than as one shimmer.
const GLITCH_SYNC_JUMPS_A_SECOND: f32 = 3.0;

/// How many bands the picture is divided into when its lines jump.
///
/// **Mine.** Twenty-four: about forty-five lines each on a 1080-line picture,
/// which is a run thick enough to see jump. Per line, which is what the
/// sideways tear above does, the same displacement reads as texture.
const GLITCH_SYNC_BANDS: f32 = 24.0;

/// How far a band jumps, against how far the whole picture is thrown.
///
/// **Mine.** Half: enough that a band clearly leaves the picture it belongs
/// to, little enough that the picture is still one picture.
const GLITCH_SYNC_BAND_SHARE: f32 = 0.5;

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

/// Every colour for its opposite, or the picture untouched.
///
/// `1 - c` per channel, which is what a negative is, and the alpha left where
/// it was: a half-transparent layer stays half transparent. Inverting the alpha
/// too would turn a picture into its own hole.
///
/// The switch rather than a branch around the pass: the pass is appended
/// whether it is on or off, because an effect that drew nothing when it was
/// off could never be proved neutral.
fn invert(source: vec4<f32>) -> vec4<f32> {
    if effect.values.x <= 0.5 {
        return source;
    }
    // The rate, in hertz. Zero is version one and every version two nobody has
    // set a rate on: inverted, and staying inverted.
    //
    // Half the cycle inverted and half of it not, which is what a strobe is.
    // The clock is the free-running one every pass in a frame shares, so two
    // layers each carrying this flash together rather than beating against
    // each other — which is the whole reason that clock is shared.
    let hz = effect.values.y;
    if hz > 0.0 && fract(effect.seconds * hz) >= 0.5 {
        return source;
    }
    return vec4<f32>(vec3<f32>(1.0) - source.rgb, source.a);
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
/// **Each channel brings its own coverage**, and the pixel is as covered as
/// the most covered of the three. That fringe on the edge of every shape is the
/// effect: on a picture with transparency — a logo, a keyed clip — the red and
/// the blue have to be able to leave the outline. With the middle sample's
/// alpha, as it was, the outline cut them off and there was nothing to see
/// (owner, 2026-10-18: "weil Alpha ja als Maske alles rausstanzt, also ist der
/// Effekt nicht mehr gegeben").
///
/// Worked out premultiplied and handed on straight, as the chain is: a red
/// that crossed from a white pixel onto a clear one is full red at that
/// coverage, not a red darkened by the clear pixel's alpha. On a picture with
/// no transparency every alpha is one and this is the split it always was.
fn rgb_split(uv: vec2<f32>) -> vec4<f32> {
    let offset = max(effect.values.x, 0.0);
    let angle = radians(effect.values.y);
    let step = vec2<f32>(cos(angle), sin(angle)) * offset * effect.texel;

    let red = read(uv + step);
    let green = read(uv);
    let blue = read(uv - step);

    let alpha = max(max(red.a, green.a), blue.a);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let carried = vec3<f32>(red.r * red.a, green.g * green.a, blue.b * blue.a);
    return vec4<f32>(carried / alpha, alpha);
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

    // **The vertical hold, which is all version two adds.** Everything above
    // moves the picture sideways; a set that had lost its grip did something
    // else, and this is that: whole runs of lines jumping, and the frame
    // itself sitting somewhere it does not belong.
    //
    // What arrives here is how far the frame may be thrown, as a fraction of
    // its own height — the control multiplied by the owner's twenty per cent,
    // done where that number lives rather than written again here. A version
    // one pass leaves this at nought, which is why version one is untouched.
    //
    // Thrown and held, then thrown again, rather than scrolled: a picture that
    // travels smoothly is the rolling error above, and this is a picture that
    // is simply in the wrong place. It wraps, because that is what a tube did
    // — what leaves the bottom comes back at the top.
    let hold = max(effect.colour.x, 0.0) * master;
    if hold > 0.0 {
        let step = floor(seconds * GLITCH_SYNC_JUMPS_A_SECOND);
        let thrown = (hash21(vec2<f32>(step, 19.0)) * 2.0 - 1.0) * hold;

        // And the bands, each on its own. Only some of them, for the reason
        // only some lines tear: all of them at once is a texture.
        let band = floor(uv.y * GLITCH_SYNC_BANDS);
        var jumped = 0.0;
        if hash21(vec2<f32>(band, step + 3.0)) > 0.65 {
            jumped = (hash21(vec2<f32>(band, step + 11.0)) * 2.0 - 1.0)
                * hold
                * GLITCH_SYNC_BAND_SHARE;
        }

        at.y = fract(at.y + thrown + jumped);
    }

    var colour = read(at);

    // **Snow**, which is the only one of the three that is per pixel rather
    // than per line, and the reason this is called artefacts rather than
    // noise: it sits on top of the picture instead of replacing it.
    let grain = hash21(vec2<f32>(uv.x * lines, uv.y * lines + seconds * 60.0)) - 0.5;
    let lifted = colour.rgb + vec3<f32>(grain * snow * 0.5) + vec3<f32>(band * 0.12);

    return vec4<f32>(clamp(lifted, vec3<f32>(0.0), vec3<f32>(1.0)), source.a);
}

/// A picture from the library, used as a stencil.
///
/// **Stretched over the whole picture**, both axes independently, whatever its
/// own size: the mask is sampled at the same place as the pixel it masks. A
/// mask is made for the picture it masks, so one authored at the composition's
/// own size lands pixel for pixel, and one authored at any other size still
/// covers the same thing. Keeping its proportions instead would put the mask's
/// own edge somewhere in the picture, which is an edge nobody drew.
///
/// **Brightness, not one channel.** Grey is the point rather than a special
/// case (owner, 2026-10-13: "dann koennten es auch graustufen sein"), so the
/// mask is read as a number from nought to one and used whole — which is what
/// makes a soft edge in the mask a soft edge in the picture. Weighted the way
/// every other luminance here is weighted, so a mask drawn in colour by
/// accident reads as the brightness somebody saw when they drew it.
///
/// Its own alpha counts as well. A mask exported with a transparent background
/// has nothing drawn there, and nothing drawn is nothing kept; on an opaque
/// black-and-white mask the alpha is one everywhere and this term does nothing.
fn mask(uv: vec2<f32>, source: vec4<f32>) -> vec4<f32> {
    let amount = clamp(effect.values.x, 0.0, 1.0);
    let cut_out = effect.values.y >= 0.5;
    let inverted = effect.values.z >= 0.5;

    let picture = textureSampleLevel(mask_texture, mask_sampler, uv, 0.0);
    var level = dot(picture.rgb, LUMA_WEIGHTS) * picture.a;
    if inverted {
        level = 1.0 - level;
    }
    // Amount fades the mask out towards doing nothing, never towards hiding
    // more: an operator riding this during a show is bringing the hole in and
    // taking it out again.
    level = mix(1.0, level, amount);

    // **The two modes are different pictures, not one with a flag.** Multiply
    // darkens towards black and leaves the layer covering what is under it;
    // cutting out lowers the layer's own opacity, so what is on the layer
    // underneath comes through the hole. Straight alpha, as everywhere in this
    // shader, so the second one is a multiply on the alpha channel alone.
    if cut_out {
        return vec4<f32>(source.rgb, source.a * level);
    }
    return vec4<f32>(source.rgb * level, source.a);
}

/// How bright one neighbour is, with its transparency counted.
///
/// Brightness times alpha, so the border between a placed picture and the
/// empty canvas around it is an edge like any other: a camera scaled down on a
/// wall is outlined, which is what an outline of "the picture" means.
fn edge_luma(at: vec2<f32>) -> f32 {
    let pixel = read(at);
    return dot(pixel.rgb, LUMA_WEIGHTS) * pixel.a;
}

/// Only the outlines, in one colour, transparent between them.
///
/// A Sobel operator on brightness: the change across the pixel horizontally
/// and vertically, each weighted towards the pixel's own row and column, and
/// the size of the two together. Compared at `width_px` apart rather than
/// against the immediate neighbours, which is what makes the line wide enough
/// to survive being spread over LED panels.
///
/// `values` is amount, threshold, width; `colour` is the line's.
///
/// **The crossfade is done premultiplied** and turned back at the end. A
/// straight-alpha mix of a picture with a line that is transparent off the
/// edges would pull the line's colour into every pixel the picture covers, at
/// half strength by the middle of the fader — a tint nobody asked for. Mixed
/// premultiplied, a transparent pixel of the outlines contributes nothing,
/// which is what transparent means.
fn edges(uv: vec2<f32>, source: vec4<f32>) -> vec4<f32> {
    let amount = clamp(effect.values.x, 0.0, 1.0);
    // Off is the picture untouched, to the bit: the renderer proves every
    // published effect neutral at its defaults, and a crossfade at nought that
    // merely rounded back would not be.
    if amount <= 0.0 {
        return source;
    }
    let threshold = clamp(effect.values.y, 0.0, 1.0);
    let width = max(effect.values.z, 1.0);
    let step = effect.texel * width;

    let top_left = edge_luma(uv + vec2<f32>(-step.x, -step.y));
    let top = edge_luma(uv + vec2<f32>(0.0, -step.y));
    let top_right = edge_luma(uv + vec2<f32>(step.x, -step.y));
    let left = edge_luma(uv + vec2<f32>(-step.x, 0.0));
    let right = edge_luma(uv + vec2<f32>(step.x, 0.0));
    let bottom_left = edge_luma(uv + vec2<f32>(-step.x, step.y));
    let bottom = edge_luma(uv + vec2<f32>(0.0, step.y));
    let bottom_right = edge_luma(uv + vec2<f32>(step.x, step.y));

    let across = (top_right + 2.0 * right + bottom_right) - (top_left + 2.0 * left + bottom_left);
    let down = (bottom_left + 2.0 * bottom + bottom_right) - (top_left + 2.0 * top + top_right);
    // Four is the most either can be for brightness between nought and one,
    // so this is nought to about one and a half and the threshold can be read
    // as a share of a hard black-to-white step.
    let strength = length(vec2<f32>(across, down)) / 4.0;
    let edge = smoothstep(threshold, threshold + EDGE_SOFTNESS, strength);

    let line_alpha = edge * clamp(effect.colour.a, 0.0, 1.0);
    let line = effect.colour.rgb * line_alpha;
    let picture = source.rgb * source.a;

    let alpha = mix(source.a, line_alpha, amount);
    let premultiplied = mix(picture, line, amount);
    if alpha <= 0.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(premultiplied / alpha, alpha);
}

/// A well-mixed thirty-two bits from thirty-two, in integers.
///
/// The PCG output function. Not the sine hash the glitch uses: that one is fine
/// for texture and poor at large inputs, and this one is fed a pixel position
/// and a frame count that climb for ever.
fn mixed(value: u32) -> u32 {
    let state = value * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

/// A number from nought up to one, different for every pixel, channel and
/// frame.
fn dither(pixel: vec2<f32>, channel: u32) -> f32 {
    let x = u32(pixel.x);
    let y = u32(pixel.y);
    let seed = mixed(x ^ mixed(y ^ mixed(effect.tick * 4u + channel)));
    // Twenty-four bits, which an `f32` holds exactly, so this never reaches one.
    return f32(seed >> 8u) / 16777216.0;
}

/// Rounds to the eight bits the picture is stored in, up or down at random in
/// proportion to how close each is.
///
/// **What keeps a fading trail from stopping.** A trail fades by being
/// multiplied a little below one every frame, and in eight bits a long trail's
/// step is less than half a level — so rounding to the nearest gives back the
/// level it started at, and the trail stands on the wall for ever at a fifth of
/// its brightness. Rounded at random, it comes out right on average however
/// small the step, and a level of nought stays nought, so it always ends.
fn stochastic(value: f32, noise: f32) -> f32 {
    return floor(value * 255.0 + noise) / 255.0;
}

/// The picture over what it was a moment ago, fading.
///
/// `values` is the length in seconds and the mode, nought for behind and one
/// for lighten. The second picture bound to this pass is not a mask: it is the
/// instance's memory, which holds what this pass drew last frame, and what is
/// drawn here goes back into it.
///
/// **Each frame the memory is faded once** by what one frame is worth against
/// the length, so a frame put in now is gone after the length has passed
/// whatever the wall's rate. Straight alpha, as everywhere in this shader.
fn echo(uv: vec2<f32>, pixel: vec2<f32>, source: vec4<f32>) -> vec4<f32> {
    let length = effect.values.x;
    // Nought is the picture untouched, to the bit: the published default, and
    // what the renderer proves every effect is at its defaults.
    if length <= 0.0 {
        return source;
    }
    let trail = textureSampleLevel(mask_texture, mask_sampler, uv, 0.0);
    let keep = exp2(-ECHO_GONE_HALVINGS * max(effect.frame, 0.0) / length);
    let faded_alpha = stochastic(trail.a * keep, dither(pixel, 3u));

    if effect.values.y >= 0.5 {
        // **Lighten**: whichever is brighter, channel by channel. Compared
        // premultiplied, so the trail's own fading counts and a trail half gone
        // is half as bright; each channel rounded on its own, because on a
        // full-frame picture the trail lives in the colour rather than in the
        // alpha, which stays at one.
        let faded = vec3<f32>(
            stochastic(trail.r * trail.a * keep, dither(pixel, 0u)),
            stochastic(trail.g * trail.a * keep, dither(pixel, 1u)),
            stochastic(trail.b * trail.a * keep, dither(pixel, 2u)),
        );
        if faded_alpha <= 0.0 && all(faded <= vec3<f32>(0.0)) {
            return source;
        }
        let alpha = max(source.a, faded_alpha);
        if alpha <= 0.0 {
            return vec4<f32>(0.0);
        }
        let premultiplied = max(source.rgb * source.a, faded);
        return vec4<f32>(min(premultiplied / alpha, vec3<f32>(1.0)), alpha);
    }

    // **Behind**: the newest frame over the trail. Where the picture is opaque
    // that is the picture and nothing else — returned as it came, rather than
    // through arithmetic that would round it — and where nothing is left of
    // the trail it is the picture too.
    if source.a >= 1.0 || faded_alpha <= 0.0 {
        return source;
    }
    // Only the alpha fades. The trail keeps its own colour, which is what makes
    // it dissolve into whatever is under the layer rather than going grey.
    let alpha = source.a + faded_alpha * (1.0 - source.a);
    let rgb = (source.rgb * source.a + trail.rgb * faded_alpha * (1.0 - source.a)) / alpha;
    return vec4<f32>(rgb, alpha);
}

/// A number from minus one up to one, from thirty-two well-mixed bits.
fn signed_unit(word: u32) -> f32 {
    // Twenty-four bits, which an `f32` holds exactly.
    return f32(word >> 8u) / 8388608.0 - 1.0;
}

/// Where a shake is along one axis at `steps`, from minus one to one.
///
/// A new place at random at every whole step, and an eased glide from one to
/// the next: still at each place for an instant and quickest between them, so
/// it reads as being thrown about rather than as a flicker. The places are
/// mixed from the step's number and the axis, so the two axes wander apart.
fn wander(steps: f32, axis: u32) -> f32 {
    let whole = floor(steps);
    let index = u32(max(whole, 0.0));
    let here = signed_unit(mixed(index * 2u + axis));
    let next = signed_unit(mixed((index + 1u) * 2u + axis));
    return mix(here, next, smoothstep(0.0, 1.0, steps - whole));
}

/// One straight-alpha pixel from between four, weighted by how near each is.
///
/// An effect pass samples nearest, which is right for every pass that reads
/// the pixel it is drawing or one a whole number of pixels away. A shake reads
/// from between pixels, and moves by less than one from frame to frame: read
/// nearest, its edges would crawl. So it blends the four itself, through
/// premultiplied colour, so that a clear pixel beside a coloured one lends it
/// coverage and not its black.
fn read_between(uv: vec2<f32>) -> vec4<f32> {
    let at = uv / effect.texel - 0.5;
    let corner = floor(at);
    let towards = at - corner;
    let texel = effect.texel;
    let a = read((corner + vec2<f32>(0.5, 0.5)) * texel);
    let b = read((corner + vec2<f32>(1.5, 0.5)) * texel);
    let c = read((corner + vec2<f32>(0.5, 1.5)) * texel);
    let d = read((corner + vec2<f32>(1.5, 1.5)) * texel);
    let top = mix(vec4<f32>(a.rgb * a.a, a.a), vec4<f32>(b.rgb * b.a, b.a), towards.x);
    let bottom = mix(vec4<f32>(c.rgb * c.a, c.a), vec4<f32>(d.rgb * d.a, d.a), towards.x);
    let blended = mix(top, bottom, towards.y);
    if blended.a <= 0.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(blended.rgb / blended.a, blended.a);
}

/// The picture thrown about at random, zoomed in so that it never leaves the
/// frame.
///
/// `values` is how far it may go, as a fraction of the shorter side with the
/// amount already in it, and how many new places a second. The zoom is the
/// least that keeps the picture over the frame wherever it is thrown: the
/// shake reaches as far in pixels across as up and down, so on a wide picture
/// it is the height that runs out first, and one zoom for both axes keeps the
/// picture its own shape.
fn shake(uv: vec2<f32>, source: vec4<f32>) -> vec4<f32> {
    let reach_of_shorter = max(effect.values.x, 0.0);
    // Nought is the picture untouched, to the bit: the published default, and
    // what the renderer proves every effect is at its defaults.
    if reach_of_shorter <= 0.0 {
        return source;
    }
    let size = 1.0 / effect.texel;
    let reach = reach_of_shorter * min(size.x, size.y) * effect.texel;
    let zoom = 1.0 - 2.0 * max(reach.x, reach.y);

    // The clock every pass in a frame shares, so every output shakes alike.
    let steps = effect.seconds * max(effect.values.y, 0.0);
    let thrown = vec2<f32>(wander(steps, 0u), wander(steps, 1u)) * reach;

    return read_between((uv - 0.5) * zoom + 0.5 + thrown);
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
        case KIND_INVERT: {
            return invert(source);
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
        case KIND_MASK: {
            return mask(input.uv, source);
        }
        case KIND_EDGES: {
            return edges(input.uv, source);
        }
        case KIND_ECHO: {
            return echo(input.uv, input.position.xy, source);
        }
        case KIND_SHAKE: {
            return shake(input.uv, source);
        }
        // Unreachable: the renderer only ever writes a kind it has a branch
        // for. Returning the picture untouched is the one answer that cannot
        // put something on a wall that nobody asked for.
        default: {
            return source;
        }
    }
}
