// The Reference view's path tracer: the unbiased estimate of the light in the world that the
// Primary view is measured against (ADR 0005, whose rules this follows). Each dispatch adds one
// sample per pixel to a running mean in `accum`, and shows the mean.
//
// A sample follows a path from the camera. At every hit on a Lambertian surface it:
//   - adds the light the Material itself gives off;
//   - adds the sun's light, by next-event estimation: a shadow ray to a random point on the sun's
//     disc, which counts if nothing is in the way;
//   - continues in a cosine-weighted random direction, so the throughput is just the albedo.
// A path that escapes collects the sky. The sun is not part of the sky, so it is never counted
// twice. Every ray goes through trace(), the only thing that knows about the Planet's storage.
//
// There is no fixed number of bounces. From the third surface on, a path ends by chance with
// probability 1 - (the brightest channel of its throughput), and a path that goes on is scaled
// up by 1 / (1 - that probability), so the expected light is exactly what it would have been
// without the chance (Russian roulette, Arvo and Kirk 1990; PBRT 4, section 13.4). The brightest
// channel, not the luminance, so that no channel is scaled down. The one approximation is the
// depth cap, MAX_DEPTH: a path that is still going after that many surfaces stops. Among
// Materials with albedo 1 a path would never end otherwise. No colour is above 1 in any channel
// (`varied_albedo` clamps it), so the cap leaves out at most albedo^MAX_DEPTH of the light:
// 0.57 % in snow (albedo 0.98), nothing measurable for the darker Materials, and for albedo 1,
// whatever part of the light is still bouncing after 256 surfaces (ADR 0005's bias ledger, held
// by the closed-box test).
//
// NOTHING IS CLAMPED, FILTERED OR REJECTED. Not a bright sample, not a sample that is not a
// number, not a firefly: each one would remove light that the estimate has to add up, and no
// number of samples gives it back. A bright sample is noise, and noise is what samples are for.
// The only things that change a sample's weight are the roulette's rescale and the sun's
// divisor below, and both are exact. A speckle that appears is a bug to find, not to hide.
//
// The sample positions are an Owen-scrambled Sobol sequence (sampling.wgsl). The dimensions of a
// path are, by number:
//   0, 1             where in the pixel the camera ray goes        (group 0, slots 0 and 1)
//   4s + 2, 4s + 3   the sun's disc at surface s                   (group s, slots 2 and 3)
//   4s + 4, 4s + 5   the bounce direction at surface s             (group s + 1, slots 0 and 1)
// so each pair that is an integral of its own is a stratified pair, and the dimensions that
// matter most come first. They run to dimension 63, which is surface 14's bounce and surface 15's
// sun; a deeper surface's are hashed random numbers. The roulette's coin at surface s is hashed
// too (its own stream: a single number that nothing is gained by stratifying). A new term goes at
// the end of the list, not in the middle, or the pairs after it lose their stratification.

@group(1) @binding(3) var<storage, read_write> accum: array<vec4<f32>>;

// The test modes. They are pipeline constants, not inputs: the Reference view the player sees is
// compiled with the defaults below, and a test that wants another builds the pipeline with its
// own (Tracer::set_reference_options), so the player's shader has no branch for any of them.
//
// Flat colours: every Voxel of a Material has the Material's base colour, so that a scene has an
// answer that can be worked out from the Material table alone.
override FLAT_COLOURS = false;
// White noise: the sample positions are hashed random numbers in every dimension, not
// Owen-scrambled Sobol; the plain Monte Carlo baseline.
override WHITE_NOISE = false;
// A fault put into the estimator, to show that the tests that guard it can fail: a test that
// cannot fail is not a test. 0 is none.
override FAULT = 0u;
// The estimator this replaces: no roulette, and every path stops after its fifth surface.
const FAULT_FOUR_BOUNCE_CUT = 1u;
const FAULT_CUT_SURFACES = 5u;
// The roulette without its rescale of the paths that go on.
const FAULT_NO_RESCALE = 2u;
// A depth cap of 190 surfaces instead of 256, which leaves out 2 % of the light at albedo 0.98.
const FAULT_SHORT_CAP = 3u;
const FAULT_SHORT_CAP_SURFACES = 190u;

// The most surfaces a path visits, the camera ray's first hit being the first.
const MAX_DEPTH = 256u;
// A path is not ended by chance before its third surface: the first ones carry most of the
// light, and ending them only adds noise.
const ROULETTE_FROM = 2u;
// The roulette's coins are hashed random numbers of their own, from here: no other dimension is.
const ROULETTE_COIN_DIM = 0x10000000u;
// The sun's light on a surface facing it head on, as a multiple of albedo (its irradiance
// divided by pi): a Lambertian surface at angle theta to the sun reflects albedo * cos(theta)
// times this.
const SUN_RADIANCE = vec3<f32>(1.6, 1.5, 1.35);
// How far a ray starts inside the empty Voxel in front of the face it leaves, in Voxels. It
// only has to beat the rounding of the hit position, so a ray cannot hit its own face.
const NUDGE = 1e-3;

// A ray leaving the face of `hit`, which `ray` found, along `dir`. It starts in the empty Voxel
// in front of that face, a hair in from the face, rather than at a float computed from the
// distance travelled: the Voxel is exact, so a ray cannot start behind the face it left.
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

// The light arriving along one path from the camera: one sample of the pixel's value.
fn radiance(first: Ray, pixel_seed: u32, sample: u32, first_group: vec4<f32>) -> vec3<f32> {
    var ray = first;
    var throughput = vec3<f32>(1.0);
    var light = vec3<f32>(0.0);
    // The sun's samples of this surface are slots 2 and 3 of group `depth`; the bounce's of the
    // surface before are slots 0 and 1 of the same group, so one group serves both.
    var group = first_group;
    var max_depth = MAX_DEPTH;
    if FAULT == FAULT_FOUR_BOUNCE_CUT {
        max_depth = FAULT_CUT_SURFACES;
    }
    if FAULT == FAULT_SHORT_CAP {
        max_depth = FAULT_SHORT_CAP_SURFACES;
    }
    for (var depth = 0u; depth < max_depth; depth++) {
        // lighting: Bounce light
        let hit = trace(ray);
        if !hit.hit {
            // lighting: Sky and atmosphere
            light += throughput * sky(ray.dir);
            break;
        }
        // lighting: Per-Voxel colour variation
        let colour = surface_albedo(hit, FLAT_COLOURS);
        // lighting: Emissive Voxels
        light += throughput * emission(hit.material);

        // The mean of the cosine over the sun's cone is (1 + cos(radius)) / 2 of its value at the
        // cone's centre, so the sample's weight is divided by that: the estimate is then the
        // sun's light exactly, not 0.04 % short of it. Facing away it is zero, as it was.
        // lighting: Sun and shadows
        let to_sun = sample_cone(frame.sun.xyz, frame.sun.w, group.zw);
        let facing = dot(hit.normal, to_sun);
        // lighting: Sun and shadows
        if facing > 0.0 && !trace(leave(ray, hit, to_sun)).hit {
            // lighting: Sun and shadows
            light += throughput * colour * SUN_RADIANCE * (facing / (0.5 * (1.0 + frame.sun.w)));
        }

        throughput *= colour;
        if FAULT != FAULT_FOUR_BOUNCE_CUT && depth >= ROULETTE_FROM {
            let brightest = max(throughput.x, max(throughput.y, throughput.z));
            // The colour is at most 1 in every channel (`varied_albedo`), so is the throughput.
            // The path goes on with probability `brightest`, and then counts for 1 / brightest.
            if hashed_f32(pixel_seed, sample, ROULETTE_COIN_DIM + depth) >= brightest {
                break;
            }
            if FAULT != FAULT_NO_RESCALE {
                throughput /= brightest;
            }
        }
        if depth + 1u == max_depth {
            break;
        }
        group = sample_group(pixel_seed, sample, depth + 1u, WHITE_NOISE);
        let bounce_dir = cosine_hemisphere(hit.normal, group.xy);
        ray = leave(ray, hit, bounce_dir);
    }
    return light;
}

@compute @workgroup_size(8, 8)
fn reference(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let sample_index = frame.params.x;
    let pixel_seed = pixel_hash(id.xy, frame.params.y);
    // A point in the pixel from the first group's first two dimensions, so edges anti-alias as
    // samples accumulate. Slots 2 and 3 of the same group are the first surface's sun sample.
    let group0 = sample_group(pixel_seed, sample_index, 0u, WHITE_NOISE);
    let estimate = radiance(camera_ray(id.xy, size, group0.xy), pixel_seed, sample_index, group0);

    // The running mean: after n samples, mean + (new - mean) / (n + 1). The first replaces
    // whatever the buffer held. Unlike a sum, it keeps its precision however long it runs.
    let at = id.y * size.x + id.x;
    var mean = estimate;
    if sample_index > 0u {
        mean = accum[at].xyz + (estimate - accum[at].xyz) / f32(sample_index + 1u);
    }
    accum[at] = vec4<f32>(mean, 1.0);
    var shown = encode(mean);
    // A debug view replaces what is shown, not what is accumulated, so switching back to the
    // lit image loses no samples.
    if frame.params.z != 0u {
        let first = camera_ray(id.xy, size, vec2<f32>(0.5));
        // lighting: Geometry (LOD and what is loaded)
        shown = debug_colour(frame.params.z, trace(first), sample_index);
    }
    textureStore(output, id.xy, vec4<f32>(shown, 1.0));
}
