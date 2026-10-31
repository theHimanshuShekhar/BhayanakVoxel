// Primary visibility: one ray per pixel through trace(), shaded by normal and albedo.

// Far away, one pixel covers many Voxels whose faces point every which way, so the face normal
// is noise, and so is their variation in colour. The rings of coarser levels (see scene.rs) make
// the Voxels bigger with distance, which keeps them from being many to a pixel, but their faces
// still point every which way: blend towards the direction away from the Planet's centre, and
// towards the Material's plain colour, as the hit gets further than FAR_BLEND_START Voxels from
// the camera.
const FAR_BLEND_START = 100.0;
const FAR_BLEND_END = 1500.0;

fn shade(ray: Ray, hit: Hit) -> vec3<f32> {
    if !hit.hit {
        return sky(ray.dir);
    }
    let radial = normalize(vec3<f32>(hit.voxel) + 0.5);
    let far = smoothstep(FAR_BLEND_START, FAR_BLEND_END, hit.t);
    let normal = normalize(mix(hit.normal, radial, far));
    let diffuse = max(dot(normal, normalize(frame.sun.xyz)), 0.0);
    let colour = varied_albedo(hit.material, hit.voxel, 1.0 - far);
    return colour * (0.25 + 0.75 * diffuse) + emission(hit.material);
}

@compute @workgroup_size(8, 8)
fn primary(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let ray = camera_ray(id.xy, size, vec2<f32>(0.5));
    let hit = trace(ray);
    if frame.params.z != 0u {
        textureStore(output, id.xy, vec4<f32>(debug_colour(frame.params.z, hit, frame.params.x), 1.0));
        return;
    }
    textureStore(output, id.xy, vec4<f32>(encode(shade(ray, hit)), 1.0));
}
