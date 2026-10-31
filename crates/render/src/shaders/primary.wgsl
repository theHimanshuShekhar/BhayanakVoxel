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

// Far away, one pixel covers many Voxels whose faces point every which way, so the face normal
// is noise, and so is their variation in colour. The rings of coarser levels (see scene.rs) make
// the Voxels bigger with distance, which keeps them from being many to a pixel, but their faces
// still point every which way: blend towards the direction away from the Planet's centre, and
// towards the Material's plain colour, as the hit gets further than FAR_BLEND_START Voxels from
// the camera.
const FAR_BLEND_START = 100.0;
const FAR_BLEND_END = 1500.0;

// How far the hit is into the blend described above, 0 to 1.
fn far_blend(hit: Hit) -> f32 {
    return smoothstep(FAR_BLEND_START, FAR_BLEND_END, hit.t);
}

// The colour of the hit's surface, which is not lit yet.
fn surface_albedo(hit: Hit) -> vec3<f32> {
    return varied_albedo(hit.material, hit.voxel, 1.0 - far_blend(hit));
}

// The light leaving the hit, towards the camera; `colour` is its surface_albedo.
fn shade(ray: Ray, hit: Hit, colour: vec3<f32>) -> vec3<f32> {
    if !hit.hit {
        return sky(ray.dir);
    }
    let radial = normalize(vec3<f32>(hit.voxel) + 0.5);
    let normal = normalize(mix(hit.normal, radial, far_blend(hit)));
    let diffuse = max(dot(normal, normalize(frame.sun.xyz)), 0.0);
    return colour * (0.25 + 0.75 * diffuse) + emission(hit.material);
}

@compute @workgroup_size(8, 8)
fn primary(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let ray = camera_ray(id.xy, size, frame.jitter.xy);
    let hit = trace(ray);
    var colour = vec3<f32>(0.0);
    if hit.hit {
        colour = surface_albedo(hit);
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
