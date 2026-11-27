// The boxes of the hardware backend's acceleration structures (#50, #236), made on the GPU from the
// scene's own occupancy masks, in place, so that no thread of the CPU reads a brick to build them.
// Joined to trace.wgsl (the scene's buffers, `find_region`) by `shaders::boxes`; the CPU twin is
// `group_prims` in hw.rs, which the tests hold this to, box for box.
//
// One job is one cell: the 4 x 4 x 4 regions of a level that the scene thread counts, which a BLAS is
// made of one or more of (`Grouping`); a cell's boxes are laid out where the job says, and their
// coordinates are from the corner of the BLAS, which is `cell` cells from the cell's own. A job is a
// row of workgroups, one workgroup for each of the cell's 64 regions, in the order x + 4y + 16z.
// A box is 8 words, as the BLAS's geometry has it (a stride of 32 bytes): its min and max corners
// (f32, in Voxels of the level from the group's corner) and the two words that the shader reads
// for a candidate (`Prim::words`). A region's boxes come after the boxes of the regions before it
// in the group, in the order of its mask's bits, a box for each: a brick that is one Material is
// its whole brick, a mixed one the min and max of the Voxels its occupancy mask sets, as the scene
// packed it into the brick's header (a mask that sets none, which no scene lists, is its whole
// brick), and a region that is one Material is one box of 64 Voxels.

struct BoxJob {
    // The first region of the group's cube, in regions of its level.
    first: vec3<i32>,
    level: u32,
    // Where the cell's boxes start in `box_out`, in boxes, and how many it has: no box is written
    // past them.
    start: u32,
    count: u32,
    // Where the cell is in its BLAS, in cells: x | y << 8 | z << 16.
    cell: u32,
    pad1: u32,
}

@group(1) @binding(0) var<storage, read> box_jobs: array<BoxJob>;
@group(1) @binding(1) var<storage, read_write> box_out: array<u32>;

const BOX_WORDS = 8u;
// Set in the first word of the box of a region that is one Material, as `Prim::REGION`.
const BOX_REGION = 0x80000000u;

var<workgroup> region_boxes: array<u32, 64>;

// The region of the cell at index `i` (x + 4y + 16z), as a region coordinate.
fn group_region(job: BoxJob, i: u32) -> vec3<i32> {
    return job.first + vec3<i32>(vec3<u32>(i & 3u, (i >> 2u) & 3u, i >> 4u));
}

// How many boxes the region record `record` of `regions` makes.
fn region_box_count(record: u32) -> u32 {
    let base = record * REGION_WORDS;
    if (regions[base + 16u] & SOLID_FLAG) != 0u {
        return 1u;
    }
    var n = 0u;
    for (var w = 0u; w < 16u; w++) {
        n += countOneBits(regions[base + w]);
    }
    return n;
}

fn pack_brick(brick: vec3<u32>) -> u32 {
    return brick.x | (brick.y << 8u) | (brick.z << 16u);
}

fn write_box(at: u32, low: vec3<u32>, high: vec3<u32>, word0: u32, word1: u32) {
    box_out[at] = bitcast<u32>(f32(low.x));
    box_out[at + 1u] = bitcast<u32>(f32(low.y));
    box_out[at + 2u] = bitcast<u32>(f32(low.z));
    box_out[at + 3u] = bitcast<u32>(f32(high.x));
    box_out[at + 4u] = bitcast<u32>(f32(high.y));
    box_out[at + 5u] = bitcast<u32>(f32(high.z));
    box_out[at + 6u] = word0;
    box_out[at + 7u] = word1;
}

@compute @workgroup_size(64)
fn box_main(@builtin(local_invocation_index) t: u32, @builtin(workgroup_id) wg: vec3<u32>) {
    let job = box_jobs[wg.y];
    // Every thread counts the boxes of one region of the group, so that each workgroup knows
    // where its region's boxes start.
    let found = find_region(job.level, group_region(job, t));
    var mine = 0u;
    if found >= 0 {
        mine = region_box_count(u32(found));
    }
    region_boxes[t] = mine;
    workgroupBarrier();
    let count = region_boxes[wg.x];
    if count == 0u {
        return;
    }
    var offset = 0u;
    for (var i = 0u; i < wg.x; i++) {
        offset += region_boxes[i];
    }
    let local = vec3<u32>(wg.x & 3u, (wg.x >> 2u) & 3u, wg.x >> 4u);
    let cell = vec3<u32>(job.cell & 255u, (job.cell >> 8u) & 255u, (job.cell >> 16u) & 255u);
    let corner = (local + cell * 4u) * 8u;
    let record = u32(find_region(job.level, group_region(job, wg.x)));
    let base = record * REGION_WORDS;
    let first_ref = regions[base + 16u];
    if (first_ref & SOLID_FLAG) != 0u {
        let index = offset;
        if t == 0u && index < job.count {
            let low = corner * 8u;
            write_box((job.start + index) * BOX_WORDS, low, low + vec3<u32>(64u), pack_brick(corner) | BOX_REGION, 0u);
        }
        return;
    }
    // This thread's eight bricks of the 512: the rank of the first is how many the mask sets
    // before it.
    let first_bit = t * 8u;
    let word_at = first_bit >> 5u;
    let word = regions[base + word_at];
    var rank = countOneBits(word & ((1u << (first_bit & 31u)) - 1u));
    for (var j = 0u; j < word_at; j++) {
        rank += countOneBits(regions[base + j]);
    }
    for (var k = 0u; k < 8u; k++) {
        let bit = first_bit + k;
        if ((word >> (bit & 31u)) & 1u) == 0u {
            continue;
        }
        let own = rank;
        rank += 1u;
        let index = offset + own;
        if index >= job.count {
            continue;
        }
        let entry = brick_refs[first_ref + own];
        var lo = vec3<u32>(0u);
        var hi = vec3<u32>(7u);
        if (entry & SOLID_FLAG) == 0u {
            // The tight box that the scene packed into the brick's header (scene.rs, `pack_header`),
            // so that this backend and the compute walk enter the brick through the same one.
            lo = vec3<u32>(header_box_low(bricks[entry + 16u]));
            hi = vec3<u32>(header_box_high(bricks[entry + 16u]));
        }
        let brick = corner + vec3<u32>(bit & 7u, (bit >> 3u) & 7u, bit >> 6u);
        let at = brick * 8u;
        write_box(
            (job.start + index) * BOX_WORDS,
            at + lo,
            at + hi + vec3<u32>(1u),
            pack_brick(brick),
            lo.x | (lo.y << 3u) | (lo.z << 6u) | (hi.x << 9u) | (hi.y << 12u) | (hi.z << 15u),
        );
    }
}
