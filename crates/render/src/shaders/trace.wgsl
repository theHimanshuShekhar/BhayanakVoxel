// trace(ray) -> Hit: the one place that knows how the Planet is stored and walked.
//
// The data layout is documented in scene.rs. reference.rs is a line-for-line CPU copy of this
// file, checked against it by tests: change them together.
//
// Voxel, brick and region coordinates are absolute i32 Planet-frame integers. Floats are only
// ever relative to the ray's `cell`, the integer Voxel the camera is in, so nothing loses
// precision far from the Planet's origin.

struct SceneHeader {
    // Minimum corner of the occupied regions in Voxels, and the table's slot count minus one.
    origin: vec3<i32>,
    table_mask: u32,
    // Extent of the occupied regions, in regions.
    dims: vec3<u32>,
    pad: u32,
}

@group(0) @binding(0) var<uniform> scene: SceneHeader;
@group(0) @binding(1) var<storage, read> table: array<u32>;
@group(0) @binding(2) var<storage, read> regions: array<u32>;
@group(0) @binding(3) var<storage, read> brick_refs: array<u32>;
@group(0) @binding(4) var<storage, read> bricks: array<u32>;

const REGION_WORDS = 17u;
const SOLID_FLAG = 0x80000000u;
// A ray crosses at most 3 * 8 - 2 = 22 cells of an 8 x 8 x 8 block.
const BLOCK_STEPS = 24;
const MAX_REGION_STEPS = 4096;
// Stands in for a zero direction component, so that 1 / d is finite.
const TINY = 1e-20;

struct Ray {
    // The Voxel the ray starts in.
    cell: vec3<i32>,
    // Start of the ray relative to the corner of `cell`, normally in 0..1.
    origin: vec3<f32>,
    // Unit length, so that distances are in Voxels.
    dir: vec3<f32>,
}

struct Hit {
    hit: bool,
    // Distance along the ray to the face it entered the Voxel through.
    t: f32,
    // Where that is, relative to the corner of `ray.cell`.
    position: vec3<f32>,
    // Unit normal of that face, pointing back towards the ray. For a ray that starts inside a
    // solid Voxel (t = 0) it faces against the ray's largest direction component.
    normal: vec3<f32>,
    // Absolute Planet-frame Voxel coordinate.
    voxel: vec3<i32>,
    material: u32,
}

fn hash_region(key: vec3<i32>) -> u32 {
    let k = bitcast<vec3<u32>>(key);
    var h = (k.x * 0x8da6b343u) ^ (k.y * 0xd8163841u) ^ (k.z * 0xcb1ab31fu);
    h ^= h >> 15u;
    h *= 0x2c1b3c6du;
    return h ^ (h >> 12u);
}

// Index of the region at absolute region coordinate `key` in `regions`, or -1.
fn find_region(key: vec3<i32>) -> i32 {
    let k = bitcast<vec3<u32>>(key);
    var slot = hash_region(key) & scene.table_mask;
    for (var i = 0u; i <= scene.table_mask; i++) {
        let at = slot * 4u;
        let index = table[at + 3u];
        if index == 0u {
            return -1;
        }
        if table[at] == k.x && table[at + 1u] == k.y && table[at + 2u] == k.z {
            return i32(index - 1u);
        }
        slot = (slot + 1u) & scene.table_mask;
    }
    return -1;
}

// The axis a DDA steps along: the smallest of `t`, with ties going to x, then y, then z.
fn first_axis(t: vec3<f32>) -> vec3<bool> {
    let x = t.x <= t.y && t.x <= t.z;
    let y = !x && t.y <= t.z;
    return vec3<bool>(x, y, !x && !y);
}

// The axis of the largest component of `v`, ties as above.
fn largest_axis(v: vec3<f32>) -> vec3<bool> {
    let x = v.x >= v.y && v.x >= v.z;
    let y = !x && v.y >= v.z;
    return vec3<bool>(x, y, !x && !y);
}

fn min3(v: vec3<f32>) -> f32 {
    return min(v.x, min(v.y, v.z));
}

fn max3(v: vec3<f32>) -> f32 {
    return max(v.x, max(v.y, v.z));
}

// Material of Voxel `bit` in the mixed brick at `at` in `bricks`.
fn mixed_material(at: u32, bit: u32) -> u32 {
    let header = bricks[at + 16u];
    let bits = header & 0xffu;
    let palette_len = header >> 8u;
    let palette = at + 17u;
    let indices = palette + (palette_len + 1u) / 2u;
    let position = bit * bits;
    let index = (bricks[indices + position / 32u] >> (position % 32u)) & ((1u << bits) - 1u);
    return (bricks[palette + index / 2u] >> ((index % 2u) * 16u)) & 0xffffu;
}

fn miss() -> Hit {
    var h: Hit;
    h.hit = false;
    return h;
}

fn hit_at(ray: Ray, t: f32, normal: vec3<f32>, voxel: vec3<i32>, material: u32) -> Hit {
    var h: Hit;
    h.hit = true;
    h.t = t;
    h.position = ray.origin + ray.dir * t;
    h.normal = normal;
    h.voxel = voxel;
    h.material = material;
    return h;
}

// The ray with what every level of the walk needs. `d` is the direction with zeros nudged off
// zero, `inv` is 1 / d, `step` is the sign of d, and `upper` is 1 on axes that step upwards.
struct Walk {
    d: vec3<f32>,
    inv: vec3<f32>,
    step: vec3<i32>,
    upper: vec3<i32>,
}

// Distance along the ray to each of the planes at absolute Voxel coordinate `plane`.
fn reach(ray: Ray, w: Walk, plane: vec3<i32>) -> vec3<f32> {
    return (vec3<f32>(plane - ray.cell) - ray.origin) * w.inv;
}

// The Voxel the ray is in at distance `t`.
fn voxel_at(ray: Ray, w: Walk, t: f32) -> vec3<i32> {
    return ray.cell + vec3<i32>(floor(ray.origin + w.d * t));
}

// Walks the Voxels of the brick at absolute brick coordinate `brick` (entry `entry` in
// `brick_refs`), entered at `t_in` through the face with normal `normal_in`.
fn hit_brick(ray: Ray, w: Walk, entry: u32, brick: vec3<i32>, t_in: f32, normal_in: vec3<f32>) -> Hit {
    let first = brick * 8;
    // The entry point is on the brick's boundary, so rounding can put it a hair outside.
    var voxel = clamp(voxel_at(ray, w, t_in), first, first + 7);
    if (entry & SOLID_FLAG) != 0u {
        return hit_at(ray, t_in, normal_in, voxel, entry & 0xffffu);
    }
    var next = reach(ray, w, voxel + w.upper);
    let delta = abs(w.inv);
    var t = t_in;
    var normal = normal_in;
    for (var i = 0; i < BLOCK_STEPS; i++) {
        let local = vec3<u32>(voxel - first);
        let bit = local.x | (local.y << 3u) | (local.z << 6u);
        if ((bricks[entry + (bit >> 5u)] >> (bit & 31u)) & 1u) != 0u {
            return hit_at(ray, t, normal, voxel, mixed_material(entry, bit));
        }
        let axis = first_axis(next);
        t = min3(next);
        voxel += select(vec3<i32>(0), w.step, axis);
        next += select(vec3<f32>(0.0), delta, axis);
        normal = select(vec3<f32>(0.0), -vec3<f32>(w.step), axis);
        if any(voxel < first) || any(voxel > first + 7) {
            break;
        }
    }
    return miss();
}

// Walks the bricks of the region at absolute region coordinate `rc` (index `region` in
// `regions`), entered at `t_in` through the face with normal `normal_in`.
fn hit_region(ray: Ray, w: Walk, region: u32, rc: vec3<i32>, t_in: f32, normal_in: vec3<f32>) -> Hit {
    let base = region * REGION_WORDS;
    let first = rc * 8;
    let first_ref = regions[base + 16u];
    if (first_ref & SOLID_FLAG) != 0u {
        // The whole region is one Material.
        let voxel = clamp(voxel_at(ray, w, t_in), first * 8, first * 8 + 63);
        return hit_at(ray, t_in, normal_in, voxel, first_ref & 0xffffu);
    }
    var brick = clamp(voxel_at(ray, w, t_in) >> vec3<u32>(3u), first, first + 7);
    var next = reach(ray, w, (brick + w.upper) * 8);
    let delta = abs(8.0 * w.inv);
    var t = t_in;
    var normal = normal_in;
    for (var i = 0; i < BLOCK_STEPS; i++) {
        let local = vec3<u32>(brick - first);
        let bit = local.x | (local.y << 3u) | (local.z << 6u);
        let word = regions[base + (bit >> 5u)];
        if ((word >> (bit & 31u)) & 1u) != 0u {
            // The entry follows one entry for each lower bit set in the mask.
            var rank = countOneBits(word & ((1u << (bit & 31u)) - 1u));
            for (var j = 0u; j < (bit >> 5u); j++) {
                rank += countOneBits(regions[base + j]);
            }
            let entry = brick_refs[first_ref + rank];
            let h = hit_brick(ray, w, entry, brick, t, normal);
            if h.hit {
                return h;
            }
        }
        let axis = first_axis(next);
        t = min3(next);
        brick += select(vec3<i32>(0), w.step, axis);
        next += select(vec3<f32>(0.0), delta, axis);
        normal = select(vec3<f32>(0.0), -vec3<f32>(w.step), axis);
        if any(brick < first) || any(brick > first + 7) {
            break;
        }
    }
    return miss();
}

// The first solid Voxel along the ray, or a miss.
fn trace(ray: Ray) -> Hit {
    if any(scene.dims == vec3<u32>(0u)) {
        return miss();
    }
    var w: Walk;
    w.d = select(ray.dir, vec3<f32>(TINY), abs(ray.dir) < vec3<f32>(TINY));
    w.inv = 1.0 / w.d;
    w.step = select(vec3<i32>(-1), vec3<i32>(1), w.d > vec3<f32>(0.0));
    w.upper = max(w.step, vec3<i32>(0));

    // Clip the ray to the box of occupied regions.
    let lowest = scene.origin >> vec3<u32>(6u);
    let highest = lowest + vec3<i32>(scene.dims) - 1;
    let t_a = reach(ray, w, scene.origin);
    let t_b = reach(ray, w, scene.origin + vec3<i32>(scene.dims) * 64);
    let t_near = min(t_a, t_b);
    let t_enter = max(max3(t_near), 0.0);
    if t_enter > min3(max(t_a, t_b)) {
        return miss();
    }
    // The ray enters through the face it crosses last. If it starts inside the box, the normal
    // only matters for a start inside a solid Voxel: face against the largest component.
    var normal = select(vec3<f32>(0.0), -vec3<f32>(w.step), largest_axis(abs(w.d)));
    if max3(t_near) > 0.0 {
        normal = select(vec3<f32>(0.0), -vec3<f32>(w.step), largest_axis(t_near));
    }

    var t = t_enter;
    var rc = clamp(voxel_at(ray, w, t) >> vec3<u32>(6u), lowest, highest);
    var next = reach(ray, w, (rc + w.upper) * 64);
    let delta = abs(64.0 * w.inv);
    for (var i = 0; i < MAX_REGION_STEPS; i++) {
        let region = find_region(rc);
        if region >= 0 {
            let h = hit_region(ray, w, u32(region), rc, t, normal);
            if h.hit {
                return h;
            }
        }
        let axis = first_axis(next);
        t = min3(next);
        rc += select(vec3<i32>(0), w.step, axis);
        next += select(vec3<f32>(0.0), delta, axis);
        normal = select(vec3<f32>(0.0), -vec3<f32>(w.step), axis);
        if any(rc < lowest) || any(rc > highest) {
            break;
        }
    }
    return miss();
}
