// Primary visibility: one ray per pixel through trace(), shaded by normal and albedo.

// A cheap per-Voxel brightness variation in 0.9..1.1, so single Voxels read on a flat face.
fn voxel_tint(voxel: vec3<i32>) -> f32 {
    var h = bitcast<u32>(voxel.x) * 0x8da6b343u ^ bitcast<u32>(voxel.y) * 0xd8163841u ^ bitcast<u32>(voxel.z) * 0xcb1ab31fu;
    h ^= h >> 15u;
    h *= 0x2c1b3c6du;
    h ^= h >> 12u;
    return 0.9 + 0.2 * f32(h & 255u) / 255.0;
}

// Far away, one pixel covers many Voxels whose faces point every which way, so the face normal
// is noise. Until coarser levels of detail exist, blend towards the direction away from the
// Planet's centre as the hit gets further than FAR_BLEND_START Voxels from the camera.
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
    let tint = mix(voxel_tint(hit.voxel), 1.0, far);
    return albedo(hit.material) * tint * (0.25 + 0.75 * diffuse);
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
