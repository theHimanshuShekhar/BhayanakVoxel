// Primary's radiance cache (#54, ADR 0005, "Bounce light"): the light that falls on the faces of
// the world, kept in a hash table in world space, so that a bounce ray that lands on a surface reads
// the light there (every bounce after the first) instead of casting rays for it. Used after
// trace.wgsl and common.wgsl; gi.wgsl fills and reads it (`gi_trace`), `gi_resolve` here folds each
// frame's samples in, and shade.wgsl draws its cells for the debug view.
//
// A cell is a square of voxels of one face direction of the surface, one voxel thick along the
// face's normal (`cache_cell_of`): the key is its coordinates, the face (0 to 5) and the level of
// the square's size, which grows with the distance from the camera (`cache_level`), so a far surface that is a pixel across is a cell too, and the
// table's size is bounded by what the camera sees, not by the world's. What a cell holds is the
// **incident** light on its face, a multiple of the surface's colour (what a lit surface is the
// colour times, the same unit as `sun_radiance`): not the emitted light of the face itself, which
// is added where it is seen, and not multiplied by a colour, so that Voxels of different colours
// in one cell share it.
//
// Samples are added to a cell with atomic integer adds of fixed-point numbers, which commute: the
// sums, and so the cache, are the same whatever order the threads ran in. A frame's samples are
// folded into the cell's running mean by `gi_resolve` (an exponential average whose weight is the
// samples counted, so a new cell is its first samples and an old one moves by how many it got), and
// a cell that has had none for STALE_FRAMES is dropped. The table is open-addressed with a short
// probe and tombstones (a dropped cell's slot is CACHE_DROPPED, not free: a probe stops at a free
// slot, and would lose the cells put beyond one); a cell that does not fit is simply not cached
// (its readers cast the rays themselves).

@group(1) @binding(19) var<storage, read_write> gi_cache: array<atomic<u32>>;

// Keep in step with GI_CACHE_SLOTS in tracer.rs.
const CACHE_SLOTS = 524288u;
const SLOT_WORDS = 12u;
// A slot's words: the cell's check (the hash of its key, never 0: 0 is a free slot), the sums of
// this frame's samples in fixed point (3) and their count, the light as a float (3) and the
// samples' weight in it, the frame it was last given samples in, and whether a sample of this frame
// came from a cell in the Edit's box (`frame.boost_min`, `boost_max`), which the average follows
// faster.
const SLOT_CHECK = 0u;
const SLOT_SUM = 1u;
const SLOT_COUNT = 4u;
const SLOT_LIGHT = 5u;
const SLOT_WEIGHT = 8u;
const SLOT_LAST = 9u;
const SLOT_BOOST = 10u;
const CACHE_PROBE = 8u;
// The check of a slot that held a cell that was dropped: not a cell (a check is odd), but not free
// either, so that a probe goes on past it to the cells that were put after it. A new cell may take
// it.
const CACHE_DROPPED = 2u;
// A sample's channel is at most this: a sun-lit face is 1.6 and an emitter's light 10 to 12 (a
// lamp, the molten core), and one emitter sample of a cell with several emitters is that times how
// many times its pick was less likely than the bounce's (the list's total over its weight), up to
// a few times 10 near a lamp: a lower cap clips the light close to the emitters, where most of it
// is. The cap is for the rare firefly. The fixed-point scale of a sum is 1/CACHE_FIXED, so a word
// holds 65 thousand samples of the most (a cell a frame has a few thousand at the most).
const CACHE_SAMPLE_MAX = 64.0;
const CACHE_FIXED = 1024.0;
// How many samples' weight a cell keeps at most (so it follows a change), and the same in the
// frames after the scene changed. A cell with more samples a frame than this follows within a
// frame or two.
const CACHE_WEIGHT_MAX = 48.0;
const CACHE_WEIGHT_MAX_AFTER_EDIT = 4.0;
// Frames without a sample after which a cell is dropped, and the age after which a reader that
// finds a cell adds a sample of its own now and then, so that a cell nothing is looking at is
// still kept in step with the scene.
const STALE_FRAMES = 180u;
const REFRESH_AGE = 8u;
// Level 0's cells are 8 voxels across (40 cm) out to this distance (voxels); beyond, a cell is
// about 1.5 to 6 % of its distance across (8 << level voxels), up to level 10.
const CACHE_LEVEL0_EDGE = 8;
const CACHE_LEVEL0_UNTIL = 256.0;
const CACHE_LEVEL_MAX = 10u;

// What a lookup found.
struct CacheCell {
    // Where the cell is (the first word of its slot); CACHE_NONE where it is not and cannot be.
    slot: u32,
    // The light, and whether there is any (some samples have been folded in).
    light: vec3<f32>,
    known: bool,
    // The frames since it was given a sample.
    age: u32,
    // The check of the key, for adding it.
    check: u32,
    home: u32,
    // Whether the cell is within the box that an Edit or a new sun made the cache forget faster in.
    boosted: bool,
}

const CACHE_NONE = 0xffffffffu;

// The level of a cell at `distance` voxels from the camera.
fn cache_level(distance: f32) -> u32 {
    if distance <= CACHE_LEVEL0_UNTIL {
        return 0u;
    }
    return min(u32(ceil(log2(distance / CACHE_LEVEL0_UNTIL))), CACHE_LEVEL_MAX);
}

// The face that a unit normal along an axis is, 0 to 5.
fn cache_face(normal: vec3<f32>) -> u32 {
    let a = abs(normal);
    var axis = 2u;
    if a.x >= a.y && a.x >= a.z {
        axis = 0u;
    } else if a.y >= a.z {
        axis = 1u;
    }
    return axis * 2u + select(0u, 1u, component(normal, axis) > 0.0);
}

// The cell's coordinates of `voxel` at `level` for a surface facing `face`: along the face, the
// cube of 8 << level voxels it is in; along the face's normal, the voxel's own layer (coarsened only
// by `level` itself, to 1 << level voxels, at a distance where that is less than a pixel). A cell
// that spanned 8 voxels along the normal would be shared by the floors on both sides of a wall
// thinner than that, and the dark side would read the lit side's light.
fn cache_cell_of(voxel: vec3<i32>, level: u32, face: u32) -> vec3<i32> {
    let across = voxel >> vec3<u32>(3u + level);
    let along = voxel >> vec3<u32>(level);
    let axis = face >> 1u;
    return select(across, along, vec3<u32>(0u, 1u, 2u) == vec3<u32>(axis));
}

// Whether the Voxel is in the box that the cache forgets faster in, in the frames after an Edit or a
// new sun.
fn cache_boosted(voxel: vec3<i32>) -> bool {
    return frame.gi.y != 0u && all(voxel >= frame.boost_min.xyz) && all(voxel < frame.boost_max.xyz);
}

// Where a thin wall splits a level 0 cell: the cell is 8 voxels across, and a wall of a voxel or
// two inside it (a wall across a floor, a partition under a ceiling) stands between floors that
// would share it and the light of the lit one. Along each of the two axes of the face, the run of
// free Voxels the surface's own is in, two Voxels above the surface (so that a step of one Voxel
// does not count, and a stair or a rough ground is not cut up), clipped to the cell: its two ends,
// in 3 bits each, are the cell's split, the same for every point of one side of a wall and another
// for the other side. A cell nothing stands in is the one split. Bit 12: the Voxel above is solid
// (a crevice). Level 0 only: farther the wall is less than a pixel, and the scan reads level 0.
fn cache_split(voxel: vec3<i32>, face: u32, level: u32) -> u32 {
    if level != 0u {
        return 0u;
    }
    let axis = face >> 1u;
    let up = select(-2, 2, (face & 1u) == 1u);
    var front = voxel;
    front[axis] += up;
    var bricks = new_brick_cache();
    if solid_at(front, &bricks) {
        return 0x1000u;
    }
    var split = 0u;
    var shift = 0u;
    for (var a = 0u; a < 3u; a++) {
        if a == axis {
            continue;
        }
        let local = u32(voxel[a] & 7);
        var lo = local;
        var hi = local;
        var at = front;
        for (var i = 1u; i <= local; i++) {
            at[a] = front[a] - i32(i);
            if solid_at(at, &bricks) {
                break;
            }
            lo = local - i;
        }
        for (var i = 1u; i < 8u - local; i++) {
            at[a] = front[a] + i32(i);
            if solid_at(at, &bricks) {
                break;
            }
            hi = local + i;
        }
        split |= (lo | (hi << 3u)) << shift;
        shift += 6u;
    }
    return split;
}

fn cache_key(voxel: vec3<i32>, normal: vec3<f32>, distance: f32) -> vec2<u32> {
    let level = cache_level(distance);
    let face = cache_face(normal);
    let cell = cache_cell_of(voxel, level, face);
    let split = cache_split(voxel, face, level);
    let h = hash_voxel(cell + vec3<i32>(i32(face * 40503u + level * 9973u + split * 2654435u)));
    // Every part of the key counts in the check, and two keys that differ in the face or the split do
    // not meet in it by xor-ing small numbers into the cell's (the cell is hashed on its own).
    let kind = face + 8u * level + 128u * split;
    let check = (hash_region(cell) ^ (kind * 0x9e3779b1u)) | 1u;
    return vec2<u32>(h & (CACHE_SLOTS - 1u), check);
}

// The cell of the surface at `voxel` facing `normal`, `distance` voxels from the camera, as the
// table has it: its light if there is any. Reads only: it adds nothing.
fn cache_incident(voxel: vec3<i32>, normal: vec3<f32>, distance: f32) -> CacheCell {
    let key = cache_key(voxel, normal, distance);
    var cell: CacheCell;
    cell.slot = CACHE_NONE;
    cell.light = vec3<f32>(0.0);
    cell.known = false;
    cell.age = 0u;
    cell.check = key.y;
    cell.home = key.x;
    cell.boosted = cache_boosted(voxel);
    var slot = key.x;
    for (var i = 0u; i < CACHE_PROBE; i++) {
        let at = slot * SLOT_WORDS;
        let found = atomicLoad(&gi_cache[at + SLOT_CHECK]);
        if found == key.y {
            cell.slot = at;
            let weight = bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_WEIGHT]));
            cell.known = weight > 0.0;
            cell.light = vec3<f32>(
                bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_LIGHT])),
                bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_LIGHT + 1u])),
                bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_LIGHT + 2u])),
            );
            cell.age = frame.gi.x - atomicLoad(&gi_cache[at + SLOT_LAST]);
            return cell;
        }
        if found == 0u {
            return cell;
        }
        slot = (slot + 1u) & (CACHE_SLOTS - 1u);
    }
    return cell;
}

// Adds a sample of the incident light to the cell of the key that `cache_incident` found nothing
// for, or to the slot it found: claims a free slot of the probe if there is no cell yet.
fn cache_add(cell: CacheCell, sample: vec3<f32>) {
    var at = cell.slot;
    if at == CACHE_NONE {
        var slot = cell.home;
        for (var i = 0u; i < CACHE_PROBE; i++) {
            let word = slot * SLOT_WORDS + SLOT_CHECK;
            // The weak exchange may fail with nothing in the way: try the slot again then. A free
            // slot or a dropped cell's is taken (the cell is not in the probe: the reader looked).
            var take = 0u;
            for (var tries = 0u; tries < 4u; tries++) {
                let claimed = atomicCompareExchangeWeak(&gi_cache[word], take, cell.check);
                if claimed.exchanged || claimed.old_value == cell.check {
                    at = slot * SLOT_WORDS;
                    break;
                }
                if claimed.old_value == CACHE_DROPPED {
                    take = CACHE_DROPPED;
                } else if claimed.old_value != 0u {
                    break;
                }
            }
            if at != CACHE_NONE {
                break;
            }
            slot = (slot + 1u) & (CACHE_SLOTS - 1u);
        }
        if at == CACHE_NONE {
            return;
        }
    }
    let v = clamp(sample, vec3<f32>(0.0), vec3<f32>(CACHE_SAMPLE_MAX));
    let fixed = vec3<u32>(round(v * CACHE_FIXED));
    atomicAdd(&gi_cache[at + SLOT_SUM], fixed.x);
    atomicAdd(&gi_cache[at + SLOT_SUM + 1u], fixed.y);
    atomicAdd(&gi_cache[at + SLOT_SUM + 2u], fixed.z);
    atomicAdd(&gi_cache[at + SLOT_COUNT], 1u);
    if cell.boosted {
        atomicOr(&gi_cache[at + SLOT_BOOST], 1u);
    }
}

// Folds the frame's samples into each slot's light, and drops (to a tombstone) the slots that have had none for a
// long time: one thread a slot. Nothing else touches the table while it runs.
@compute @workgroup_size(64)
fn gi_resolve(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x;
    if slot >= CACHE_SLOTS {
        return;
    }
    let at = slot * SLOT_WORDS;
    let check = atomicLoad(&gi_cache[at + SLOT_CHECK]);
    if check == 0u || check == CACHE_DROPPED {
        return;
    }
    let count = atomicLoad(&gi_cache[at + SLOT_COUNT]);
    if count == 0u {
        if frame.gi.x - atomicLoad(&gi_cache[at + SLOT_LAST]) > STALE_FRAMES {
            for (var w = 1u; w < SLOT_WORDS; w++) {
                atomicStore(&gi_cache[at + w], 0u);
            }
            atomicStore(&gi_cache[at + SLOT_CHECK], CACHE_DROPPED);
        }
        return;
    }
    let sum = vec3<f32>(
        f32(atomicLoad(&gi_cache[at + SLOT_SUM])),
        f32(atomicLoad(&gi_cache[at + SLOT_SUM + 1u])),
        f32(atomicLoad(&gi_cache[at + SLOT_SUM + 2u])),
    );
    let mean = sum / (CACHE_FIXED * f32(count));
    let kept = bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_WEIGHT]));
    let boosted = atomicLoad(&gi_cache[at + SLOT_BOOST]) != 0u;
    let limit = select(CACHE_WEIGHT_MAX, CACHE_WEIGHT_MAX_AFTER_EDIT, boosted);
    let n = f32(count);
    // A cell's first samples are its light; after that each frame moves it by their share.
    let share = n / (n + min(kept, limit));
    let before = vec3<f32>(
        bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_LIGHT])),
        bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_LIGHT + 1u])),
        bitcast<f32>(atomicLoad(&gi_cache[at + SLOT_LIGHT + 2u])),
    );
    let light = mix(before, mean, share);
    atomicStore(&gi_cache[at + SLOT_LIGHT], bitcast<u32>(light.x));
    atomicStore(&gi_cache[at + SLOT_LIGHT + 1u], bitcast<u32>(light.y));
    atomicStore(&gi_cache[at + SLOT_LIGHT + 2u], bitcast<u32>(light.z));
    atomicStore(&gi_cache[at + SLOT_WEIGHT], bitcast<u32>(min(kept + n, 1.0e6)));
    atomicStore(&gi_cache[at + SLOT_LAST], frame.gi.x);
    atomicStore(&gi_cache[at + SLOT_SUM], 0u);
    atomicStore(&gi_cache[at + SLOT_SUM + 1u], 0u);
    atomicStore(&gi_cache[at + SLOT_SUM + 2u], 0u);
    atomicStore(&gi_cache[at + SLOT_COUNT], 0u);
    atomicStore(&gi_cache[at + SLOT_BOOST], 0u);
}

// A colour of the cell's own, for the debug view: the same cell, the same colour, and a cell of
// another level or face another.
fn cache_cell_colour(voxel: vec3<i32>, normal: vec3<f32>, distance: f32) -> vec3<f32> {
    let level = cache_level(distance);
    let face = cache_face(normal);
    let cell = cache_cell_of(voxel, level, face);
    let split = cache_split(voxel, face, level);
    let h = hash_voxel(cell + vec3<i32>(i32(face), i32(level), 7 + i32(split)));
    let rgb = vec3<f32>(f32(h & 255u), f32((h >> 8u) & 255u), f32((h >> 16u) & 255u)) / 255.0;
    return 0.25 + 0.75 * rgb;
}
