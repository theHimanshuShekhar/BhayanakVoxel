// The Reference view's path tracer: the unbiased estimate of the light in the world that the
// Primary view is measured against (ADR 0005, whose rules this follows). Each dispatch adds a
// batch of samples per pixel (one, unless the still-frame policy asks for more) to a running
// mean in `accum`, and shows the mean.
//
// A sample follows a path from the camera. At every hit on a Lambertian surface it:
//   - adds the light the Material itself gives off;
//   - adds the sun's light, by next-event estimation: a shadow ray to a random point on the sun's
//     disc, which counts if nothing is in the way;
//   - continues in a cosine-weighted random direction, so the throughput is just the albedo.
// A path that escapes collects the sky. The sun is not part of the sky, so it is never counted
// twice. Every ray goes through trace(), the only thing that knows about the Planet's storage.
//
// The sky is the atmosphere's (#53, sky.wgsl): the camera sees it from where the camera is (the
// sky-view table), and a ray that has bounced and escapes reads the **sun-free** table (the sky as a
// surface sees it, which has no sun disc: the sun is counted by its next-event estimate only). That
// table is also sampled: at every surface a second next-event estimate chooses a direction of the
// sky with a probability proportional to its brightness (`sky_sample`) and counts it if nothing is
// in the way, and the bounce ray that escapes is weighed against it (multiple importance sampling,
// the balance heuristic: Veach and Guibas 1995). The two estimators do not both count a direction
// in full; the weights add up to 1. The weights use the true density of the bounce, `bounce_pdf`,
// including the mirroring below. With the atmosphere off the sky has no samples and every escaped
// ray counts in full, as it did before.
//
// The emissive Voxels (#54, emitters.wgsl) are lights that are sampled directly, as the sky is: at
// every surface a third next-event estimate chooses one emitter from the bounded list of the cell the
// surface is in, and a direction over the solid angle that the emitter's faces cover (so that a
// close emitter makes no speckle), and counts it if nothing is in the way. The bounce ray that
// finds an emitter is weighed against that sample by the balance heuristic, with the true density of
// the sampling (`emitter_hit_pdf`) and of the bounce, so the weights add up to 1; an emitter that
// the list does not hold is found by the bounce rays alone and counted in full. A room that is lit
// by one emissive Voxel and nothing else converges for that reason.
//
// The first surface of a path (the one the camera sees) is shaded with the smooth computed normal
// of #202 where the Material has one (`shading_normal`, common.wgsl): the sun's cosine and the
// bounce's cosine-weighted direction use it, while the shadow ray, and a bounce that would go
// below the face, use the face's geometric normal, so no ray starts inside the surface. Later
// surfaces use face normals. The camera ray's first hit is seen through the haze, which fades
// its light, and only its light, to the horizon's colour.
//
// The sun and the sky alone (`frame.light`, #52): in the bench's sun-and-sky view the path stops at
// its first surface (it follows no bounce, so it has no GI) and adds no emitted light, which is
// what Primary draws in that view too (shade.wgsl: no ambient term, no emission). What is left is
// the sun's light with its shadows, and the sky, which both views draw from the same disc.
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
//   64 + 2s, 65 + 2s the sky's direction at surface s, s < 4       (groups 16 and 17)
//   72 + 4s ... 75 + 4s the emitter at surface s: which box, which of its faces, and the two numbers
//                    that place the point in the face, s < 4        (groups 18 to 21)
// so each pair that is an integral of its own is a stratified pair, and the dimensions that
// matter most come first. The sun's and the bounce's run to dimension 63, which is surface 14's
// bounce and surface 15's sun; a deeper surface's are hashed random numbers. The roulette's coin at surface s is hashed
// too (its own stream: a single number that nothing is gained by stratifying), and so are the sky's
// and the emitters' from surface 4 on. A new term goes at the end of the list, not in the middle, or the
// pairs after it lose their stratification.

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
// The sun counts wherever it faces the surface, with no shadow ray: the same estimator without its
// visibility, so that the bench can tell which pixels of a view have a shadow's edge in them (an
// edge that the image with shadows has and this one does not is a shadow's).
override NO_SHADOW_RAYS = false;
// How the sky is estimated, when it is the atmosphere's. 0: both estimators, weighed against each
// other, which is the estimator. 1: bounce rays only (the sky is found only by the bounce that
// escapes). 2: the sky's samples only. The three have the same expected value where the bounce rays
// cannot leave below the horizon, which a test holds: it is how the weights are shown to add up.
override SKY_MODE = 0u;
// Haze on the bounce rays, which the estimator leaves off (ADR 0005's bias ledger): for the test that
// measures what leaving it off is worth. When set, the air along every segment between two surfaces
// is the air at the ground, whose extinction is the most any segment of a path has (it thins with
// height), and what it adds is the horizon's sky: an upper bound of the haze, not the air itself.
override BOUNCE_HAZE = false;
// How the emissive Voxels are estimated. 0: both estimators, weighed against each other, which is the
// estimator. 1: bounce rays only (an emitter is found only by the bounce that hits it). 2: the
// emitters' own samples only (where the list has the emitter: one the list does not hold is still
// found by a bounce ray). The three have the same expected value wherever every emitter is in the
// list of every surface that sees it, which a test holds: it is how the weights are shown to add up.
override EMITTER_MODE = 0u;
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
// The sky counted in full by both of its estimators.
const FAULT_SKY_DOUBLE_COUNT = 4u;
// The emitters counted in full by both of their estimators.
const FAULT_EMITTER_DOUBLE_COUNT = 5u;

// The most surfaces a path visits, the camera ray's first hit being the first.
const MAX_DEPTH = 256u;
// A path is not ended by chance before its third surface: the first ones carry most of the
// light, and ending them only adds noise.
const ROULETTE_FROM = 2u;
// The roulette's coins are hashed random numbers of their own, from here: no other dimension is.
const ROULETTE_COIN_DIM = 0x10000000u;
// The sun's light on a surface facing it head on, as a multiple of albedo (its irradiance
// divided by pi), is `frame.sun_radiance` (look.rs): a Lambertian surface at angle theta to the
// sun reflects albedo * cos(theta) times it.
// The density, over solid angle, with which the bounce at a surface with shading normal `normal` and
// geometric normal `face` chooses `dir` (above the face): the cosine-weighted direction about the
// shading normal where it is above the face, and the mirror image of the one that was below it. Both
// are the same density where the two normals are one.
fn bounce_pdf(normal: vec3<f32>, face: vec3<f32>, dir: vec3<f32>) -> f32 {
    let above = dot(dir, face);
    if above < 0.0 {
        return 0.0;
    }
    let mirrored = dir - 2.0 * above * face;
    return (max(dot(normal, dir), 0.0) + max(dot(normal, mirrored), 0.0)) / PI;
}

// How much of the sky `dir` that a bounce ray found counts, against the sky's own samples: its
// density over the two, the balance heuristic. 1 where the sky is not sampled.
fn escape_weight(normal: vec3<f32>, face: vec3<f32>, dir: vec3<f32>) -> f32 {
    if !atmosphere_on() || SKY_MODE == 1u || FAULT == FAULT_SKY_DOUBLE_COUNT {
        return 1.0;
    }
    if SKY_MODE == 2u {
        return 0.0;
    }
    let bounce = bounce_pdf(normal, face, dir);
    let sky = sky_pdf(dir);
    if bounce + sky <= 0.0 {
        return 1.0;
    }
    return bounce / (bounce + sky);
}

// The four numbers that choose an emitter at surface `depth`: which box, which of its faces and the
// point in it. The first four surfaces' are a group each, 18 to 21; the rest's are hashed in groups of
// their own.
fn emitter_uniform(pixel_seed: u32, sample: u32, depth: u32) -> vec4<f32> {
    return sample_group(pixel_seed, sample, select(0x2000u + depth, 18u + depth, depth < 4u), WHITE_NOISE);
}

// The two numbers that choose the sky's direction at surface `depth`: the first four surfaces' are
// two slots each of the groups 16 and 17, the rest's are hashed, in groups of their own so that no
// other dimension's numbers are repeated.
fn sky_uniform(pixel_seed: u32, sample: u32, depth: u32) -> vec2<f32> {
    if depth < 4u {
        let group = sample_group(pixel_seed, sample, 16u + depth / 2u, WHITE_NOISE);
        return select(group.xy, group.zw, (depth & 1u) == 1u);
    }
    return sample_group(pixel_seed, sample, 0x1000u + depth, WHITE_NOISE).xy;
}

// The light arriving along one path from the camera: one sample of the pixel's value. `uv` is the
// pixel's place in the image (0 to 1), which the haze on the camera ray's first hit is looked up by.
fn radiance(
    first: Ray, pixel_seed: u32, sample: u32, first_group: vec4<f32>, uv: vec2<f32>,
) -> vec3<f32> {
    var ray = first;
    var throughput = vec3<f32>(1.0);
    var light = vec3<f32>(0.0);
    // The sun's samples of this surface are slots 2 and 3 of group `depth`; the bounce's of the
    // surface before are slots 0 and 1 of the same group, so one group serves both.
    var group = first_group;
    // The distance to the camera ray's first hit, for the haze; negative if it hit nothing.
    var first_t = -1.0;
    // The normals of the surface that the bounce ray left, for the weight of the sky it finds.
    var bounce_normal = vec3<f32>(0.0, 0.0, 1.0);
    var bounce_face = vec3<f32>(0.0, 0.0, 1.0);
    // The Voxel that the bounce ray left, whose cell's list of emitters the ray found one of.
    var bounce_voxel = vec3<i32>(0);
    var max_depth = MAX_DEPTH;
    if FAULT == FAULT_FOUR_BOUNCE_CUT {
        max_depth = FAULT_CUT_SURFACES;
    }
    if FAULT == FAULT_SHORT_CAP {
        max_depth = FAULT_SHORT_CAP_SURFACES;
    }
    // Without GI (`frame.light.y` is 0, in the bench's sun-and-sky-only view) the path ends at its
    // first surface, so that what is left is the sun's light there and the sky seen directly.
    if frame.light.y == 0u {
        max_depth = 1u;
    }
    for (var depth = 0u; depth < max_depth; depth++) {
        // lighting: Bounce light
        let hit = trace(ray);
        if !hit.hit {
            if depth == 0u {
                // lighting: Sky and atmosphere
                light += throughput * sky(ray.dir);
            } else {
                // lighting: Sky and atmosphere
                light += throughput * escaped_sky(ray.dir) * escape_weight(bounce_normal, bounce_face, ray.dir);
            }
            break;
        }
        if depth == 0u {
            first_t = hit.t;
        } else if BOUNCE_HAZE && atmosphere_on() {
            let across = ray.dir - atmo.up.xyz * dot(ray.dir, atmo.up.xyz);
            let along = select(across, vec3<f32>(1.0, 0.0, 0.0), dot(across, across) < 1e-10);
            // The look's extra haze (the tint) counts too: it fades to the sky.
            let through = exp(-(medium_at(0.0).extinction + atmo.tint_horizon.w) / AIR_VOXELS_PER_METRE * hit.t);
            // lighting: Sky and atmosphere
            let fades_to = escaped_sky(normalize(along));
            // lighting: Sky and atmosphere
            light += throughput * fades_to * (vec3<f32>(1.0) - through);
            throughput *= through;
        }
        // The normal the surface is shaded with: smooth for the camera's first hit, else the face's.
        var normal = hit.normal;
        if depth == 0u {
            normal = shading_normal(hit);
        }
        // lighting: Per-Voxel colour variation
        let colour = surface_albedo(hit, FLAT_COLOURS);
        // lighting: Emissive Voxels
        let emitted = emission(hit.material) * f32(frame.light.z);
        // The light that a bounce ray finds on an emitter counts in full, unless the emitter's own
        // sample could have found it from the surface the ray left (it is in that cell's list):
        // then the two are weighed against each other. The camera's own ray (depth 0) sees it.
        var emitted_weight = 1.0;
        if depth > 0u && any(emitted > vec3<f32>(0.0)) && EMITTER_MODE != 1u && FAULT != FAULT_EMITTER_DOUBLE_COUNT {
            let listed = emitter_hit_pdf(ray, hit, bounce_voxel);
            if listed > 0.0 {
                let bounce = bounce_pdf(bounce_normal, bounce_face, ray.dir);
                emitted_weight = select(bounce / (bounce + listed), 0.0, EMITTER_MODE == 2u);
            }
        }
        // lighting: Emissive Voxels
        light += throughput * emitted * emitted_weight;

        // The mean of the cosine over the sun's cone is (1 + cos(radius)) / 2 of its value at the
        // cone's centre, so the sample's weight is divided by that: the estimate is then the
        // sun's light exactly, not 0.04 % short of it. Facing away it is zero, as it was.
        // lighting: Sun and shadows
        let to_sun = sample_cone(frame.sun.xyz, frame.sun.w, group.zw);
        let facing = dot(normal, to_sun);
        // lighting: Sun and shadows
        if facing > 0.0 && dot(hit.normal, to_sun) > 0.0 && (NO_SHADOW_RAYS || !trace(leave(ray, hit, to_sun)).hit) {
            // lighting: Sun and shadows
            light += throughput * colour * frame.sun_radiance.xyz * (facing / (0.5 * (1.0 + frame.sun.w)));
        }

        // The sky's next-event estimate: a direction chosen in proportion to the sky's brightness,
        // which counts where nothing is in the way. The surface gives back the sky's light along it
        // times its colour and the bounce's density of it, over the density that chose it (with
        // the bounce's in the denominator too, unless it is the only estimator): the same colour and
        // density that the bounce ray that found it would have had, so the weights add up to 1.
        // Only where there is GI: the sun-and-sky-only view has no bounce light, and no sky light on
        // a surface.
        if atmosphere_on() && frame.light.y != 0u && SKY_MODE != 1u {
            let chosen = sky_sample(sky_uniform(pixel_seed, sample, depth));
            let toward = bounce_pdf(normal, hit.normal, chosen.dir);
            // lighting: Sky and atmosphere
            if chosen.pdf > 0.0 && toward > 0.0 && !trace(leave(ray, hit, chosen.dir)).hit {
                let over = select(toward + chosen.pdf, chosen.pdf, SKY_MODE == 2u || FAULT == FAULT_SKY_DOUBLE_COUNT);
                // lighting: Sky and atmosphere
                light += throughput * colour * escaped_sky(chosen.dir) * (toward / over);
            }
        }

        // The emitters' next-event estimate: one box of the list of the cell this surface is in, and a
        // direction over the solid angle it covers, which counts where the first thing the ray
        // finds is that box. The surface gives back its colour times the emitter's light times the
        // bounce's density of that direction, over the density that chose it (with the bounce's in
        // the denominator too, unless it is the only estimator): the colour and the density the
        // bounce ray that found the emitter would have had, so the weights add up to 1.
        if frame.light.z != 0u && frame.light.y != 0u && EMITTER_MODE != 1u {
            let at = leave(ray, hit, hit.normal);
            let lamp = sample_emitter(at, hit.voxel, emitter_uniform(pixel_seed, sample, depth));
            let toward = bounce_pdf(normal, hit.normal, lamp.dir);
            if lamp.pdf > 0.0 && toward > 0.0 {
                let shadow = leave(ray, hit, lamp.dir);
                // lighting: Emissive Voxels
                let seen = trace(shadow);
                if seen.hit && hit_in_box(shadow, seen, emitter_box(lamp.box)) {
                    let over = select(toward + lamp.pdf, lamp.pdf, EMITTER_MODE == 2u || FAULT == FAULT_EMITTER_DOUBLE_COUNT);
                    // lighting: Emissive Voxels
                    light += throughput * colour * emission(lamp.material) * (toward / over);
                }
            }
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
        bounce_normal = normal;
        bounce_face = hit.normal;
        bounce_voxel = hit.voxel;
        var bounce_dir = cosine_hemisphere(normal, group.xy);
        // A smooth normal tilts the hemisphere, so a direction can point below the face: mirror it
        // about the face, which keeps it cosine-weighted about the face's side.
        let below = dot(bounce_dir, hit.normal);
        if below < 0.0 {
            bounce_dir -= 2.0 * below * hit.normal;
        }
        ray = leave(ray, hit, bounce_dir);
    }
    if first_t >= 0.0 {
        // lighting: Sky and atmosphere
        light = hazed(light, first_t, uv, first.dir);
    }
    return light;
}

@compute @workgroup_size(8, 8)
fn reference(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let first_sample = frame.params.x;
    // A batch: this many samples of every pixel in one dispatch (the still-frame policy, ADR 0005
    // and #189). 0 is a batch of 1, so a caller that never heard of batches is unchanged.
    let batch = max(frame.params.w, 1u);
    let pixel_seed = pixel_hash(id.xy, frame.params.y);
    let at = id.y * size.x + id.x;
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(size);

    // The running mean: after n samples, mean + (new - mean) / (n + 1). The first replaces
    // whatever the buffer held. Unlike a sum, it keeps its precision however long it runs.
    //
    // The batch is a loop over the samples a frame would otherwise have taken one dispatch each:
    // the same operations on the same numbers in the same order, with the mean kept in a register
    // and written once at the end. An f32 stored and loaded again is the same f32, so a batch of k
    // gives the accumulation, bit for bit, that k single samples do (a test holds it). The sample
    // index is what seeds a sample, so how the samples are split into batches changes nothing.
    var mean = vec3<f32>(0.0);
    if first_sample > 0u {
        mean = accum[at].xyz;
    }
    for (var i = 0u; i < batch; i++) {
        let sample_index = first_sample + i;
        // A point in the pixel from the first group's first two dimensions, so edges anti-alias
        // as samples accumulate. Slots 2 and 3 of the same group are the first surface's sun
        // sample.
        let group0 = sample_group(pixel_seed, sample_index, 0u, WHITE_NOISE);
        let estimate =
            radiance(camera_ray(id.xy, size, group0.xy), pixel_seed, sample_index, group0, uv);
        if sample_index == 0u {
            mean = estimate;
        } else {
            mean = mean + (estimate - mean) / f32(sample_index + 1u);
        }
    }
    accum[at] = vec4<f32>(mean, 1.0);
    // The light of the mean, linear: post.wgsl exposes and tones it.
    var shown = mean;
    // A debug view replaces what is shown, not what is accumulated, so switching back to the
    // lit image loses no samples. Its colours are encoded already, and post.wgsl copies them.
    if frame.params.z != 0u {
        let first = camera_ray(id.xy, size, vec2<f32>(0.5));
        // lighting: Geometry (LOD and what is loaded)
        shown = debug_colour(frame.params.z, trace(first), first_sample + batch - 1u);
    }
    textureStore(output, id.xy, vec4<f32>(shown, 1.0));
}
