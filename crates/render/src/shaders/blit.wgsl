// Copies an image to the window's surface. The images are the size of the surface, so each texel
// is one pixel: the Primary view's (what the upscaler built) and the Reference view's (traced at
// the surface's size, ADR 0005).
//
// `fade` mixes them: 0 shows the Primary image alone, 1 the Reference image alone, and in between
// the Reference view is fading in over the last Primary image (about 150 ms, #189). The mix is
// made here, on the way to the surface, and nowhere else: the Reference view's running mean is
// never read by this pass and never written by it, so a pixel of the accumulation is only ever
// the light of the samples that were added to it. Both images hold sRGB-encoded values, which are
// mixed as they are.

@group(0) @binding(0) var primary: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var reference: texture_2d<f32>;

struct Fade {
    // x: how much of the Reference image to show, 0 to 1. The rest is unused.
    amount: vec4<f32>,
}
@group(0) @binding(3) var<uniform> fade: Fade;

struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
    // One triangle covering the whole viewport.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Vertex;
    out.position = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

@fragment
fn fragment(in: Vertex) -> @location(0) vec4<f32> {
    // Both are sampled everywhere, so that the sampling is in uniform control flow.
    let under = textureSampleLevel(primary, image_sampler, in.uv, 0.0);
    let over = textureSampleLevel(reference, image_sampler, in.uv, 0.0);
    // At 0 and at 1 the result is one image exactly, whatever is in the other.
    if fade.amount.x <= 0.0 {
        return under;
    }
    if fade.amount.x >= 1.0 {
        return over;
    }
    return mix(under, over, fade.amount.x);
}
