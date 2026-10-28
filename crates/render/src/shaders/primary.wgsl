// Primary visibility: one ray per pixel through trace(), shaded by normal and albedo.

struct Camera {
    // The Voxel the camera is in, then its position inside that Voxel. The CPU splits the f64
    // position, so these stay small and exact wherever the camera is.
    cell: vec4<i32>,
    origin: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
    // Tangent of half the vertical field of view, then three unused.
    params: vec4<f32>,
}

@group(1) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(1) var output: texture_storage_2d<rgba8unorm, write>;

const SUN = vec3<f32>(0.5, 0.8, 0.3);

// Hardcoded albedo per Material id (see planet-gen: 1 rock, 2 soil).
fn albedo(material: u32) -> vec3<f32> {
    switch material {
        case 1u: { return vec3<f32>(0.45, 0.43, 0.40); }
        case 2u: { return vec3<f32>(0.30, 0.50, 0.20); }
        default: { return vec3<f32>(1.0, 0.0, 1.0); }
    }
}

fn sky(dir: vec3<f32>) -> vec3<f32> {
    return mix(vec3<f32>(0.75, 0.82, 0.92), vec3<f32>(0.25, 0.45, 0.85), clamp(dir.y, 0.0, 1.0));
}

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
    let diffuse = max(dot(normal, normalize(SUN)), 0.0);
    let tint = mix(voxel_tint(hit.voxel), 1.0, far);
    return albedo(hit.material) * tint * (0.25 + 0.75 * diffuse);
}

@compute @workgroup_size(8, 8)
fn primary(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let aspect = f32(size.x) / f32(size.y);
    let ndc = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(size) * 2.0 - 1.0;
    let tan_half = camera.params.x;
    var ray: Ray;
    ray.cell = camera.cell.xyz;
    ray.origin = camera.origin.xyz;
    ray.dir = normalize(camera.forward.xyz + camera.right.xyz * (ndc.x * aspect * tan_half) - camera.up.xyz * (ndc.y * tan_half));
    let colour = shade(ray, trace(ray));
    // The output is written as sRGB-encoded values to a plain unorm texture.
    textureStore(output, id.xy, vec4<f32>(pow(colour, vec3<f32>(1.0 / 2.2)), 1.0));
}
