// Primary's denoiser (#55): temporal accumulation with clamping, grown to A-SVGF (adaptive temporal,
// variance-guided à-trous), on the two noisy signals of Primary's light and on nothing else.
//
// **Where it sits.** Between `gi_trace`/`shadows` and `shade_pass`, at the traced resolution and
// before the upscaler, on the signals before the surface's colour multiplies them: the sun's weight
// (a multiple of the sun's radiance) and the incident light of the bounce, the sky and the emitters
// (`shade` does `colour * (gi + the sun's radiance * sun)`). So the surface's own detail (the Voxels'
// colours) is never blurred, only the light on it. The upscaler then averages the denoised lit image
// over its jittered frames, as it always did: nothing of it is duplicated here (no jitter, no
// resampling to the output size, no history of colours); this history is of light, kept per traced
// pixel and carried to where the camera has moved by the geometry of the hit.
//
// Two kernels, both over the traced image, in one timed pass called `denoise` with the kernel that
// makes the frame's samples (`gi_samples`, gi.wgsl):
//   `denoise_temporal`  reprojects the history by the hit's geometry and checks it against the
//                       pixel's (distance to its plane, normal), clamps it to what the pixel's
//                       neighbours show, shortens it where it disagrees with them or where an Edit
//                       is changing the light, blends the frame in and keeps the first two moments
//                       of the luminance for a variance. Its result is the history.
//   `denoise_atrous`    an iteration (the step is a pipeline constant: 1, 2, 4, 8) of an edge-stopped
//                       5 x 5 blur: the distance to the centre's tangent plane, the normal, and the
//                       luminance in units of the centre's own standard deviation, so a pixel with a
//                       long history (a small variance) is hardly blurred and a new one (a big one)
//                       is. The history is the temporal result, not the filtered one: it is the mean
//                       of the samples and its moments are of the same samples, which the filter's
//                       output (a blur of the neighbours) would not be.
//
// What is not here: Schied's A-SVGF measures a temporal gradient by shading again, in the new
// frame, the samples that the last frame took. That needs the same random numbers a second time,
// which Primary's sampling does not give (the frame's number is in them). The adaptive part is the
// pixel's neighbourhood instead (`TEMPORAL_REJECT_FROM`), with the signals the renderer has on top of
// it: an Edit's box and a new sun.
//
// This file reads no source of light: it filters the two signals it is handed, and the reads of
// those (in gi.wgsl) carry their `// lighting:` comments.

struct Camera {
    cell: vec4<i32>,
    origin: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    // Tangent of half the vertical field of view, then three unused.
    params: vec4<f32>,
}

struct Denoise {
    camera: Camera,
    // The camera of the frame before, whose pixels the history is of.
    previous: Camera,
    // How far `camera` is from `previous`, in Voxels (the CPU takes the difference in f64), then
    // one unused.
    shift: vec4<f32>,
    // 1 if there is a history (0 on the first frame, after a new sun or a resize), 1 if the smooth
    // normals pass ran, the most samples a history has (an f32's bits), and the most it has in the
    // cells that an Edit or a new sun is changing (the same).
    params: vec4<u32>,
    // The Voxels that an Edit or a new sun is changing the light of: the first and one past the
    // last, and in w of the first whether the box is on at all (0 or 1).
    boost_min: vec4<i32>,
    boost_max: vec4<i32>,
}

@group(0) @binding(0) var<uniform> denoise: Denoise;
// What primary visibility and the smooth normals wrote (primary.wgsl, shade.wgsl).
@group(0) @binding(1) var gbuffer: texture_2d<u32>;
@group(0) @binding(2) var surface: texture_2d<u32>;
@group(0) @binding(3) var shading_normals: texture_2d<f32>;
// The frame's samples: the incident light in xyz and the sun's weight in w (`gi_samples`).
@group(0) @binding(4) var samples: texture_2d<f32>;
// The history of the frame before, read: the accumulated samples as `samples` has them, then the
// second moments of the light's luminance and of the sun's weight with the number of samples in the
// pixel (x, y, z), then the geometry as `Geo` packs it. And of this frame, written; the
// accumulated samples are what the filter reads as its first input.
@group(0) @binding(5) var accum_in: texture_2d<f32>;
@group(0) @binding(6) var moments_in: texture_2d<f32>;
@group(0) @binding(7) var geo_in: texture_2d<u32>;
@group(0) @binding(8) var accum_out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(9) var moments_out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(10) var geo_out: texture_storage_2d<rg32uint, write>;
// The variance of each signal's estimate (x: the light's luminance, y: the sun's weight).
@group(0) @binding(11) var variance_out: texture_storage_2d<rg32float, write>;
// What an iteration of the filter reads (a signal as `samples` has it, its variance and the frame's
// geometry) and writes.
@group(0) @binding(12) var signal: texture_2d<f32>;
@group(0) @binding(13) var variance: texture_2d<f32>;
@group(0) @binding(14) var geo: texture_2d<u32>;
@group(0) @binding(15) var signal_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(16) var variance_next: texture_storage_2d<rg32float, write>;

// The step of an iteration of the à-trous filter, in pixels.
override ATROUS_STEP = 1;

// ---- How a history is judged. The light is linear light.
//
// The history of a pixel is not used where the point it reprojects to is not on the pixel's surface:
// farther from its plane than this many Voxels (plus the pixel's footprint, which grows with the
// distance and with how slanted the surface is), or with a normal whose cosine to the pixel's is
// under NORMAL_MIN.
const PLANE_TOLERANCE = 2.0;
const NORMAL_MIN = 0.9;
// The neighbours that count for the pixel's neighbourhood (the 3 x 3 round it) are on its surface in
// the same way, a little more loosely.
const NEIGHBOUR_NORMAL_MIN = 0.8;
// The history is clamped to the neighbourhood's mean plus or minus this many standard deviations,
// and a slack of this share of the mean and this much on top (a neighbourhood of noisy samples has a
// range that the mean of its light is often outside of, and a hard clamp to it takes light away: the
// bias that the upscaler's clamp has the same slack for).
const CLAMP_SIGMAS = 3.0;
const CLAMP_SLACK_RELATIVE = 0.1;
const CLAMP_SLACK_ABSOLUTE = 0.004;
// The adaptive part: a history shortens in proportion to how far, in standard deviations of the
// neighbourhood (with a slack of the same kind), its value is from the neighbourhood's mean, from
// TEMPORAL_REJECT_FROM on, and is gone a deviation after it.
const TEMPORAL_REJECT_FROM = 2.0;
const TEMPORAL_REJECT_SLACK_RELATIVE = 0.15;
const TEMPORAL_REJECT_SLACK_ABSOLUTE = 0.01;
// A history with fewer samples than this takes its variance from the samples round the pixel (3 x
// 3), scaled up by SPATIAL_VARIANCE_SCALE because the samples of neighbouring pixels are not
// independent (the lattice's are blended, `gi_reconstruct`), not from its own moments.
const MOMENT_SAMPLES = 4.0;
const SPATIAL_VARIANCE_SCALE = 3.0;

// ---- The filter's edge stops.
// SVGF's luminance stop: the difference over this many standard deviations of the centre's
// (blurred) variance, with a floor so that a pixel with none still filters a little. The stop is
// tight (1.5, not SVGF's 4) because every pixel is filtered, a long history's too: what a settled
// pixel has left is mostly the radiance cache's error, which is the same from frame to frame (a
// cell's light is the mean of all its samples so far) and so is not in the history's own variance;
// only the filter can average it, over the cells it is in, and a tight stop is what keeps an edge
// or a texture from being blurred with it.
const PHI_LIGHT = 1.5;
const PHI_SUN = 1.5;
const LIGHT_FLOOR = 0.002;
const LIGHT_FLOOR_RELATIVE = 0.01;
const SUN_FLOOR = 0.002;
// The depth's: the distance to the centre's tangent plane over this many Voxels, plus a quarter of a
// pixel's footprint a step of the filter (a curved surface leaves its tangent plane the farther the
// tap is), in a Gaussian. The normal's: the cosine to this power.
const PHI_PLANE = 2.0;
const NORMAL_POWER = 24.0;
// The B3 spline's weights at 0, 1 and 2 taps from the centre.
const ATROUS_KERNEL = array<f32, 3>(0.375, 0.25, 0.0625);

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// A unit vector as two numbers in 0..1 (octahedral), and back.
fn oct_encode(n: vec3<f32>) -> vec2<f32> {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    var e = p;
    if n.z < 0.0 {
        e = (1.0 - abs(p.yx)) * select(vec2<f32>(-1.0), vec2<f32>(1.0), p >= vec2<f32>(0.0));
    }
    return e * 0.5 + 0.5;
}

fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    let f = e * 2.0 - 1.0;
    var n = vec3<f32>(f.x, f.y, 1.0 - abs(f.x) - abs(f.y));
    let t = max(-n.z, 0.0);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return normalize(n);
}

// A pixel's surface as the filters see it: the normal the pixel is lit with, and the distance to the
// hit along the pixel's ray in Voxels, which is negative for a miss.
struct Geo {
    normal: vec3<f32>,
    t: f32,
}

fn pack_geo(g: Geo) -> vec4<u32> {
    return vec4<u32>(pack2x16unorm(oct_encode(g.normal)), bitcast<u32>(g.t), 0u, 0u);
}

fn unpack_geo(w: vec4<u32>) -> Geo {
    var g: Geo;
    g.t = bitcast<f32>(w.y);
    g.normal = oct_decode(unpack2x16unorm(w.x));
    return g;
}

fn load_geo(tex: texture_2d<u32>, p: vec2<i32>) -> Geo {
    return unpack_geo(textureLoad(tex, p, 0));
}

// The face's normal, from the face code of primary.wgsl.
fn face_normal(code: u32) -> vec3<f32> {
    var n = vec3<f32>(0.0);
    if code != 0u {
        let axis = (code - 1u) / 2u;
        n[axis] = select(-1.0, 1.0, ((code - 1u) % 2u) == 1u);
    }
    return n;
}

// The surface of pixel `p` of this frame, from the G-buffer and the smooth normals.
fn current_geo(p: vec2<i32>) -> Geo {
    let g = textureLoad(gbuffer, p, 0);
    var out: Geo;
    out.t = bitcast<f32>(g.x);
    out.normal = face_normal(g.y >> 24u);
    let smoothed = textureLoad(shading_normals, p, 0);
    if denoise.params.y != 0u && smoothed.w > 0.5 {
        out.normal = smoothed.xyz;
    }
    return out;
}

// The direction of the ray through the centre of pixel `p` of an image `size` across (no jitter: a
// sample's place in its pixel is the upscaler's business, and a still camera must find a pixel's
// history in the pixel).
fn pixel_direction(cam: Camera, p: vec2<f32>, size: vec2<f32>) -> vec3<f32> {
    let aspect = size.x / size.y;
    let ndc = (p + 0.5) / size * 2.0 - 1.0;
    let tan_half = cam.params.x;
    return normalize(cam.forward.xyz + cam.right.xyz * (ndc.x * aspect * tan_half) - cam.up.xyz * (ndc.y * tan_half));
}

// The size in Voxels of a pixel across at distance `t`.
fn pixel_footprint(t: f32, size: vec2<f32>) -> f32 {
    return t * 2.0 * denoise.camera.params.x / size.y;
}

// How much farther along a ray a surface at an angle (the cosine of it to the ray) is, for a step
// sideways: the tangent, kept finite.
fn grazing(cosine: f32) -> f32 {
    let c = max(abs(cosine), 0.06);
    return min(sqrt(1.0 - c * c) / c, 16.0);
}

fn in_boost(voxel: vec3<i32>) -> bool {
    return denoise.boost_min.w != 0 && all(voxel >= denoise.boost_min.xyz) && all(voxel < denoise.boost_max.xyz);
}

// ---- Temporal accumulation

@compute @workgroup_size(8, 8)
fn denoise_temporal(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(textureDimensions(gbuffer));
    if i32(id.x) >= size.x || i32(id.y) >= size.y {
        return;
    }
    let p = vec2<i32>(id.xy);
    let here = current_geo(p);
    if here.t < 0.0 {
        // The sky has no light of this kind; a pixel starts again if a surface comes into it.
        textureStore(accum_out, p, vec4<f32>(0.0));
        textureStore(moments_out, p, vec4<f32>(0.0));
        textureStore(geo_out, p, pack_geo(here));
        textureStore(variance_out, p, vec4<f32>(0.0));
        return;
    }
    let sizef = vec2<f32>(size);
    let dir = pixel_direction(denoise.camera, vec2<f32>(id.xy), sizef);
    let position = dir * here.t;
    let footprint = pixel_footprint(here.t, sizef);
    let current = textureLoad(samples, p, 0);

    // This frame's samples round the pixel that are on its surface: their mean and spread, for the
    // clamp, for how far the history is from them and for a variance where the history has none.
    var sum = current;
    var sum2 = current * current;
    var lum = luma(current.xyz);
    var lum2 = lum * lum;
    var n = 1.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let q = p + vec2<i32>(dx, dy);
            if (dx == 0 && dy == 0) || any(q < vec2<i32>(0)) || any(q >= size) {
                continue;
            }
            let g = current_geo(q);
            if g.t < 0.0 || dot(g.normal, here.normal) < NEIGHBOUR_NORMAL_MIN {
                continue;
            }
            let there = pixel_direction(denoise.camera, vec2<f32>(q), sizef) * g.t;
            if abs(dot(here.normal, there - position)) > PLANE_TOLERANCE + 1.5 * footprint * (1.0 + grazing(dot(here.normal, dir))) {
                continue;
            }
            let s = textureLoad(samples, q, 0);
            let l = luma(s.xyz);
            sum += s;
            sum2 += s * s;
            lum += l;
            lum2 += l * l;
            n += 1.0;
        }
    }
    let mean = sum / n;
    let spread = sqrt(max(sum2 / n - mean * mean, vec4<f32>(0.0)));
    let lum_mean = lum / n;
    let lum_var = max(lum2 / n - lum_mean * lum_mean, 0.0);
    let lum_spread = sqrt(lum_var);

    // Where this pixel's point was in the last frame, and the history there: the bilinear mean of
    // the four texels round that point which are on the pixel's surface.
    var past = vec4<f32>(0.0);
    var past_moments = vec3<f32>(0.0);
    var past_weight = 0.0;
    if denoise.params.x != 0u {
        let from_previous = position + denoise.shift.xyz;
        let z = dot(from_previous, denoise.previous.forward.xyz);
        if z > 1e-4 {
            let aspect = sizef.x / sizef.y;
            let tan_half = denoise.previous.params.x;
            let projected = vec2<f32>(
                dot(from_previous, denoise.previous.right.xyz) / (z * aspect * tan_half),
                -dot(from_previous, denoise.previous.up.xyz) / (z * tan_half),
            );
            let at = (projected * 0.5 + 0.5) * sizef - 0.5;
            let base = floor(at);
            let f = at - base;
            let tolerance = PLANE_TOLERANCE + 1.5 * footprint * (1.0 + grazing(dot(here.normal, dir)));
            for (var k = 0; k < 4; k++) {
                let q = vec2<i32>(base) + vec2<i32>(k & 1, k >> 1);
                if any(q < vec2<i32>(0)) || any(q >= size) {
                    continue;
                }
                let weight = select(1.0 - f.x, f.x, (k & 1) == 1) * select(1.0 - f.y, f.y, (k >> 1) == 1);
                let g = load_geo(geo_in, q);
                if weight <= 0.0 || g.t < 0.0 {
                    continue;
                }
                let there = pixel_direction(denoise.previous, vec2<f32>(q), sizef) * g.t;
                if abs(dot(g.normal, from_previous - there)) > tolerance || dot(g.normal, here.normal) < NORMAL_MIN {
                    continue;
                }
                past += weight * textureLoad(accum_in, q, 0);
                past_moments += weight * textureLoad(moments_in, q, 0).xyz;
                past_weight += weight;
            }
        }
    }

    var count = 0.0;
    var kept = current;
    let l_current = luma(current.xyz);
    var moments = vec2<f32>(l_current * l_current, current.w * current.w);
    let longest = bitcast<f32>(denoise.params.z);
    if past_weight > 1e-3 {
        past = past / past_weight;
        past_moments = past_moments / past_weight;
        count = past_moments.z;

        // How spread the history's own samples are, per pixel (its second moments less its mean's
        // square): a pixel whose light is noisy has a history that is a few of those spreads from the
        // neighbourhood's mean by chance, and a clamp or a rejection that does not know it takes
        // light away (the bias a darker, browner cave has); one that has settled has none to spare.
        let past_l = luma(past.xyz);
        let hist_l = sqrt(max(past_moments.x - past_l * past_l, 0.0));
        let hist_w = sqrt(max(past_moments.y - past.w * past.w, 0.0));
        let hist = vec4<f32>(past.xyz / max(past_l, 1e-4) * hist_l, hist_w);

        // The adaptive part: how far the history is from the neighbourhood's mean in standard
        // deviations (with a slack for what 3 x 3 samples do not show), in the light's luminance and
        // in the sun's weight; the history is shortened by how far past TEMPORAL_REJECT_FROM it is.
        let off_light = abs(past_l - lum_mean)
            / (sqrt(lum_var + hist_l * hist_l) + TEMPORAL_REJECT_SLACK_RELATIVE * lum_mean + TEMPORAL_REJECT_SLACK_ABSOLUTE);
        let off_sun = abs(past.w - mean.w)
            / (sqrt(spread.w * spread.w + hist_w * hist_w) + TEMPORAL_REJECT_SLACK_RELATIVE * mean.w + TEMPORAL_REJECT_SLACK_ABSOLUTE);
        count *= 1.0 - clamp(max(off_light, off_sun) - TEMPORAL_REJECT_FROM, 0.0, 1.0);

        // The clamp.
        let slack = CLAMP_SLACK_RELATIVE * mean + CLAMP_SLACK_ABSOLUTE;
        let range = CLAMP_SIGMAS * sqrt(spread * spread + hist * hist) + slack;
        past = clamp(past, mean - range, mean + range);

        // An Edit or a new sun is changing the light in this pixel's cells: a short history, so
        // that the new light is not averaged away over a long one.
        if in_boost(bitcast<vec3<i32>>(textureLoad(surface, p, 0).xyz)) {
            count = min(count, bitcast<f32>(denoise.params.w));
        }
        let alpha = 1.0 / (count + 1.0);
        kept = mix(past, current, alpha);
        moments = mix(past_moments.xy, moments, alpha);
        count = min(count + 1.0, longest);
    } else {
        count = 1.0;
    }

    // The variance of the estimate, which the filter's luminance stop reads: the samples' own
    // variance, from the moments once there are a few of them and else from the neighbourhood, over
    // the number of samples that an average of this length has (an exponential average's variance is
    // that of one sample times a / (2 - a)).
    let l_kept = luma(kept.xyz);
    var var_light = max(moments.x - l_kept * l_kept, 0.0);
    var var_sun = max(moments.y - kept.w * kept.w, 0.0);
    if count < MOMENT_SAMPLES {
        var_light = lum_var * SPATIAL_VARIANCE_SCALE;
        var_sun = max(sum2.w / n - mean.w * mean.w, 0.0) * SPATIAL_VARIANCE_SCALE;
    }
    let share = 1.0 / count;
    let estimate = share / (2.0 - share);

    textureStore(accum_out, p, kept);
    textureStore(moments_out, p, vec4<f32>(moments, count, 0.0));
    textureStore(geo_out, p, pack_geo(here));
    textureStore(variance_out, p, vec4<f32>(var_light * estimate, var_sun * estimate, 0.0, 0.0));
}

// ---- Variance-guided à-trous filter

fn load_variance(p: vec2<i32>, size: vec2<i32>) -> vec2<f32> {
    return textureLoad(variance, clamp(p, vec2<i32>(0), size - 1), 0).xy;
}

@compute @workgroup_size(8, 8)
fn denoise_atrous(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(textureDimensions(signal));
    if i32(id.x) >= size.x || i32(id.y) >= size.y {
        return;
    }
    let p = vec2<i32>(id.xy);
    let here = load_geo(geo, p);
    if here.t < 0.0 {
        textureStore(signal_out, p, vec4<f32>(0.0));
        textureStore(variance_next, p, vec4<f32>(0.0));
        return;
    }
    let sizef = vec2<f32>(size);
    let centre = textureLoad(signal, p, 0);

    // The centre's variance, blurred by a 3 x 3 so that one pixel's chance low does not stop the
    // filter, as SVGF does.
    var blurred = vec2<f32>(0.0);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let w = select(1.0, 2.0, dx == 0) * select(1.0, 2.0, dy == 0) / 16.0;
            blurred += w * load_variance(p + vec2<i32>(dx, dy), size);
        }
    }
    let l_centre = luma(centre.xyz);
    let light_scale = 1.0 / (PHI_LIGHT * sqrt(blurred.x) + LIGHT_FLOOR + LIGHT_FLOOR_RELATIVE * l_centre);
    let sun_scale = 1.0 / (PHI_SUN * sqrt(blurred.y) + SUN_FLOOR);

    let position = pixel_direction(denoise.camera, vec2<f32>(id.xy), sizef) * here.t;
    let footprint = pixel_footprint(here.t, sizef);
    let plane_scale = 1.0 / (PHI_PLANE + 0.25 * footprint * f32(ATROUS_STEP));

    var sum_light = vec3<f32>(0.0);
    var sum_sun = 0.0;
    var total_light = 0.0;
    var total_sun = 0.0;
    var var_light = 0.0;
    var var_sun = 0.0;
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            let q = p + vec2<i32>(dx, dy) * ATROUS_STEP;
            if any(q < vec2<i32>(0)) || any(q >= size) {
                continue;
            }
            let kernel = ATROUS_KERNEL[abs(dx)] * ATROUS_KERNEL[abs(dy)];
            let s = textureLoad(signal, q, 0);
            var w_light = kernel;
            var w_sun = kernel;
            if dx != 0 || dy != 0 {
                let g = load_geo(geo, q);
                if g.t < 0.0 {
                    continue;
                }
                let there = pixel_direction(denoise.camera, vec2<f32>(q), sizef) * g.t;
                let plane = abs(dot(here.normal, there - position)) * plane_scale;
                let w_geometry = exp(-plane * plane) * pow(max(dot(here.normal, g.normal), 0.0), NORMAL_POWER);
                w_light *= w_geometry * exp(-abs(luma(s.xyz) - l_centre) * light_scale);
                w_sun *= w_geometry * exp(-abs(s.w - centre.w) * sun_scale);
            }
            let v = textureLoad(variance, q, 0).xy;
            sum_light += w_light * s.xyz;
            sum_sun += w_sun * s.w;
            total_light += w_light;
            total_sun += w_sun;
            var_light += w_light * w_light * v.x;
            var_sun += w_sun * w_sun * v.y;
        }
    }
    // The centre's own weight is never zero, so the totals are positive.
    let filtered = vec4<f32>(sum_light / total_light, max(sum_sun / total_sun, 0.0));
    let filtered_variance = vec2<f32>(var_light / (total_light * total_light), var_sun / (total_sun * total_sun));
    textureStore(signal_out, p, filtered);
    textureStore(variance_next, p, vec4<f32>(filtered_variance, 0.0, 0.0));
}
