// The sky and the air as the views see them: the one place that reads the atmosphere's tables
// (#53; they are filled by atmosphere.wgsl and indexed by atmosphere_model.wgsl, which this is
// joined after). Both views read the same functions, so they differ only by what ADR 0005 allows:
//   `sky(dir)`          what the camera sees where its ray hits nothing: the sky-view table, which
//                       is the sky from the camera's own height;
//   `hazed(...)`        what the camera sees of a surface through the air (aerial perspective:
//                       the light it gives, less what the air takes, plus what the air adds);
//   `escaped_sky(dir)`  the **sun-free** table, the sky as a surface sees it, which a ray that
//                       escapes after a bounce reads (the Reference's; Primary has no bounces);
//   `sky_sample(u)`     a direction chosen with a probability proportional to that table's
//                       brightness, and `sky_pdf(dir)` the probability density of a direction,
//                       which the Reference weighs the bounce rays against.
// With the atmosphere off (`frame.flags.w` is 0: the gradient sky and its haze that the renderer
// had before #53, which the tests whose answers come from constants, and the look-dev sheet's
// "today", use) all of them are the gradient's, and `sky_pdf` is 0 everywhere: nothing samples it.
//
// The Reference does not march the air: the camera ray's first hit gets the same `hazed` that
// Primary's does, and a bounce ray gets no haze at all (ADR 0005's bias ledger).

@group(1) @binding(11) var<uniform> atmo: Atmo;
@group(1) @binding(12) var sky_view: texture_2d<f32>;
@group(1) @binding(13) var aerial_scatter: texture_3d<f32>;
@group(1) @binding(14) var aerial_transmittance: texture_3d<f32>;
@group(1) @binding(15) var ground_sky: texture_2d<f32>;
@group(1) @binding(16) var<storage, read> sky_cdf: array<f32>;
@group(1) @binding(17) var sky_sampler: sampler;

// Voxels to the metres that the atmosphere is in. Keep in step with brick_store::VOXELS_PER_METRE
// (a test holds it).
const AIR_VOXELS_PER_METRE = 20.0;

fn atmosphere_on() -> bool {
    return frame.flags.w != 0u;
}

// The sky's gradient from the horizon to the zenith, along the local up (`look_up`, the Planet's
// radial up), not the world's y.
fn gradient_sky(dir: vec3<f32>) -> vec3<f32> {
    let height = clamp(dot(dir, frame.look_up.xyz), 0.0, 1.0);
    return mix(frame.horizon.xyz, frame.zenith.xyz, height);
}

// What the camera sees where its ray hits nothing.
fn sky(dir: vec3<f32>) -> vec3<f32> {
    if !atmosphere_on() {
        return gradient_sky(dir);
    }
    let angles = dir_to_sky_angles(dir);
    let uv = sky_view_uv(atmo.radii.z, angles.x, angles.y);
    let at = vec2<f32>(to_sub_uv(uv.x, SKY_VIEW_SIZE.x), to_sub_uv(uv.y, SKY_VIEW_SIZE.y));
    return textureSampleLevel(sky_view, sky_sampler, at, 0.0).xyz;
}

// What a ray that has bounced and escapes sees: the sun-free table, below the horizon too, where it
// is the horizon's (as the gradient's is).
fn escaped_sky(dir: vec3<f32>) -> vec3<f32> {
    if !atmosphere_on() {
        return gradient_sky(dir);
    }
    let angles = dir_to_sky_angles(dir);
    let uv = vec2<f32>(angles.y / ATMO_PI, max(angles.x, 0.0));
    return textureSampleLevel(ground_sky, sky_sampler, uv, 0.0).xyz;
}

// The light of a hit `t` Voxels from the camera as the camera sees it: the surface's own light
// `light` through the air along the ray `dir` (a unit vector), whose pixel is at `uv` (0 to 1 over
// the image). With the gradient: the horizon's colour fades in as `1 - exp(-haze * t)`. Only for
// the first hit of the camera ray; the sky itself is not hazed.
fn hazed(light: vec3<f32>, t: f32, uv: vec2<f32>, dir: vec3<f32>) -> vec3<f32> {
    if !atmosphere_on() {
        return mix(frame.horizon.xyz, light, exp(-frame.horizon.w * t));
    }
    let seg = air_segment(atmo.radii.z, clamp(dot(dir, atmo.up.xyz), -1.0, 1.0));
    let into_air = t / AIR_VOXELS_PER_METRE - seg.x;
    if seg.y <= seg.x || into_air <= 0.0 {
        return light;
    }
    // The volume holds, for slice k, the air out to depth (k + 1) / AERIAL_SLICES; before the first
    // it fades to nothing, which is a metre or less from the camera.
    let w = aerial_depth(into_air);
    let z = max(w - 0.5 / AERIAL_SLICES, 0.5 / AERIAL_SLICES);
    let at = vec3<f32>(uv, z);
    let added = textureSampleLevel(aerial_scatter, sky_sampler, at, 0.0).xyz;
    let through = textureSampleLevel(aerial_transmittance, sky_sampler, at, 0.0).xyz;
    let fade = min(w * AERIAL_SLICES, 1.0);
    return light * mix(vec3<f32>(1.0), through, fade) + added * fade;
}

// ---- Sampling the sun-free table in proportion to its brightness.
//
// `sky_cdf` (cdf_kernel in atmosphere.wgsl) holds, for the table's 32 rows, the cumulative share
// of the table's weight up to each, then for each row the cumulative share of its 64 texels, then
// the total weight. A texel is a patch of equal solid angle of the sky's half of the sphere (the
// azimuth from the sun, mirrored, by the sine of the elevation), so a direction's probability
// density is its texel's share of the weight over the patch's solid angle: 2 pi / (64 * 32).

const SKY_TOTAL_AT = 2080u;
const SKY_TEXEL_SOLID_ANGLE = 6.283185307 / 2048.0;

struct SkySample {
    dir: vec3<f32>,
    // The density over solid angle; 0 where there is nothing to sample (no atmosphere, or no light).
    pdf: f32,
}

// The first index in [lo, hi] whose cumulative share is above `u`, in the cumulative table that
// starts at `at` (the last share of each is 1, so there is one).
fn first_above(at: u32, lo_in: u32, hi_in: u32, u: f32) -> u32 {
    var lo = lo_in;
    var hi = hi_in;
    while lo < hi {
        let mid = (lo + hi) / 2u;
        if sky_cdf[at + mid] > u {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    return lo;
}

fn texel_pdf(i: u32, j: u32) -> f32 {
    let total = sky_cdf[SKY_TOTAL_AT];
    if total <= 0.0 {
        return 0.0;
    }
    let weight = table_weight(textureLoad(ground_sky, vec2<u32>(i, j), 0).xyz);
    return weight / total / SKY_TEXEL_SOLID_ANGLE;
}

// A direction of the sky's half of the sphere, with probability proportional to its brightness in
// the sun-free table, from two numbers in 0..1. The row is chosen by the first, the texel in it by
// the second, and the rest of each number places the direction in its texel (and the second's,
// which side of the sun's vertical plane).
fn sky_sample(u: vec2<f32>) -> SkySample {
    var out: SkySample;
    out.dir = atmo.up.xyz;
    out.pdf = 0.0;
    if !atmosphere_on() || sky_cdf[SKY_TOTAL_AT] <= 0.0 {
        return out;
    }
    let j = first_above(0u, 0u, GROUND_SKY_H - 1u, u.x);
    let row_lo = select(0.0, sky_cdf[j - 1u], j > 0u);
    let fy = clamp((u.x - row_lo) / max(sky_cdf[j] - row_lo, 1e-12), 0.0, 1.0);
    let base = GROUND_SKY_H + j * GROUND_SKY_W;
    let i = first_above(base, 0u, GROUND_SKY_W - 1u, u.y);
    let col_lo = select(0.0, sky_cdf[base + i - 1u], i > 0u);
    let fx = clamp((u.y - col_lo) / max(sky_cdf[base + i] - col_lo, 1e-12), 0.0, 1.0);
    // The texel's two halves are its two sides of the sun's plane.
    let side = select(1.0, -1.0, fx >= 0.5);
    let fphi = select(2.0 * fx, 2.0 * fx - 1.0, fx >= 0.5);
    let dphi = (f32(i) + fphi) / f32(GROUND_SKY_W) * ATMO_PI;
    let sin_el = (f32(j) + fy) / f32(GROUND_SKY_H);
    let cos_el = sqrt(max(1.0 - sin_el * sin_el, 0.0));
    let up = atmo.up.xyz;
    let towards_sun = sun_horizontal();
    let across = cross(up, towards_sun);
    out.dir = normalize(cos_el * (cos(dphi) * towards_sun + side * sin(dphi) * across) + sin_el * up);
    out.pdf = texel_pdf(i, j);
    return out;
}

// The density over solid angle with which `sky_sample` chooses `dir`; 0 below the horizon.
fn sky_pdf(dir: vec3<f32>) -> f32 {
    if !atmosphere_on() {
        return 0.0;
    }
    let angles = dir_to_sky_angles(dir);
    if angles.x <= 0.0 {
        return 0.0;
    }
    let i = min(u32(angles.y / ATMO_PI * f32(GROUND_SKY_W)), GROUND_SKY_W - 1u);
    let j = min(u32(angles.x * f32(GROUND_SKY_H)), GROUND_SKY_H - 1u);
    return texel_pdf(i, j);
}
