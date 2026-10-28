// Casts caller-supplied rays through trace() and returns the raw hits. Used by tests to compare
// the GPU against the CPU reference, and available to anything that needs a ray cast.

// Three vec4s per ray: its cell (as i32 bits), origin and direction (w unused).
@group(1) @binding(0) var<storage, read> probe_rays: array<vec4<f32>>;
// Three vec4s per ray, as raw bits: position and t; normal and 1 for a hit (0 for a miss);
// Voxel and Material.
@group(1) @binding(1) var<storage, read_write> probe_hits: array<vec4<u32>>;

@compute @workgroup_size(64)
fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let count = arrayLength(&probe_rays) / 3u;
    if id.x >= count {
        return;
    }
    var ray: Ray;
    ray.cell = bitcast<vec3<i32>>(probe_rays[id.x * 3u].xyz);
    ray.origin = probe_rays[id.x * 3u + 1u].xyz;
    ray.dir = probe_rays[id.x * 3u + 2u].xyz;
    let h = trace(ray);
    let p = bitcast<vec3<u32>>(h.position);
    let n = bitcast<vec3<u32>>(h.normal);
    let v = bitcast<vec3<u32>>(h.voxel);
    probe_hits[id.x * 3u] = vec4<u32>(p, bitcast<u32>(h.t));
    probe_hits[id.x * 3u + 1u] = vec4<u32>(n, select(0u, 1u, h.hit));
    probe_hits[id.x * 3u + 2u] = vec4<u32>(v, h.material);
}
