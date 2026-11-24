// The atmosphere stage's kernels (#53): they fill the lookup tables that both views read
// (Hillaire 2020; the model and the tables' indexing are in atmosphere_model.wgsl, which this is
// joined after). In the order they run:
//   transmittance_kernel  the air's transmittance to the top of the atmosphere (once);
//   multiscatter_kernel   the multiple scattering's table Psi (once);
//   sky_view_kernel       the sky from the camera, by direction;
//   ground_sky_kernel     the **sun-free** copy: the sky from a surface, by direction over the
//                         upper hemisphere, which the Reference's escaped rays read;
//   cdf_kernel            the sampling distribution of that table, proportional to its brightness;
//   aerial_kernel         the light the air adds along each ray of the camera's view, and what it
//                         lets through, by distance (aerial perspective).
// Nothing here knows about the Planet's bricks: the air is a function of the altitude alone.
//
// "Sun-free" is the table's contract: it holds the light of the sky as scattered by the air, never
// the sun's own disc (which neither view draws as a sky: the sun is the Sun and shadows row's, and
// the Reference finds it by its own shadow ray), so that a path that escapes does not count the
// sun twice.
//
// The light is in the units of the sun's radiance on a surface (look.rs): the in-scattered
// radiance is E * transmittance * scattering * phase with E = pi * the sun's light above the air.

@group(0) @binding(0) var<uniform> atmo: Atmo;
struct CameraBasis {
    cell: vec4<i32>,
    origin: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    params: vec4<f32>,
}
@group(0) @binding(1) var<uniform> camera: CameraBasis;
@group(0) @binding(2) var table_sampler: sampler;
@group(0) @binding(3) var transmittance: texture_2d<f32>;
@group(0) @binding(4) var multiscatter: texture_2d<f32>;
@group(0) @binding(5) var transmittance_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(6) var multiscatter_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var sky_view_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(8) var ground_sky_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(9) var aerial_scatter_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(10) var aerial_transmittance_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(11) var ground_sky_in: texture_2d<f32>;
@group(0) @binding(12) var<storage, read_write> sky_cdf_out: array<f32>;

const TRANSMITTANCE_STEPS = 64u;
const SKY_STEPS = 48u;
const MULTISCATTER_STEPS = 20u;
const AERIAL_STEPS_PER_SLICE = 4u;
// How much finer the steps are near the end of a ray that the air is thickest at than at the other
// end: the k-th step ends at (exp(STEP_CURVE * k / steps) - 1) / (exp(STEP_CURVE) - 1) of the way.
// The haze is a layer some tens of metres thick, which equal steps along a ray of 100 km miss.
const STEP_CURVE = 10.0;

// The k-th of `steps` pieces of the air t0..t1, near to far: [a, b]. The pieces grow by a constant
// factor from the end that is at the lower altitude (`reverse` when that is the far end).
fn step_range(k: u32, steps: u32, t0: f32, t1: f32, reverse: bool) -> vec2<f32> {
    let len = t1 - t0;
    let denom = exp(STEP_CURVE) - 1.0;
    let n = f32(steps);
    let kk = f32(k);
    if reverse {
        let a = t1 - len * (exp(STEP_CURVE * (n - kk) / n) - 1.0) / denom;
        let b = t1 - len * (exp(STEP_CURVE * (n - kk - 1.0) / n) - 1.0) / denom;
        return vec2<f32>(a, b);
    }
    let a = t0 + len * (exp(STEP_CURVE * kk / n) - 1.0) / denom;
    let b = t0 + len * (exp(STEP_CURVE * (kk + 1.0) / n) - 1.0) / denom;
    return vec2<f32>(a, b);
}

// The integral of a source `s` that is attenuated by `ext` over a step of length dt, per unit of
// `s`: (1 - T) / ext, which is dt where the air is too thin for that to be computed.
fn step_gain(ext: vec3<f32>, dt: f32, step_t: vec3<f32>) -> vec3<f32> {
    let thin = ext * dt < vec3<f32>(1e-4);
    return select((vec3<f32>(1.0) - step_t) / max(ext, vec3<f32>(1e-12)), dt * (vec3<f32>(1.0) - 0.5 * ext * dt), thin);
}

// The sun's light that reaches radius r, whose sun is at zenith cosine mu_s: zero where the Planet
// is in the way.
fn sun_transmittance(r_in: f32, mu_s: f32) -> vec3<f32> {
    let r = max(r_in, atmo.radii.x);
    if sphere_roots(r, mu_s, atmo.radii.x).x > 0.0 {
        return vec3<f32>(0.0);
    }
    let uv = transmittance_uv(r, mu_s);
    let at = vec2<f32>(to_sub_uv(uv.x, TRANSMITTANCE_SIZE.x), to_sub_uv(uv.y, TRANSMITTANCE_SIZE.y));
    return textureSampleLevel(transmittance, table_sampler, at, 0.0).xyz;
}

// Psi(h, mu_s): the multiple scattering's light at altitude h per unit of sun, to be multiplied by
// the air's scattering coefficient there.
fn multiscatter_light(h: f32, mu_s: f32) -> vec3<f32> {
    let uv = multiscatter_uv(h, mu_s);
    let at = vec2<f32>(to_sub_uv(uv.x, MULTISCATTER_SIZE), to_sub_uv(uv.y, MULTISCATTER_SIZE));
    return textureSampleLevel(multiscatter, table_sampler, at, 0.0).xyz;
}

// What has been added along a ray so far and what the air has let through.
struct Integral {
    l: vec3<f32>,
    t: vec3<f32>,
}

// One piece [a, b] of a ray (r, mu) whose sun is at zenith cosine mu_s and angle nu from the ray,
// added to `state`: the sun's single scattering, whose light the air has taken some of on the way
// in, and the multiple scattering's.
fn accumulate(state: Integral, r: f32, mu: f32, mu_s: f32, nu: f32, a: f32, b: f32) -> Integral {
    let dt = b - a;
    let tm = 0.5 * (a + b);
    let rt = max(radius_at(r, mu, tm), atmo.radii.x);
    let h = altitude(rt);
    let m = medium_at(h);
    let step_t = exp(-m.extinction * dt);
    let mu_s_t = clamp((r * mu_s + tm * nu) / rt, -1.0, 1.0);
    let sun = sun_transmittance(rt, mu_s_t);
    let phase = m.rayleigh * rayleigh_phase(nu) + m.mie * mie_phase(nu, atmo.mie_absorption.w);
    let e = ATMO_PI * atmo.sun_light.xyz;
    let source = e * (sun * phase + multiscatter_light(h, mu_s_t) * m.scattering);
    var out: Integral;
    out.l = state.l + state.t * source * step_gain(m.extinction, dt, step_t);
    out.t = state.t * step_t;
    return out;
}

// The light along the ray (r, mu) through all of its air.
fn integrate_ray(r: f32, mu: f32, mu_s: f32, nu: f32, steps: u32) -> vec3<f32> {
    let seg = air_segment(r, mu);
    var state: Integral;
    state.l = vec3<f32>(0.0);
    state.t = vec3<f32>(1.0);
    if seg.y <= seg.x {
        return state.l;
    }
    let reverse = radius_at(r, mu, seg.y) < radius_at(r, mu, seg.x);
    for (var k = 0u; k < steps; k++) {
        let range = step_range(k, steps, seg.x, seg.y, reverse);
        state = accumulate(state, r, mu, mu_s, nu, range.x, range.y);
    }
    return state.l;
}

@compute @workgroup_size(8, 8)
fn transmittance_kernel(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= u32(TRANSMITTANCE_SIZE.x) || id.y >= u32(TRANSMITTANCE_SIZE.y) {
        return;
    }
    let unit = vec2<f32>(f32(id.x) / (TRANSMITTANCE_SIZE.x - 1.0), f32(id.y) / (TRANSMITTANCE_SIZE.y - 1.0));
    let p = transmittance_params(unit);
    let r = p.x;
    let mu = p.y;
    let t1 = max(sphere_roots(r, mu, atmo.radii.y).y, 0.0);
    var od = vec3<f32>(0.0);
    for (var k = 0u; k < TRANSMITTANCE_STEPS; k++) {
        let range = step_range(k, TRANSMITTANCE_STEPS, 0.0, t1, false);
        let tm = 0.5 * (range.x + range.y);
        od += medium_at(altitude(radius_at(r, mu, tm))).extinction * (range.y - range.x);
    }
    textureStore(transmittance_out, id.xy, vec4<f32>(exp(-od), 1.0));
}

// The multiple scattering (Hillaire 2020, section 5.1): the light of the air's second order of
// scattering at a point, taken as isotropic, and the geometric series of the orders after it. At
// each (sun cosine, altitude) the whole sphere of directions is marched (64 of them, evenly in
// solid angle); the ground reflects the sun with its albedo.
@compute @workgroup_size(8, 8)
fn multiscatter_kernel(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = u32(MULTISCATTER_SIZE);
    if id.x >= size || id.y >= size {
        return;
    }
    let unit = vec2<f32>(f32(id.x), f32(id.y)) / (MULTISCATTER_SIZE - 1.0);
    let mu_s = unit.x * 2.0 - 1.0;
    let h0 = ms_height_span() * unit.y * unit.y * unit.y;
    let r = atmo.radii.x + h0;
    let sun = vec3<f32>(sqrt(max(1.0 - mu_s * mu_s, 0.0)), mu_s, 0.0);
    var l_sum = vec3<f32>(0.0);
    var f_sum = vec3<f32>(0.0);
    for (var a = 0u; a < 8u; a++) {
        for (var b = 0u; b < 8u; b++) {
            let theta = 2.0 * ATMO_PI * (f32(a) + 0.5) / 8.0;
            let cos_phi = 1.0 - 2.0 * (f32(b) + 0.5) / 8.0;
            let sin_phi = sqrt(max(1.0 - cos_phi * cos_phi, 0.0));
            let dir = vec3<f32>(cos(theta) * sin_phi, cos_phi, sin(theta) * sin_phi);
            let mu = dir.y;
            let nu = dot(dir, sun);
            let ground = sphere_roots(r, mu, atmo.radii.x);
            var t1 = max(sphere_roots(r, mu, atmo.radii.y).y, 0.0);
            let hits = ground.x > 0.0;
            if hits {
                t1 = ground.x;
            }
            let reverse = radius_at(r, mu, t1) < r;
            var l_ray = vec3<f32>(0.0);
            var f_ray = vec3<f32>(0.0);
            var throughput = vec3<f32>(1.0);
            for (var k = 0u; k < MULTISCATTER_STEPS; k++) {
                let range = step_range(k, MULTISCATTER_STEPS, 0.0, t1, reverse);
                let dt = range.y - range.x;
                let tm = 0.5 * (range.x + range.y);
                let rt = max(radius_at(r, mu, tm), atmo.radii.x);
                let m = medium_at(altitude(rt));
                let step_t = exp(-m.extinction * dt);
                let mu_s_t = clamp((r * mu_s + tm * nu) / rt, -1.0, 1.0);
                let gain = step_gain(m.extinction, dt, step_t);
                // The sun's light scattered once, isotropically, and the same with a unit source,
                // which is what the series of the orders after it is made of.
                l_ray += throughput * sun_transmittance(rt, mu_s_t) * m.scattering * gain / (4.0 * ATMO_PI);
                f_ray += throughput * m.scattering * gain;
                throughput *= step_t;
            }
            if hits {
                let mu_s_g = clamp((r * mu_s + t1 * nu) / atmo.radii.x, -1.0, 1.0);
                l_ray += throughput * atmo.ground.xyz / ATMO_PI * sun_transmittance(atmo.radii.x, mu_s_g) * max(mu_s_g, 0.0);
            }
            l_sum += l_ray;
            f_sum += f_ray;
        }
    }
    let l_mean = l_sum / 64.0;
    let f_mean = min(f_sum / 64.0, vec3<f32>(0.99));
    textureStore(multiscatter_out, id.xy, vec4<f32>(l_mean / (vec3<f32>(1.0) - f_mean), 1.0));
}

// The tinted sky at radius r in the direction `angle` from the zenith, at azimuth `dphi` from the
// sun: held short of the horizon for a camera near the ground; and below the horizon the planet's
// air, or for a camera near the ground (where nothing below the horizon is drawn) the horizon's
// sky. The sky-view table and what the extra haze fades to are both this, so the far ground fades
// into what `sky()` shows in the directions where nothing is drawn.
fn tinted_sky(r: f32, angle: f32, mu_s: f32, dphi: f32) -> vec3<f32> {
    let soft_mu = cos(soft_angle(r, angle));
    var sky = tint_sky(integrate_ray(r, soft_mu, mu_s, cos_between(soft_mu, mu_s, dphi), SKY_STEPS), soft_mu) + night_floor(r, soft_mu);
    if angle > horizon_angles(r).y {
        let mu = cos(angle);
        let below = tint_sky(integrate_ray(r, mu, mu_s, cos_between(mu, mu_s, dphi), SKY_STEPS), mu) + night_floor(r, mu);
        sky = mix(below, sky, horizon_fill());
    }
    return sky;
}

@compute @workgroup_size(8, 8)
fn sky_view_kernel(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= u32(SKY_VIEW_SIZE.x) || id.y >= u32(SKY_VIEW_SIZE.y) {
        return;
    }
    let r = atmo.radii.z;
    let unit = vec2<f32>(f32(id.x) / (SKY_VIEW_SIZE.x - 1.0), f32(id.y) / (SKY_VIEW_SIZE.y - 1.0));
    let angles = sky_view_params(r, unit);
    let mu_s = clamp(dot(atmo.sun_dir.xyz, atmo.up.xyz), -1.0, 1.0);
    let angle = acos(clamp(angles.x, -1.0, 1.0));
    textureStore(sky_view_out, id.xy, vec4<f32>(tinted_sky(r, angle, mu_s, angles.y), 1.0));
}

// The sun-free table: the sky as a surface sees it, over the sky's half of the sphere, in texels
// of equal solid angle (the azimuth from the sun by the sine of the elevation), so that a texel's
// share of the table's brightness is its share of the sky's light.
@compute @workgroup_size(8, 8)
fn ground_sky_kernel(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= GROUND_SKY_W || id.y >= GROUND_SKY_H {
        return;
    }
    let r = atmo.radii.x + atmo.up.w;
    let dphi = (f32(id.x) + 0.5) / f32(GROUND_SKY_W) * ATMO_PI;
    let mu = (f32(id.y) + 0.5) / f32(GROUND_SKY_H);
    let mu_s = clamp(dot(atmo.sun_dir.xyz, atmo.up.xyz), -1.0, 1.0);
    let nu = cos_between(mu, mu_s, dphi);
    textureStore(ground_sky_out, id.xy, vec4<f32>(tint_sky(integrate_ray(r, mu, mu_s, nu, SKY_STEPS), mu) + night_floor(r, mu), 1.0));
}

var<workgroup> row_sums: array<f32, 32>;

// The sampling distribution of the sun-free table, as two cumulative tables in `sky_cdf_out`: [0, 32)
// the share of the rows up to and including each, then for each row its 64 texels' share of it.
// The last of each is exactly 1. Word 2080 is the table's total weight.
@compute @workgroup_size(32)
fn cdf_kernel(@builtin(local_invocation_id) lid: vec3<u32>) {
    let j = lid.x;
    var sum = 0.0;
    for (var i = 0u; i < GROUND_SKY_W; i++) {
        sum += table_weight(textureLoad(ground_sky_in, vec2<u32>(i, j), 0).xyz);
    }
    row_sums[j] = sum;
    var run = 0.0;
    for (var i = 0u; i < GROUND_SKY_W; i++) {
        run += table_weight(textureLoad(ground_sky_in, vec2<u32>(i, j), 0).xyz);
        sky_cdf_out[GROUND_SKY_H + j * GROUND_SKY_W + i] = run / sum;
    }
    workgroupBarrier();
    if j == 0u {
        var total = 0.0;
        for (var row = 0u; row < GROUND_SKY_H; row++) {
            total += row_sums[row];
        }
        var cumulative = 0.0;
        for (var row = 0u; row < GROUND_SKY_H; row++) {
            cumulative += row_sums[row];
            sky_cdf_out[row] = cumulative / total;
        }
        sky_cdf_out[GROUND_SKY_H + GROUND_SKY_H * GROUND_SKY_W] = total;
    }
}

// The aerial perspective volume (Hillaire 2020, section 5.3), one thread for each ray of the
// camera's view, marching slice after slice of the ray's depth: the light the air adds along the
// ray to that depth, and the share of the light behind it that gets through, for the lookup to
// apply as `behind * transmittance + added`. A ray that never meets the air has nothing added and
// lets everything through.
@compute @workgroup_size(8, 8)
fn aerial_kernel(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = u32(AERIAL_SLICES);
    if id.x >= size || id.y >= size {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / AERIAL_SLICES;
    let ndc = uv * 2.0 - 1.0;
    let tan_half = camera.params.x;
    let dir = normalize(camera.forward.xyz + camera.right.xyz * (ndc.x * atmo.radii.w * tan_half) - camera.up.xyz * (ndc.y * tan_half));
    let r = atmo.radii.z;
    let mu = clamp(dot(dir, atmo.up.xyz), -1.0, 1.0);
    let mu_s = clamp(dot(atmo.sun_dir.xyz, atmo.up.xyz), -1.0, 1.0);
    let nu = clamp(dot(dir, atmo.sun_dir.xyz), -1.0, 1.0);
    let seg = air_segment(r, mu);
    let length_in_air = max(seg.y - seg.x, 0.0);
    // What the extra haze fades to: the sky in the ray's direction, as the sky-view table has it,
    // so that the far ground dissolves into the sky behind it and no line is left between them.
    let sky_angles = dir_to_sky_angles(dir);
    let fade = tinted_sky(r, acos(sky_angles.x), mu_s, sky_angles.y);
    var state: Integral;
    state.l = vec3<f32>(0.0);
    state.t = vec3<f32>(1.0);
    var prev = 0.0;
    for (var k = 0u; k < size; k++) {
        let next = aerial_distance(f32(k + 1u) / AERIAL_SLICES);
        let a = min(prev, length_in_air);
        let b = min(next, length_in_air);
        if b > a {
            for (var s = 0u; s < AERIAL_STEPS_PER_SLICE; s++) {
                let lo = a + (b - a) * f32(s) / f32(AERIAL_STEPS_PER_SLICE);
                let hi = a + (b - a) * f32(s + 1u) / f32(AERIAL_STEPS_PER_SLICE);
                state = accumulate(state, r, mu, mu_s, nu, seg.x + lo, seg.x + hi);
            }
        }
        // The look's tint: the air's light by the ray's elevation, and then the extra haze over the
        // distance travelled in air, which fades what is behind it and what the air added to its colour.
        let extra = exp(-atmo.tint_horizon.w * min(next, length_in_air));
        let added = tint_sky(state.l, mu) * extra + fade * (1.0 - extra);
        textureStore(aerial_scatter_out, vec3<u32>(id.x, id.y, k), vec4<f32>(added, 0.0));
        textureStore(aerial_transmittance_out, vec3<u32>(id.x, id.y, k), vec4<f32>(state.t * extra, 1.0));
        prev = next;
    }
}
