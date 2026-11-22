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
//! * **Regular BLAS grouping** ([`group_of`], [`Grouping`]): every level's bricks go into
//!   **cells** of [`GROUP_REGIONS`]-region cubes of the level's region grid (what the scene thread
//!   lists and counts), and the cells into BLASes of 1, 2 or 4 cells along an edge (4, 8 or 16
//!   regions; one cell by default, `--hw-group-regions` in the benchmark), both functions of the
//!   region's coordinate alone: never of what is in it, so an Edit changes the contents of a BLAS
//!   and not which BLAS a brick is in, and the TLAS keeps its shape. A BLAS is built again whole
//!   when any of its cells is stale, so a bigger one costs the GPU more boxes and the frame thread
//!   fewer builds (a build costs it per BLAS, and a submit that traces per instance of the TLAS).
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
//! * **Builds are budgeted** ([`BUILD_BUDGET`] boxes a frame, counted over every build of the
//!   frame, whichever draw or update makes it), but only for the groups that **only lost**
//!   ([`RingPatch::grown`]): a group whose every box now lies inside a box it had may wait for
//!   its turn, nearest the camera first, and then traces the BLAS it had, whose boxes cover
//!   everything that is left of it: a ray enters a box that has nothing in it any more and
//!   finds nothing there, which is what the group's own rebuild would do, a few frames later. A
//!   group that **gained** (a brick that is new, a brick with a Voxel where it had none, a region
//!   that is new) or that has no BLAS yet is built in the frame it is listed in, whatever the
//!   budget, since a ray cannot hit what no box covers: that is the whole of "no hole". A group
//!   that has waited [`MAX_WAIT_FRAMES`] frames is built whatever the budget. A jump, which makes
//!   every group again, builds them all at once: there is nothing right to show until it has.
//!
//! What this does **not** do: the compute path's clip to the ball that has the Planet in it
//! (nothing is outside it, so it only saves steps), compaction, or update-in-place of a BLAS (a
//! group's BLAS is built again whole, from its new boxes).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::Result;
use crate::scene::{
    Level, REGION_BRICKS, REGION_FIRST_REF, REGION_VOXELS, REGION_WORDS, SOLID_FLAG, Scene,
    SceneUpdate, hash_region,
};

/// Regions along each edge of a **cell**: 4 x 4 x 4 regions, 32 bricks and 256 Voxels of its level
/// across. A cell is the unit the scene thread lists and counts ([`RingPatch::groups`]) and the box
/// pass makes boxes for; a BLAS is a cube of 1, 2 or 4 cells along each edge ([`Grouping`]), so
/// that a box's coordinates in a BLAS are 0..=1024 at the most, exact in `f32`.
pub const GROUP_REGIONS: i32 = 4;
/// Bricks along each edge of a cell.
pub const GROUP_BRICKS: i32 = GROUP_REGIONS * REGION_BRICKS;
/// The most cells along an edge of a BLAS, as a power of two (4: 16 regions, 128 bricks).
pub const MAX_BLAS_SHIFT: u8 = 2;
/// The TLAS's origin is a multiple of this many Voxels of level 0, nearest to the camera.
pub const ANCHOR_SNAP: i32 = 4096;
/// How many boxes of BLASes a **frame** builds of the groups that may wait (see the module's
/// notes), counted over every build of the frame; the groups that may not wait are built whatever
/// it is, and a frame that has built nothing builds the nearest group of those that may. An
/// update after a jump is not held to it. A first value: a build of 52 BLASes of the flight cost
/// the GPU 1.3 ms (#236), so this is about the 1.5 ms of GPU time that no frame's patch has
/// reached, and it is only a bound on a burst.
pub const BUILD_BUDGET: u64 = 1 << 18;
/// The bytes of a box in the pool, and the BLAS's stride: its corners and the two words.
pub const BOX_BYTES: u64 = 32;
/// The most frames a group that only lost waits for its build (about half a second at 60 a
/// second): after that it is built whatever the budget, so that a group far from the camera is
/// not left behind by the groups nearer it for ever.
pub const MAX_WAIT_FRAMES: u32 = 30;
/// The most updates the log keeps (the oldest go first) if nobody takes it: the window client never
/// does, and the bench takes it every frame.
pub const LOG_LIMIT: usize = 4096;
/// Jobs of the box pass a dispatch has: a row of workgroups each, and the limit of a dimension of
/// a dispatch is 65535. A chunk of jobs is bound at an offset of a multiple of 1 MiB.
const JOB_CHUNK: usize = 32768;

/// A BLAS: its level and the coordinate of its cube in the level's grid of groups.
pub type GroupKey = (u8, [i32; 3]);

/// The cell of the region at `region` (a region coordinate of its level): its cube of
/// [`GROUP_REGIONS`] regions in a regular grid. A function of the coordinate and of nothing the
/// bricks hold.
pub fn group_of(region: [i32; 3]) -> [i32; 3] {
    region.map(|r| r.div_euclid(GROUP_REGIONS))
}

/// The first Voxel of a cell's cube, in Voxels of level 0.
pub fn group_origin(level: u8, group: [i32; 3]) -> [i32; 3] {
    group.map(|g| (g * GROUP_REGIONS * REGION_VOXELS) << level)
}

/// How the cells of each level go into BLASes: a BLAS is a cube of `1 << shift` cells along each
/// edge (4, 8 or 16 regions), a regular grid like the cells', so which BLAS a brick is in is still
/// a function of its coordinate and nothing else. The default is one cell a BLAS.
///
/// Fewer, larger BLASes cost the frame thread less (the builds are recorded and submitted a BLAS
/// at a time, and a submit that traces checks every instance of the TLAS) and the GPU more (a
/// BLAS is built again whole, so a patch builds more boxes): which is better is for the 4090 to
/// say (`--hw-group-regions`, #236).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grouping {
    shift: [u8; crate::lod::LEVELS],
}

impl Default for Grouping {
    fn default() -> Self {
        Self {
            shift: [0; crate::lod::LEVELS],
        }
    }
}

impl Grouping {
    /// Levels below `from` have BLASes of `near` regions along an edge, and the others `far`
    /// (each 4, 8 or 16).
    pub fn split(near: i32, far: i32, from: usize) -> std::result::Result<Self, String> {
        let shift = |regions: i32| match regions {
            4 => Ok(0),
            8 => Ok(1),
            16 => Ok(2),
            _ => Err(format!(
                "a BLAS of {regions} regions an edge: only 4, 8 or 16"
            )),
        };
        let (near, far) = (shift(near)?, shift(far)?);
        let mut g = Self::default();
        for (level, s) in g.shift.iter_mut().enumerate() {
            *s = if level < from { near } else { far };
        }
        Ok(g)
    }

    /// Every level's BLASes are `regions` along an edge.
    pub fn uniform(regions: i32) -> std::result::Result<Self, String> {
        Self::split(regions, regions, 0)
    }

    /// `N` (every level), or `NEAR:FAR@LEVEL` (levels below LEVEL have BLASes of NEAR regions an
    /// edge, the others FAR): the value of `--hw-group-regions`.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let number = |t: &str| {
            t.trim()
                .parse::<i32>()
                .map_err(|_| format!("`{text}` is not N or NEAR:FAR@LEVEL"))
        };
        match text.split_once(':') {
            None => Self::uniform(number(text)?),
            Some((near, rest)) => {
                let (far, level) = rest
                    .split_once('@')
                    .ok_or_else(|| format!("`{text}` is not N or NEAR:FAR@LEVEL"))?;
                let level = level
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("`{text}` is not N or NEAR:FAR@LEVEL"))?;
                Self::split(number(near)?, number(far)?, level)
            }
        }
    }

    /// The text [`Grouping::parse`] takes, for the report.
    pub fn describe(&self) -> String {
        let first = self.shift[0];
        match self.shift.iter().position(|&s| s != first) {
            None => format!("{}", GROUP_REGIONS << first),
            Some(at) => format!(
                "{}:{}@{at}",
                GROUP_REGIONS << first,
                GROUP_REGIONS << self.shift[at]
            ),
        }
    }

    fn shift(&self, level: u8) -> u8 {
        self.shift[(level as usize).min(self.shift.len() - 1)]
    }

    /// Regions along an edge of a BLAS of `level`.
    pub fn regions(&self, level: u8) -> i32 {
        GROUP_REGIONS << self.shift(level)
    }

    /// The BLAS the cell `cell` (a coordinate of cells) of `level` is in.
    pub fn blas_of(&self, level: u8, cell: [i32; 3]) -> GroupKey {
        let s = self.shift(level);
        (level, cell.map(|c| c >> s))
    }

    /// The cells of the BLAS `blas` of `level`, with where each is in it (in cells), in the order
    /// of x, then y, then z.
    pub fn cells(&self, level: u8, blas: [i32; 3]) -> impl Iterator<Item = ([i32; 3], [u32; 3])> {
        let s = self.shift(level);
        let n = 1u32 << s;
        (0..n * n * n).map(move |i| {
            let at = [i % n, i / n % n, i / (n * n)];
            (std::array::from_fn(|a| (blas[a] << s) + at[a] as i32), at)
        })
    }

    /// The first Voxel of a BLAS's cube, in Voxels of level 0.
    pub fn origin(&self, level: u8, blas: [i32; 3]) -> [i32; 3] {
        blas.map(|g| (g * self.regions(level) * REGION_VOXELS) << level)
    }
}

static PROCESS_GROUPING: std::sync::OnceLock<Grouping> = std::sync::OnceLock::new();

/// Sets the grouping that every hardware backend made after this has (the benchmark's
/// `--hw-group-regions`); the first call wins, and it is for a program's start.
pub fn set_process_grouping(grouping: Grouping) {
    let _ = PROCESS_GROUPING.set(grouping);
}

/// The grouping a hardware backend starts with: the process's, or one cell a BLAS.
pub fn process_grouping() -> Grouping {
    PROCESS_GROUPING.get().copied().unwrap_or_default()
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
    /// Word 0: the brick's coordinate in the BLAS (8 bits an axis; for a whole region, its first
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

/// The regions of the cell at `group` of `level` that the scene lists: their indices in
/// `scene.regions` and where in the cell (0..64, as x + 4y + 16z) they are.
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

/// How many boxes the cell at `group` of `level` has. 0 for a cell that has no region.
pub fn group_box_count(scene: &Scene, level: usize, group: [i32; 3]) -> u32 {
    group_regions(scene, level, group)
        .map(|(region, _)| region_box_count(scene, region))
        .sum()
}

/// The boxes of the group at `group` of `level`: one for each brick of each of its regions that
/// the scene lists, tight; or one for a region that is one Material. In region and brick order.
/// **What the GPU's [`BoxPass`] makes, box for box.**
pub fn group_prims(scene: &Scene, level: usize, group: [i32; 3]) -> Vec<Prim> {
    cell_prims(scene, level, group, [0; 3])
}

/// The boxes of the cell at `cell` of `level` as they are in a BLAS in which the cell is `at`
/// cells from its corner.
fn cell_prims(scene: &Scene, level: usize, cell: [i32; 3], at: [u32; 3]) -> Vec<Prim> {
    let mut out = Vec::new();
    for (region, i) in group_regions(scene, level, cell) {
        let local = [
            i as i32 % GROUP_REGIONS,
            i as i32 / GROUP_REGIONS % GROUP_REGIONS,
            i as i32 / (GROUP_REGIONS * GROUP_REGIONS),
        ];
        let local: [i32; 3] = std::array::from_fn(|a| local[a] + at[a] as i32 * GROUP_REGIONS);
        region_prims(scene, region, local, &mut out);
    }
    out
}

/// The boxes of the BLAS at `blas` of `level` under `grouping`: its cells' boxes, a cell's after
/// the cells before it (x, then y, then z). **What the GPU makes for it, box for box.**
pub fn blas_prims(scene: &Scene, grouping: &Grouping, level: usize, blas: [i32; 3]) -> Vec<Prim> {
    grouping
        .cells(level as u8, blas)
        .flat_map(|(cell, at)| cell_prims(scene, level, cell, at))
        .collect()
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
    let pack = |brick: [i32; 3]| brick[0] as u32 | (brick[1] as u32) << 8 | (brick[2] as u32) << 16;
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

/// Where an update's time went on the frame thread, in the order of the work (they add up to
/// `prepare + build`: each is the time between two readings of one clock). For the benchmark to say
/// which part of the hardware backend's cost it is (#236).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HwPhases {
    /// What came before the pump: for an update the patch's groups merged into the queue (the
    /// search for a stale one, if there is none), for a draw's follow the TLAS's frame moved, which
    /// writes every instance again.
    pub before: Duration,
    /// Choosing the groups (the queue sorted by the camera's distance, the budget), and for each
    /// the pool's room, its BLAS (made, if it is new) and its job for the box pass.
    pub plan: Duration,
    /// The TLAS's instances and the table of them written (`write_buffer`), and the bind group
    /// made again if the TLAS or a buffer was.
    pub instances: Duration,
    /// Recording the box pass (the jobs written, a bind group and a dispatch a chunk of them).
    pub boxes: Duration,
    /// Making the entries of the BLASes' builds and recording them and the TLAS's.
    pub builds: Duration,
    /// Finishing the command buffer.
    pub finish: Duration,
    /// `Queue::submit` of that command buffer alone. The writes of the buffers that the frame's
    /// update made with `write_buffer` since the last submit (the scene's upload, the instances)
    /// are sent with the first submit after them, which is this one when an update came first: so
    /// the frame's own submit is cheaper for it.
    pub submit: Duration,
    /// Waiting for the GPU to be done: zero, but in the Edit stress ([`HwScene::set_wait`]).
    pub wait: Duration,
}

/// Every BLAS that the scene has a cell in under `grouping`, with how many boxes it has: what a
/// full build makes of it.
pub fn blas_counts(scene: &Scene, grouping: &Grouping) -> BTreeMap<GroupKey, u32> {
    let mut blases = BTreeMap::new();
    for ((level, cell), count) in group_counts(scene) {
        *blases.entry(grouping.blas_of(level, cell)).or_insert(0) += count;
    }
    blases
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
    /// The same by level: BLASes and boxes built of each.
    pub blas_by_level: [u32; crate::lod::LEVELS],
    pub prims_by_level: [u64; crate::lod::LEVELS],
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
    /// `prepare + build` in the parts of the work.
    pub phases: HwPhases,
}

/// One cell's job for the box pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxJob {
    pub level: u8,
    /// The cell, in cells of its level.
    pub group: [i32; 3],
    /// Where the cell is in its BLAS, in cells: its boxes' coordinates are from the BLAS's corner.
    pub cell: [u32; 3],
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
    /// Jobs to a dispatch ([`JOB_CHUNK`]).
    chunk: usize,
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
            chunk: JOB_CHUNK,
        }
    }

    /// Jobs to a dispatch, for a test that wants more dispatches than there are jobs to fill a
    /// dimension of one (the offset of its bind group has to be a multiple
    /// of 256 bytes), so a multiple of 8 jobs.
    pub fn set_chunk(&mut self, jobs: usize) {
        self.chunk = jobs.max(1);
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
                    (j.cell[0] | j.cell[1] << 8 | j.cell[2] << 16) as i32,
                    0,
                ]
            })
            .collect();
        queue.write_buffer(&self.jobs, 0, bytemuck::cast_slice(&words));
        // Untimed on purpose: it runs between frames, with the builds, and its cost is the update's
        // (the Edit stress waits for it), not a pass of the frame.
        let descriptor = wgpu::ComputePassDescriptor {
            label: Some("hardware boxes"),
            timestamp_writes: None,
        };
        let mut pass = encoder.begin_compute_pass(&descriptor); // untimed
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, scene_group, &[]);
        // One row of workgroups a job, a workgroup a region of its group: 64. A frame's jobs are
        // a few hundred at the most, a jump's all of the Planet's groups, which the dispatches
        // of a chunk of jobs each (bound at its own offset) keep under the limit of a dimension.
        for (n, chunk) in jobs.chunks(self.chunk).enumerate() {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hardware boxes"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.jobs,
                            offset: (n * self.chunk) as u64 * 32,
                            size: std::num::NonZeroU64::new(chunk.len() as u64 * 32),
                        }),
                    },
                    crate::tracer::buffer_entry(1, out),
                ],
            });
            pass.set_bind_group(1, &group, &[]);
            pass.dispatch_workgroups(
                (GROUP_REGIONS * GROUP_REGIONS * GROUP_REGIONS) as u32,
                chunk.len() as u32,
                1,
            );
        }
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

/// How much a call of `pump` may build of the groups that may wait.
#[derive(Clone, Copy, Debug)]
enum Limit {
    /// All of them.
    All,
    /// Up to `left` boxes, and one group whatever its size if `at_least_one`.
    Boxes { left: u64, at_least_one: bool },
}

/// A group that is stale and not yet built again.
#[derive(Clone, Copy, Debug)]
struct Wait {
    /// How many boxes it has now (0: it is gone).
    count: u32,
    /// Whether it may not wait for the budget: it gained boxes (see [`RingPatch::grown`]), or it
    /// has no BLAS to trace while it waits.
    urgent: bool,
    /// The frames it has waited.
    waited: u32,
}

/// The acceleration structures of a scene, and what binds them for the shaders (group 2 of the
/// hardware pipelines).
pub struct HwScene {
    layout: wgpu::BindGroupLayout,
    boxes: BoxPass,
    /// How the cells go into BLASes.
    grouping: Grouping,
    /// How many boxes each cell of the scene has now (a cell with none is not here), as the scene
    /// thread's patches have counted them.
    cells: BTreeMap<GroupKey, u32>,
    /// The BLASes, by [`Grouping::blas_of`].
    groups: BTreeMap<GroupKey, Group>,
    /// The BLASes that are stale and not yet built again.
    pending: BTreeMap<GroupKey, Wait>,
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
    /// The frames a group may wait ([`MAX_WAIT_FRAMES`]).
    max_wait: u32,
    /// The boxes and the groups built since the last frame's draw closed the frame: what the
    /// budget of the frame has had of it, whichever update or draw built them.
    spent: u64,
    built: u32,
    /// The frame whose draw last followed the camera, so that a draw after it in the same frame
    /// (another view) has no budget of its own.
    frame: Option<u64>,
    wait: bool,
    last: HwUpdate,
    /// What the updates since [`HwScene::take_cost`] cost the frame thread, in all.
    cost: Duration,
    /// The updates since the log was last taken ([`HwScene::take_log`]), for the bench's totals: at
    /// most [`LOG_LIMIT`], the oldest dropped.
    log: VecDeque<HwUpdate>,
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
            grouping: process_grouping(),
            cells: BTreeMap::new(),
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
            max_wait: MAX_WAIT_FRAMES,
            spent: 0,
            built: 0,
            frame: None,
            wait: false,
            last: HwUpdate::default(),
            cost: Duration::ZERO,
            log: VecDeque::new(),
        };
        hw.update(device, queue, scene_group, scene, &SceneUpdate::Rebuilt);
        Ok(hw)
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Every update since the last call, oldest first (the first build is the first of them).
    pub fn take_log(&mut self) -> Vec<HwUpdate> {
        std::mem::take(&mut self.log).into()
    }

    /// What every update since the last call cost the frame thread (their `prepare` and `build`),
    /// whichever call made them: [`HwScene::update`] or [`HwScene::follow`], which is in a draw.
    pub fn take_cost(&mut self) -> Duration {
        std::mem::take(&mut self.cost)
    }

    fn record(&mut self, update: HwUpdate) {
        self.cost += update.prepare + update.build;
        if self.log.len() >= LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(update.clone());
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

    /// The frames a group that only lost may wait for its build, for tests that want to see it
    /// built for having waited ([`MAX_WAIT_FRAMES`]).
    pub fn set_max_wait(&mut self, frames: u32) {
        self.max_wait = frames;
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
                // The patch lists cells; what is stale is the BLAS each is in, which is urgent if
                // a cell of it gained boxes (or it has no BLAS), and has all its cells' boxes.
                let mut blases: BTreeSet<GroupKey> = BTreeSet::new();
                for &(key, count) in &patch.groups {
                    if count > 0 {
                        self.cells.insert(key, count);
                    } else {
                        self.cells.remove(&key);
                    }
                    let blas = self.grouping.blas_of(key.0, key.1);
                    let urgent = patch.grown.binary_search(&key).is_ok()
                        || (count > 0 && !self.groups.contains_key(&blas));
                    let wait = self.pending.entry(blas).or_insert(Wait {
                        count: 0,
                        urgent,
                        waited: 0,
                    });
                    wait.urgent |= urgent;
                    blases.insert(blas);
                }
                for blas in blases {
                    let count = self.blas_count(blas);
                    if let Some(wait) = self.pending.get_mut(&blas) {
                        wait.count = count;
                    }
                }
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
                phases: HwPhases {
                    before: started.elapsed(),
                    ..HwPhases::default()
                },
                ..HwUpdate::default()
            });
            return;
        }
        let limit = if all { Limit::All } else { self.left() };
        self.pump(device, queue, scene_group, started, limit);
    }

    /// What of the frame's budget is left: the boxes, and whether the frame has built nothing yet
    /// (so that it builds a group that is over the budget by itself).
    fn left(&self) -> Limit {
        Limit::Boxes {
            left: self.budget.saturating_sub(self.spent),
            at_least_one: self.built == 0,
        }
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
        self.cells = group_counts(scene);
        let blases: BTreeSet<GroupKey> = self
            .cells
            .keys()
            .map(|&(level, cell)| self.grouping.blas_of(level, cell))
            .collect();
        self.pending = blases
            .into_iter()
            .map(|blas| {
                let count = self.blas_count(blas);
                (
                    blas,
                    Wait {
                        count,
                        urgent: true,
                        waited: 0,
                    },
                )
            })
            .collect();
    }

    /// How many boxes the BLAS `blas` has now: its cells' counts.
    fn blas_count(&self, blas: GroupKey) -> u32 {
        self.grouping
            .cells(blas.0, blas.1)
            .map(|(cell, _)| self.cells.get(&(blas.0, cell)).copied().unwrap_or(0))
            .sum()
    }

    /// Changes how the cells go into BLASes: every BLAS is made again (queued, all of it), and the
    /// next update or [`HwScene::flush`] builds them. For a program's start and for the tests.
    pub fn set_grouping(&mut self, grouping: Grouping, scene: &Scene) {
        self.grouping = grouping;
        self.restart(scene);
        self.tlas_stale = true;
    }

    /// How the cells go into BLASes.
    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    /// The instances of the TLAS.
    pub fn instances(&self) -> u32 {
        self.groups.len() as u32
    }

    /// Gives back what the group `group` of `slot` held: its room in the pool.
    fn release(&mut self, group: &Group) {
        self.pool.release(group.start..group.start + group.capacity);
    }

    /// How far the group at `key` is from the camera, squared, in Voxels of level 0.
    fn distance2(&self, key: GroupKey) -> f64 {
        let origin = self.grouping.origin(key.0, key.1);
        let half =
            f64::from(self.grouping.regions(key.0) * REGION_VOXELS / 2) * f64::from(1u32 << key.0);
        (0..3)
            .map(|a| {
                let d = f64::from(origin[a]) + half - self.camera[a];
                d * d
            })
            .sum()
    }

    /// Builds the stale groups that may not wait, and as many of the others as `limit` allows,
    /// nearest the camera first, and the TLAS.
    fn pump(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
        started: Instant,
        limit: Limit,
    ) {
        let all = matches!(limit, Limit::All);
        let entered = Instant::now();
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
            .filter(|(_, wait)| wait.count == 0)
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
        // The groups that may not wait come first, whatever the budget (and are in it); the rest,
        // nearest the camera first, until what is left of it is spent. A group that has waited
        // too long may not wait either.
        let mut order: Vec<(bool, f64, GroupKey, u32)> = self
            .pending
            .iter()
            .map(|(&key, wait)| {
                let urgent =
                    wait.urgent || wait.waited >= self.max_wait || !self.groups.contains_key(&key);
                (!urgent, self.distance2(key), key, wait.count)
            })
            .collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        let (mut left, at_least_one) = match limit {
            Limit::All => (u64::MAX, false),
            Limit::Boxes { left, at_least_one } => (left, at_least_one),
        };
        let mut spent = 0u64;
        // The BLASes to build (and their boxes), and the box pass's jobs: a job a cell of them.
        let mut built: Vec<(GroupKey, u32)> = Vec::new();
        let mut jobs: Vec<BoxJob> = Vec::new();
        for (may_wait, _, key, count) in order {
            if may_wait {
                if u64::from(count) > left && !(at_least_one && built.is_empty()) {
                    break;
                }
                left = left.saturating_sub(u64::from(count));
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
            let mut at = start;
            for (cell, local) in self.grouping.cells(key.0, key.1) {
                let n = self.cells.get(&(key.0, cell)).copied().unwrap_or(0);
                if n > 0 {
                    jobs.push(BoxJob {
                        level: key.0,
                        group: cell,
                        cell: local,
                        start: at,
                        count: n,
                    });
                    at += n;
                }
            }
            built.push((key, count));
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
        if built.is_empty() && touched.is_empty() && !self.tlas_stale {
            // Everything that waits may wait (a second view of the frame, or a budget that is
            // spent): nothing to build, and no TLAS build for nothing.
            return;
        }
        self.spent += spent;
        self.built += built.len() as u32;
        let planned = Instant::now();
        self.sync_instances(device, queue, touched);
        let synced = Instant::now();
        let kind = if built.is_empty() { "anchor" } else { "patch" };
        self.boxes.encode(
            device,
            queue,
            &mut encoder,
            scene_group,
            &self.pool_buffer,
            &jobs,
        );
        let boxed = Instant::now();
        let sizes: Vec<wgpu::BlasAABBGeometrySizeDescriptor> = built
            .iter()
            .map(|&(_, count)| blas_descriptor(count))
            .collect();
        let entries: Vec<wgpu::BlasBuildEntry<'_>> = built
            .iter()
            .zip(&sizes)
            .map(|(key, size)| {
                let group = &self.groups[&key.0];
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
        let recorded = Instant::now();
        let commands = encoder.finish();
        let finished = Instant::now();
        queue.submit(std::iter::once(commands));
        let submitted = Instant::now();
        if self.wait {
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("device poll failed");
        }
        let done = Instant::now();
        let mut blas_by_level = [0u32; crate::lod::LEVELS];
        let mut prims_by_level = [0u64; crate::lod::LEVELS];
        for &((level, _), count) in &built {
            blas_by_level[(level as usize).min(crate::lod::LEVELS - 1)] += 1;
            prims_by_level[(level as usize).min(crate::lod::LEVELS - 1)] += u64::from(count);
        }
        let phases = HwPhases {
            before: entered - started,
            plan: planned - entered,
            instances: synced - planned,
            boxes: boxed - synced,
            builds: recorded - boxed,
            finish: finished - recorded,
            submit: submitted - finished,
            wait: done - submitted,
        };
        self.tlas_stale = false;
        self.record(HwUpdate {
            kind: if all { "build" } else { kind },
            blas_rebuilt: built.len() as u32,
            prims_rebuilt: built.iter().map(|&(_, n)| u64::from(n)).sum(),
            blas_by_level,
            prims_by_level,
            tlas_rebuilt: true,
            tlas_instances: self.groups.len() as u32,
            blas_total: self.groups.len() as u32,
            prims_total: self.prims_total(),
            pending: self.pending.len() as u32,
            prepare: synced - started,
            build: done - synced,
            phases,
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
                    let origin = self.grouping.origin(key.0, key.1);
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
    ///
    /// `frame` says which frame the draw is of (the profiler's count of them), `None` for a draw
    /// that is not a frame: the frame's budget is what [`HwScene::update`] and the first draw of
    /// the frame have between them, so that a second view drawn in the frame builds nothing of
    /// the groups that may wait, and it is the draw that closes the frame.
    pub fn follow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene_group: &wgpu::BindGroup,
        frame: Option<u64>,
        position: [f64; 3],
    ) {
        let first_of_frame = frame.is_none() || self.frame != frame;
        self.frame = frame;
        let limit = if first_of_frame {
            self.left()
        } else {
            Limit::Boxes {
                left: 0,
                at_least_one: false,
            }
        };
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
            self.pump(device, queue, scene_group, started, limit);
        } else if !self.pending.is_empty() {
            self.pump(device, queue, scene_group, Instant::now(), limit);
        }
        if first_of_frame {
            // The frame is over: what it built is spent, and what still waits has waited a frame.
            self.spent = 0;
            self.built = 0;
            for wait in self.pending.values_mut() {
                wait.waited += 1;
            }
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
            self.pump(device, queue, scene_group, Instant::now(), Limit::All);
            // That was no jump: it says so in the log.
            if let Some(last) = self.log.back_mut() {
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
                    cell: [0; 3],
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
