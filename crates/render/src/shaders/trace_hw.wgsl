// The hardware backend of trace() (#50, a spike): ray queries over procedural AABBs, one per
// non-empty brick of each level (hw.rs), with the compute path's in-brick DDA (`hit_brick`)
// run on each candidate and its hit committed with `rayQueryGenerateIntersection`.
//
// `shaders::hardware` joins this to trace.wgsl, with trace.wgsl's `trace_within` renamed
// `trace_within_compute`; this file defines the `trace_within` that `trace` calls, so the passes
// are the same with either backend. It needs `enable wgpu_ray_query;` first in the module, and the
// device's EXPERIMENTAL_RAY_QUERY.
//
// The TLAS is in a frame whose origin is `hw_anchor` (Voxels of level 0) so that its floats are
// fine near the camera; a ray's world origin is its cell relative to the anchor, plus its
// origin. Distances along the ray are the same in every frame (the instances' scales are 2^level
// and the direction is not renormalised), so `t` needs no conversion.

@group(2) @binding(0) var hw_tlas: acceleration_structure;
// Two vec4s an instance: the group's first Voxel (level 0) and its level; and where its boxes'
// words start (in boxes).
@group(2) @binding(1) var<storage, read> hw_instances: array<vec4<i32>>;
// Eight words a box (the boxes' pool, which the acceleration structures were built from): the box's
// corners, then the two words of `Prim::words` in hw.rs.
@group(2) @binding(2) var<storage, read> hw_prims: array<u32>;
@group(2) @binding(3) var<uniform> hw_anchor: vec4<i32>;

const HW_REGION_BOX = 0x80000000u;

// The entry in `brick_refs` of the brick at absolute brick coordinate `brick` of `level`, or
// 0xffffffff if the level has none there.
fn hw_brick_entry(level: u32, brick: vec3<i32>) -> u32 {
    let rc = brick >> vec3<u32>(3u);
    let region = find_region(level, rc);
    if region < 0 {
        return 0xffffffffu;
    }
    let base = u32(region) * REGION_WORDS;
    let first_ref = regions[base + 16u];
    if (first_ref & SOLID_FLAG) != 0u {
        return first_ref;
    }
    let local = vec3<u32>(brick - rc * 8);
    let bit = local.x | (local.y << 3u) | (local.z << 6u);
    let word = regions[base + (bit >> 5u)];
    if ((word >> (bit & 31u)) & 1u) == 0u {
        return 0xffffffffu;
    }
    var rank = countOneBits(word & ((1u << (bit & 31u)) - 1u));
    for (var j = 0u; j < (bit >> 5u); j++) {
        rank += countOneBits(regions[base + j]);
    }
    return brick_refs[first_ref + rank];
}

// The hit in the box that a ray query found a candidate in, if it is nearer than `limit`.
fn hw_candidate(ray: Ray, w: Walk, c: RayIntersection, limit: f32) -> Hit {
    let at = c.instance_custom_data * 2u;
    let group = hw_instances[at];
    let level = u32(group.w);
    let prim = (u32(hw_instances[at + 1u].x) + c.primitive_index) * 8u + 6u;
    let w0 = hw_prims[prim];
    let w1 = hw_prims[prim + 1u];
    let local = vec3<i32>(vec3<u32>(w0 & 255u, (w0 >> 8u) & 255u, (w0 >> 16u) & 255u));
    let brick = (group.xyz >> vec3<u32>(level + 3u)) + local;
    let whole_region = (w0 & HW_REGION_BOX) != 0u;
    // The box in Voxels of the level, from `low` up to, and not including, `high`.
    var low = brick * 8;
    var high = low + 64;
    if !whole_region {
        let lo = vec3<i32>(vec3<u32>(w1 & 7u, (w1 >> 3u) & 7u, (w1 >> 6u) & 7u));
        let hi = vec3<i32>(vec3<u32>((w1 >> 9u) & 7u, (w1 >> 12u) & 7u, (w1 >> 15u) & 7u));
        high = low + hi + 1;
        low = low + lo;
    }
    let span = box_span(ray, w, level, low, high);
    let t_enter = span.x;
    let t_exit = span.y;
    if t_enter > t_exit || t_exit < 0.0 || t_enter >= limit {
        return miss();
    }
    // As hit_level: the face the ray crosses last, or, from inside, against its largest component.
    var normal = select(vec3<f32>(0.0), -vec3<f32>(w.step), largest_axis(abs(w.d)));
    if t_enter > 0.0 {
        normal = box_normal(ray, w, level, low, high);
    }
    let t_in = max(t_enter, 0.0);
    if whole_region {
        let rc = brick >> vec3<u32>(3u);
        let region = find_region(level, rc);
        if region < 0 {
            return miss();
        }
        return hit_region(ray, w, level, u32(region), rc, t_in, normal);
    }
    let entry = hw_brick_entry(level, brick);
    if entry == 0xffffffffu {
        return miss();
    }
    if (entry & SOLID_FLAG) != 0u {
        return hit_brick(ray, w, level, entry, brick, t_in, normal);
    }
    // The box is the one that the walk of the compute path enters the brick through (`hit_brick`).
    return walk_brick(ray, w, level, entry, brick, low, high - 1, t_in, normal);
}

fn hw_walk(ray: Ray) -> Walk {
    var w: Walk;
    w.d = select(ray.dir, vec3<f32>(TINY), abs(ray.dir) < vec3<f32>(TINY));
    w.inv = 1.0 / w.d;
    w.step = select(vec3<i32>(-1), vec3<i32>(1), w.d > vec3<f32>(0.0));
    w.upper = max(w.step, vec3<i32>(0));
    return w;
}

// The first solid Voxel along the ray that is nearer than `reach_t`, or a miss: trace.wgsl's
// `trace_within`, by ray query.
fn trace_within(ray: Ray, reach_t: f32) -> Hit {
    let w = hw_walk(ray);
    let origin = vec3<f32>(ray.cell - hw_anchor.xyz) + ray.origin;
    var rq: ray_query;
    rayQueryInitialize(&rq, hw_tlas, RayDesc(RAY_FLAG_NONE, 0xffu, 0.0, reach_t, origin, w.d));
    var nearest = miss();
    var limit = reach_t;
    loop {
        if !rayQueryProceed(&rq) {
            break;
        }
        let c = rayQueryGetCandidateIntersection(&rq);
        if c.kind != RAY_QUERY_INTERSECTION_AABB {
            continue;
        }
        let h = hw_candidate(ray, w, c, limit);
        if h.hit && h.t < limit {
            // The hit is in the box's range, up to rounding: commit it inside the query's.
            let commit = clamp(h.t, 0.0, limit);
            rayQueryGenerateIntersection(&rq, commit);
            limit = h.t;
            nearest = h;
        }
    }
    return nearest;
}

// Whether a solid Voxel is in the ray's way within `reach_t`, which ends at the first one that any
// box has (the order the boxes come in is the hardware's, not the ray's): `occluded`, for the
// backend with early-out shadows (`shaders::hardware`).
fn occluded_any_hit(ray: Ray, reach_t: f32) -> bool {
    let w = hw_walk(ray);
    let origin = vec3<f32>(ray.cell - hw_anchor.xyz) + ray.origin;
    var rq: ray_query;
    rayQueryInitialize(&rq, hw_tlas, RayDesc(RAY_FLAG_TERMINATE_ON_FIRST_HIT, 0xffu, 0.0, reach_t, origin, w.d));
    loop {
        if !rayQueryProceed(&rq) {
            break;
        }
        let c = rayQueryGetCandidateIntersection(&rq);
        if c.kind != RAY_QUERY_INTERSECTION_AABB {
            continue;
        }
        let h = hw_candidate(ray, w, c, reach_t);
        if h.hit && h.t < reach_t {
            return true;
        }
    }
    return false;
}
