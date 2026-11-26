// What the primary and reference kernels share: their bindings, and what Materials look like. The
// sky and the air are sky.wgsl's. Used after trace.wgsl.

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
    // The local up (a unit vector, the camera's `horizon_up`) that the gradient sky (the atmosphere
    // off, sky.wgsl) is a gradient along, then one unused.
    look_up: vec4<f32>,
    // The sun's radiance on a surface that faces it: a multiple of the surface's colour (look.rs).
    sun_radiance: vec4<f32>,
    // The gradient sky at the zenith, then one unused.
    zenith: vec4<f32>,
    // The gradient sky at the horizon and below, then its haze per Voxel along the camera ray.
    horizon: vec4<f32>,
    // 1 if natural Materials are shaded with smooth computed normals (0: face normals); the end of
    // the smooth range in Voxels (an f32's bits); the weight of the face's normal that the smooth
    // normal is kept to the face's side with (an f32's bits); 1 if the sky and the air are the
    // atmosphere's (sky.wgsl; 0: the gradient above).
    flags: vec4<u32>,
    // The frame that Primary's sun samples are of (0 on the first frame after the upscaler's
    // history started over, a new view or a new sun, so that nothing before a frame decides its
    // shadows), then whether the view has GI (0 none; 1 Primary's bounce and radiance cache, or
    // the Reference's bounce light; 2 Primary's flat ambient of 0.25, the stand-in that #54
    // replaced, kept only for the bench's "before" numbers, and the Reference has its bounce light
    // then too), then 1 if it has emitted light, then the number of shadow rays a pixel of Primary
    // casts (at least 1). Both flags are 0 in the bench's sun-and-sky-only view, where neither
    // view has GI or emitters.
    light: vec4<u32>,
    // Primary's radiance cache (gi.wgsl): the number of the frame (it counts every frame of
    // Primary, from 1, whatever the upscaler's history does), then 1 for the frames after an Edit
    // or a new sun, when the cells of the box below forget faster, then how `shadows` weighs each
    // ray of the sun (0: the cosine at the disc's centre; 1: the ray's own cosine, as the
    // Reference's samples are), then one unused.
    gi: vec4<u32>,
    // The first Voxel of that box and one past its last (level 0 coordinates), then one unused each.
    boost_min: vec4<i32>,
    boost_max: vec4<i32>,
    // The moon (daylight.rs, #203): the unit vector towards it, then the cosine of its disc's
    // angular radius (2 with no moon: nothing is at that cosine).
    moon: vec4<f32>,
    // The colour of its disc (sky radiance), then 1 if there is a moon.
    moon_disc: vec4<f32>,
    // How visible the stars are, then how visible the moon's disc and halo are (0 to 1), then two
    // unused.
    night: vec4<f32>,
    // The rows of the matrix from a world direction to the stars' own, which the sky turns with the
    // time of day.
    star_x: vec4<f32>,
    star_y: vec4<f32>,
    star_z: vec4<f32>,
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

// Whether a hit of `material` is shaded with smooth computed normals: every Natural Material (see
// gpu_materials.rs). Built walls and Lights keep their hard edges.
fn is_smooth(material: u32) -> bool {
    let at = material * RECORD_VEC4S;
    return at + 3u < arrayLength(&materials) && materials[at + 3u].w != 0.0;
}

// Hits keep their face normals from `smooth_end()` on, which is 30 m (600 Voxels) or, where level 0
// ends sooner, a little before that (see `smooth_limit` in tracer.rs), and the smooth normal is
// faded in over the last fifth of the way to it: the window is over level 0's bricks, and past
// level 0 it would count the coarser level's bricks (not listed in level 0) as air.
const SMOOTH_BLEND_FROM = 0.8;
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
// quarter of the face's normal (the weight in `flags.z`: 0.25, the owner's pick of 2026-11-16; 0
// for look-dev). On a stair-step slope that is the slope's normal; on a flat face, the face's;
// where there is no solid round it, the face's. The prototype's, as rendered (#148).
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
    return normalize(normalize(-sum) + bitcast<f32>(frame.flags.z) * face);
}

// The normal that `hit` is shaded with: smooth, for a natural Material in range when the stage is
// on, else the face's; in between (the last fifth of the range), the two mixed.
fn shading_normal(hit: Hit) -> vec3<f32> {
    let end = bitcast<f32>(frame.flags.y);
    if frame.flags.x == 0u || hit.t >= end || !is_smooth(hit.material) {
        return hit.normal;
    }
    // Over 24 to 30 m the face's normal gives way to the smooth one, so that the change at 30 m is
    // not a ring on the ground.
    let weight = clamp((end - hit.t) / (end * (1.0 - SMOOTH_BLEND_FROM)), 0.0, 1.0);
    let smoothed = smooth_normal(hit.voxel, hit.normal);
    return normalize(mix(hit.normal, smoothed, weight) + 1e-6 * hit.normal);
}

// How far a ray starts inside the empty Voxel in front of the face it leaves, in Voxels. It
// only has to beat the rounding of the hit position, so a ray cannot hit its own face.
const NUDGE = 1e-3;

// A ray leaving the face of `hit`, which `ray` found, along `dir`. It starts in the empty Voxel
// in front of that face, a hair in from the face, rather than at a float computed from the
// distance travelled: the Voxel is exact, so a ray cannot start behind the face it left. It
// leaves the **geometric** face, whatever normal the surface is shaded with: both views' shadow
// rays and the Reference's bounces are made with it (ADR 0005).
fn leave(ray: Ray, hit: Hit, dir: vec3<f32>) -> Ray {
    let n = hit.normal;
    let corner = vec3<f32>(hit.voxel - ray.cell) + n;
    // Along the face, where the hit is; across it, 0 or 1 in the new Voxel as n points.
    let along = clamp(hit.position - corner, vec3<f32>(NUDGE), vec3<f32>(1.0 - NUDGE));
    var next: Ray;
    next.cell = hit.voxel + vec3<i32>(n);
    next.origin = select(along, vec3<f32>(0.5) - n * (0.5 - NUDGE), n != vec3<f32>(0.0));
    next.dir = dir;
    return next;
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
// The radiance cache's cell of each hit, as a colour of its own (the same colour for the same cell),
// black where the cache holds nothing yet (shade.wgsl draws it; the Reference has no cache and shows
// its lit image).
const VIEW_CACHE_CELLS = 7u;
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
