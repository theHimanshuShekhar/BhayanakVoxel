//! The hardware ray-tracing backend behind `trace(ray) -> Hit` (#50, a spike; #236).
//!
//! Ray queries (`wgpu::Features::EXPERIMENTAL_RAY_QUERY`, Vulkan only) over **procedural AABBs,
//! one per non-empty brick** of each level; a ray query reports each box it enters as a candidate
//! and the shader (`shaders/trace_hw.wgsl`) runs the same in-brick DDA as the compute path on it
//! (`hit_brick`), then `rayQueryGenerateIntersection`s the hit. The compute `trace()` stays the
//! default, the fallback and the CPU-checkable reference (`reference.rs`).
//!
//! Shadow rays (`occluded` in the shaders) end at the first hit with `RtBackend::Hardware`
//! (`RAY_FLAG_TERMINATE_ON_FIRST_HIT`), as the compute walk does, and look for the nearest like
//! any ray with `RtBackend::HardwareNearest`; every other ray is the nearest-hit query.
//!
//! * **Tight boxes** ([`tight_box`]): a brick's box is the min/max of the Voxels of its occupancy
//!   mask, not the whole 8 x 8 x 8 (Tuxedo Labs, GPC 2025; #136). A brick that is one Material is
//!   its whole box. A region that is one Material (the core) is one box of 64 Voxels, since its
//!   bricks are not listed. **They are made on the GPU** ([`BoxPass`], `shaders/hw_boxes.wgsl`),
//!   from the occupancy masks that the scene already has there, in one pool of boxes that the
//!   BLASes are built from and the ray-query shader reads the candidates' words from; the CPU
//!   twin, [`group_prims`], is what the tests hold the GPU to, box for box (#236).
//! * **Regular BLAS grouping** ([`group_of`]): every level's bricks go into BLASes by
//!   [`GROUP_REGIONS`]-region cubes of the level's region grid, a function of the region's
//!   coordinate alone: never of what is in it, so an Edit changes the contents of a BLAS and not
//!   which BLAS a brick is in, and the TLAS keeps its shape.
//! * Each BLAS is one TLAS instance with a translation and a scale of 2^level, so a box is in
//!   level Voxels in the BLAS (0..256) whatever the level; the instance's custom data indexes the
//!   instance table the shader reads (group origin, level, where its boxes start in the pool).
//! * The TLAS is in a frame whose origin is [`ANCHOR_SNAP`]-aligned and near the camera, so that
//!   its `f32`s are fine near the camera whatever the Planet's coordinates are; it is rebuilt
//!   (instances only; the BLASes are as they were) when the camera leaves it.
//!
//! # Keeping up with streaming, off the frame thread (#236)
//!
//! * **Which groups are stale** comes with the patch: the scene thread, which wrote the regions,
//!   lists the groups they are in and counts their boxes ([`RingPatch::groups`]), so the frame
//!   thread searches nothing and reads no brick.
//! * **Nothing waits for the GPU.** The boxes' pass, the BLAS builds and the TLAS build are
//!   recorded and submitted, in the order of the queue, after the scene's own upload and before
//!   the frame that traces: wgpu makes the barriers between them. A box is never written while a
//!   frame that was submitted earlier reads it, for the same reason.
//! * **Builds are budgeted** ([`BUILD_BUDGET`] boxes a frame): the groups to build again wait in a
//!   queue, nearest the camera first (so an Edit, which is at the camera, is built the frame it
//!   is made), and a group whose turn has not come keeps the BLAS it had in the TLAS, so there is
//!   never a hole and never a half-built BLAS to trace. What it shows for a few frames is the
//!   boxes it had; the words of the bricks are the scene's, as they are. A jump, which makes
//!   every group again, builds them all at once: there is nothing right to show until it has.
//!
//! What this does **not** do: the compute path's clip to the ball that has the Planet in it
//! (nothing is outside it, so it only saves steps), compaction, or update-in-place of a BLAS (a
//! group's BLAS is built again whole, from its new boxes).

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::Result;
use crate::scene::{
    Level, REGION_BRICKS, REGION_FIRST_REF, REGION_VOXELS, REGION_WORDS, SOLID_FLAG, Scene,
    SceneUpdate, hash_region,
};

/// Regions along each edge of a BLAS's cube: 4 x 4 x 4 regions, 32 bricks and 256 Voxels of its
/// level across. A boxes' coordinates in a BLAS are then 0..=256, exact in `f32`.
pub const GROUP_REGIONS: i32 = 4;
/// Bricks along each edge of a BLAS's cube.
pub const GROUP_BRICKS: i32 = GROUP_REGIONS * REGION_BRICKS;
/// The TLAS's origin is a multiple of this many Voxels of level 0, nearest to the camera.
pub const ANCHOR_SNAP: i32 = 4096;
/// How many boxes of BLASes an update builds at the most (and, in the frames that follow, a
/// pump of the queue): at least the nearest group is always built. An update after a jump is not
/// held to it. A first value: a build of 52 BLASes of the flight cost the GPU 1.3 ms (#236), so
/// this is about the 1.5 ms of GPU time that no frame's patch has reached, and it is only a
/// bound on a burst.
pub const BUILD_BUDGET: u64 = 1 << 18;
/// The bytes of a box in the pool, and the BLAS's stride: its corners and the two words.
pub const BOX_BYTES: u64 = 32;

/// A BLAS: its level and the coordinate of its cube in the level's grid of groups.
pub type GroupKey = (u8, [i32; 3]);

/// The group of the region at `region` (a region coordinate of its level): its cube of
/// [`GROUP_REGIONS`] regions in a regular grid. A function of the coordinate and of nothing the
/// bricks hold.
pub fn group_of(region: [i32; 3]) -> [i32; 3] {
    region.map(|r| r.div_euclid(GROUP_REGIONS))
}

/// The first Voxel of a group's cube, in Voxels of level 0.
pub fn group_origin(level: u8, group: [i32; 3]) -> [i32; 3] {
    group.map(|g| (g * GROUP_REGIONS * REGION_VOXELS) << level)
}

/// The smallest and largest Voxel (as local coordinates 0..=7) that a brick's 512-bit occupancy
/// mask has set, as `[x, y, z]` corners, or `None` for an empty mask. `mask` is the 16 words of a
/// mixed brick (Voxel `x | y << 3 | z << 6` is bit `i % 32` of word `i / 32`).
pub fn tight_box(mask: &[u32]) -> Option<([u8; 3], [u8; 3])> {
    let mut lo = [u8::MAX; 3];
    let mut hi = [0u8; 3];
    let mut any = false;
    for (w, &word) in mask.iter().take(16).enumerate() {
        let mut bits = word;
        while bits != 0 {
            let b = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let i = w * 32 + b;
            let v = [(i & 7) as u8, (i >> 3 & 7) as u8, (i >> 6) as u8];
            for a in 0..3 {
                lo[a] = lo[a].min(v[a]);
                hi[a] = hi[a].max(v[a]);
            }
            any = true;
        }
    }
    any.then_some((lo, hi))
}

/// The index in `scene.regions` of the region at `key` of `level`, if the scene has one: the
/// lookup of `find_region` in `trace.wgsl`.
pub fn find_region(scene: &Scene, level: usize, key: [i32; 3]) -> Option<usize> {
    let lv: &Level = scene.levels.get(level)?;
    let mask = lv.table_mask as usize;
    let mut slot = hash_region(key) as usize & mask;
    for _ in 0..=mask {
        let at = (lv.table_base as usize + slot) * 4;
        let index = *scene.table.get(at + 3)?;
        if index == 0 {
            return None;
        }
        if scene.table[at..at + 3] == key.map(|k| k as u32) {
            return Some(index as usize - 1);
        }
        slot = (slot + 1) & mask;
    }
    None
}

/// One box of a BLAS: its corners in the BLAS's own frame (Voxels of the level, from the corner
/// of the group's cube), and the two words the shader reads for the candidate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prim {
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Word 0: the brick's coordinate in the group (5 bits an axis; for a whole region, its first
    /// brick's) and [`Prim::REGION`]. Word 1: the box's corner Voxels in the brick, 3 bits an axis
    /// each, min then max (not used for a region).
    pub words: [u32; 2],
}

impl Prim {
    /// Set in word 0 of the box of a region that is one Material.
    pub const REGION: u32 = 0x8000_0000;

    /// The eight words of the box in the pool: its corners as `f32` bits, then its two words.
    pub fn record(&self) -> [u32; 8] {
        let [a, b, c] = self.min.map(f32::to_bits);
        let [d, e, f] = self.max.map(f32::to_bits);
        [a, b, c, d, e, f, self.words[0], self.words[1]]
    }
}

/// How many boxes the region at index `region` of `scene.regions` makes: one for each brick its
/// mask lists, or one if it is one Material.
pub fn region_box_count(scene: &Scene, region: usize) -> u32 {
    let base = region * REGION_WORDS;
    let Some(record) = scene.regions.get(base..base + REGION_WORDS) else {
        return 0;
    };
    if record[REGION_FIRST_REF] & SOLID_FLAG != 0 {
        1
    } else {
        record[..16].iter().map(|w| w.count_ones()).sum()
    }
}

/// The regions of the group at `group` of `level` that the scene lists: their indices in
/// `scene.regions` and where in the group (0..64, as x + 4y + 16z) they are.
fn group_regions(
    scene: &Scene,
    level: usize,
    group: [i32; 3],
) -> impl Iterator<Item = (usize, usize)> {
    let first = group.map(|g| g * GROUP_REGIONS);
    (0..(GROUP_REGIONS * GROUP_REGIONS * GROUP_REGIONS) as usize).filter_map(move |i| {
        let local = [
            i as i32 % GROUP_REGIONS,
            i as i32 / GROUP_REGIONS % GROUP_REGIONS,
            i as i32 / (GROUP_REGIONS * GROUP_REGIONS),
        ];
        let key: [i32; 3] = std::array::from_fn(|a| first[a] + local[a]);
        find_region(scene, level, key).map(|region| (region, i))
    })
}

/// How many boxes the group at `group` of `level` has: what its BLAS is made of. 0 for a group
/// that has no region.
pub fn group_box_count(scene: &Scene, level: usize, group: [i32; 3]) -> u32 {
    group_regions(scene, level, group)
        .map(|(region, _)| region_box_count(scene, region))
        .sum()
}

/// The boxes of the group at `group` of `level`: one for each brick of each of its regions that
/// the scene lists, tight; or one for a region that is one Material. In region and brick order.
/// **What the GPU's [`BoxPass`] makes, box for box.**
pub fn group_prims(scene: &Scene, level: usize, group: [i32; 3]) -> Vec<Prim> {
    let mut out = Vec::new();
    for (region, i) in group_regions(scene, level, group) {
        let local = [
            i as i32 % GROUP_REGIONS,
            i as i32 / GROUP_REGIONS % GROUP_REGIONS,
            i as i32 / (GROUP_REGIONS * GROUP_REGIONS),
        ];
        region_prims(scene, region, local, &mut out);
    }
    out
}

/// Appends the boxes of the region at index `region` of `scene.regions`, `local` regions from the
/// group's corner.
fn region_prims(scene: &Scene, region: usize, local: [i32; 3], out: &mut Vec<Prim>) {
    let base = region * REGION_WORDS;
    let Some(record) = scene.regions.get(base..base + REGION_WORDS) else {
        return;
    };
    let first_ref = record[REGION_FIRST_REF];
    let corner = local.map(|l| l * REGION_BRICKS);
    let pack = |brick: [i32; 3]| brick[0] as u32 | (brick[1] as u32) << 5 | (brick[2] as u32) << 10;
    if first_ref & SOLID_FLAG != 0 {
        let min = corner.map(|c| (c * 8) as f32);
        out.push(Prim {
            min,
            max: min.map(|m| m + REGION_VOXELS as f32),
            words: [pack(corner) | Prim::REGION, 0],
        });
        return;
    }
    let mut rank = 0usize;
    for bit in 0..512usize {
        if record[bit / 32] >> (bit % 32) & 1 == 0 {
            continue;
        }
        // What the GPU does for a word that is not there: it reads a zero.
        let entry = scene
            .brick_refs
            .get(first_ref as usize + rank)
            .copied()
            .unwrap_or(0);
        rank += 1;
        let (lo, hi) = if entry & SOLID_FLAG != 0 {
            ([0u8; 3], [7u8; 3])
        } else {
            let at = entry as usize;
            scene
                .bricks
                .get(at..at + 16)
                .and_then(tight_box)
                .unwrap_or(([0u8; 3], [7u8; 3]))
        };
        let brick = [
            corner[0] + (bit & 7) as i32,
            corner[1] + (bit >> 3 & 7) as i32,
            corner[2] + (bit >> 6) as i32,
        ];
        let at = brick.map(|b| (b * 8) as f32);
        out.push(Prim {
            min: std::array::from_fn(|a| at[a] + f32::from(lo[a])),
            max: std::array::from_fn(|a| at[a] + f32::from(hi[a]) + 1.0),
            words: [
                pack(brick),
                u32::from(lo[0])
                    | u32::from(lo[1]) << 3
                    | u32::from(lo[2]) << 6
                    | u32::from(hi[0]) << 9
                    | u32::from(hi[1]) << 12
                    | u32::from(hi[2]) << 15,
            ],
        });
    }
}

/// Every group that the scene has a region in, with how many boxes it has, found from its tables
/// in one pass: the BLASes a full build makes. The regular grid is the only thing that decides
/// them.
pub fn group_counts(scene: &Scene) -> BTreeMap<GroupKey, u32> {
    let mut groups = BTreeMap::new();
    for (level, lv) in scene.levels.iter().enumerate() {
        for slot in 0..=lv.table_mask as usize {
            let at = (lv.table_base as usize + slot) * 4;
            if let Some(w) = scene.table.get(at..at + 4)
                && w[3] != 0
            {
                let key = group_of([w[0] as i32, w[1] as i32, w[2] as i32]);
                *groups.entry((level as u8, key)).or_insert(0) +=
                    region_box_count(scene, w[3] as usize - 1);
            }
        }
    }
    groups.retain(|_, count| *count > 0);
    groups
}

/// What an update of the acceleration structures did and cost, and what there is now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HwUpdate {
    /// Why: `build` (all of it, after a jump), `patch` (the groups an Edit or a move touched, as
    /// many as the budget let), `anchor` (the camera left the TLAS's frame: instances only) or
    /// `none`.
    pub kind: &'static str,
    /// BLASes built (each from the pool's boxes, made by the GPU) and the boxes in them.
    pub blas_rebuilt: u32,
    pub prims_rebuilt: u64,
    /// Whether the TLAS was built again, and how many instances it has.
    pub tlas_rebuilt: bool,
    pub tlas_instances: u32,
    /// BLASes and boxes there are in all.
    pub blas_total: u32,
    pub prims_total: u64,
    /// Groups that are stale and wait for a later update's budget.
    pub pending: u32,
    /// The frame thread's CPU time of planning the update: picking the groups, the pool's room,
    /// the BLASes that are new and the TLAS's instances. No box is made by the CPU.
    pub prepare: Duration,
    /// Recording the passes and builds and submitting them: the cost of the update to the frame
    /// thread. With [`HwScene::set_wait`] it includes waiting for the GPU to be done, which is
    /// the cost of the builds themselves (the benchmark's Edit stress asks for that; a frame
    /// never waits).
    pub build: Duration,
}

/// One group's job for the box pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxJob {
    pub level: u8,
    pub group: [i32; 3],
    /// Where its boxes start in the output, in boxes.
    pub start: u32,
    /// How many it has: no box is written past them.
    pub count: u32,
}

/// The compute pass that makes the boxes of groups on the GPU, from the scene's buffers (the
/// bind group the passes of the tracer use, as group 0): see `shaders/hw_boxes.wgsl`.
pub struct BoxPass {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    jobs: wgpu::Buffer,
    jobs_capacity: usize,
}

impl BoxPass {
    pub fn new(device: &wgpu::Device, scene_layout: &wgpu::BindGroupLayout) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hardware boxes"),
            entries: &[
                crate::tracer::layout_entry(
                    0,
                    crate::tracer::buffer_binding(crate::tracer::STORAGE_READ),
                ),
                crate::tracer::layout_entry(
                    1,
                    crate::tracer::buffer_binding(wgpu::BufferBindingType::Storage {
                        read_only: false,
                    }),
                ),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hardware boxes"),
            source: wgpu::ShaderSource::Wgsl(crate::shaders::boxes().into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hardware boxes"),
            bind_group_layouts: &[Some(scene_layout), Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hardware boxes"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("box_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            pipeline,
            layout,
            jobs: Self::jobs_buffer(device, 16),
            jobs_capacity: 16,
        }
    }

    fn jobs_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware box jobs"),
            size: capacity as u64 * 32,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Records the pass that writes the boxes of `jobs` into `out` (8 words a box, from each
    /// job's start), reading the scene in `scene_group` as it is when the pass runs.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene_group: &wgpu::BindGroup,
        out: &wgpu::Buffer,
        jobs: &[BoxJob],
    ) {
        if jobs.is_empty() {
            return;
        }
        if jobs.len() > self.jobs_capacity {
            self.jobs_capacity = jobs.len() + jobs.len() / 2;
            self.jobs = Self::jobs_buffer(device, self.jobs_capacity);
        }
        let words: Vec<[i32; 8]> = jobs
            .iter()
            .map(|j| {
                let first = j.group.map(|g| g * GROUP_REGIONS);
                [
                    first[0],
                    first[1],
                    first[2],
                    i32::from(j.level),
                    j.start as i32,
                    j.count as i32,
                    0,
                    0,
                ]
            })
            .collect();
        queue.write_buffer(&self.jobs, 0, bytemuck::cast_slice(&words));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hardware boxes"),
            layout: &self.layout,
            entries: &[
                crate::tracer::buffer_entry(0, &self.jobs),
                crate::tracer::buffer_entry(1, out),
            ],
        });
        // Untimed on purpose: it runs between frames, with the builds, and its cost is the update's
        // (the Edit stress waits for it), not a pass of the frame.
        let descriptor = wgpu::ComputePassDescriptor {
            label: Some("hardware boxes"),
            timestamp_writes: None,
        };
        let mut pass = encoder.begin_compute_pass(&descriptor); // untimed
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, scene_group, &[]);
        pass.set_bind_group(1, &group, &[]);
        // One row of workgroups a job, a workgroup a region of its group: 64. A frame's jobs are
        // a few hundred at the most, a jump's all of the Planet's groups.
        assert!(
            jobs.len() <= device.limits().max_compute_workgroups_per_dimension as usize,
            "more groups than rows of workgroups"
        );
        pass.dispatch_workgroups(
            (GROUP_REGIONS * GROUP_REGIONS * GROUP_REGIONS) as u32,
            jobs.len() as u32,
            1,
        );
    }
}

/// A first-fit allocator over the pool of boxes (in boxes): what is free below `top` is in `free`
/// (ascending, never touching), and above `top` up to `capacity` is all free.
#[derive(Debug, Default)]
struct Pool {
    capacity: u32,
    top: u32,
    free: Vec<Range<u32>>,
}

impl Pool {
    fn alloc(&mut self, n: u32) -> Option<u32> {
        if let Some(i) = self.free.iter().position(|r| r.end - r.start >= n) {
            let start = self.free[i].start;
            self.free[i].start += n;
            if self.free[i].is_empty() {
                self.free.remove(i);
            }
            return Some(start);
        }
        if self.top + n <= self.capacity {
            let start = self.top;
            self.top += n;
            return Some(start);
        }
        None
    }

    fn release(&mut self, range: Range<u32>) {
        if range.is_empty() {
            return;
        }
        let at = self.free.partition_point(|r| r.start < range.start);
        self.free.insert(at, range);
        let mut i = at.saturating_sub(1);
        while i + 1 < self.free.len() {
            if self.free[i].end >= self.free[i + 1].start {
                self.free[i].end = self.free[i].end.max(self.free[i + 1].end);
                self.free.remove(i + 1);
            } else {
                i += 1;
            }
        }
        if let Some(last) = self.free.last()
            && last.end == self.top
        {
            self.top = last.start;
            self.free.pop();
        }
    }

    /// Boxes in use.
    #[cfg(test)]
    fn used(&self) -> u32 {
        self.top - self.free.iter().map(|r| r.end - r.start).sum::<u32>()
    }
}

/// A group on the GPU.
struct Group {
    blas: wgpu::Blas,
    /// Where its boxes are in the pool, and the room it has there (and the BLAS was made for).
    start: u32,
    capacity: u32,
    /// How many boxes its BLAS was last built from.
    count: u32,
    /// Its instance in the TLAS and the instance table.
    slot: u32,
}

/// The acceleration structures of a scene, and what binds them for the shaders (group 2 of the
/// hardware pipelines).
pub struct HwScene {
    layout: wgpu::BindGroupLayout,
    boxes: BoxPass,
    groups: BTreeMap<GroupKey, Group>,
    /// The groups that are stale and not yet built again, with how many boxes each has now (0: it
    /// is gone).
    pending: BTreeMap<GroupKey, u32>,
    /// The boxes of every group, 8 words a box, a group's after another's.
    pool: Pool,
    pool_buffer: wgpu::Buffer,
    /// The slots of the TLAS's instances that are free below `next_slot`, and who has each of the
    /// others.
    free_slots: Vec<u32>,
    next_slot: u32,
    slot_key: Vec<Option<GroupKey>>,
    /// Set when the TLAS or the instance table was made again, or the pool was: the bind group is.
    rebind: bool,
    /// Set when the TLAS's frame moved: its instances have changed and it is to be built again.
    tlas_stale: bool,
    /// The instance table as the GPU has it: two `vec4`s a slot: the group's first Voxel and
    /// level, and where its boxes start in the pool.
    table: Vec<[i32; 8]>,
    tlas: wgpu::Tlas,
    tlas_capacity: u32,
    instances: wgpu::Buffer,
    anchor_buffer: wgpu::Buffer,
    anchor: [i32; 3],
    /// Where the camera was last said to be: what the queue is ordered by.
    camera: [f64; 3],
    bind_group: wgpu::BindGroup,
    budget: u64,
    wait: bool,
    last: HwUpdate,
    /// Every update since the log was last taken ([`HwScene::take_log`]), for the bench's totals.
    log: Vec<HwUpdate>,
}

fn blas_descriptor(count: u32) -> wgpu::BlasAABBGeometrySizeDescriptor {
    wgpu::BlasAABBGeometrySizeDescriptor {
        primitive_count: count,
        flags: wgpu::AccelerationStructureGeometryFlags::OPAQUE,
    }
}

/// The bind group layout of the hardware bindings, group 2: the TLAS, the instance table, the
/// boxes' pool and the anchor.
pub fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let entry = |binding, ty| crate::tracer::layout_entry(binding, ty);
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("hardware ray tracing"),
        entries: &[
            entry(
                0,
                wgpu::BindingType::AccelerationStructure {
                    vertex_return: false,
                },
            ),
            entry(
                1,
                crate::tracer::buffer_binding(crate::tracer::STORAGE_READ),
            ),
            entry(
                2,
                crate::tracer::buffer_binding(crate::tracer::STORAGE_READ),
            ),
            entry(
                3,
                crate::tracer::buffer_binding(wgpu::BufferBindingType::Uniform),
            ),
        ],
    })
}

/// The anchor for a camera at `position` (Voxels): the nearest [`ANCHOR_SNAP`] multiple.
pub fn anchor_for(position: [f64; 3]) -> [i32; 3] {
    position.map(|p| ((p / f64::from(ANCHOR_SNAP)).round() as i32).saturating_mul(ANCHOR_SNAP))
}

/// The BLAS's room for `count` boxes: a little more than it has, so that a group that gains a few
/// is not made again.
fn capacity_for(count: u32) -> u32 {
    count + count / 8 + 8
}

impl HwScene {
    /// Builds the acceleration structures of `scene` (whose buffers are on the GPU, bound as
    /// `scene_group` of `scene_layout`) in a frame anchored near `position`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: wgpu::BindGroupLayout,
        scene_layout: &wgpu::BindGroupLayout,
        scene_group: &wgpu::BindGroup,
        scene: &Scene,
        position: [f64; 3],
    ) -> Result<Self> {
        let anchor = anchor_for(position);
        let tlas = Self::tlas(device, 1);
        let instances = Self::instance_buffer(device, 1);
        let pool_buffer = Self::pool_buffer(device, 1024);
        let anchor_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware anchor"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &anchor_buffer,
            0,
            bytemuck::bytes_of(&[anchor[0], anchor[1], anchor[2], 0]),
        );
        let bind_group = Self::bind(
            device,
            &layout,
            &tlas,
            &instances,
            &pool_buffer,
            &anchor_buffer,
        );
        let mut hw = Self {
            layout,
            boxes: BoxPass::new(device, scene_layout),
            groups: BTreeMap::new(),
            pending: BTreeMap::new(),
            pool: Pool {
                capacity: 1024,
                ..Pool::default()
            },
            pool_buffer,
            free_slots: Vec::new(),
            next_slot: 0,
            slot_key: Vec::new(),
            rebind: false,
            tlas_stale: false,
            table: Vec::new(),
            tlas,
            tlas_capacity: 1,
            instances,
            anchor_buffer,
            anchor,
            camera: position,
            bind_group,
            budget: BUILD_BUDGET,
            wait: false,
            last: HwUpdate::default(),
            log: Vec::new(),
        };
        hw.update(device, queue, scene_group, scene, &SceneUpdate::Rebuilt);
        Ok(hw)
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Every update since the last call, oldest first (the first build is the first of them).
    pub fn take_log(&mut self) -> Vec<HwUpdate> {
        std::mem::take(&mut self.log)
    }

    fn record(&mut self, update: HwUpdate) {
        self.log.push(update.clone());
        self.last = update;
    }

    /// What the last update did and cost.
    pub fn last_update(&self) -> &HwUpdate {
        &self.last
    }

    /// How many groups are stale and wait for a later update's budget.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The most boxes an update builds (see [`BUILD_BUDGET`]), for tests that want a queue.
    pub fn set_budget(&mut self, boxes: u64) {
        self.budget = boxes;
    }

    /// Whether an update waits for the GPU to be done with it, so that its time is the builds'
    /// own: for the benchmark's Edit stress. Off, nothing the frame thread does waits.
    pub fn set_wait(&mut self, wait: bool) {
        self.wait = wait;
    }

    fn tlas(device: &wgpu::Device, capacity: u32) -> wgpu::Tlas {
        device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: Some("tlas"),
            max_instances: capacity,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        })
    }

    fn instance_buffer(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware instances"),
            size: u64::from(capacity) * 32,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn pool_buffer(device: &wgpu::Device, boxes: u32) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware boxes' pool"),
            size: u64::from(boxes) * BOX_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::BLAS_INPUT
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        tlas: &wgpu::Tlas,
        instances: &wgpu::Buffer,
        pool: &wgpu::Buffer,
        anchor: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hardware ray tracing"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: tlas.as_binding(),
                },
                crate::tracer::buffer_entry(1, instances),
                crate::tracer::buffer_entry(2, pool),
                crate::tracer::buffer_entry(3, anchor),
            ],
        })
    }

    /// Brings the acceleration structures up to date with `scene` after `update`, in the commands
    /// of the queue and without waiting: the groups that a rings patch lists are queued, and as
    /// many as the budget allows, nearest the camera first, are built; anything else makes every
    /// group again, all at once. `scene_group` is the scene's buffers as the frame's passes bind
    /// them, which have the update in them.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
        scene: &Scene,
        update: &SceneUpdate,
    ) {
        let started = Instant::now();
        let all = match update {
            SceneUpdate::Rings(patch) => {
                self.pending.extend(patch.groups.iter().copied());
                false
            }
            _ => {
                self.restart(scene);
                true
            }
        };
        if self.pending.is_empty() && !self.tlas_stale {
            self.record(HwUpdate {
                kind: "none",
                blas_total: self.groups.len() as u32,
                tlas_instances: self.groups.len() as u32,
                prims_total: self.prims_total(),
                prepare: started.elapsed(),
                ..HwUpdate::default()
            });
            return;
        }
        self.pump(device, queue, scene_group, started, all);
    }

    /// Builds every group again after a jump or a scene made again: nothing that there was is
    /// right, so it all goes, and the queue is every group the scene has.
    fn restart(&mut self, scene: &Scene) {
        for slot in 0..self.next_slot as usize {
            self.tlas[slot] = None;
        }
        self.groups.clear();
        self.pool.top = 0;
        self.pool.free.clear();
        self.free_slots.clear();
        self.next_slot = 0;
        self.slot_key.clear();
        self.table.clear();
        self.pending = group_counts(scene).into_iter().collect();
    }

    /// Gives back what the group `group` of `slot` held: its room in the pool.
    fn release(&mut self, group: &Group) {
        self.pool.release(group.start..group.start + group.capacity);
    }

    /// How far the group at `key` is from the camera, squared, in Voxels of level 0.
    fn distance2(&self, key: GroupKey) -> f64 {
        let origin = group_origin(key.0, key.1);
        let half = f64::from(GROUP_REGIONS * REGION_VOXELS / 2) * f64::from(1u32 << key.0);
        (0..3)
            .map(|a| {
                let d = f64::from(origin[a]) + half - self.camera[a];
                d * d
            })
            .sum()
    }

    /// Builds the stale groups that the budget allows (all of them if `all`), and the TLAS.
    fn pump(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
        started: Instant,
        all: bool,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("acceleration structures"),
        });
        // The slots whose instance changed, and the groups to build.
        let mut touched: Vec<u32> = Vec::new();
        // The groups that are gone are dropped at once: it costs nothing, and a group with no box
        // has nothing to trace.
        let gone: Vec<GroupKey> = self
            .pending
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(key, _)| *key)
            .collect();
        for key in gone {
            self.pending.remove(&key);
            if let Some(group) = self.groups.remove(&key) {
                self.release(&group);
                self.slot_key[group.slot as usize] = None;
                self.free_slots.push(group.slot);
                touched.push(group.slot);
            }
        }
        // The rest, nearest the camera first, until the budget is spent.
        let mut order: Vec<(f64, GroupKey, u32)> = self
            .pending
            .iter()
            .map(|(&key, &count)| (self.distance2(key), key, count))
            .collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let budget = if all { u64::MAX } else { self.budget };
        let mut spent = 0u64;
        let mut jobs: Vec<BoxJob> = Vec::new();
        for (_, key, count) in order {
            if !jobs.is_empty() && spent + u64::from(count) > budget {
                break;
            }
            spent += u64::from(count);
            self.pending.remove(&key);
            // The BLAS and the room it has are kept if the boxes still fit: its old boxes are
            // given back first, so that it is built where it was.
            let (blas, slot, capacity) = match self.groups.remove(&key) {
                Some(old) if old.capacity >= count => {
                    self.release(&old);
                    (old.blas, old.slot, old.capacity)
                }
                old => {
                    let slot = old.map(|old| {
                        self.release(&old);
                        old.slot
                    });
                    let slot = slot.unwrap_or_else(|| {
                        self.free_slots.pop().unwrap_or_else(|| {
                            self.next_slot += 1;
                            self.next_slot - 1
                        })
                    });
                    let capacity = capacity_for(count);
                    (Self::blas(device, capacity), slot, capacity)
                }
            };
            let start = loop {
                if let Some(start) = self.pool.alloc(capacity) {
                    break start;
                }
                self.grow_pool(device, &mut encoder, capacity);
            };
            if self.slot_key.len() <= slot as usize {
                self.slot_key.resize(slot as usize + 1, None);
            }
            self.slot_key[slot as usize] = Some(key);
            touched.push(slot);
            jobs.push(BoxJob {
                level: key.0,
                group: key.1,
                start,
                count,
            });
            self.groups.insert(
                key,
                Group {
                    blas,
                    start,
                    capacity,
                    count,
                    slot,
                },
            );
        }
        self.sync_instances(device, queue, touched);
        let prepare = started.elapsed();
        let submitting = Instant::now();
        let kind = if jobs.is_empty() { "anchor" } else { "patch" };
        self.boxes.encode(
            device,
            queue,
            &mut encoder,
            scene_group,
            &self.pool_buffer,
            &jobs,
        );
        let sizes: Vec<wgpu::BlasAABBGeometrySizeDescriptor> =
            jobs.iter().map(|job| blas_descriptor(job.count)).collect();
        let entries: Vec<wgpu::BlasBuildEntry<'_>> = jobs
            .iter()
            .zip(&sizes)
            .map(|(job, size)| {
                let group = &self.groups[&(job.level, job.group)];
                wgpu::BlasBuildEntry {
                    blas: &group.blas,
                    geometry: wgpu::BlasGeometries::AabbGeometries(vec![wgpu::BlasAabbGeometry {
                        size,
                        stride: BOX_BYTES,
                        aabb_buffer: &self.pool_buffer,
                        primitive_offset: (u64::from(group.start) * BOX_BYTES) as u32,
                    }]),
                }
            })
            .collect();
        encoder.build_acceleration_structures(entries.iter(), std::iter::once(&self.tlas));
        queue.submit(std::iter::once(encoder.finish()));
        if self.wait {
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("device poll failed");
        }
        let build = submitting.elapsed();
        self.tlas_stale = false;
        self.record(HwUpdate {
            kind: if all { "build" } else { kind },
            blas_rebuilt: jobs.len() as u32,
            prims_rebuilt: jobs.iter().map(|j| u64::from(j.count)).sum(),
            tlas_rebuilt: true,
            tlas_instances: self.groups.len() as u32,
            blas_total: self.groups.len() as u32,
            prims_total: self.prims_total(),
            pending: self.pending.len() as u32,
            prepare,
            build,
        });
    }

    fn prims_total(&self) -> u64 {
        self.groups.values().map(|g| u64::from(g.count)).sum()
    }

    fn blas(device: &wgpu::Device, capacity: u32) -> wgpu::Blas {
        device.create_blas(
            &wgpu::CreateBlasDescriptor {
                label: Some("brick boxes"),
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            },
            wgpu::BlasGeometrySizeDescriptors::AABBs {
                descriptors: vec![blas_descriptor(capacity)],
            },
        )
    }

    /// Makes the pool bigger (at least `at_least` boxes more than what is in use), with the boxes
    /// that it has: copied on the GPU, in `encoder`, before anything in it is written.
    fn grow_pool(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        at_least: u32,
    ) {
        let capacity = (self.pool.capacity * 2)
            .max(self.pool.top + at_least)
            .max(1024);
        let bigger = Self::pool_buffer(device, capacity);
        if self.pool.top > 0 {
            encoder.copy_buffer_to_buffer(
                &self.pool_buffer,
                0,
                &bigger,
                0,
                u64::from(self.pool.top) * BOX_BYTES,
            );
        }
        self.pool_buffer = bigger;
        self.pool.capacity = capacity;
        self.rebind = true;
    }

    /// Writes the TLAS's instances and the instance table of the `touched` slots: what each says
    /// is the group it has now, or nothing. If the TLAS is too small for the slots there are, it
    /// is made again, with every instance.
    fn sync_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mut touched: Vec<u32>,
    ) {
        let slots = self.next_slot.max(1);
        if slots > self.tlas_capacity {
            self.tlas_capacity = slots + slots / 2 + 16;
            self.tlas = Self::tlas(device, self.tlas_capacity);
            self.instances = Self::instance_buffer(device, self.tlas_capacity);
            self.rebind = true;
            touched = (0..self.next_slot).collect();
        }
        self.table.resize(self.next_slot as usize, [0; 8]);
        let (mut first, mut last) = (usize::MAX, 0usize);
        for slot in touched {
            let at = slot as usize;
            first = first.min(at);
            last = last.max(at);
            match self.slot_key.get(at).copied().flatten() {
                Some(key) => {
                    let group = &self.groups[&key];
                    let origin = group_origin(key.0, key.1);
                    let s = (1u32 << key.0) as f32;
                    let t: [f32; 3] = std::array::from_fn(|a| (origin[a] - self.anchor[a]) as f32);
                    self.tlas[at] = Some(wgpu::TlasInstance::new(
                        &group.blas,
                        [s, 0.0, 0.0, t[0], 0.0, s, 0.0, t[1], 0.0, 0.0, s, t[2]],
                        slot,
                        0xff,
                    ));
                    self.table[at] = [
                        origin[0],
                        origin[1],
                        origin[2],
                        i32::from(key.0),
                        group.start as i32,
                        0,
                        0,
                        0,
                    ];
                }
                None => {
                    self.tlas[at] = None;
                    self.table[at] = [0; 8];
                }
            }
        }
        if first <= last && last < self.table.len() {
            queue.write_buffer(
                &self.instances,
                first as u64 * 32,
                bytemuck::cast_slice(&self.table[first..=last]),
            );
        }
        if self.rebind {
            self.bind_group = Self::bind(
                device,
                &self.layout,
                &self.tlas,
                &self.instances,
                &self.pool_buffer,
                &self.anchor_buffer,
            );
            self.rebind = false;
        }
    }

    /// Takes in the camera's position, from a frame that is about to draw: if it left the TLAS's
    /// frame, moves the frame (the instances' transforms change, the BLASes do not); and builds
    /// the next of the stale groups that wait for their turn. Nothing here waits for the GPU.
    pub fn follow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
        position: [f64; 3],
    ) {
        self.camera = position;
        let anchor = anchor_for(position);
        // Hysteresis of a snap: within 1.5 snaps of the frame's origin is as good as at it.
        let far = (0..3).any(|a| {
            (position[a] - f64::from(self.anchor[a])).abs() > f64::from(ANCHOR_SNAP) * 1.5
        });
        if anchor != self.anchor && far {
            self.anchor = anchor;
            queue.write_buffer(
                &self.anchor_buffer,
                0,
                bytemuck::bytes_of(&[anchor[0], anchor[1], anchor[2], 0]),
            );
            // Every group's transform is in the frame of the anchor: written again, at the
            // pump's sync, which the queued groups share.
            let started = Instant::now();
            self.tlas_stale = true;
            let everyone: Vec<u32> = (0..self.next_slot).collect();
            self.sync_instances(device, queue, everyone);
            self.pump(device, queue, scene_group, started, false);
        } else if !self.pending.is_empty() {
            self.pump(device, queue, scene_group, Instant::now(), false);
        }
    }

    /// Builds every group that is stale, whatever the budget: for a test that wants the
    /// structures to be as the scene is, and a benchmark's settle.
    pub fn flush(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
    ) {
        if !self.pending.is_empty() {
            self.pump(device, queue, scene_group, Instant::now(), true);
            // That was no jump: it says so in the log.
            if let Some(last) = self.log.last_mut() {
                last.kind = "patch";
            }
            self.last.kind = "patch";
        }
    }

    /// The frame's origin, in Voxels of level 0.
    pub fn anchor(&self) -> [i32; 3] {
        self.anchor
    }

    /// The groups that are built and how many boxes each was built from: for tests.
    pub fn built(&self) -> BTreeMap<GroupKey, u32> {
        self.groups.iter().map(|(&k, g)| (k, g.count)).collect()
    }

    /// The boxes of every built group as the GPU has them, read back (waits): for tests.
    pub fn read_boxes(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> BTreeMap<GroupKey, Vec<[u32; 8]>> {
        let words = read_back(
            device,
            queue,
            &self.pool_buffer,
            u64::from(self.pool.top) * BOX_BYTES,
        );
        self.groups
            .iter()
            .map(|(&key, group)| {
                let boxes = (0..group.count as usize)
                    .map(|i| {
                        let at = (group.start as usize + i) * 8;
                        std::array::from_fn(|w| words[at + w])
                    })
                    .collect();
                (key, boxes)
            })
            .collect()
    }
}

/// The first `bytes` of `buffer` (which has `COPY_SRC`), read back as words: waits.
fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    bytes: u64,
) -> Vec<u32> {
    if bytes == 0 {
        return Vec::new();
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hardware read back"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("hardware read back"),
    });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, bytes);
    queue.submit(std::iter::once(encoder.finish()));
    staging.slice(..).map_async(wgpu::MapMode::Read, |result| {
        result.expect("map the read back");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll failed");
    let words = bytemuck::cast_slice(
        &staging
            .slice(..)
            .get_mapped_range()
            .expect("the read back is mapped"),
    )
    .to_vec();
    staging.unmap();
    words
}

impl BoxPass {
    /// The boxes of the groups of `jobs` as the GPU makes them, read back (waits): what the tests
    /// compare with [`group_prims`]. Each job's boxes are laid out after the last's.
    pub fn make(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
        jobs: &[(u8, [i32; 3], u32)],
    ) -> Vec<Vec<[u32; 8]>> {
        let mut at = 0u32;
        let jobs: Vec<BoxJob> = jobs
            .iter()
            .map(|&(level, group, count)| {
                let job = BoxJob {
                    level,
                    group,
                    start: at,
                    count,
                };
                at += count;
                job
            })
            .collect();
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware boxes' test output"),
            size: u64::from(at.max(1)) * BOX_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hardware boxes' test"),
        });
        self.encode(device, queue, &mut encoder, scene_group, &out, &jobs);
        queue.submit(std::iter::once(encoder.finish()));
        let words = read_back(device, queue, &out, u64::from(at) * BOX_BYTES);
        jobs.iter()
            .map(|job| {
                (0..job.count as usize)
                    .map(|i| {
                        let base = (job.start as usize + i) * 8;
                        std::array::from_fn(|w| words[base + w])
                    })
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
