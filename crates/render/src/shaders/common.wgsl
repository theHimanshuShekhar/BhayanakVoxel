// What the primary and reference kernels share: their bindings, what Materials look like, the
// sky, and how a linear colour is written to the image. Used after trace.wgsl.

struct Camera {
    // The Voxel the camera is in, then its position inside that Voxel. The CPU splits the f64
    // position, so these stay small and exact wherever the camera is.
    cell: vec4<i32>,
    origin: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    // Tangent of half the vertical field of view, then three unused.
    params: vec4<f32>,
}

struct Frame {
    // The unit vector towards the sun, then the cosine of the sun's angular radius.
    sun: vec4<f32>,
    // The index of this sample among those accumulated (0 starts a new image), the seed of the
    // random numbers, then two unused.
    params: vec4<u32>,
}

@group(1) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(1) var output: texture_storage_2d<rgba8unorm, write>;
@group(1) @binding(2) var<uniform> frame: Frame;

// Hardcoded albedo per Material id (see planet-gen: 1 rock, 2 soil).
fn albedo(material: u32) -> vec3<f32> {
    switch material {
        case 1u: { return vec3<f32>(0.45, 0.43, 0.40); }
        case 2u: { return vec3<f32>(0.30, 0.50, 0.20); }
        default: { return vec3<f32>(1.0, 0.0, 1.0); }
    }
}

// A simple gradient from the horizon to the zenith, until the atmosphere replaces it. It uses
// the world's y axis as up, which is right near the +y pole and nowhere else.
fn sky(dir: vec3<f32>) -> vec3<f32> {
    return mix(vec3<f32>(0.75, 0.82, 0.92), vec3<f32>(0.25, 0.45, 0.85), clamp(dir.y, 0.0, 1.0));
}

// The ray from the camera through `pixel` of an image `size` pixels across. `offset` says where
// in the pixel, from (0, 0) at its top left to (1, 1); (0.5, 0.5) is the centre.
fn camera_ray(pixel: vec2<u32>, size: vec2<u32>, offset: vec2<f32>) -> Ray {
    let aspect = f32(size.x) / f32(size.y);
    let ndc = (vec2<f32>(pixel) + offset) / vec2<f32>(size) * 2.0 - 1.0;
    let tan_half = camera.params.x;
    var ray: Ray;
    ray.cell = camera.cell.xyz;
    ray.origin = camera.origin.xyz;
    ray.dir = normalize(camera.forward.xyz + camera.right.xyz * (ndc.x * aspect * tan_half) - camera.up.xyz * (ndc.y * tan_half));
    return ray;
}

// Linear colour to the image's sRGB-encoded values, which a plain unorm texture then clamps to
// 0..1.
fn encode(linear: vec3<f32>) -> vec3<f32> {
    return pow(linear, vec3<f32>(1.0 / 2.2));
}
