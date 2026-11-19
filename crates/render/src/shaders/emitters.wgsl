// The emissive Voxels as lights that are sampled directly (#54, ADR 0005, "Emissive Voxels"): the
// buffer that crates/render/src/emitters.rs builds (keep the layout in step with it), and the
// functions the views take light from emitters with. Used after trace.wgsl, common.wgsl and
// sampling.wgsl.
//
// An emitter is a box of Voxels of one emissive Material (`EmitterBox`). A shaded point takes the
// list of the cell it is in (64 Voxels across, at most EMITTER_LIST_MAX boxes), picks one box in
// proportion to `box_importance`, and a direction over the solid angle that the box's faces cover as
// seen from the point: `sample_emitter`. That direction has the same density wherever in the
// solid angle it points, so an emitter that is close cannot make a speckle. `emitter_hit_pdf` is the
// density with which the sampling would have chosen the direction of a ray that hit a box, which the
// Reference weighs the bounce ray against (the balance heuristic, as for the sky) and Primary uses to
// leave out of its bounce rays the light that its own emitter sample counts.
//
// Every one of them works in the box's own frame, relative to its first Voxel, so the numbers stay
// small wherever the box is: a point is `vec3<f32>(start.cell - box.lo) + start.origin`.

@group(1) @binding(18) var<storage, read> emitters: array<u32>;

// Keep in step with MAX_PER_CELL in emitters.rs.
const EMITTER_LIST_MAX = 8u;
const EMITTER_CELL_SHIFT = 6u;
const EMITTER_BOX_WORDS = 8u;
// How far outside its box a hit still counts as on it, in Voxels: the rounding of a hit's position.
const EMITTER_HIT_SLACK = 2e-3;

struct EmitterBox {
    lo: vec3<i32>,
    hi: vec3<i32>,
    material: u32,
}

fn emitter_box(index: u32) -> EmitterBox {
    let at = emitters[3] + index * EMITTER_BOX_WORDS;
    var b: EmitterBox;
    b.lo = bitcast<vec3<i32>>(vec3<u32>(emitters[at], emitters[at + 1u], emitters[at + 2u]));
    b.material = emitters[at + 3u];
    b.hi = bitcast<vec3<i32>>(vec3<u32>(emitters[at + 4u], emitters[at + 5u], emitters[at + 6u]));
    return b;
}

// The list of the cell that `voxel` is in: x is where it starts in the lists, y how many boxes it has.
fn emitter_list(voxel: vec3<i32>) -> vec2<u32> {
    let cell = voxel >> vec3<u32>(EMITTER_CELL_SHIFT);
    let key = bitcast<vec3<u32>>(cell);
    let mask = emitters[0];
    var slot = hash_region(cell) & mask;
    for (var i = 0u; i <= mask; i++) {
        let at = 4u + slot * 4u;
        let packed = emitters[at + 3u];
        if packed == 0u {
            return vec2<u32>(0u);
        }
        if emitters[at] == key.x && emitters[at + 1u] == key.y && emitters[at + 2u] == key.z {
            return vec2<u32>(packed & 0xffffffu, packed >> 24u);
        }
        slot = (slot + 1u) & mask;
    }
    return vec2<u32>(0u);
}

fn list_box(first: u32, i: u32) -> u32 {
    return emitters[emitters[2] + first + i];
}

fn component(v: vec3<f32>, axis: u32) -> f32 {
    if axis == 0u {
        return v.x;
    }
    if axis == 1u {
        return v.y;
    }
    return v.z;
}

fn contains_voxel(b: EmitterBox, voxel: vec3<i32>) -> bool {
    return all(voxel >= b.lo) && all(voxel < b.hi);
}

// Whether a hit found by a ray that started in `ray.cell` is on the box (closed, to the rounding of
// the hit's position).
fn hit_in_box(ray: Ray, hit: Hit, b: EmitterBox) -> bool {
    let p = vec3<f32>(ray.cell - b.lo) + hit.position;
    let size = vec3<f32>(b.hi - b.lo);
    return all(p >= vec3<f32>(-EMITTER_HIT_SLACK)) && all(p <= size + EMITTER_HIT_SLACK);
}

// How much a box counts in choosing one from the list, for a point `o` in the box's frame: its light
// times a stand-in for the solid angle it covers (its faces' area that `o` is outside of, over the
// squared distance to it and that area, so that it never passes the light itself). 0 for a point
// inside the box, and for a box that holds `voxel`, the Voxel the shaded point is on: its light
// leaves above its faces and never reaches itself.
fn box_importance(o: vec3<f32>, b: EmitterBox, voxel: vec3<i32>) -> f32 {
    if contains_voxel(b, voxel) {
        return 0.0;
    }
    let size = vec3<f32>(b.hi - b.lo);
    let outside = max(max(-o, o - size), vec3<f32>(0.0));
    var area = 0.0;
    if outside.x > 0.0 {
        area += size.y * size.z;
    }
    if outside.y > 0.0 {
        area += size.z * size.x;
    }
    if outside.z > 0.0 {
        area += size.x * size.y;
    }
    if area <= 0.0 {
        return 0.0;
    }
    // lighting: Emissive Voxels
    let e = emission(b.material);
    let light = max(e.x, max(e.y, e.z));
    return light * area / (dot(outside, outside) + area);
}

// The face of the box on `axis` that `o` is outside of, as seen from `o`: its rectangle for
// `sph_quad`, which has no solid angle where `o` is between the box's two faces along that axis.
fn box_face(o: vec3<f32>, size: vec3<f32>, axis: u32) -> SphQuad {
    let b = (axis + 1u) % 3u;
    let c = (axis + 2u) % 3u;
    let along = component(o, axis);
    let extent = component(size, axis);
    var plane = 0.0;
    var z0 = 0.0;
    if along < 0.0 {
        z0 = along;
    } else if along > extent {
        plane = extent;
        z0 = extent - along;
    }
    let x0 = -component(o, b);
    let y0 = -component(o, c);
    return sph_quad(x0, y0, z0, x0 + component(size, b), y0 + component(size, c));
}

// The solid angle of each of the three faces (x, y, z) that `o` is outside of.
fn box_solid_angles(o: vec3<f32>, b: EmitterBox) -> vec3<f32> {
    let size = vec3<f32>(b.hi - b.lo);
    return vec3<f32>(
        box_face(o, size, 0u).solid_angle,
        box_face(o, size, 1u).solid_angle,
        box_face(o, size, 2u).solid_angle,
    );
}

struct EmitterSample {
    // The direction from the point to the sampled point of the box, and the density over solid
    // angle with which it was chosen (0 where nothing was: no list, no light, or no solid angle).
    dir: vec3<f32>,
    pdf: f32,
    // The box that was chosen and its Material.
    box: u32,
    material: u32,
}

// One emitter chosen from the list of the cell that `voxel` is in, as seen from `start` (a ray's
// cell and origin: the point that leaves the surface of `voxel`). `u` are four numbers in 0..1: the
// box, the face, and the point of the face.
fn sample_emitter(start: Ray, voxel: vec3<i32>, u: vec4<f32>) -> EmitterSample {
    var s: EmitterSample;
    s.dir = vec3<f32>(0.0, 0.0, 1.0);
    s.pdf = 0.0;
    s.box = 0u;
    s.material = 0u;
    let list = emitter_list(voxel);
    if list.y == 0u {
        return s;
    }
    var weights = array<f32, EMITTER_LIST_MAX>();
    var total = 0.0;
    for (var i = 0u; i < min(list.y, EMITTER_LIST_MAX); i++) {
        let b = emitter_box(list_box(list.x, i));
        let o = vec3<f32>(start.cell - b.lo) + start.origin;
        weights[i] = box_importance(o, b, voxel);
        total += weights[i];
    }
    if total <= 0.0 {
        return s;
    }
    // The box: the first whose share of the total is above `u.x`, else the last that has any.
    var chosen = 0u;
    var found = false;
    var cumulative = 0.0;
    for (var i = 0u; i < min(list.y, EMITTER_LIST_MAX); i++) {
        if weights[i] > 0.0 {
            chosen = i;
            cumulative += weights[i];
            if u.x * total < cumulative {
                found = true;
                break;
            }
        }
    }
    let index = list_box(list.x, chosen);
    let b = emitter_box(index);
    let o = vec3<f32>(start.cell - b.lo) + start.origin;
    let size = vec3<f32>(b.hi - b.lo);
    let angles = box_solid_angles(o, b);
    let omega = angles.x + angles.y + angles.z;
    if omega <= 0.0 {
        return s;
    }
    // The face, in proportion to its solid angle, then the point in it.
    var axis = 2u;
    if u.y * omega < angles.x {
        axis = 0u;
    } else if u.y * omega < angles.x + angles.y {
        axis = 1u;
    }
    let q = box_face(o, size, axis);
    if q.solid_angle <= 0.0 {
        return s;
    }
    let xy = sph_quad_sample(q, u.zw);
    var point = o;
    let b_axis = (axis + 1u) % 3u;
    let c_axis = (axis + 2u) % 3u;
    var plane = 0.0;
    if component(o, axis) > component(size, axis) {
        plane = component(size, axis);
    }
    point[axis] = plane;
    point[b_axis] = component(o, b_axis) + xy.x;
    point[c_axis] = component(o, c_axis) + xy.y;
    s.dir = normalize(point - o);
    s.pdf = weights[chosen] / total / omega;
    s.box = index;
    s.material = b.material;
    return s;
}

// The density over solid angle with which `sample_emitter` would have chosen the direction of a ray
// that went from `ray` (a bounce ray: its cell and origin are where it left the surface of `voxel`)
// to the emissive `hit`; 0 if the list of `voxel`'s cell has no box that `hit` is on, so that the
// sampling could not have found it and the bounce ray counts it in full.
fn emitter_hit_pdf(ray: Ray, hit: Hit, voxel: vec3<i32>) -> f32 {
    let list = emitter_list(voxel);
    if list.y == 0u {
        return 0.0;
    }
    var total = 0.0;
    var mine = 0.0;
    var on = EMITTER_LIST_MAX;
    for (var i = 0u; i < min(list.y, EMITTER_LIST_MAX); i++) {
        let b = emitter_box(list_box(list.x, i));
        let o = vec3<f32>(ray.cell - b.lo) + ray.origin;
        let w = box_importance(o, b, voxel);
        total += w;
        if w > 0.0 && on == EMITTER_LIST_MAX && hit_in_box(ray, hit, b) {
            on = i;
            mine = w;
        }
    }
    if on == EMITTER_LIST_MAX || total <= 0.0 {
        return 0.0;
    }
    let b = emitter_box(list_box(list.x, on));
    let o = vec3<f32>(ray.cell - b.lo) + ray.origin;
    let angles = box_solid_angles(o, b);
    let omega = angles.x + angles.y + angles.z;
    if omega <= 0.0 {
        return 0.0;
    }
    return mine / total / omega;
}
