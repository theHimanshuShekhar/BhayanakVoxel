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

// ---- The night's marks on the sky (#203): the moon's disc and halo, and the stars. They are what the
// camera sees where its ray escapes, in both views, from the same functions of the direction: a
// pure function of the direction and of the frame's time of day, with no random number in it, so
// that the Reference and Primary draw the same ones. They are not in the tables: the tables are the
// sky as a surface sees it, and the moon's light is the key light's (the moon disc is not lit twice).

// The stars: cells of the sphere of directions scaled up by STAR_CELLS, each with one star (or
// none) placed in it, so a star is a soft round dot a fraction of a degree wide and none is the
// same as the next. The dot is a Gaussian a tenth of a cell across, held 0.3 of a cell from the
// cell's faces, so only the cell the pixel is in can have one.
const STAR_CELLS = 34.0;
const STAR_DENSITY = 0.55;
const STAR_SIGMA = 0.12;

fn star_hash(cell: vec3<i32>, salt: u32) -> u32 {
    var h = hash_voxel(cell) ^ (salt * 0x9e3779b9u);
    h ^= h >> 16u;
    h *= 0x7feb352du;
    h ^= h >> 15u;
    h *= 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn unit_of(h: u32) -> f32 {
    return f32(h >> 8u) / 16777216.0;
}

// The light of the stars in the direction `dir` (before they are hidden by the air and the sky's
// own light): a pale lilac-white or a pale warm one, each its own brightness, mostly dim.
fn star_light(dir: vec3<f32>) -> vec3<f32> {
    let p = vec3<f32>(
        dot(frame.star_x.xyz, dir),
        dot(frame.star_y.xyz, dir),
        dot(frame.star_z.xyz, dir),
    );
    let q = p * STAR_CELLS;
    let cell = vec3<i32>(floor(q));
    if unit_of(star_hash(cell, 0u)) > STAR_DENSITY {
        return vec3<f32>(0.0);
    }
    let centre = vec3<f32>(cell) + 0.3 + 0.4 * vec3<f32>(
        unit_of(star_hash(cell, 1u)),
        unit_of(star_hash(cell, 2u)),
        unit_of(star_hash(cell, 3u)),
    );
    let d = q - centre;
    let dot_light = exp(-dot(d, d) / (2.0 * STAR_SIGMA * STAR_SIGMA));
    let rank = unit_of(star_hash(cell, 4u));
    // Most are dim, a few bright: the fourth power of a uniform number.
    let brightness = 0.012 + 0.10 * rank * rank * rank * rank;
    let warm = unit_of(star_hash(cell, 5u));
    let colour = mix(vec3<f32>(0.85, 0.80, 1.0), vec3<f32>(1.0, 0.88, 0.78), warm);
    return colour * (brightness * dot_light);
}

// The moon's disc and halo in the direction `dir`: a soft-edged disc a little darker at its limb,
// and a wide faint glow round it that falls off with the angle.
fn moon_light(dir: vec3<f32>) -> vec3<f32> {
    if frame.moon_disc.w == 0.0 || frame.night.y <= 0.0 {
        return vec3<f32>(0.0);
    }
    let c = clamp(dot(dir, frame.moon.xyz), -1.0, 1.0);
    let radius = acos(clamp(frame.moon.w, -1.0, 1.0));
    let angle = acos(c);
    // The disc: full inside 0.85 of its radius, gone at 1.1 of it.
    let cover = 1.0 - smoothstep(0.85 * radius, 1.1 * radius, angle);
    let rho = clamp(angle / max(radius, 1e-4), 0.0, 1.0);
    let limb = 0.78 + 0.22 * sqrt(1.0 - rho * rho);
    let glow = 1.0 / (1.0 + (angle / (2.2 * radius)) * (angle / (2.2 * radius)) * 6.0);
    let halo = 0.16 * glow * glow;
    return frame.moon_disc.xyz * ((cover * limb + halo) * frame.night.y);
}

// How much of the night's marks the air in front of them lets through: for a camera near the ground
// they fade out toward the horizon, where the ray crosses the thickest air; from orbit there is no
// horizon of the sort, and the marks are seen against the dark.
fn night_veil(dir: vec3<f32>) -> f32 {
    let mu = dot(dir, atmo.up.xyz);
    return mix(1.0, smoothstep(0.0, 0.22, mu), horizon_fill());
}

fn night_marks(dir: vec3<f32>, sky_light: vec3<f32>) -> vec3<f32> {
    if frame.moon_disc.w == 0.0 && frame.night.x <= 0.0 {
        return vec3<f32>(0.0);
    }
    var marks = moon_light(dir);
    if frame.night.x > 0.0 {
        // A star is lost in the sky's own light: gone where that is as bright as the twilight glow.
        let own = dot(sky_light, vec3<f32>(0.2126, 0.7152, 0.0722));
        marks += star_light(dir) * (frame.night.x * exp(-own / 0.045));
    }
    return marks * night_veil(dir);
}

// What the camera sees where its ray hits nothing.
fn sky(dir: vec3<f32>) -> vec3<f32> {
    if !atmosphere_on() {
        return gradient_sky(dir);
    }
    let angles = dir_to_sky_angles(dir);
    let uv = sky_view_uv(atmo.radii.z, angles.x, angles.y);
    let at = vec2<f32>(to_sub_uv(uv.x, SKY_VIEW_SIZE.x), to_sub_uv(uv.y, SKY_VIEW_SIZE.y));
    let table = textureSampleLevel(sky_view, sky_sampler, at, 0.0).xyz;
    // lighting: Moon and stars
    return table + night_marks(dir, table);
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
