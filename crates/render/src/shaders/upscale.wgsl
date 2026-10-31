// The temporal upscaler (TAAU): one output pixel per invocation, built from the frame traced at
// the internal resolution and from the history of the output.
//
// Each frame is traced with its sample of every pixel at a different point of it (the jitter).
// For an output pixel this pass
//   - weighs the traced samples around it by their distance from the pixel's centre, which
//     reconstructs the pixel from whichever of them fall near it this frame;
//   - finds where the pixel was in the last frame by reprojecting the point it shows (known from
//     the G-buffer's hit distance) with the previous camera, and reads the history there;
//   - clamps the history to the range of colours of the traced samples around the pixel, so
//     that what no longer belongs there (a Voxel uncovered, a shadow that moved) is not carried
//     on, and
//   - blends the two, the history's share growing with the frames it has been through.
// A still camera therefore keeps adding samples from new points of each pixel, which is what
// makes the image sharp; a moving one gets the history of where its pixels were, or the current
// samples only where there is none.
//
// Colours are the sRGB-encoded values of the traced image, in the history as well. The history
// keeps, in its alpha, the number of frames each pixel has been through.

struct Camera {
    cell: vec4<i32>,
    origin: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    // Tangent of half the vertical field of view, then three unused.
    params: vec4<f32>,
}

struct Upscale {
    camera: Camera,
    // The camera of the frame before, whose image the history is.
    previous: Camera,
    // How far `camera` is from `previous`, in Voxels, then one unused.
    shift: vec4<f32>,
    // Where in its pixel the traced sample is, (0, 0) to (1, 1), then two unused.
    jitter: vec4<f32>,
    // 1 if there is a history (0 on the first frame), the debug view, then two unused.
    params: vec4<u32>,
}

@group(0) @binding(0) var<uniform> upscale: Upscale;
// The traced image (sRGB-encoded) and the G-buffer, of which this reads the hit distance (the
// first word, an f32's bits; see primary.wgsl), at the internal resolution.
@group(0) @binding(1) var colour: texture_2d<f32>;
@group(0) @binding(2) var gbuffer: texture_2d<u32>;
// The history of the last frame, read, and of this one, written; at the output resolution.
@group(0) @binding(3) var history: texture_2d<f32>;
@group(0) @binding(4) var history_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var image: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(6) var history_sampler: sampler;

// Frames after which a pixel's history has the share it keeps: the current frame is then 1/16 of
// the pixel. Keep this in step with the colour scale of the history length view.
const MAX_HISTORY = 16.0;
// 1 / (2 sigma^2) of the Gaussian that weighs traced samples by their distance, in traced pixels,
// from the centre of the output pixel.
const SHARPNESS = 8.0;
// How far (in encoded colour) the clamp may move the history before the pixel's history is
// thrown away: it was of something else.
const CLAMP_REJECT = 0.1;
// Keep in step with DebugView in debug_view.rs.
const VIEW_HISTORY_LENGTH = 5u;

fn load_colour(p: vec2<i32>, size: vec2<i32>) -> vec3<f32> {
    return textureLoad(colour, clamp(p, vec2<i32>(0), size - 1), 0).rgb;
}

// The history at `position` (in output pixels, a texel's centre being at +0.5), by Catmull-Rom
// interpolation: a bilinear read would soften the image a little more every frame it moves. The
// sixteen texels that it weighs are read with five bilinear taps instead (the middle four texels
// in one, and one for each of the four texels' neighbours along an axis), leaving out the four
// corners, whose weights are small. A position at a texel's centre reads that texel alone.
fn read_history(position: vec2<f32>, size: vec2<i32>) -> vec3<f32> {
    let texel = vec2<f32>(size);
    let centre = floor(position - 0.5) + 0.5;
    let f = position - centre;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = f2 - 0.5 * (f3 + f);
    let w1 = 1.5 * f3 - 2.5 * f2 + 1.0;
    let w3 = 0.5 * (f3 - f2);
    let w2 = 1.0 - w0 - w1 - w3;
    // The middle two texels as one bilinear tap, at the point that splits them as w1 : w2.
    let w12 = w1 + w2;
    let near = (centre + w2 / w12) / texel;
    let before = (centre - 1.0) / texel;
    let after = (centre + 2.0) / texel;
    var sum = vec3<f32>(0.0);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(near.x, before.y), 0.0).rgb * (w12.x * w0.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(before.x, near.y), 0.0).rgb * (w0.x * w12.y);
    sum += textureSampleLevel(history, history_sampler, near, 0.0).rgb * (w12.x * w12.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(after.x, near.y), 0.0).rgb * (w3.x * w12.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(near.x, after.y), 0.0).rgb * (w12.x * w3.y);
    let total = w12.x * w0.y + w0.x * w12.y + w12.x * w12.y + w3.x * w12.y + w12.x * w3.y;
    return sum / total;
}

// The colour the history length view shows: dark violet for a pixel with a frame, to yellow at
// MAX_HISTORY. (The same ramp as the sample count view.)
fn length_colour(frames: f32) -> vec3<f32> {
    let scale = clamp((frames - 1.0) / (MAX_HISTORY - 1.0), 0.0, 1.0);
    return mix(vec3<f32>(0.10, 0.0, 0.30), vec3<f32>(1.0, 0.90, 0.20), scale);
}

@compute @workgroup_size(8, 8)
fn upscale_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let out_size = vec2<i32>(textureDimensions(image));
    if i32(id.x) >= out_size.x || i32(id.y) >= out_size.y {
        return;
    }
    let in_size = vec2<i32>(textureDimensions(colour));
    let pixel = vec2<i32>(id.xy);
    // The centre of the output pixel, in traced pixels; the traced sample of pixel j is at
    // j + jitter. The nearest of them is the middle of the 3x3 that is looked at.
    let centre = (vec2<f32>(pixel) + 0.5) * vec2<f32>(in_size) / vec2<f32>(out_size);
    let jitter = upscale.jitter.xy;
    let nearest = vec2<i32>(floor(centre - jitter + 0.5));

    var low = vec3<f32>(1e9);
    var high = vec3<f32>(-1e9);
    var sum = vec3<f32>(0.0);
    var total = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let p = nearest + vec2<i32>(dx, dy);
            let c = load_colour(p, in_size);
            low = min(low, c);
            high = max(high, c);
            let d = vec2<f32>(p) + jitter - centre;
            let w = exp(-dot(d, d) * SHARPNESS);
            sum += c * w;
            total += w;
        }
    }

    var shown: vec3<f32>;
    var frames = 1.0;
    if upscale.params.x == 0u {
        // The first frame is plain primary visibility: the sample nearest to the pixel.
        shown = load_colour(nearest, in_size);
    } else {
        shown = sum / total;
        // Where this pixel's point was in the last frame. The point is the hit of the nearest
        // sample's distance along the ray through this pixel's centre; a miss is infinitely far,
        // so only the camera's turn moves it.
        let ndc = (vec2<f32>(pixel) + 0.5) / vec2<f32>(out_size) * 2.0 - 1.0;
        let aspect = f32(out_size.x) / f32(out_size.y);
        let tan_half = upscale.camera.params.x;
        let dir = normalize(upscale.camera.forward.xyz
            + upscale.camera.right.xyz * (ndc.x * aspect * tan_half)
            - upscale.camera.up.xyz * (ndc.y * tan_half));
        let t = bitcast<f32>(textureLoad(gbuffer, clamp(nearest, vec2<i32>(0), in_size - 1), 0).x);
        var from_previous = dir;
        if t >= 0.0 {
            from_previous = dir * t + upscale.shift.xyz;
        }
        let z = dot(from_previous, upscale.previous.forward.xyz);
        let previous_tan_half = upscale.previous.params.x;
        let projected = vec2<f32>(
            dot(from_previous, upscale.previous.right.xyz) / (z * aspect * previous_tan_half),
            -dot(from_previous, upscale.previous.up.xyz) / (z * previous_tan_half),
        );
        let at = (projected * 0.5 + 0.5) * vec2<f32>(out_size);
        if z > 1e-4 && all(at >= vec2<f32>(0.0)) && all(at < vec2<f32>(out_size)) {
            let past = read_history(at, out_size);
            let kept = clamp(past, low, high);
            if distance(kept, past) <= CLAMP_REJECT {
                frames = min(textureLoad(history, vec2<i32>(at), 0).a + 1.0, MAX_HISTORY);
                // A traced sample far from the pixel's centre (a weak total weight) says little
                // about it, so it takes a smaller share than a near one.
                shown = mix(kept, shown, clamp(total, 0.0, 1.0) / frames);
            }
            // Otherwise the history was of something else: it is thrown away, and the pixel
            // starts again from this frame's samples alone (frames is 1, the current colour).
        }
    }

    textureStore(history_out, pixel, vec4<f32>(shown, frames));
    if upscale.params.y == VIEW_HISTORY_LENGTH {
        shown = length_colour(frames);
    }
    textureStore(image, pixel, vec4<f32>(shown, 1.0));
}
