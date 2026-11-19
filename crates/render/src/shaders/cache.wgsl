// Primary's radiance cache (#54, ADR 0005, "Bounce light"): the light that falls on the faces of
// the world, kept in a hash table in world space, so that a bounce ray that lands on a surface reads
// the light there (every bounce after the first) instead of casting rays for it. Used after
// trace.wgsl and common.wgsl; gi.wgsl fills and reads it (`gi_trace`), `gi_resolve` here folds each
// frame's samples in, and shade.wgsl draws its cells for the debug view.
//
// A cell is a cube of voxels of one face direction of the surface: the key is the cube's
// coordinates, the face (0 to 5) and the level of the cube's size, which grows with the distance
// from the camera (`cache_level`), so a far surface that is a pixel across is a cell too, and the
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
// probe; a cell that does not fit is simply not cached (its readers cast the rays themselves).

@group(1) @binding(19) var<storage, read_write> gi_cache: array<atomic<u32>>;

// Keep in step with GI_CACHE_SLOTS in tracer.rs.
const CACHE_SLOTS = 524288u;
const SLOT_WORDS = 12u;
// A slot's words: the cell's check (the hash of its key, never 0: 0 is a free slot), the sums of
// this frame's samples in fixed point (3) and their count, the light as a float (3) and the
// samples' weight in it, and the frame it was last given samples in.
const SLOT_CHECK = 0u;
const SLOT_SUM = 1u;
const SLOT_COUNT = 4u;
const SLOT_LIGHT = 5u;
const SLOT_WEIGHT = 8u;
const SLOT_LAST = 9u;
const CACHE_PROBE = 8u;
// A sample's channel is at most this (one sun-lit face is 1.6, an emitter's light 6 or so), and
// the fixed-point scale of a sum is 1/CACHE_FIXED: 262 thousand samples of the most fit in a word.
const CACHE_SAMPLE_MAX = 16.0;
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

// The cell's coordinates of `voxel` at `level`: the cube of 8 << level voxels it is in.
fn cache_cell_of(voxel: vec3<i32>, level: u32) -> vec3<i32> {
    return voxel >> vec3<u32>(3u + level);
}

fn cache_key(voxel: vec3<i32>, normal: vec3<f32>, distance: f32) -> vec2<u32> {
    let level = cache_level(distance);
    let cell = cache_cell_of(voxel, level);
    let face = cache_face(normal);
    let h = hash_voxel(cell + vec3<i32>(i32(face * 40503u + level * 9973u)));
    let check = hash_region(cell ^ vec3<i32>(i32(face), i32(level), 0x5bd1)) | 1u;
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
            // The weak exchange may fail with nothing in the way: try the slot again then.
            for (var tries = 0u; tries < 4u; tries++) {
                let claimed = atomicCompareExchangeWeak(&gi_cache[word], 0u, cell.check);
                if claimed.exchanged || claimed.old_value == cell.check {
                    at = slot * SLOT_WORDS;
                    break;
                }
                if claimed.old_value != 0u {
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
}

// Folds the frame's samples into each slot's light, and drops the slots that have had none for a
// long time: one thread a slot. Nothing else touches the table while it runs.
@compute @workgroup_size(64)
fn gi_resolve(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x;
    if slot >= CACHE_SLOTS {
        return;
    }
    let at = slot * SLOT_WORDS;
    if atomicLoad(&gi_cache[at + SLOT_CHECK]) == 0u {
        return;
    }
    let count = atomicLoad(&gi_cache[at + SLOT_COUNT]);
    if count == 0u {
        if frame.gi.x - atomicLoad(&gi_cache[at + SLOT_LAST]) > STALE_FRAMES {
            for (var w = 0u; w < SLOT_WORDS; w++) {
                atomicStore(&gi_cache[at + w], 0u);
            }
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
    let limit = select(CACHE_WEIGHT_MAX, CACHE_WEIGHT_MAX_AFTER_EDIT, frame.gi.y != 0u);
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
}

// A colour of the cell's own, for the debug view: the same cell, the same colour, and a cell of
// another level or face another.
fn cache_cell_colour(voxel: vec3<i32>, normal: vec3<f32>, distance: f32) -> vec3<f32> {
    let level = cache_level(distance);
    let cell = cache_cell_of(voxel, level);
    let h = hash_voxel(cell + vec3<i32>(i32(cache_face(normal)), i32(level), 7));
    let rgb = vec3<f32>(f32(h & 255u), f32((h >> 8u) & 255u), f32((h >> 16u) & 255u)) / 255.0;
    return 0.25 + 0.75 * rgb;
}
