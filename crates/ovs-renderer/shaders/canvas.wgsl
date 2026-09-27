// The composition shader: one instanced quad per layer.
//
// WGSL rather than GLSL, and not by preference. naga's GLSL front end does not
// implement binding qualifiers at all, so a GLSL shader cannot name a texture or
// a sampler — which rules it out for everything past a flat colour. WGSL is
// naga's first-class input and supports the full binding model.
//
// Coordinate convention: +Y is UP in clip space here. naga emits the
// OpenGL-to-Vulkan Y conversion when it lowers this to SPIR-V, so a shader
// author writes what a shader author expects and Vulkan's own +Y-down
// convention never leaks into shader source. The viewport is therefore a plain
// positive one: flipping it as well would apply the conversion twice and put
// every wall upside down. That is a property of the toolchain rather than of
// Vulkan, so `the_coordinate_convention_is_y_up` asserts it against a rendered
// frame rather than trusting this comment.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec4<f32>,
    @location(1) uv: vec2<f32>,
    // Flat: it is a per-instance constant, and interpolating it would be a
    // rounding error's worth of wrong across a large quad.
    @location(2) @interpolate(flat) identity: f32,
    @location(3) @interpolate(flat) passthrough: f32,
    @location(4) @interpolate(flat) planar: f32,
    @location(5) @interpolate(flat) sample_scale: f32,
}

@vertex
fn vertex_main(
    @builtin(vertex_index) vertex_index: u32,
    // the quad's centre in NDC, then the point its source rectangle turns about
    // in texture space — two floats that were padding until the input end of a
    // piece learnt to turn (owner, 2026-09-23)
    @location(0) placement: vec4<f32>,
    // straight RGBA, alpha already scaled by opacity and bus gain
    @location(1) colour: vec4<f32>,
    // left, top, right, bottom, in texture space
    @location(2) source: vec4<f32>,
    // what this quad's blend mode fades towards: 0 for black, 1 for white
    @location(3) identity: f32,
    // the basis, as two column vectors: x axis then y axis
    @location(4) basis: vec4<f32>,
    // 1 when the texture is already premultiplied and must not be scaled again
    @location(5) passthrough: f32,
    // 0 for an RGBA texture; otherwise which Y'CbCr convention its planes use,
    // as `TextureLayout::shader_code` numbers them
    @location(6) planar: f32,
    // what a sample is multiplied by before it is converted: 1 for eight-bit
    // planes, and the ten-bit correction for sixteen-bit ones
    @location(7) sample_scale: f32,
    // the two-by-two matrix the source rectangle is turned by, row-major, with
    // the correction for the texture's shape already in it. The identity for
    // everything that is not a turned piece, which is why there is no second
    // path for one
    @location(8) source_turn: vec4<f32>,
) -> VertexOutput {
    // Two triangles, six vertices. The geometry comes from the vertex index, so
    // only the rectangle and the colour are per-instance and the whole
    // composition is one draw call.
    var offsets = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let corner = offsets[vertex_index];

    // The corner as a sign pair, then placed through the basis. With no
    // rotation the basis is the quad's half-extents and this is an axis-aligned
    // rectangle; with rotation it is the same arithmetic on a turned basis,
    // which is why there is no second path for turned quads.
    let sign = corner * 2.0 - vec2<f32>(1.0, 1.0);
    let offset = basis.xy * sign.x + basis.zw * sign.y;

    var out: VertexOutput;
    out.position = vec4<f32>(placement.xy + offset, 0.0, 1.0);
    // Texture space has its origin at the top-left while the quad is built
    // bottom-up, so V runs the other way here: the quad's bottom corner takes
    // the source rectangle's bottom edge. One flip, in one place, rather than in
    // every caller that supplies pixels.
    //
    // A quad usually samples its whole image, and `0,0,1,1` makes this the
    // identity. A piece samples one rectangle of the composition, which is the
    // same arithmetic with a different rectangle rather than a second shader.
    let square = vec2<f32>(
        mix(source.x, source.z, corner.x),
        mix(source.w, source.y, corner.y),
    );
    // Turned about the pivot the instance carries. With the identity matrix the
    // arm is put back exactly where it came from, so an unturned quad takes the
    // same arithmetic rather than a branch.
    let pivot = placement.zw;
    let arm = square - pivot;
    out.uv = pivot
        + vec2<f32>(
            source_turn.x * arm.x + source_turn.y * arm.y,
            source_turn.z * arm.x + source_turn.w * arm.y,
        );
    out.colour = colour;
    out.identity = identity;
    out.passthrough = passthrough;
    out.planar = planar;
    out.sample_scale = sample_scale;
    return out;
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
// The colour planes of a picture that has them. An RGBA texture binds its own
// image here twice — a descriptor set must name something for every binding it
// declares — and `planar` being zero means these are never read.
@group(0) @binding(2) var chroma_b: texture_2d<f32>;
@group(0) @binding(3) var chroma_r: texture_2d<f32>;

// Turns one Y'CbCr sample into linear-ish RGB.
//
// The conversion the decoder used to do with `swscale`, on the processor, into
// a frame 2.7 times larger than the one it started with — which was then copied
// twice a layer a frame and was most of what the renderer spent its time on
//. Here it is a handful of multiplies in a pass that was already
// sampling the texture.
//
// The chroma planes are sampled with the same normalised coordinates as the
// luma one, so a half-size plane is upsampled bilinearly by the sampler and
// nobody has to write that. It is the same thing `swscale` was doing.
// `scale` brings a sample into the domain the constants below are written in.
// It is 1 for an eight-bit plane, which is every clip this renderer played
// until Main 10 — and for a ten-bit one it is the number that makes 64..940
// land exactly where 16..235 does, so none of the arithmetic here changes.
fn from_ycbcr(y_in: f32, cb_in: f32, cr_in: f32, convention: f32, scale: f32) -> vec3<f32> {
    let y = y_in * scale;
    let cb = cb_in * scale;
    let cr = cr_in * scale;
    // The odd codes are the narrow range every video file uses unless it says
    // otherwise: luma 16..235, chroma 16..240. Shown as though it were the
    // whole range, a clip is visibly washed out and its blacks are grey.
    // Odd rather than a list of codes, so a matrix added to
    // `TextureLayout::shader_code` needs no change here — the pairing of
    // narrow-then-full is the convention that numbering follows.
    let narrow = (convention - 2.0 * floor(convention * 0.5)) > 0.5;
    var luma = y;
    var blue = cb - 0.5;
    var red = cr - 0.5;
    if (narrow) {
        luma = (y - 16.0 / 255.0) * (255.0 / 219.0);
        blue = (cb - 128.0 / 255.0) * (255.0 / 224.0);
        red = (cr - 128.0 / 255.0) * (255.0 / 224.0);
    }

    // 1 and 2 are BT.709, which is HD and everything after it; 3 and 4 are
    // BT.601, which is standard definition and a good deal of older camera
    // material; 5 and 6 are BT.2020, which most Main 10 material is tagged as.
    // The difference is not subtle in greens and skin.
    //
    // BT.2020's *matrix* only. The primaries are not converted and a PQ or HLG
    // transfer is not tone-mapped here — see `ColourMatrix`.
    if (convention > 4.5) {
        return vec3<f32>(
            luma + 1.4746 * red,
            luma - 0.164553 * blue - 0.571353 * red,
            luma + 1.8814 * blue,
        );
    }
    if (convention > 2.5) {
        return vec3<f32>(
            luma + 1.402 * red,
            luma - 0.344136 * blue - 0.714136 * red,
            luma + 1.772 * blue,
        );
    }
    return vec3<f32>(
        luma + 1.5748 * red,
        luma - 0.187324 * blue - 0.468124 * red,
        luma + 1.8556 * blue,
    );
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Always samples, always multiplies. A quad with no image of its own is
    // given a one-by-one white texture, which is the identity for a multiply,
    // so textured and untextured quads take the same path instead of branching
    // per fragment.
    var texel = textureSample(source_texture, source_sampler, input.uv);
    if (input.planar > 0.5) {
        // Three planes rather than one: the first is luma, and these two are
        // the colour. Opaque, because a codec that carries alpha is not one of
        // the ones decoded this way.
        let cb = textureSample(chroma_b, source_sampler, input.uv).r;
        let cr = textureSample(chroma_r, source_sampler, input.uv).r;
        texel = vec4<f32>(
            clamp(
                from_ycbcr(texel.r, cb, cr, input.planar, input.sample_scale),
                vec3<f32>(0.0),
                vec3<f32>(1.0),
            ),
            1.0,
        );
    }
    let colour = texel * input.colour;

    // Premultiplied, but towards the blend mode's identity rather than always
    // towards black. That is what makes a mode exact at partial opacity instead
    // of only at full: fading a layer out has to tend towards "this layer does
    // nothing", which for a minimum is white and for everything else is black.
    //
    // A bus is the exception. Its image is already premultiplied — it *is* a
    // composited picture, alpha and all — so multiplying by alpha again would
    // darken every edge in it. Those quads say so, and are passed through.
    let premultiplied = mix(vec3<f32>(input.identity), colour.rgb, colour.a);
    let rgb = select(premultiplied, colour.rgb, input.passthrough > 0.5);

    // The alpha channel is never written — see vulkan::blend — so what is
    // returned here is only about the colour.
    return vec4<f32>(rgb, colour.a);
}
