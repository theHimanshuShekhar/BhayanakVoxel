// The reference path tracer: the ground truth that later real-time lighting is measured against.
// Each dispatch adds one sample per pixel to a running mean in `accum`, and shows the mean.
//
// A sample follows a path from the camera. At every hit on a Lambertian surface it:
//   - adds the sun's light, by next-event estimation: a shadow ray to a random point on the
//     sun's disc, which counts if nothing is in the way;
//   - continues in a cosine-weighted random direction, so the throughput is just the albedo.
// A path that escapes collects the sky. The sun is not part of the sky, so it is never counted
// twice. Every ray goes through trace(), the only thing that knows about the Planet's storage.
//
// Paths stop after MAX_BOUNCES bounces, which leaves out the light of longer paths: the one
// deliberate bias. Everything else is an unbiased estimate, so the mean converges to the image.

@group(1) @binding(3) var<storage, read_write> accum: array<vec4<f32>>;

// Bounces of indirect light after the camera ray's own hit.
const MAX_BOUNCES = 4u;
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

// The light arriving along one path from the camera.
fn radiance(first: Ray, rng: ptr<function, u32>) -> vec3<f32> {
    var ray = first;
    var throughput = vec3<f32>(1.0);
    var light = vec3<f32>(0.0);
    for (var bounce = 0u; bounce <= MAX_BOUNCES; bounce++) {
        let hit = trace(ray);
        if !hit.hit {
            light += throughput * sky(ray.dir);
            break;
        }
        let colour = albedo(hit.material);

        let to_sun = sample_cone(frame.sun.xyz, frame.sun.w, vec2<f32>(next_f32(rng), next_f32(rng)));
        let facing = dot(hit.normal, to_sun);
        if facing > 0.0 && !trace(leave(ray, hit, to_sun)).hit {
            light += throughput * colour * SUN_RADIANCE * facing;
        }

        throughput *= colour;
        let bounce_dir = cosine_hemisphere(hit.normal, vec2<f32>(next_f32(rng), next_f32(rng)));
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
    var rng = seed_rng(id.xy, sample_index, frame.params.y);
    // A random point in the pixel, so edges anti-alias as samples accumulate.
    let offset = vec2<f32>(next_f32(&rng), next_f32(&rng));
    let estimate = radiance(camera_ray(id.xy, size, offset), &rng);

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
        shown = debug_colour(frame.params.z, trace(first), sample_index);
    }
    textureStore(output, id.xy, vec4<f32>(shown, 1.0));
}
