// Primary's global illumination (#54, ADR 0005, "Bounce light" and "Emissive Voxels"): the light
// that reaches each pixel's surface from everything but the sun's own shadow ray (`shadows`' job):
// one bounce found by a ray through `trace()`, the emissive Voxels' own light, and from the bounce's
// landing on, the radiance cache (cache.wgsl) in place of every bounce after it. `gi_trace` writes
// it as the incident light on the pixel's surface, a multiple of the surface's colour, which
// `shade_pass` multiplies by the colour as it does the sun's; cache.wgsl's `gi_resolve` runs after
// it in the same pass.
//
// For each pixel with a surface:
//   * one emitter sample (the Reference's `sample_emitter`, from the cell's list of emitters), which
//     counts where the first thing along it is that emitter;
//   * one bounce ray, cosine-weighted about the shading normal as the Reference's first bounce is.
//     Where it lands: on nothing, the sky as a surface sees it (`escaped_sky`); on an emitter,
//     its light, unless the list the emitter sample came from holds that emitter (then the sample
//     counted it: the two share the light, as the Reference's weights do, by a partition here
//     rather than a weighing, which has the same mean); on a surface, the light the cache has
//     there times its colour. Where the cache has nothing there yet, or has a cell that no one has
//     fed for a while, the pixel works out the landing's light itself (`gi_fill`: the sun, an
//     emitter sample and one more bounce, read from the cache) and adds it to the cache. The
//     sample that the pixel took of its own surface is added to the cache too, for one pixel in
//     sixteen (the cell's light is then the mean of a surface's pixels), so that the cache is
//     fed from what the camera sees, and the contention of the adds stays bounded.
// Nothing is kept from a frame but the cache; the noise of a pixel's sample is the upscaler's to
// average, as it is with the sun's two rays.

@group(1) @binding(20) var gi_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(21) var gi_light: texture_2d<f32>;

// The scramble of Primary's GI samples: a number of its own, so that they do not follow the sun's
// or the Reference's.
const GI_SEED = 0x61490054u;
// The groups of the Owen-scrambled Sobol sequence (sampling.wgsl) the samples are taken from, a
// group for each: the pixel's bounce, the pixel's emitter, the landing's bounce and sun, the
// landing's emitter. The sample index is the frame's number, so the frames stratify one another.
const GI_GROUP_BOUNCE = 2u;
const GI_GROUP_EMITTER = 3u;
const GI_GROUP_FILL = 4u;
const GI_GROUP_FILL_EMITTER = 5u;
// One pixel in this many adds its own sample to the cache, and one reader in this many of a cell
// that has not been fed for REFRESH_AGE frames adds a fresh sample of it (more of them for the
// frames after the scene changed: `frame.gi.y`).
const GI_FEED_ONE_IN = 16u;
const GI_REFRESH_ONE_IN = 32u;
const GI_REFRESH_ONE_IN_AFTER_EDIT = 8u;

// The ray that a bounce from `hit` leaves along: cosine-weighted about `normal`, and mirrored about
// the geometric face when the smooth normal tilts it below, as the Reference's is.
fn gi_bounce(ray: Ray, hit: Hit, normal: vec3<f32>, u: vec2<f32>) -> Ray {
    var dir = cosine_hemisphere(normal, u);
    let below = dot(dir, hit.normal);
    if below < 0.0 {
        dir -= 2.0 * below * hit.normal;
    }
    return leave(ray, hit, dir);
}

// The light of the emitter that one sample of the cell's list found from `hit`, as incident light:
// its emission times the bounce's density of the direction over the density it was chosen with, if
// the first thing the ray finds is that emitter.
fn gi_emitters(ray: Ray, hit: Hit, normal: vec3<f32>, u: vec4<f32>) -> vec3<f32> {
    if frame.light.z == 0u {
        return vec3<f32>(0.0);
    }
    let lamp = sample_emitter(leave(ray, hit, hit.normal), hit.voxel, u);
    let toward = bounce_pdf(normal, hit.normal, lamp.dir);
    if lamp.pdf <= 0.0 || toward <= 0.0 {
        return vec3<f32>(0.0);
    }
    let shadow = leave(ray, hit, lamp.dir);
    // lighting: Emissive Voxels
    let seen = trace(shadow);
    if seen.hit && hit_in_box(shadow, seen, emitter_box(lamp.box)) {
        // lighting: Emissive Voxels
        return emission(lamp.material) * (toward / lamp.pdf);
    }
    return vec3<f32>(0.0);
}

// The sun's light at `hit` as one shadow ray towards a point of its disc: as `shadows` weighs it.
fn gi_sun(ray: Ray, hit: Hit, u: vec2<f32>) -> vec3<f32> {
    // lighting: Sun and shadows
    let diffuse = max(dot(hit.normal, normalize(frame.sun.xyz)), 0.0);
    if diffuse <= 0.0 {
        return vec3<f32>(0.0);
    }
    // lighting: Sun and shadows
    let to_sun = sample_cone(frame.sun.xyz, frame.sun.w, u);
    // lighting: Sun and shadows
    if dot(hit.normal, to_sun) > 0.0 && !trace(leave(ray, hit, to_sun)).hit {
        // lighting: Sun and shadows
        return frame.sun_radiance.xyz * diffuse;
    }
    return vec3<f32>(0.0);
}

// The emitted light of `landed`, which a bounce ray (`ray`) from the surface of `voxel` found: all
// of it, unless the emitter is one that the sample of `voxel`'s cell's list could choose, which
// counted it already.
fn gi_emitted(ray: Ray, landed: Hit, voxel: vec3<i32>) -> vec3<f32> {
    // lighting: Emissive Voxels
    let light = emission(landed.material) * f32(frame.light.z);
    if any(light > vec3<f32>(0.0)) && emitter_hit_pdf(ray, landed, voxel) > 0.0 {
        return vec3<f32>(0.0);
    }
    return light;
}

// How far `landed`, the hit of `ray`, is from the camera, in voxels: what its cell's size is
// chosen by.
fn gi_distance(ray: Ray, landed: Hit) -> f32 {
    return length(vec3<f32>(ray.cell - camera.cell.xyz) + landed.position - camera.origin.xyz);
}

// One sample of the light incident on the surface `hit` that a bounce ray landed on: the sun, an
// emitter, and one more bounce that reads the cache where it lands (what the cache does not have
// yet it counts as dark). This is what the cache's cells are made of.
fn gi_fill(ray: Ray, hit: Hit, pixel_seed: u32) -> vec3<f32> {
    let n = frame.gi.x;
    let g = sample_group(pixel_seed, n, GI_GROUP_FILL, false);
    var light = gi_sun(ray, hit, g.zw);
    light += gi_emitters(ray, hit, hit.normal, sample_group(pixel_seed, n, GI_GROUP_FILL_EMITTER, false));
    let next = gi_bounce(ray, hit, hit.normal, g.xy);
    // lighting: Bounce light
    let landed = trace(next);
    if !landed.hit {
        // lighting: Sky and atmosphere
        return light + escaped_sky(next.dir);
    }
    light += gi_emitted(next, landed, hit.voxel);
    // lighting: Bounce light
    let cell = cache_incident(landed.voxel, landed.normal, gi_distance(next, landed));
    if cell.known {
        // lighting: Per-Voxel colour variation
        light += surface_albedo(landed, false) * cell.light;
    }
    return light;
}

// The light incident on the pixel's surface from the emitters, one bounce and the cache: one sample.
fn gi_gather(ray: Ray, hit: Hit, normal: vec3<f32>, pixel_seed: u32) -> vec3<f32> {
    let n = frame.gi.x;
    var light = gi_emitters(ray, hit, normal, sample_group(pixel_seed, n, GI_GROUP_EMITTER, false));
    let next = gi_bounce(ray, hit, normal, sample_group(pixel_seed, n, GI_GROUP_BOUNCE, false).xy);
    // lighting: Bounce light
    let landed = trace(next);
    if !landed.hit {
        // lighting: Sky and atmosphere
        return light + escaped_sky(next.dir);
    }
    light += gi_emitted(next, landed, hit.voxel);
    // lighting: Bounce light
    let cell = cache_incident(landed.voxel, landed.normal, gi_distance(next, landed));
    var incident = cell.light;
    // A cell that is not there yet is worked out here and added; one that is there and has not been
    // fed for a while is added to now and then, by a reader in a few dozen, so that it follows the
    // scene even when nothing the camera sees feeds it.
    let every = select(GI_REFRESH_ONE_IN, GI_REFRESH_ONE_IN_AFTER_EDIT, frame.gi.y != 0u);
    let chosen = pixel_hash(vec2<u32>(pixel_seed, n), GI_SEED) % every == 0u;
    if !cell.known || (cell.age > REFRESH_AGE && chosen) {
        let fresh = gi_fill(next, landed, pixel_seed);
        cache_add(cell, fresh);
        if !cell.known {
            incident = fresh;
        }
    }
    // lighting: Per-Voxel colour variation
    return light + surface_albedo(landed, false) * incident;
}

@compute @workgroup_size(8, 8)
fn gi_trace(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(gbuffer);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    var gi = vec3<f32>(0.0);
    var hit = load_hit(id.xy);
    // Only with the cache's GI: the sun-and-sky-only view has none, and the bench's flat-ambient
    // baseline has its constant in `shade` instead.
    if hit.hit && frame.light.y == 1u {
        let ray = camera_ray(id.xy, size, frame.jitter.xy);
        hit.position = ray.origin + ray.dir * hit.t;
        var normal = hit.normal;
        let smoothed = textureLoad(shading_normals, id.xy, 0);
        if frame.flags.x != 0u && smoothed.w > 0.5 {
            normal = smoothed.xyz;
        }
        let pixel_seed = pixel_hash(id.xy, GI_SEED);
        gi = gi_gather(ray, hit, normal, pixel_seed);
        if pixel_hash(id.xy, GI_SEED ^ frame.gi.x) % GI_FEED_ONE_IN == 0u {
            // The surface's own sample, with the sun's light there as `shadows` found it, is what
            // the cell holds.
            // lighting: Sun and shadows
            let incident = gi + frame.sun_radiance.xyz * textureLoad(sun_weights, id.xy, 0).x;
            // lighting: Bounce light
            let own = cache_incident(hit.voxel, hit.normal, hit.t);
            cache_add(own, incident);
        }
    }
    textureStore(gi_out, id.xy, vec4<f32>(gi, 0.0));
}
