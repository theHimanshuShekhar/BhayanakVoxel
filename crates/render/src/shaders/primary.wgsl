// Primary visibility: one ray per pixel through trace(), and the G-buffer that the passes after it
// read, at the same resolution as the image. It lights nothing: the lit image is drawn by the
// kernels of shade.wgsl, once the smooth normals are known (#202), from what this writes in two
// stores, one of two words and one of four per pixel (see Target in tracer.rs):
//   gbuffer, x: the distance along the ray to the hit, in Voxels, as the bits of an f32; -1 if it
//      hit nothing;
//   gbuffer, y: bytes 0 to 2 are the colour of the hit's surface before it is lit, encoded like the
//      image (black for a miss); byte 3 is the face that was hit: 0 for a miss, else
//      1 + 2 * axis + (1 if the normal points along the positive direction);
//   surface: the hit's absolute Voxel (three i32s, as bits), then its Material; zeros for a miss.
// A debug view is drawn here, in the one kernel that has the hit: the view's colours, already
// encoded, which the passes after it copy as they are.
@group(1) @binding(5) var gbuffer: texture_storage_2d<rg32uint, write>;
@group(1) @binding(6) var surface: texture_storage_2d<rgba32uint, write>;

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
    let packed = pack4x8unorm(vec4<f32>(encode(colour), f32(face_code(hit)) / 255.0));
    textureStore(gbuffer, id.xy, vec4<u32>(bitcast<u32>(select(-1.0, hit.t, hit.hit)), packed, 0u, 0u));
    var voxel = vec4<u32>(0u);
    if hit.hit {
        voxel = vec4<u32>(bitcast<vec3<u32>>(hit.voxel), hit.material);
    }
    textureStore(surface, id.xy, voxel);
    // The upscaler draws the history length view itself, over the lit image.
    if frame.params.z != 0u && frame.params.z != VIEW_HISTORY_LENGTH && frame.params.z != VIEW_CACHE_CELLS {
        textureStore(output, id.xy, vec4<f32>(debug_colour(frame.params.z, hit, frame.params.x), 1.0));
    }
}
