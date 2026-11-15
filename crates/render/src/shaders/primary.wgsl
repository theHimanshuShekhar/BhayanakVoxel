// Primary visibility: one ray per pixel through trace(), shaded by normal and albedo. It also
// writes the G-buffer that the passes after it read, at the same resolution as the image, in one
// store of two words per pixel (see Target in tracer.rs):
//   x: the distance along the ray to the hit, in Voxels, as the bits of an f32; -1 if it hit
//      nothing;
//   y: bytes 0 to 2 are the colour of the hit's surface before it is lit, encoded like the image
//      (black for a miss); byte 3 is the face that was hit: 0 for a miss, else
//      1 + 2 * axis + (1 if the normal points along the positive direction).
@group(1) @binding(5) var gbuffer: texture_storage_2d<rg32uint, write>;

// The face code of the hit's normal, which is a unit vector along an axis.
fn face_code(hit: Hit) -> u32 {
    if !hit.hit {
        return 0u;
    }
    let n = hit.normal;
    var axis = 2u;
    var along = n.z;
    if abs(n.x) > 0.5 {
        axis = 0u;
        along = n.x;
    } else if abs(n.y) > 0.5 {
        axis = 1u;
        along = n.y;
    }
    return 1u + 2u * axis + select(0u, 1u, along > 0.0);
}

// The light leaving the hit, towards the camera; `colour` is its surface_albedo.
//
// Every read of a source of light, every `trace(` call and every line of this function, control
// flow aside, needs a whole-line `// lighting: <feature>` comment on one of the two lines directly
// above it, naming its row in ADR 0005's table of lighting features (a comment at the end of the
// line does not count). One comment covers the next two lines, so give each term its own. A test
// (`shaders.rs`) fails when a feature has no row, when a line is not marked, and when a binding
// or a shader file appears that it does not know. A PR that adds a lighting feature here adds it to
// the Reference view or marks it in that table (ADR 0005, "How a feature enters").
fn shade(ray: Ray, hit: Hit, colour: vec3<f32>) -> vec3<f32> {
    if !hit.hit {
        // lighting: Sky and atmosphere
        return sky(ray.dir);
    }
    // lighting: Sun and shadows
    let diffuse = max(dot(hit.normal, normalize(frame.sun.xyz)), 0.0);
    // lighting: Ambient term
    let ambient = 0.25;
    // lighting: Sun and shadows
    let lit = colour * (ambient + 0.75 * diffuse);
    // lighting: Emissive Voxels
    return lit + emission(hit.material);
}

@compute @workgroup_size(8, 8)
fn primary(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let ray = camera_ray(id.xy, size, frame.jitter.xy);
    // lighting: Geometry (LOD and what is loaded)
    let hit = trace(ray);
    var colour = vec3<f32>(0.0);
    if hit.hit {
        // lighting: Per-Voxel colour variation
        colour = surface_albedo(hit, false);
    }
    let surface = pack4x8unorm(vec4<f32>(encode(colour), f32(face_code(hit)) / 255.0));
    textureStore(gbuffer, id.xy, vec4<u32>(bitcast<u32>(select(-1.0, hit.t, hit.hit)), surface, 0u, 0u));
    // The upscaler draws the history length view itself, over the lit image.
    if frame.params.z != 0u && frame.params.z != VIEW_HISTORY_LENGTH {
        textureStore(output, id.xy, vec4<f32>(debug_colour(frame.params.z, hit, frame.params.x), 1.0));
        return;
    }
    textureStore(output, id.xy, vec4<f32>(encode(shade(ray, hit, colour)), 1.0));
}
