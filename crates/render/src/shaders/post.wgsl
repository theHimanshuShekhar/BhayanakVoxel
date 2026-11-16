// The art direction's display transform (#202), run on the linear HDR image of either view: auto-
// exposure, bloom and the tone curve, in that order of dependence. It lights nothing: it changes
// how the light of the image is shown, and never reads or writes a view's running mean (the
// Reference view's mean stays the light of its samples, ADR 0005).
//
// The passes, each its own line in the GPU timings:
//   exposure        the mean of the log of the luminance, one partial sum per workgroup;
//   exposure_adapt  those summed, the exposure that puts the mean at middle grey (within -3 to +5
//                   stops), and the exposure of this frame: the one before it, moved towards that
//                   by `k` (1 - e^(-dt / tau)) in stops;
//   bloom_down      the image at a quarter of its size, a box of 4 x 4;
//   bloom_h         that blurred along x at three scales, one image each;
//   bloom_v         those blurred along y and averaged: the bloom, at a quarter of the size;
//   tone            the image, plus `bloom_strength` of the bloom, times the exposure, through the
//                   tone curve (the AgX-like one is the encoded image itself; Neutral is followed
//                   by the sRGB encoding), as the 8-bit image the upscaler and the copy to the
//                   window read.
// Bloom is linear, so it is blurred before the exposure and added in the same light: it commutes
// with exposure, and the exposure of the frame need not be known when it is blurred.

struct Post {
    // The source image's size in pixels, then the bloom images' (a quarter, rounded up).
    sizes: vec4<u32>,
    // The tone curve (0 off, 1 Neutral, 2 AgX, 3 Neutral desaturated); 1 if auto-exposure is on;
    // 1 if bloom is; then 1 if the image holds a debug view's colours, which are already encoded
    // and are copied as they are.
    stages: vec4<u32>,
    // How much of the bloom is added; the adaptation rate k (1 sets the exposure at once); 1 if the
    // exposure is held as it is; the share of colour that curve 3 keeps.
    params: vec4<f32>,
    // The three bloom blurs' standard deviations, in pixels of the bloom images; one unused.
    sigmas: vec4<f32>,
    // The number of partial sums (the workgroups of `exposure`); 1 if the exposure is metered on
    // 4 x 4 boxes of pixels (the Reference view's), 0 if on pixels; then two unused.
    counts: vec4<u32>,
}

@group(0) @binding(0) var<uniform> post: Post;
@group(0) @binding(1) var source: texture_2d<f32>;
// One partial sum of the log luminance for each workgroup of `exposure`.
@group(0) @binding(2) var<storage, read_write> partials: array<f32>;
// The exposure's state: x is the exposure in stops (log2), y the exposure, z is 1 once it has been
// set (the first frame sets it at once).
@group(0) @binding(3) var<storage, read_write> exposure_state: array<f32>;
@group(0) @binding(4) var down_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var down_in: texture_2d<f32>;
@group(0) @binding(6) var blur_h0: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var blur_h1: texture_storage_2d<rgba16float, write>;
@group(0) @binding(8) var blur_h2: texture_storage_2d<rgba16float, write>;
@group(0) @binding(9) var blur_in0: texture_2d<f32>;
@group(0) @binding(10) var blur_in1: texture_2d<f32>;
@group(0) @binding(11) var blur_in2: texture_2d<f32>;
@group(0) @binding(12) var bloom_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(13) var bloom_in: texture_2d<f32>;
@group(0) @binding(14) var bloom_sampler: sampler;
@group(0) @binding(15) var shown: texture_storage_2d<rgba8unorm, write>;

// The grey that the exposure puts the image's mean at, and how far it may go: -3 to +5 stops.
const MIDDLE_GREY = 0.18;
const MIN_EXPOSURE = 0.125;
const MAX_EXPOSURE = 32.0;

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// The exposure that puts the mean of the log luminance at middle grey, within its range.
fn settled_exposure(mean_log_luminance: f32) -> f32 {
    return clamp(MIDDLE_GREY / exp(mean_log_luminance), MIN_EXPOSURE, MAX_EXPOSURE);
}

var<workgroup> sums: array<f32, 256>;

@compute @workgroup_size(16, 16)
fn exposure(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    var value = 0.0;
    if id.x < post.sizes.x && id.y < post.sizes.y {
        let boxes = post.counts.y != 0u;
        // Metered on boxes, the thread at a box's corner takes the box's mean and the rest of the
        // box's threads count nothing: a few noisy samples' dark pixels then weigh as the box's
        // light does, not each as the darkest the exposure looks at.
        if !boxes || (id.x % 4u == 0u && id.y % 4u == 0u) {
            var c = vec3<f32>(0.0);
            if boxes {
                let last = vec2<i32>(post.sizes.xy) - 1;
                for (var y = 0; y < 4; y++) {
                    for (var x = 0; x < 4; x++) {
                        c += textureLoad(source, min(vec2<i32>(id.xy) + vec2<i32>(x, y), last), 0).rgb;
                    }
                }
                c /= 16.0;
            } else {
                c = textureLoad(source, vec2<i32>(id.xy), 0).rgb;
            }
            // A pixel that is black or not a number counts as the darkest the exposure looks at.
            value = log(max(luminance(c), 1e-4));
            if value != value {
                value = log(1e-4);
            }
        }
    }
    sums[local] = value;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride >>= 1u) {
        if local < stride {
            sums[local] += sums[local + stride];
        }
        workgroupBarrier();
    }
    if local == 0u {
        partials[group.y * groups.x + group.x] = sums[0];
    }
}

@compute @workgroup_size(256)
fn exposure_adapt(@builtin(local_invocation_index) local: u32) {
    var sum = 0.0;
    for (var i = local; i < post.counts.x; i += 256u) {
        sum += partials[i];
    }
    sums[local] = sum;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride >>= 1u) {
        if local < stride {
            sums[local] += sums[local + stride];
        }
        workgroupBarrier();
    }
    if local == 0u {
        var pixels = f32(post.sizes.x) * f32(post.sizes.y);
        if post.counts.y != 0u {
            pixels = f32(post.sizes.z) * f32(post.sizes.w);
        }
        let target_stops = log2(settled_exposure(sums[0] / pixels));
        // The eye adapts in stops, so a step of N stops takes the same time to cross whether the
        // image went brighter or darker. The first frame has nothing to adapt from.
        var stops = target_stops;
        if exposure_state[2] != 0.0 {
            stops = exposure_state[0] + (target_stops - exposure_state[0]) * post.params.y;
            // A held exposure is the one already metered (the Reference view's, once it has enough
            // samples to meter): it is kept as it is.
            if post.params.z != 0.0 {
                stops = exposure_state[0];
            }
        }
        exposure_state[0] = stops;
        exposure_state[1] = exp2(stops);
        exposure_state[2] = 1.0;
    }
}

@compute @workgroup_size(8, 8)
fn bloom_down(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= post.sizes.z || id.y >= post.sizes.w {
        return;
    }
    let last = vec2<i32>(post.sizes.xy) - 1;
    var sum = vec3<f32>(0.0);
    for (var y = 0; y < 4; y++) {
        for (var x = 0; x < 4; x++) {
            let at = clamp(vec2<i32>(id.xy) * 4 + vec2<i32>(x, y), vec2<i32>(0), last);
            sum += textureLoad(source, at, 0).rgb;
        }
    }
    textureStore(down_out, id.xy, vec4<f32>(sum / 16.0, 1.0));
}

// One Gaussian of standard deviation `sigma` over `image` at `at`, along `axis`, with the image's
// edge repeated beyond it (as the CPU prototype's was).
fn blur(image: texture_2d<f32>, at: vec2<i32>, axis: vec2<i32>, sigma: f32) -> vec3<f32> {
    let last = vec2<i32>(post.sizes.zw) - 1;
    let radius = i32(ceil(sigma * 3.0));
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var i = -radius; i <= radius; i++) {
        let w = exp(-f32(i * i) / (2.0 * sigma * sigma));
        sum += textureLoad(image, clamp(at + axis * i, vec2<i32>(0), last), 0).rgb * w;
        total += w;
    }
    return sum / total;
}

@compute @workgroup_size(8, 8)
fn bloom_h(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= post.sizes.z || id.y >= post.sizes.w {
        return;
    }
    let at = vec2<i32>(id.xy);
    textureStore(blur_h0, id.xy, vec4<f32>(blur(down_in, at, vec2<i32>(1, 0), post.sigmas.x), 1.0));
    textureStore(blur_h1, id.xy, vec4<f32>(blur(down_in, at, vec2<i32>(1, 0), post.sigmas.y), 1.0));
    textureStore(blur_h2, id.xy, vec4<f32>(blur(down_in, at, vec2<i32>(1, 0), post.sigmas.z), 1.0));
}

@compute @workgroup_size(8, 8)
fn bloom_v(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= post.sizes.z || id.y >= post.sizes.w {
        return;
    }
    let at = vec2<i32>(id.xy);
    let glow = blur(blur_in0, at, vec2<i32>(0, 1), post.sigmas.x)
        + blur(blur_in1, at, vec2<i32>(0, 1), post.sigmas.y)
        + blur(blur_in2, at, vec2<i32>(0, 1), post.sigmas.z);
    textureStore(bloom_out, id.xy, vec4<f32>(glow / 3.0, 1.0));
}

// The Khronos PBR Neutral tone mapper: colour-faithful until a colour is close to white, then a
// shoulder that rolls the brightest channel off towards 1 and desaturates what is brightest. The
// constants are the standard's; this is the owner's pick of #148.
fn neutral(colour: vec3<f32>) -> vec3<f32> {
    let start_compression = 0.8 - 0.04;
    let desaturation = 0.15;
    let lowest = min(colour.x, min(colour.y, colour.z));
    var offset = 0.04;
    if lowest < 0.08 {
        offset = lowest - 6.25 * lowest * lowest;
    }
    var c = colour - offset;
    let peak = max(c.x, max(c.y, c.z));
    if peak < start_compression {
        return c;
    }
    let d = 1.0 - start_compression;
    let new_peak = 1.0 - d * d / (peak + d - start_compression);
    c *= new_peak / peak;
    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(c, vec3<f32>(new_peak), g);
}

// The prototype's AgX-like curve (Sobotka's, in the polynomial form of Wrensch), as it rendered the
// look-dev sheet (#200). Its result is already display-encoded.
fn agx(colour: vec3<f32>) -> vec3<f32> {
    let agx_in = mat3x3<f32>(
        vec3<f32>(0.842479, 0.04232824, 0.04237565),
        vec3<f32>(0.0784336, 0.8784686, 0.0784336),
        vec3<f32>(0.07922374, 0.07916613, 0.87914297),
    );
    let agx_out = mat3x3<f32>(
        vec3<f32>(1.1968790, -0.05289685, -0.05297164),
        vec3<f32>(-0.09802088, 1.1519031, -0.09804345),
        vec3<f32>(-0.09902974, -0.09896118, 1.1510737),
    );
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    let v = agx_in * max(colour, vec3<f32>(1e-10));
    let x = (clamp(log2(v), vec3<f32>(min_ev), vec3<f32>(max_ev)) - min_ev) / (max_ev - min_ev);
    let x2 = x * x;
    let x4 = x2 * x2;
    let curve = 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2
        + 0.1191 * x - 0.00232;
    return clamp(agx_out * curve, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Linear colour to the image's sRGB-encoded values with the true curve, clamped to 0..1 (the same
// as `encode` in common.wgsl, which the kernels use; this file is a module of its own).
fn encode_srgb(linear: vec3<f32>) -> vec3<f32> {
    let c = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055, 12.92 * c, c <= vec3<f32>(0.0031308));
}

@compute @workgroup_size(8, 8)
fn tone(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= post.sizes.x || id.y >= post.sizes.y {
        return;
    }
    let c = textureLoad(source, vec2<i32>(id.xy), 0).rgb;
    // A debug view's colours are what the view means: not exposed, not bloomed, not toned.
    if post.stages.w != 0u {
        textureStore(shown, id.xy, vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
        return;
    }
    var colour = c;
    if post.stages.z != 0u {
        let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(post.sizes.xy);
        colour += post.params.x * textureSampleLevel(bloom_in, bloom_sampler, uv, 0.0).rgb;
    }
    if post.stages.y != 0u {
        colour *= exposure_state[1];
    }
    if post.stages.x == 2u {
        textureStore(shown, id.xy, vec4<f32>(agx(max(colour, vec3<f32>(0.0))), 1.0));
        return;
    }
    if post.stages.x != 0u {
        colour = neutral(max(colour, vec3<f32>(0.0)));
    }
    if post.stages.x == 3u {
        let luminance = dot(colour, vec3<f32>(0.2126, 0.7152, 0.0722));
        colour = vec3<f32>(luminance) + (colour - vec3<f32>(luminance)) * post.params.w;
    }
    textureStore(shown, id.xy, vec4<f32>(encode_srgb(colour), 1.0));
}
