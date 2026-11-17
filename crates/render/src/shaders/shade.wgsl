// Primary's light, drawn from what primary.wgsl wrote, in three kernels: `normals` (the smooth
// computed normal of each hit, #202: its own pass, so that its cost has a line of its own),
// `shadows` (the sun's light at each hit, #52: two shadow rays a pixel, its own pass for the same
// reason) and `shade_pass`, which draws the lit image as linear light for post.wgsl to expose and
// tone.
//
// Every read of a source of light, every ray cast through the walk and every line of `shade` (the light), control
// flow aside, needs a whole-line `// lighting: <feature>` comment on one of the two lines directly
// above it, naming its row in ADR 0005's table of lighting features (a comment at the end of the
// line does not count). One comment covers the next two lines, so give each term its own. A test
// (`shaders.rs`) fails when a feature has no row, when a line is not marked, and when a binding
// or a shader file appears that it does not know. A PR that adds a lighting feature here adds it to
// the Reference view or marks it in that table (ADR 0005, "How a feature enters").
@group(1) @binding(5) var gbuffer: texture_2d<u32>;
@group(1) @binding(6) var surface: texture_2d<u32>;
// The smooth normal of each hit, written by `normals`: a unit vector, and 1 in w where there is one.
@group(1) @binding(7) var shading_normals_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(8) var shading_normals: texture_2d<f32>;
// The sun's light at each hit, written by `shadows`: the weight that multiplies the sun's radiance
// and the surface's colour (0 in shadow, or where the sun is behind the surface), and read by
// `shade_pass`.
@group(1) @binding(9) var sun_weights_out: texture_storage_2d<r32float, write>;
@group(1) @binding(10) var sun_weights: texture_2d<f32>;

// The face's normal, from the face code of primary.wgsl (the inverse of its `face_code`).
fn face_normal(code: u32) -> vec3<f32> {
    var n = vec3<f32>(0.0);
    if code != 0u {
        let axis = (code - 1u) / 2u;
        n[axis] = select(-1.0, 1.0, ((code - 1u) % 2u) == 1u);
    }
    return n;
}

// The hit that primary visibility found at `pixel`, as far as shading needs it (not its
// position: `shadows` finds that from the distance, as `trace()` does).
fn load_hit(pixel: vec2<u32>) -> Hit {
    let g = textureLoad(gbuffer, pixel, 0);
    let s = textureLoad(surface, pixel, 0);
    var hit: Hit;
    hit.t = bitcast<f32>(g.x);
    hit.hit = hit.t >= 0.0;
    hit.normal = face_normal(g.y >> 24u);
    hit.voxel = bitcast<vec3<i32>>(s.xyz);
    hit.material = s.w;
    return hit;
}

@compute @workgroup_size(8, 8)
fn normals(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(gbuffer);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let hit = load_hit(id.xy);
    var normal = vec4<f32>(0.0);
    if hit.hit {
        normal = vec4<f32>(shading_normal(hit), 1.0);
    }
    textureStore(shading_normals_out, id.xy, normal);
}

// The scramble of Primary's sun samples: a number of its own, so that they do not follow the
// Reference's seed (`frame.params.y`).
const SUN_SEED = 0x5AD00052u;

// The two numbers in 0..1 that choose a point on the sun's disc: the Reference's own sequence
// (sampling.wgsl, Owen-scrambled Sobol, the dimensions 2 and 3 of its group), scrambled per pixel.
// Nothing is read from an earlier frame: the same sample number gives the same point.
fn sun_sample(pixel: vec2<u32>, sample: u32) -> vec2<f32> {
    return sample_group(pixel_hash(pixel, SUN_SEED), sample, 0u, false).zw;
}

// The sun's light at the hit of each pixel (ADR 0005, "Sun and shadows"): the surface's cosine to
// the sun's centre, as it was before shadows, times the share of the sun that can be seen from it.
// What can be seen is `frame.light.w` shadow rays (2 by default, `tracer::DEFAULT_SHADOW_RAYS`),
// from the **geometric face**, each towards its own point of the sun's disc, through trace(): the
// same disc as the Reference's. The points are consecutive samples of one Owen-scrambled Sobol
// sequence, frame * rays + ray, so that the rays of a pixel stratify the disc between them, and the
// frames after stratify it again. A smooth normal shades the hit but never moves a ray's origin. A
// ray is not cast where the sun is behind the shading normal or the face: nothing would be lit.
// Nothing is carried over from another frame, so a sun that has changed is right on the next one.
//
// The Reference weighs each sample by its own cosine over the mean cosine of the disc; Primary
// weighs the rays by the cosine at the disc's centre, so that a lit surface, the whole picture but
// the penumbra, has no noise from the wide disc. The two agree in expectation where the surface
// faces the sun, and differ by the change of the cosine over the disc where it does not and where
// the sun is half hidden: the bench's shadow-edge error (#52) measures that.
@compute @workgroup_size(8, 8)
fn shadows(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(gbuffer);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let ray = camera_ray(id.xy, size, frame.jitter.xy);
    var hit = load_hit(id.xy);
    var weight = 0.0;
    if hit.hit {
        hit.position = ray.origin + ray.dir * hit.t;
        var normal = hit.normal;
        let smoothed = textureLoad(shading_normals, id.xy, 0);
        if frame.flags.x != 0u && smoothed.w > 0.5 {
            normal = smoothed.xyz;
        }
        // lighting: Sun and shadows
        let diffuse = max(dot(normal, normalize(frame.sun.xyz)), 0.0);
        let rays = max(frame.light.w, 1u);
        var seen = 0u;
        for (var k = 0u; diffuse > 0.0 && k < rays; k++) {
            // lighting: Sun and shadows
            let to_sun = sample_cone(frame.sun.xyz, frame.sun.w, sun_sample(id.xy, frame.light.x * rays + k));
            // lighting: Sun and shadows
            if dot(hit.normal, to_sun) > 0.0 && !trace(leave(ray, hit, to_sun)).hit {
                seen += 1u;
            }
        }
        // lighting: Sun and shadows
        weight = diffuse * f32(seen) / f32(rays);
    }
    textureStore(sun_weights_out, id.xy, vec4<f32>(weight, 0.0, 0.0, 0.0));
}

// The light leaving the hit, towards the camera; `colour` is its surface_albedo and `sun` the
// weight of the sun's light there (`shadows`); `uv` is the pixel's place in the image, 0 to 1, which
// the air between it and the camera is looked up by (sky.wgsl).
fn shade(ray: Ray, hit: Hit, colour: vec3<f32>, sun: f32, uv: vec2<f32>) -> vec3<f32> {
    if !hit.hit {
        // lighting: Sky and atmosphere
        return sky(ray.dir);
    }
    // lighting: Ambient term
    let ambient = select(0.0, 0.25, frame.light.y != 0u);
    // lighting: Sun and shadows
    let lit = colour * (ambient + frame.sun_radiance.xyz * sun);
    // lighting: Emissive Voxels
    let light = lit + emission(hit.material) * f32(frame.light.z);
    // lighting: Sky and atmosphere
    return hazed(light, hit.t, uv, ray.dir);
}

@compute @workgroup_size(8, 8)
fn shade_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    // The primary kernel drew a debug view; the upscaler draws the history length one itself, over
    // the lit image.
    if frame.params.z != 0u && frame.params.z != VIEW_HISTORY_LENGTH {
        return;
    }
    let ray = camera_ray(id.xy, size, frame.jitter.xy);
    let hit = load_hit(id.xy);
    var colour = vec3<f32>(0.0);
    var sun = 0.0;
    if hit.hit {
        // lighting: Per-Voxel colour variation
        colour = surface_albedo(hit, false);
        // lighting: Sun and shadows
        sun = textureLoad(sun_weights, id.xy, 0).x;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(size);
    textureStore(output, id.xy, vec4<f32>(shade(ray, hit, colour, sun, uv), 1.0));
}
