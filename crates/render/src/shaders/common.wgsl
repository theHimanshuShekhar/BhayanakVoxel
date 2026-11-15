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
    // The index of this sample among those accumulated (0 starts a new image; of a batch, its
    // first), the seed of the random numbers, the debug view (0 is the lit image), then the
    // number of samples in the batch (0 is 1).
    params: vec4<u32>,
    // Where in its pixel primary visibility traces its ray, (0, 0) to (1, 1), then two unused.
    jitter: vec4<f32>,
    // The local up (a unit vector, the camera's `horizon_up`) that the sky is a gradient along,
    // then one unused.
    look_up: vec4<f32>,
    // The sun's radiance on a surface that faces it: a multiple of the surface's colour (look.rs).
    sun_radiance: vec4<f32>,
    // The sky at the zenith, then one unused.
    zenith: vec4<f32>,
    // The sky at the horizon and below, then the haze per Voxel along the camera ray.
    horizon: vec4<f32>,
    // 1 if natural Materials are shaded with smooth computed normals (0: face normals), then three
    // unused.
    flags: vec4<u32>,
}

@group(1) @binding(0) var<uniform> camera: Camera;
// What a kernel draws: the linear light of the pixel, as 16-bit floats, for the exposure, bloom and
// tone curve (post.wgsl) to turn into the image; or, for a debug view, the view's colours, which
// are already encoded and which that pass copies as they are.
@group(1) @binding(1) var output: texture_storage_2d<rgba16float, write>;
@group(1) @binding(2) var<uniform> frame: Frame;

// The Material table (materials.toml), one record per id. Four vec4s each; keep this in step
// with gpu_materials.rs, which documents them:
//   0: base colour, roughness
//   1: noise scale (Voxels), hue jitter (radians), value jitter, metalness
//   2: emitted light (colour times intensity)
//   3: transparency, IOR, 1 if the table defines the id, 1 if hits are shaded with smooth normals
@group(1) @binding(4) var<storage, read> materials: array<vec4<f32>>;

const RECORD_VEC4S = 4u;
const UNKNOWN = vec3<f32>(1.0, 0.0, 1.0);

// Whether the table defines `material`. An id past its end, or in a gap in it, is unknown.
fn is_known(material: u32) -> bool {
    let at = material * RECORD_VEC4S;
    return at + 3u < arrayLength(&materials) && materials[at + 3u].z != 0.0;
}

// The Material's base colour: magenta if it is unknown.
fn albedo(material: u32) -> vec3<f32> {
    if !is_known(material) {
        return UNKNOWN;
    }
    return materials[material * RECORD_VEC4S].xyz;
}

// The light the Material gives off, which is none if it is unknown.
fn emission(material: u32) -> vec3<f32> {
    if !is_known(material) {
        return vec3<f32>(0.0);
    }
    return materials[material * RECORD_VEC4S + 2u].xyz;
}

fn hash_voxel(cell: vec3<i32>) -> u32 {
    var h = bitcast<u32>(cell.x) * 0x8da6b343u ^ bitcast<u32>(cell.y) * 0xd8163841u ^ bitcast<u32>(cell.z) * 0xcb1ab31fu;
    h ^= h >> 15u;
    h *= 0x2c1b3c6du;
    return h ^ (h >> 12u);
}

// The Material's colour at `voxel`: its base colour with the hue and brightness turned by up to
// the Material's jitter, by an amount that depends only on the patch of `noise_scale` Voxels
// that `voxel` is in. There is no colour stored per Voxel: this is a function of the Voxel's
// position alone, and it does not change with distance.
fn varied_albedo(material: u32, voxel: vec3<i32>) -> vec3<f32> {
    let base = albedo(material);
    if !is_known(material) {
        return base;
    }
    let params = materials[material * RECORD_VEC4S + 1u];
    let h = hash_voxel(vec3<i32>(floor(vec3<f32>(voxel) / params.x)));
    // Two independent numbers in -1..1.
    let value = f32(h & 255u) / 255.0 * 2.0 - 1.0;
    let hue = f32((h >> 8u) & 255u) / 255.0 * 2.0 - 1.0;
    // Turn the colour about the grey axis (Rodrigues' rotation) for the hue.
    let angle = hue * params.y;
    let axis = vec3<f32>(0.57735027);
    let turned = base * cos(angle) + cross(axis, base) * sin(angle) + axis * dot(axis, base) * (1.0 - cos(angle));
    // No more than 1 in any channel: a surface that gave back more light than it took would let a
    // path's throughput grow, and the reference's depth cap would no longer bound its bias.
    return min(max(turned, vec3<f32>(0.0)) * (1.0 + value * params.z), vec3<f32>(1.0));
}

// The colour of the Voxel's surface before it is lit: the one function that both views take
// their colours from. Voxels are never more than about a pixel across (the LOD bands, see lod.rs),
// so the Voxel's own colour variation is drawn at every distance, with no blending towards a
// plain colour. `flat_colours` is for tests with an answer that is known exactly (the Reference
// view's FLAT_COLOURS): the Material's base colour, with no variation. Primary passes false.
fn surface_albedo(hit: Hit, flat_colours: bool) -> vec3<f32> {
    if flat_colours {
        return albedo(hit.material);
    }
    // lighting: Per-Voxel colour variation
    return varied_albedo(hit.material, hit.voxel);
}

// Whether a hit of `material` is shaded with smooth computed normals: every Material that is not a
// Building one (see gpu_materials.rs). Built walls keep their hard edges.
fn is_smooth(material: u32) -> bool {
    let at = material * RECORD_VEC4S;
    return at + 3u < arrayLength(&materials) && materials[at + 3u].w != 0.0;
}

// A simple gradient from the horizon to the zenith, until the atmosphere replaces it: along the
// local up (`look_up`, the Planet's radial up), not the world's y.
fn sky(dir: vec3<f32>) -> vec3<f32> {
    // lighting: Sky and atmosphere
    let height = clamp(dot(dir, frame.look_up.xyz), 0.0, 1.0);
    // lighting: Sky and atmosphere
    return mix(frame.horizon.xyz, frame.zenith.xyz, height);
}

// The light of a hit `t` Voxels from the camera as the camera sees it through the haze: the horizon's
// colour fades in as `1 - exp(-haze * t)`. Only for the first hit of the camera ray; the sky itself
// is not hazed.
fn hazed(light: vec3<f32>, t: f32) -> vec3<f32> {
    // lighting: Sky and atmosphere
    return mix(frame.horizon.xyz, light, exp(-frame.horizon.w * t));
}

// Hits further than this (30 m) keep their face normals: the bricks are not the held level 0 ring
// past it, and a Voxel is under a pixel there anyway.
const SMOOTH_UP_TO = 600.0;
// The smoothing looks at the Voxels of a cube of 5 round the empty Voxel in front of the face.
const SMOOTH_WINDOW = 2;
// How much the solid Voxels at each squared distance (0 to 12 Voxels, in a cube of 5) count:
// exp(-d2 / (2 * 1.4^2)).
const SMOOTH_WEIGHTS = array<f32, 13>(
    1.0, 0.774837429, 0.600373041, 0.465191504, 0.360447789, 0.279288438, 0.216403135,
    0.167677249, 0.129922608, 0.100668900, 0.078002031, 0.060438894, 0.046830317,
);

// The surface's normal at the Voxel `voxel`, which a ray entered through the face with unit normal
// `face`: the direction away from the solid Voxels round the empty one in front of that face, each
// weighted by how near it is (a Gaussian of 1.4 Voxels), kept to the face's side by adding a
// quarter of the face's normal. On a stair-step slope that is the slope's normal; on a flat face,
// the face's; where there is no solid round it, the face's. The prototype's, as rendered (#148).
fn smooth_normal(voxel: vec3<i32>, face: vec3<f32>) -> vec3<f32> {
    let front = voxel + vec3<i32>(face);
    var cache = new_brick_cache();
    var sum = vec3<f32>(0.0);
    for (var dz = -SMOOTH_WINDOW; dz <= SMOOTH_WINDOW; dz++) {
        for (var dy = -SMOOTH_WINDOW; dy <= SMOOTH_WINDOW; dy++) {
            for (var dx = -SMOOTH_WINDOW; dx <= SMOOTH_WINDOW; dx++) {
                let d = vec3<i32>(dx, dy, dz);
                if solid_at(front + d, &cache) {
                    sum += vec3<f32>(d) * SMOOTH_WEIGHTS[u32(dx * dx + dy * dy + dz * dz)];
                }
            }
        }
    }
    if dot(sum, sum) < 1e-6 {
        return face;
    }
    return normalize(normalize(-sum) + 0.25 * face);
}

// The normal that `hit` is shaded with: smooth, for a natural Material in range when the stage is
// on, else the face's.
fn shading_normal(hit: Hit) -> vec3<f32> {
    if frame.flags.x == 0u || hit.t > SMOOTH_UP_TO || !is_smooth(hit.material) {
        return hit.normal;
    }
    return smooth_normal(hit.voxel, hit.normal);
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

// Linear colour to the image's sRGB-encoded values, with the true sRGB curve (IEC 61966-2-1: a
// straight line near black, then 1.055 * x^(1/2.4) - 0.055), in the Reference and Primary views
// alike. The result is clamped to 0..1, as the plain unorm texture it is stored in would. It is
// a plain clamp and no tone mapping: light brighter than 1 is white.
fn encode(linear: vec3<f32>) -> vec3<f32> {
    let c = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055, 12.92 * c, c <= vec3<f32>(0.0031308));
}

// What the debug views show, as the values written to the image (already encoded). `view` is
// never 0 here: that is the lit image, which the kernels draw themselves. The colours of misses
// are black. Keep the numbers in step with DebugView in debug_view.rs.
const VIEW_NORMALS = 1u;
const VIEW_ALBEDO = 2u;
const VIEW_DEPTH = 3u;
const VIEW_SAMPLE_COUNT = 4u;
// Drawn by the upscaler (upscale.wgsl) in primary mode; in the reference image the history is its
// samples.
const VIEW_HISTORY_LENGTH = 5u;
// The normal that the hit is shaded with, as `(n + 1) / 2`: the smooth one, or the face's.
const VIEW_SHADING_NORMALS = 6u;
// Voxels at which the depth view is half as bright as at the camera.
const DEPTH_HALF = 64.0;
// The sample count at which the sample count view reaches its brightest colour.
const SAMPLES_BRIGHTEST = 1024.0;

fn debug_colour(view: u32, hit: Hit, sample_index: u32) -> vec3<f32> {
    if view == VIEW_SAMPLE_COUNT || view == VIEW_HISTORY_LENGTH {
        let scale = clamp(log2(f32(sample_index + 1u)) / log2(SAMPLES_BRIGHTEST), 0.0, 1.0);
        return mix(vec3<f32>(0.10, 0.0, 0.30), vec3<f32>(1.0, 0.90, 0.20), scale);
    }
    if !hit.hit {
        return vec3<f32>(0.0);
    }
    switch view {
        case VIEW_NORMALS: { return hit.normal * 0.5 + 0.5; }
        case VIEW_SHADING_NORMALS: { return shading_normal(hit) * 0.5 + 0.5; }
        case VIEW_ALBEDO: { return encode(albedo(hit.material)); }
        case VIEW_DEPTH: { return vec3<f32>(DEPTH_HALF / (DEPTH_HALF + hit.t)); }
        default: { return vec3<f32>(1.0, 0.0, 1.0); }
    }
}
