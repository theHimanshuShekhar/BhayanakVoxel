// Primary's light, drawn from what primary.wgsl wrote, in two kernels: `normals` (the smooth
// computed normal of each hit, #202: its own pass, so that its cost has a line of its own) and
// `shade_pass`, which draws the lit image as linear light for post.wgsl to expose and tone.
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

// The face's normal, from the face code of primary.wgsl (the inverse of its `face_code`).
fn face_normal(code: u32) -> vec3<f32> {
    var n = vec3<f32>(0.0);
    if code != 0u {
        let axis = (code - 1u) / 2u;
        n[axis] = select(-1.0, 1.0, ((code - 1u) % 2u) == 1u);
    }
    return n;
}

// The hit that primary visibility found at `pixel`, as far as shading needs it.
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

// The light leaving the hit, towards the camera; `colour` is its surface_albedo and `normal` the
// normal it is shaded with.
fn shade(ray: Ray, hit: Hit, colour: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    if !hit.hit {
        // lighting: Sky and atmosphere
        return sky(ray.dir);
    }
    // lighting: Sun and shadows
    let diffuse = max(dot(normal, normalize(frame.sun.xyz)), 0.0);
    // lighting: Ambient term
    let ambient = 0.25;
    // lighting: Sun and shadows
    let lit = colour * (ambient + frame.sun_radiance.xyz * diffuse);
    // lighting: Emissive Voxels
    let light = lit + emission(hit.material);
    // lighting: Sky and atmosphere
    return hazed(light, hit.t);
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
    var normal = hit.normal;
    if hit.hit {
        // lighting: Per-Voxel colour variation
        colour = surface_albedo(hit, false);
        let smoothed = textureLoad(shading_normals, id.xy, 0);
        if frame.flags.x != 0u && smoothed.w > 0.5 {
            normal = smoothed.xyz;
        }
    }
    textureStore(output, id.xy, vec4<f32>(shade(ray, hit, colour, normal), 1.0));
}
