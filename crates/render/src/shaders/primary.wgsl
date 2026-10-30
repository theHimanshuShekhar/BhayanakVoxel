// Primary visibility: one ray per pixel through trace(), shaded by normal and albedo. It also
// writes the G-buffer that the passes after it read, at the same resolution as the image: the
// distance to the hit, the face normal and the albedo (see Target in tracer.rs).

// Distance along the ray to the hit, in Voxels; negative if the ray hit nothing.
@group(1) @binding(5) var gbuffer_depth: texture_storage_2d<r32float, write>;
// The hit's face normal, which is a unit vector along an axis; zero if the ray hit nothing.
@group(1) @binding(6) var gbuffer_normal: texture_storage_2d<rgba8snorm, write>;
// The colour of the hit's surface before it is lit, encoded like the image; black if the ray hit
// nothing.
@group(1) @binding(7) var gbuffer_albedo: texture_storage_2d<rgba8unorm, write>;

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
    textureStore(gbuffer_depth, id.xy, vec4<f32>(select(-1.0, hit.t, hit.hit), 0.0, 0.0, 0.0));
    textureStore(gbuffer_normal, id.xy, vec4<f32>(select(vec3<f32>(0.0), hit.normal, hit.hit), 0.0));
    var colour = vec3<f32>(0.0);
    if hit.hit {
        colour = surface_albedo(hit);
    }
    textureStore(gbuffer_albedo, id.xy, vec4<f32>(encode(colour), 1.0));
    // The upscaler draws the history length view itself, over the lit image.
    if frame.params.z != 0u && frame.params.z != VIEW_HISTORY_LENGTH {
        textureStore(output, id.xy, vec4<f32>(debug_colour(frame.params.z, hit, frame.params.x), 1.0));
        return;
    }
    textureStore(output, id.xy, vec4<f32>(encode(shade(ray, hit, colour)), 1.0));
}
