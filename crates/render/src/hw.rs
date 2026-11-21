//! The hardware ray-tracing backend behind `trace(ray) -> Hit` (#50, a spike).
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
//!   bricks are not listed.
//! * **Regular BLAS grouping** ([`group_of`]): every level's bricks go into BLASes by
//!   [`GROUP_REGIONS`]-region cubes of the level's region grid, a function of the region's
//!   coordinate alone: never of what is in it, so an Edit changes the contents of a BLAS and not
//!   which BLAS a brick is in, and the TLAS keeps its shape.
//! * Each BLAS is one TLAS instance with a translation and a scale of 2^level, so a box is in
//!   level Voxels in the BLAS (0..256) whatever the level; the instance's custom data indexes the
//!   instance table the shader reads (group origin, level, where its boxes' words start).
//! * The TLAS is in a frame whose origin is [`ANCHOR_SNAP`]-aligned and near the camera, so that
//!   its `f32`s are fine near the camera whatever the Planet's coordinates are; it is rebuilt
//!   (instances only; the BLASes are as they were) when the camera leaves it.
//!
//! What the spike does **not** do: the compute path's clip to the ball that has the Planet in it
//! (nothing is outside it, so it only saves steps), compaction, update-in-place of a BLAS (an Edit rebuilds the BLASes of the groups it touched),
//! or a TLAS built in a pass of the frame (an update waits for the GPU, which is how it is timed).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use wgpu::util::DeviceExt;

use crate::Result;
use crate::scene::{
    Level, REGION_BRICKS, REGION_FIRST_REF, REGION_VOXELS, REGION_WORDS, RingPatch, SOLID_FLAG,
    Scene, SceneUpdate, hash_region,
};

/// Regions along each edge of a BLAS's cube: 4 x 4 x 4 regions, 32 bricks and 256 Voxels of its
/// level across. A boxes' coordinates in a BLAS are then 0..=256, exact in `f32`.
pub const GROUP_REGIONS: i32 = 4;
/// Bricks along each edge of a BLAS's cube.
pub const GROUP_BRICKS: i32 = GROUP_REGIONS * REGION_BRICKS;
/// The TLAS's origin is a multiple of this many Voxels of level 0, nearest to the camera.
pub const ANCHOR_SNAP: i32 = 4096;

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
}

/// The boxes of the group at `group` of `level`: one for each brick of each of its regions that
/// the scene lists, tight; or one for a region that is one Material. In region and brick order.
pub fn group_prims(scene: &Scene, level: usize, group: [i32; 3]) -> Vec<Prim> {
    let mut out = Vec::new();
    let first = group.map(|g| g * GROUP_REGIONS);
    for z in 0..GROUP_REGIONS {
        for y in 0..GROUP_REGIONS {
            for x in 0..GROUP_REGIONS {
                let local = [x, y, z];
                let key: [i32; 3] = std::array::from_fn(|a| first[a] + local[a]);
                let Some(region) = find_region(scene, level, key) else {
                    continue;
                };
                region_prims(scene, region, local, &mut out);
            }
        }
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
        let entry = scene.brick_refs.get(first_ref as usize + rank).copied();
        rank += 1;
        let Some(entry) = entry else { continue };
        let (lo, hi) = if entry & SOLID_FLAG != 0 {
            ([0u8; 3], [7u8; 3])
        } else {
            let at = entry as usize;
            match scene.bricks.get(at..at + 16).and_then(tight_box) {
                Some(b) => b,
                None => continue,
            }
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

/// Every group that the scene has a region in, found from its tables: the BLASes a full build
/// makes. The regular grid is the only thing that decides them.
pub fn all_groups(scene: &Scene) -> BTreeSet<GroupKey> {
    let mut groups = BTreeSet::new();
    for (level, lv) in scene.levels.iter().enumerate() {
        for slot in 0..=lv.table_mask as usize {
            let at = (lv.table_base as usize + slot) * 4;
            if let Some(w) = scene.table.get(at..at + 4)
                && w[3] != 0
            {
                groups.insert((
                    level as u8,
                    group_of([w[0] as i32, w[1] as i32, w[2] as i32]),
                ));
            }
        }
    }
    groups
}

/// What the last update of the acceleration structures did and cost, and what there is now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HwUpdate {
    /// Why: `build` (all of it), `patch` (the groups an Edit or a move touched), `anchor` (the
    /// camera left the TLAS's frame: instances only) or `none`.
    pub kind: &'static str,
    /// BLASes rebuilt (each with a new box buffer) and the boxes in them.
    pub blas_rebuilt: u32,
    pub prims_rebuilt: u64,
    /// Whether the TLAS was built again, and how many instances it has.
    pub tlas_rebuilt: bool,
    pub tlas_instances: u32,
    /// BLASes and boxes there are in all.
    pub blas_total: u32,
    pub prims_total: u64,
    /// CPU time of working out the boxes and making the buffers and BLASes.
    pub prepare: Duration,
    /// Recording the builds, submitting them and waiting for the GPU to be done: the cost of
    /// the acceleration structures' build as the frame would see it, apart from the CPU's.
    pub build: Duration,
}

/// Where the regions and table slots of a scene of rings are (by the level and key they hold), as
/// of the last update: what turns the indices that a [`RingPatch`] lists back into the groups they
/// are in. CPU only.
#[derive(Default)]
pub struct RegionIndex {
    slot_key: HashMap<usize, (u8, [i32; 3])>,
    region_key: HashMap<usize, (u8, [i32; 3])>,
}

impl RegionIndex {
    /// The index of every region and table slot that `scene` has.
    pub fn of(scene: &Scene) -> Self {
        let mut index = Self::default();
        for (level, lv) in scene.levels.iter().enumerate() {
            for slot in 0..=lv.table_mask as usize {
                let global = lv.table_base as usize + slot;
                if let Some(w) = scene.table.get(global * 4..global * 4 + 4)
                    && w[3] != 0
                {
                    let key = (level as u8, [w[0] as i32, w[1] as i32, w[2] as i32]);
                    index.slot_key.insert(global, key);
                    index.region_key.insert(w[3] as usize - 1, key);
                }
            }
        }
        index
    }

    /// The groups whose boxes `patch` made stale in `scene` (which it was made for), and the index
    /// brought up to date with it.
    pub fn dirty(&mut self, scene: &Scene, patch: &RingPatch) -> BTreeSet<GroupKey> {
        let mut dirty = BTreeSet::new();
        let mark = |dirty: &mut BTreeSet<GroupKey>, (level, key): (u8, [i32; 3])| {
            dirty.insert((level, group_of(key)));
        };
        for &slot in &patch.table {
            if let Some(&old) = self.slot_key.get(&slot) {
                mark(&mut dirty, old);
            }
            let w = scene.table.get(slot * 4..slot * 4 + 4);
            match w {
                Some(w) if w[3] != 0 => {
                    let level = scene
                        .levels
                        .iter()
                        .position(|lv| {
                            (lv.table_base as usize..=(lv.table_base + lv.table_mask) as usize)
                                .contains(&slot)
                        })
                        .unwrap_or(0) as u8;
                    let key = (level, [w[0] as i32, w[1] as i32, w[2] as i32]);
                    mark(&mut dirty, key);
                    self.slot_key.insert(slot, key);
                    self.region_key.insert(w[3] as usize - 1, key);
                }
                _ => {
                    self.slot_key.remove(&slot);
                }
            }
        }
        for &region in &patch.regions {
            if let Some(&key) = self.region_key.get(&region) {
                mark(&mut dirty, key);
            }
        }
        for &key in &patch.rewritten {
            mark(&mut dirty, (0, key));
        }
        // The coarse levels' (their words are rewritten in place just the same, and the box that
        // was made from them is stale).
        for &(level, key) in &patch.rewritten_coarse {
            mark(&mut dirty, (level as u8, key));
        }
        // Entries of `brick_refs` written in place: the region they belong to has them from its
        // `first_ref`, for as many bricks as its mask has.
        if !patch.brick_refs.is_empty() {
            let mut owners: Vec<(u32, usize)> = self
                .region_key
                .keys()
                .filter_map(|&r| {
                    let first = *scene.regions.get(r * REGION_WORDS + REGION_FIRST_REF)?;
                    (first & SOLID_FLAG == 0).then_some((first, r))
                })
                .collect();
            owners.sort_unstable();
            for range in &patch.brick_refs {
                let end = owners.partition_point(|&(first, _)| (first as usize) < range.end);
                for &(first, r) in owners[..end].iter().rev() {
                    if first as usize + 512 <= range.start {
                        break;
                    }
                    let count: u32 = scene.regions[r * REGION_WORDS..][..16]
                        .iter()
                        .map(|w| w.count_ones())
                        .sum();
                    if first as usize + count as usize > range.start {
                        mark(&mut dirty, self.region_key[&r]);
                    }
                }
            }
        }
        dirty
    }
}

/// A group on the GPU.
struct Group {
    blas: wgpu::Blas,
    /// Kept alive: the BLAS was built from it.
    _boxes: wgpu::Buffer,
    /// The two words of each box (see [`Prim::words`]); these are in `HwScene::words` at
    /// `base` and are kept to write them again when that buffer grows.
    words: Vec<u32>,
    base: u32,
}

/// The acceleration structures of a scene, and what binds them for the shaders (group 2 of the
/// hardware pipelines).
pub struct HwScene {
    layout: wgpu::BindGroupLayout,
    groups: BTreeMap<GroupKey, Group>,
    tlas: wgpu::Tlas,
    tlas_capacity: u32,
    /// The instance table: two `vec4`s an instance: the group's first Voxel and level, and where
    /// its boxes' words start.
    instances: wgpu::Buffer,
    instances_capacity: u32,
    /// The boxes' words, two a box, a group's after another's.
    words: wgpu::Buffer,
    words_capacity: usize,
    words_used: usize,
    anchor_buffer: wgpu::Buffer,
    anchor: [i32; 3],
    bind_group: wgpu::BindGroup,
    /// Where the scene's regions and table slots are, to find the groups a patch touches.
    index: RegionIndex,
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
/// boxes' words and the anchor.
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

impl HwScene {
    /// Builds the acceleration structures of `scene` in a frame anchored near `position`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: wgpu::BindGroupLayout,
        scene: &Scene,
        position: [f64; 3],
    ) -> Result<Self> {
        let anchor = anchor_for(position);
        let tlas = device.create_tlas(&wgpu::CreateTlasDescriptor {
            label: Some("tlas"),
            max_instances: 1,
            flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
            update_mode: wgpu::AccelerationStructureUpdateMode::Build,
        });
        let (instances, words, anchor_buffer) = (
            Self::instance_buffer(device, 1),
            Self::words_buffer(device, 2),
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("hardware anchor"),
                contents: bytemuck::bytes_of(&[anchor[0], anchor[1], anchor[2], 0]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            }),
        );
        let bind_group = Self::bind(device, &layout, &tlas, &instances, &words, &anchor_buffer);
        let mut hw = Self {
            layout,
            groups: BTreeMap::new(),
            tlas,
            tlas_capacity: 1,
            instances,
            instances_capacity: 1,
            words,
            words_capacity: 2,
            words_used: 0,
            anchor_buffer,
            anchor,
            bind_group,
            index: RegionIndex::default(),
            last: HwUpdate::default(),
            log: Vec::new(),
        };
        hw.rebuild_all(device, queue, scene, "build");
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

    fn instance_buffer(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware instances"),
            size: u64::from(capacity) * 32,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn words_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hardware boxes' words"),
            size: capacity as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        tlas: &wgpu::Tlas,
        instances: &wgpu::Buffer,
        words: &wgpu::Buffer,
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
                crate::tracer::buffer_entry(2, words),
                crate::tracer::buffer_entry(3, anchor),
            ],
        })
    }

    /// Makes the group `key` from `scene`: its boxes and BLAS (to be built). `None` for a group
    /// with no box.
    fn make_group(
        device: &wgpu::Device,
        scene: &Scene,
        key: GroupKey,
    ) -> Option<(Group, Vec<Prim>)> {
        let prims = group_prims(scene, key.0 as usize, key.1);
        if prims.is_empty() {
            return None;
        }
        // 32 bytes a box: min, max and the two words that the shader reads: the hardware reads the
        // first 24, and the stride of 32 is a multiple of 8.
        let mut bytes: Vec<u32> = Vec::with_capacity(prims.len() * 8);
        for p in &prims {
            bytes.extend(p.min.map(f32::to_bits));
            bytes.extend(p.max.map(f32::to_bits));
            bytes.extend(p.words);
        }
        let boxes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("hardware boxes"),
            contents: bytemuck::cast_slice(&bytes),
            usage: wgpu::BufferUsages::BLAS_INPUT,
        });
        let blas = device.create_blas(
            &wgpu::CreateBlasDescriptor {
                label: Some("brick boxes"),
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            },
            wgpu::BlasGeometrySizeDescriptors::AABBs {
                descriptors: vec![blas_descriptor(prims.len() as u32)],
            },
        );
        let words = prims.iter().flat_map(|p| p.words).collect();
        Some((
            Group {
                blas,
                _boxes: boxes,
                words,
                base: 0,
            },
            prims,
        ))
    }

    /// Builds every group again.
    fn rebuild_all(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        kind: &'static str,
    ) {
        self.groups.clear();
        self.words_used = 0;
        let keys = all_groups(scene);
        self.refresh(device, queue, scene, &keys, kind, Duration::ZERO);
        self.index = RegionIndex::of(scene);
    }

    /// Rebuilds the BLASes of `keys` (taking out those with no box now), and the TLAS.
    fn refresh(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        keys: &BTreeSet<GroupKey>,
        kind: &'static str,
        searched: Duration,
    ) {
        let prepared = Instant::now();
        let mut made = Vec::new();
        let mut prims_rebuilt = 0u64;
        for &key in keys {
            match Self::make_group(device, scene, key) {
                Some((group, prims)) => {
                    prims_rebuilt += prims.len() as u64;
                    made.push((key, group));
                }
                None => {
                    self.groups.remove(&key);
                }
            }
        }
        // Room for the new words: after the others' (an old group's words are left where they
        // are, unused, until the buffer is full and everything is packed again).
        let needed: usize = made.iter().map(|(_, g)| g.words.len()).sum();
        let mut order: Vec<GroupKey> = Vec::new();
        if self.words_used + needed > self.words_capacity {
            let live: Vec<GroupKey> = self
                .groups
                .keys()
                .copied()
                .filter(|k| !keys.contains(k))
                .collect();
            let live_words: usize = live.iter().map(|k| self.groups[k].words.len()).sum();
            self.words_capacity = ((live_words + needed) * 3 / 2).max(2);
            self.words = Self::words_buffer(device, self.words_capacity);
            self.words_used = 0;
            order = live;
        }
        for (key, group) in made {
            self.groups.insert(key, group);
            order.push(key);
        }
        for key in order {
            let group = self.groups.get_mut(&key).expect("group is there");
            group.base = (self.words_used / 2) as u32;
            queue.write_buffer(
                &self.words,
                self.words_used as u64 * 4,
                bytemuck::cast_slice(&group.words),
            );
            self.words_used += group.words.len();
        }
        // The search for the groups to make again is part of making them.
        let prepare = searched + prepared.elapsed();
        let built = Instant::now();
        let rebuilt = self.build(device, queue, keys);
        let build = built.elapsed();
        self.record(HwUpdate {
            kind,
            blas_rebuilt: rebuilt,
            prims_rebuilt,
            tlas_rebuilt: true,
            tlas_instances: self.groups.len() as u32,
            blas_total: self.groups.len() as u32,
            prims_total: self
                .groups
                .values()
                .map(|g| (g.words.len() / 2) as u64)
                .sum(),
            prepare,
            build,
        });
    }

    /// Builds the BLASes of `keys` and the TLAS over every group, and waits for it. Returns how
    /// many BLASes were built.
    fn build(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        keys: &BTreeSet<GroupKey>,
    ) -> u32 {
        let count = self.groups.len() as u32;
        if count > self.tlas_capacity {
            self.tlas_capacity = count + count / 2 + 16;
            self.tlas = device.create_tlas(&wgpu::CreateTlasDescriptor {
                label: Some("tlas"),
                max_instances: self.tlas_capacity,
                flags: wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            });
        }
        if count > self.instances_capacity {
            self.instances_capacity = self.tlas_capacity;
            self.instances = Self::instance_buffer(device, self.instances_capacity);
        }
        self.bind_group = Self::bind(
            device,
            &self.layout,
            &self.tlas,
            &self.instances,
            &self.words,
            &self.anchor_buffer,
        );
        self.fill_instances(queue);
        // One geometry each; the descriptors and the entries borrow what they describe.
        let sizes: Vec<(GroupKey, wgpu::BlasAABBGeometrySizeDescriptor)> = keys
            .iter()
            .filter_map(|k| {
                self.groups
                    .get(k)
                    .map(|g| (*k, blas_descriptor((g.words.len() / 2) as u32)))
            })
            .collect();
        let entries: Vec<wgpu::BlasBuildEntry<'_>> = sizes
            .iter()
            .map(|(k, size)| {
                let group = &self.groups[k];
                wgpu::BlasBuildEntry {
                    blas: &group.blas,
                    geometry: wgpu::BlasGeometries::AabbGeometries(vec![wgpu::BlasAabbGeometry {
                        size,
                        stride: 32,
                        aabb_buffer: &group._boxes,
                        primitive_offset: 0,
                    }]),
                }
            })
            .collect();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("acceleration structures"),
        });
        encoder.build_acceleration_structures(entries.iter(), std::iter::once(&self.tlas));
        queue.submit(std::iter::once(encoder.finish()));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");
        entries.len() as u32
    }

    /// Writes the TLAS's instances (one a group, `custom data` its index) and their table.
    fn fill_instances(&mut self, queue: &wgpu::Queue) {
        let mut table: Vec<[i32; 8]> = Vec::with_capacity(self.groups.len());
        for (i, ((level, group), g)) in self.groups.iter().enumerate() {
            let origin = group_origin(*level, *group);
            let s = (1u32 << level) as f32;
            let t: [f32; 3] = std::array::from_fn(|a| (origin[a] - self.anchor[a]) as f32);
            self.tlas[i] = Some(wgpu::TlasInstance::new(
                &g.blas,
                [s, 0.0, 0.0, t[0], 0.0, s, 0.0, t[1], 0.0, 0.0, s, t[2]],
                i as u32,
                0xff,
            ));
            table.push([
                origin[0],
                origin[1],
                origin[2],
                i32::from(*level),
                g.base as i32,
                0,
                0,
                0,
            ]);
        }
        for i in self.groups.len()..self.tlas_capacity as usize {
            self.tlas[i] = None;
        }
        if !table.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&table));
        }
        queue.write_buffer(
            &self.anchor_buffer,
            0,
            bytemuck::bytes_of(&[self.anchor[0], self.anchor[1], self.anchor[2], 0]),
        );
    }

    /// The groups that `update` changed `scene` in: `None` for all of them.
    pub fn dirty_groups(
        &mut self,
        scene: &Scene,
        update: &SceneUpdate,
    ) -> Option<BTreeSet<GroupKey>> {
        let SceneUpdate::Rings(patch) = update else {
            return None;
        };
        Some(self.index.dirty(scene, patch))
    }

    /// Brings the acceleration structures up to date with `scene` after `update`: only the groups
    /// the update touched are built again (a rings patch), or all of them (anything else).
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        update: &SceneUpdate,
    ) {
        let searching = Instant::now();
        let dirty = self.dirty_groups(scene, update);
        let searched = searching.elapsed();
        match dirty {
            Some(dirty) if dirty.is_empty() => {
                self.record(HwUpdate {
                    kind: "none",
                    blas_total: self.groups.len() as u32,
                    tlas_instances: self.groups.len() as u32,
                    prims_total: self.last.prims_total,
                    prepare: searched,
                    ..HwUpdate::default()
                });
            }
            Some(dirty) => self.refresh(device, queue, scene, &dirty, "patch", searched),
            None => self.rebuild_all(device, queue, scene, "build"),
        }
    }

    /// Moves the TLAS's frame to the camera's if it left it: the instances' transforms change,
    /// the BLASes do not.
    pub fn follow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, position: [f64; 3]) {
        let anchor = anchor_for(position);
        // Hysteresis of a snap: within 1.5 snaps of the frame's origin is as good as at it.
        let far = (0..3).any(|a| {
            (position[a] - f64::from(self.anchor[a])).abs() > f64::from(ANCHOR_SNAP) * 1.5
        });
        if anchor == self.anchor || !far {
            return;
        }
        self.anchor = anchor;
        let built = Instant::now();
        self.fill_instances(queue);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tlas"),
        });
        encoder.build_acceleration_structures(std::iter::empty(), std::iter::once(&self.tlas));
        queue.submit(std::iter::once(encoder.finish()));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");
        self.record(HwUpdate {
            kind: "anchor",
            tlas_rebuilt: true,
            tlas_instances: self.groups.len() as u32,
            blas_total: self.groups.len() as u32,
            prims_total: self.last.prims_total,
            build: built.elapsed(),
            ..HwUpdate::default()
        });
    }

    /// The frame's origin, in Voxels of level 0.
    pub fn anchor(&self) -> [i32; 3] {
        self.anchor
    }
}

#[cfg(test)]
mod tests;
