// Copies an image to the window's surface. The upscaled image is the size of the surface, so
// each texel is one pixel; the reference image is the traced size, and is stretched to fit.

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

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
    return textureSampleLevel(image, image_sampler, in.uv, 0.0);
}
