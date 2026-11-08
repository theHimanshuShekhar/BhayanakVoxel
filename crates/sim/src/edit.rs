//! Applying an Edit to the Planet's Voxels. This is the only code that changes them: the server
//! calls it for each Edit it accepts, and a client calls it for the Edits it predicts and for
//! the ones the server sends. All of them must end with identical bricks (ADR 0003), so
//! everything here is integer maths in a fixed order, and `clippy.toml` bans what is not.

use brick_store::{
    Brick, BrickCoord, BrickStore, LodRefresh, Material, VOXELS_PER_METRE, VoxelCoord,
};
use materials::MaterialTable;
use net::{EditMode, EditOp, RejectReason};

use crate::Ground;
use crate::TICK_RATE_HZ;
use crate::brush::{BrickCache, BrickChange, MAX_COORD, Plan, implied_brick};

/// How far from a player's eyes an Edit's centre may be: 8 m (issue 15), in Voxels.
pub const EDIT_REACH: f64 = 8.0 * VOXELS_PER_METRE;

/// Edits a player may make per second, and at once after a rest (issue 15).
pub const OPS_PER_SECOND: u32 = 20;

/// The Voxels in a cubic metre: what the Edit limits below, which are in cubic metres, turn into.
const VOXELS_PER_CUBIC_METRE: f64 = VOXELS_PER_METRE * VOXELS_PER_METRE * VOXELS_PER_METRE;

/// Voxels a player's Edits may change per second: 8 m^3 (issue 15, and ADR 0007: the one budget
/// that is in metres and not a count, per #137), 64 000 at 5 cm.
pub const VOXELS_PER_SECOND: u32 = (8.0 * VOXELS_PER_CUBIC_METRE) as u32;

/// Voxels a player may change in one burst after a rest: nine seconds' worth.
pub const VOXEL_BURST: u32 = 9 * VOXELS_PER_SECOND;

/// How many times as dear a Voxel that a Smooth looks at is as one that another Edit does: a
/// Smooth reads the 27 Voxels round each (35 to 60 ns in release against 4 to 12).
pub const SMOOTH_WEIGHT: u64 = 6;

/// Units of work a player may ask of the server per second: 600 000 at 10 cm Voxels, scaled with
/// the Edit rate. A unit is what the server spends on looking at a Voxel for a Dig, Place or
/// Flatten, about 10 ns, so this is 48 ms of the server's time a second for each player. What an
/// Edit asks is [`work`], which does not depend on what is in the Planet, and is known before the
/// Edit is worked out.
pub const WORK_PER_SECOND: u64 = (600.0 * VOXELS_PER_CUBIC_METRE) as u64;

/// Units of work a player may ask for at once after a rest: 360 000 at 10 cm Voxels, scaled with
/// the rate (0.6 of a second's worth), so that any one Brush can be used from rest.
pub const WORK_BURST: u64 = (360.0 * VOXELS_PER_CUBIC_METRE) as u64;

/// How much bigger, in tenths, the box that holds a cube that leans the most is than the cube:
/// 5.1 times its volume (the cube of the square root of 3 is 5.196; the most measured is 5.08).
const LEANING_BOX_TENTHS: u64 = 51;

/// The biggest radius whose Brush, of `weight` units of work for each Voxel it examines, a player
/// can afford from rest: a cube of that radius changes no more than [`VOXEL_BURST`] Voxels, and
/// the box that holds it when it leans the most ([`LEANING_BOX_TENTHS`]) asks no more than
/// [`WORK_BURST`].
const fn affordable_radius(weight: u64) -> u16 {
    let mut radius = 0u16;
    loop {
        let side = 2 * (radius as u64 + 1) + 1;
        let volume = side * side * side;
        if volume > VOXEL_BURST as u64 || LEANING_BOX_TENTHS * volume * weight > 10 * WORK_BURST {
            return radius;
        }
        radius += 1;
    }
}

const fn least(a: u16, b: u16) -> u16 {
    if a < b { a } else { b }
}

/// Largest Brush a player may use: 2 m (issue 15), 40 Voxels, unless the bursts afford less.
pub const MAX_BRUSH_RADIUS: u16 = least((2.0 * VOXELS_PER_METRE) as u16, affordable_radius(1));

/// The biggest Brush that a Smooth or a Flatten may use: 1 m, 20 Voxels, unless the bursts afford
/// less. They read the Voxels beside the ones they change (a Smooth, 27 for each) and are the
/// dearest to work out.
pub const MAX_SMOOTH_RADIUS: u16 = least(VOXELS_PER_METRE as u16, affordable_radius(SMOOTH_WEIGHT));

/// What the server has to spend to work out `op`, in units (see [`WORK_PER_SECOND`]): the Voxels
/// of the box that holds its Brush ([`crate::examined`]), whatever it ends up changing, and for a
/// Smooth [`SMOOTH_WEIGHT`] times as many. An op that no box holds costs more than anyone has.
pub fn work(op: &EditOp) -> u64 {
    let weight = if op.mode == EditMode::Smooth {
        SMOOTH_WEIGHT
    } else {
        1
    };
    crate::examined(op).saturating_mul(weight)
}

/// Seconds of holding per unit of hardness (0 to 10 in `materials.toml`): sand (1) takes 0.15 s,
/// soil (2) 0.3 s, rock (6) 0.9 s, obsidian (8) 1.2 s.
pub const SECONDS_PER_HARDNESS: f64 = 0.15;

/// The least a Dig takes, so that a Material with no hardness (water) is not dug faster than the
/// server's 20 Edits a second.
pub const MIN_DIG_SECONDS: f64 = 0.05;

/// The hardness that a Dig aimed at a Voxel of `material` takes: the Material's own, and the
/// greatest there is (10, see `materials.toml`) for air or a Material that the table does not
/// define. A client aims at solid Voxels it can see, so the second case is for a Dig that someone
/// else's has just emptied or that a modified client aims at the air beside the rock, with a
/// Brush that reaches into it: it is no way round the time.
pub fn target_hardness(material: Option<Material>) -> f32 {
    material
        .and_then(|material| MaterialTable::built_in().get(material))
        .map_or(10.0, |def| def.hardness)
}

/// How long a Dig takes to make, in seconds, in a Material of this hardness: the time the client
/// makes the player hold the button, and the time the server makes a player wait between Digs
/// ([`dig_ticks`]).
pub fn dig_seconds(hardness: f32) -> f64 {
    (f64::from(hardness) * SECONDS_PER_HARDNESS).max(MIN_DIG_SECONDS)
}

/// [`dig_seconds`] in server ticks, to the nearest tick, so that the server's count of it does not
/// depend on the clock.
pub fn dig_ticks(hardness: f32) -> u64 {
    (dig_seconds(hardness) * f64::from(TICK_RATE_HZ)).round() as u64
}

/// What the server checks of an Edit before it applies it, apart from the limits that depend on
/// who asked and when (reach, rates and volume: see [`within_reach`], [`EditBudget`] and the
/// server): that the Brush is no bigger than a player may use (a Smooth or a Flatten no bigger
/// than [`MAX_SMOOTH_RADIUS`]), that its centre is within [`MAX_COORD`] of the Planet's, that its
/// rotation is one of the 8, and that the Material it fills with is one that the table defines.
pub fn validate(op: &EditOp) -> Result<(), RejectReason> {
    let shaping = matches!(op.mode, EditMode::Smooth | EditMode::Flatten);
    let largest = if shaping {
        MAX_SMOOTH_RADIUS
    } else {
        MAX_BRUSH_RADIUS
    };
    if op.brush.radius > largest {
        return Err(RejectReason::BrushTooLarge);
    }
    if op
        .centre
        .0
        .into_iter()
        .any(|c| c.unsigned_abs() > MAX_COORD.unsigned_abs())
    {
        return Err(RejectReason::OutOfRange);
    }
    if op.rotation >= 8 {
        return Err(RejectReason::BadRotation);
    }
    let fills = matches!(op.mode, EditMode::Place | EditMode::Flatten);
    if fills && MaterialTable::built_in().get(op.material).is_none() {
        return Err(RejectReason::UnknownMaterial);
    }
    Ok(())
}

/// What one player may still ask of the server in Edits: a token bucket for each of three limits,
/// in integers so that it does not depend on a clock. Credit is counted in 1/[`TICK_RATE_HZ`] of
/// an Edit, a Voxel or a unit of work, so that each tick adds a whole number (the rate per
/// second) and an Edit, a Voxel or a unit costs [`TICK_RATE_HZ`]. The limits are:
///
/// - the Edits made, [`OPS_PER_SECOND`];
/// - the Voxels changed, [`VOXELS_PER_SECOND`]: what the Planet loses or gains, known once the
///   Edit is worked out;
/// - the work asked of the server, [`WORK_PER_SECOND`]: what working it out costs, known from
///   the op alone, which is what stops an Edit that changes nothing from being free.
///
/// The server keeps one for each player, and the player's client keeps another, ticked as its
/// character is, so that it can tell that an Edit will be turned down and not make it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditBudget {
    ops: u32,
    voxels: u64,
    work: u64,
}

impl EditBudget {
    /// What a player has after a rest: a second of Edits, [`VOXEL_BURST`] Voxels and
    /// [`WORK_BURST`] units of work.
    pub fn full() -> Self {
        Self {
            ops: OPS_PER_SECOND * TICK_RATE_HZ,
            voxels: u64::from(VOXEL_BURST) * u64::from(TICK_RATE_HZ),
            work: WORK_BURST * u64::from(TICK_RATE_HZ),
        }
    }

    /// One tick's credit.
    pub fn refill(&mut self) {
        let full = Self::full();
        self.ops = (self.ops + OPS_PER_SECOND).min(full.ops);
        self.voxels = (self.voxels + u64::from(VOXELS_PER_SECOND)).min(full.voxels);
        self.work = (self.work + WORK_PER_SECOND).min(full.work);
    }

    /// Spends one Edit's credit, if there is that much.
    pub fn take_op(&mut self) -> bool {
        self.ops
            .checked_sub(TICK_RATE_HZ)
            .map(|ops| self.ops = ops)
            .is_some()
    }

    /// Whether there is an Edit's credit.
    pub fn has_op(&self) -> bool {
        self.ops >= TICK_RATE_HZ
    }

    /// Whether `units` of work are within what is left.
    pub fn has_work(&self, units: u64) -> bool {
        units
            .checked_mul(u64::from(TICK_RATE_HZ))
            .is_some_and(|cost| cost <= self.work)
    }

    /// Spends `units` of work, if there is that much.
    pub fn take_work(&mut self, units: u64) -> bool {
        let spent = self.has_work(units);
        if spent {
            self.work -= units * u64::from(TICK_RATE_HZ);
        }
        spent
    }

    /// Gives back `units` of work that were spent, up to what a rest has.
    pub fn refund_work(&mut self, units: u64) {
        let full = Self::full().work;
        self.work = (self.work + units * u64::from(TICK_RATE_HZ)).min(full);
    }

    /// Gives back `changed` Voxels that were spent, up to what a rest has.
    pub fn refund_volume(&mut self, changed: u64) {
        let full = Self::full().voxels;
        self.voxels = (self.voxels + changed * u64::from(TICK_RATE_HZ)).min(full);
    }

    /// Whether `changed` Voxels are within what is left.
    pub fn has_volume(&self, changed: u64) -> bool {
        changed
            .checked_mul(u64::from(TICK_RATE_HZ))
            .is_some_and(|cost| cost <= self.voxels)
    }

    /// Spends `changed` Voxels' credit, if there is that much.
    pub fn take_volume(&mut self, changed: u64) -> bool {
        let spent = self.has_volume(changed);
        if spent {
            self.voxels -= changed * u64::from(TICK_RATE_HZ);
        }
        spent
    }
}

/// Whether `centre` is within [`EDIT_REACH`] of `eye`, a position in Voxels.
pub fn within_reach(eye: [f64; 3], centre: VoxelCoord) -> bool {
    // The middle of the Voxel. A NaN eye is out of reach, as the comparison is false.
    let distance2: f64 = (0..3)
        .map(|a| {
            let d = f64::from(centre.0[a]) + 0.5 - eye[a];
            d * d
        })
        .sum();
    distance2 <= EDIT_REACH * EDIT_REACH
}

/// What an applied Edit overwrote: the bricks as they were before, so that it can be taken back.
/// It also says which coarse bricks of the store's LOD pyramid the Edit changed.
#[derive(Debug, Default)]
pub struct Undo {
    bricks: Vec<(BrickCoord, Option<Brick>)>,
    lod: LodRefresh,
}

impl Undo {
    /// The bricks the Edit changed, in ascending order.
    pub fn bricks(&self) -> impl Iterator<Item = BrickCoord> + '_ {
        self.bricks.iter().map(|&(coord, _)| coord)
    }

    /// What the Edit did to the store's coarse levels: [`LodRefresh::changed`] is the coarse
    /// bricks that read differently afterwards, and so the ones to upload again.
    pub fn lod(&self) -> &LodRefresh {
        &self.lod
    }

    /// Puts the bricks back as they were, and the store's coarse levels with them, and says
    /// which coarse bricks that changed. The Edit must be the latest change to them.
    pub fn revert(self, store: &mut BrickStore) -> LodRefresh {
        let changed: Vec<_> = self.bricks().collect();
        self.revert_bricks(store);
        store.refresh_lod(changed)
    }

    /// [`Undo::revert`] for level 0 only: the coarse levels are not remade, and are as stale as
    /// the Edit left them if it was written with [`write_plan_from`]. The caller remakes them
    /// over the bricks that were reverted ([`BrickStore::refresh_lod`]) before anything reads them.
    pub fn revert_bricks(self, store: &mut BrickStore) {
        for (coord, before) in self.bricks {
            match before {
                Some(brick) => store.insert(coord, brick),
                None => {
                    store.remove(coord);
                }
            }
        }
    }
}

/// Applies `op` to `store`: writes the Voxels that [`crate::plan`] lists for it. The Edit should
/// have passed [`validate`].
///
/// The Planet's core is not in the store (see [`planet_gen::generate_planet`]); it is what the
/// store's [`brick_store::BrickSource`] says a brick that is not held is. So a brick that the
/// Edit changes is first stored as that, if it is not air. Bricks that end up uniform collapse to
/// a flag, and one that ends up all air stays in the store as air, so that the core does not show
/// through where it was dug out.
///
/// Every coarse level of the store's LOD pyramid, if it has one, is brought up to date before
/// this returns, remaking only the coarse bricks over the bricks that changed. The returned
/// [`Undo::lod`] lists the coarse bricks whose content changed.
pub fn apply(store: &mut BrickStore, op: &EditOp) -> Undo {
    let plan = crate::plan_bricks(store, op);
    apply_plan(store, &plan)
}

/// Writes `plan`, which [`crate::plan_bricks`] made from `store` as it is now.
pub fn apply_plan(store: &mut BrickStore, plan: &Plan) -> Undo {
    apply_plan_from(store, plan, None, None)
}

/// [`apply_plan`], finding the bricks that the store does not hold in the character's `ground`
/// and through the `cache`, as [`crate::plan_bricks_from`] does, and not by making them again.
///
/// Brick by brick: a brick is looked up in the store once, whatever number of its Voxels change,
/// and written as masks ([`Brick::clear`] and [`Brick::fill`]); the coarse levels are remade once
/// for each brick that changed, level by level, after all of them are written.
pub fn apply_plan_from(
    store: &mut BrickStore,
    plan: &Plan,
    ground: Option<&Ground>,
    cache: Option<&mut BrickCache>,
) -> Undo {
    let mut undo = write_plan_from(store, plan, ground, cache);
    undo.lod = store.refresh_lod(undo.bricks());
    undo
}

/// [`apply_plan_from`] for level 0 only: the store's coarse levels are left as they were, which
/// is stale over the bricks of [`Undo::bricks`] until the caller remakes them
/// ([`BrickStore::refresh_lod`]) and before anything reads them. It is what the client's frame
/// thread does of an Edit, the rest being for the scene thread, which reads the coarse levels.
pub fn write_plan_from(
    store: &mut BrickStore,
    plan: &Plan,
    ground: Option<&Ground>,
    mut cache: Option<&mut BrickCache>,
) -> Undo {
    let mut undo = Undo::default();
    for change in plan.bricks() {
        let coord = change.coord;
        match store.brick_mut(coord) {
            Some(brick) => {
                let before = brick.clone();
                write_brick(brick, change);
                undo.bricks.push((coord, Some(before)));
            }
            None => {
                let mut brick = implied_brick(store, ground, cache.as_deref_mut(), coord);
                // A brick that is air is held only once something is put in it, so that one that
                // is dug out of the core stays held as air and a dig of air holds nothing.
                let holds = !brick.is_air() || !change.filled.is_empty();
                write_brick(&mut brick, change);
                if holds {
                    store.insert(coord, brick);
                }
                undo.bricks.push((coord, None));
            }
        }
    }
    undo
}

/// Writes the Voxels of one brick that `change` says: the ones that become air, and the ones that
/// become each Material.
fn write_brick(brick: &mut Brick, change: &BrickChange) {
    brick.clear(&change.cleared);
    for (material, mask) in &change.filled {
        brick.fill(mask, *material);
    }
}

#[cfg(test)]
mod equivalence;

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Interior, LodRefresh, Material, VoxelCoord};
    use net::{Alignment, Brush, EditMode, Shape};
    use planet_gen::{CORE, ROCK, SOIL};

    /// The brick `level` levels coarser that covers `coord`.
    fn ancestor(coord: BrickCoord, level: usize) -> BrickCoord {
        (0..level).fold(coord, |c, _| c.parent())
    }

    const STONE: Material = Material(7);

    /// An empty store whose bricks that are not held read as the Planet's core.
    fn core_store() -> BrickStore {
        let mut store = BrickStore::new();
        store.set_source(CORE);
        store
    }

    fn dig(store: &mut BrickStore, centre: [i32; 3], radius: u16) -> Undo {
        apply(store, &EditOp::dig_sphere(VoxelCoord(centre), radius))
    }

    /// A store with a solid cube of STONE from `lo` to `hi` inclusive.
    fn cube(lo: [i32; 3], hi: [i32; 3]) -> BrickStore {
        let mut store = BrickStore::new();
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    store.set(VoxelCoord([x, y, z]), Some(STONE));
                }
            }
        }
        store
    }

    fn dug(store: &BrickStore, lo: [i32; 3], hi: [i32; 3]) -> usize {
        let mut count = 0;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    count += usize::from(store.get(VoxelCoord([x, y, z])).is_none());
                }
            }
        }
        count
    }

    #[test]
    fn a_sphere_of_radius_zero_is_the_centre_voxel_alone() {
        let mut store = cube([0; 3], [4; 3]);
        dig(&mut store, [2, 2, 2], 0);
        assert_eq!(dug(&store, [0; 3], [4; 3]), 1);
        assert_eq!(store.get(VoxelCoord([2, 2, 2])), None);
    }

    #[test]
    fn a_sphere_covers_exactly_the_voxels_within_the_radius() {
        // Counts of integer points with x^2 + y^2 + z^2 <= r^2, worked out independently.
        for (radius, count) in [(1, 7), (2, 33), (3, 123), (10, 4169)] {
            let r = i32::from(radius);
            let mut store = cube([-r - 2; 3], [r + 2; 3]);
            dig(&mut store, [0; 3], radius);
            assert_eq!(
                dug(&store, [-r - 2; 3], [r + 2; 3]),
                count,
                "radius {radius}"
            );
        }
    }

    #[test]
    fn the_rim_is_decided_by_exact_squared_distance() {
        let mut store = cube([-8; 3], [8; 3]);
        dig(&mut store, [0; 3], 5);
        // 3^2 + 4^2 + 0 = 25 is on the sphere; 3^2 + 4^2 + 1 = 26 is outside it.
        assert_eq!(store.get(VoxelCoord([3, 4, 0])), None);
        assert_eq!(store.get(VoxelCoord([-3, 0, -4])), None);
        assert_eq!(store.get(VoxelCoord([3, 4, 1])), Some(STONE));
        assert_eq!(store.get(VoxelCoord([5, 1, 0])), Some(STONE));
        assert_eq!(store.get(VoxelCoord([5, 0, 0])), None);
    }

    #[test]
    fn a_sphere_straddling_brick_boundaries_and_zero_is_the_same_everywhere() {
        // Centred on a corner where eight bricks meet, with negative coordinates on one side.
        let mut store = cube([-12; 3], [11; 3]);
        dig(&mut store, [-1, -1, -1], 6);
        let mut want = 0;
        for x in -12..12i32 {
            for y in -12..12i32 {
                for z in -12..12i32 {
                    let inside = (x + 1).pow(2) + (y + 1).pow(2) + (z + 1).pow(2) <= 36;
                    want += usize::from(inside);
                    assert_eq!(
                        store.get(VoxelCoord([x, y, z])).is_none(),
                        inside,
                        "{x} {y} {z}"
                    );
                }
            }
        }
        assert_eq!(want, 925);
    }

    #[test]
    fn digging_air_changes_nothing_and_allocates_nothing() {
        let mut store = BrickStore::new();
        let undo = dig(&mut store, [4000, -50, 7], 10);
        assert!(store.is_empty());
        assert_eq!(undo.bricks().count(), 0);
    }

    #[test]
    fn digging_out_a_whole_brick_collapses_it_to_air() {
        let mut store = BrickStore::new();
        store.insert(BrickCoord([0, 0, 0]), Brick::solid(STONE));
        // Radius 7 about the brick's middle reaches every corner of it: 3.5^2 * 3 < 7^2.
        dig(&mut store, [3, 3, 3], 7);
        let brick = store.brick(BrickCoord([0, 0, 0])).unwrap();
        assert!(brick.is_air() && brick.is_uniform());
    }

    #[test]
    fn a_core_brick_dug_out_completely_hashes_differently_from_an_untouched_one() {
        // In the core a brick that is not stored is rock, so air held there is a real change.
        let untouched = core_store();
        let mut dug = core_store();
        dig(&mut dug, [3, 3, 3], 7);
        let brick = dug.brick(BrickCoord([0, 0, 0])).unwrap();
        assert!(brick.is_air(), "the brick is held, as air");
        assert_ne!(dug.content_hash(), untouched.content_hash());
        // Taking the brick out of the store, as a faulty Edit might, is seen as well.
        let mut deleted = dug.clone();
        deleted.remove(BrickCoord([0, 0, 0]));
        assert_ne!(dug.content_hash(), deleted.content_hash());
    }

    #[test]
    fn digging_part_of_a_brick_leaves_it_mixed_and_the_rest_intact() {
        let mut store = BrickStore::new();
        store.insert(BrickCoord([-1, -1, -1]), Brick::solid(STONE));
        dig(&mut store, [-1, -1, -1], 1);
        let brick = store.brick(BrickCoord([-1, -1, -1])).unwrap();
        assert!(!brick.is_uniform());
        assert_eq!(store.get(VoxelCoord([-1, -1, -1])), None);
        assert_eq!(store.get(VoxelCoord([-8, -8, -8])), Some(STONE));
    }

    #[test]
    fn undo_lists_only_the_bricks_that_changed_and_reverting_restores_them() {
        let mut store = cube([0; 3], [15; 3]);
        let before = store.content_hash();
        // Reaches the bricks around (8, 8, 8) but only digs where there is something.
        let undo = dig(&mut store, [7, 7, 7], 3);
        assert_eq!(undo.bricks().count(), 8);
        assert!(undo.bricks().is_sorted());
        assert_ne!(store.content_hash(), before);
        undo.revert(&mut store);
        assert_eq!(store.content_hash(), before);
    }

    #[test]
    fn reverting_removes_bricks_that_the_edit_had_to_create() {
        let mut store = core_store();
        let undo = dig(&mut store, [0, 0, 0], 3);
        assert!(!store.is_empty(), "the core is dug into");
        undo.revert(&mut store);
        assert!(store.is_empty());
    }

    #[test]
    fn a_dig_reads_the_core_from_the_stores_own_source_and_not_from_the_planet_generator() {
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 50,
            material: Material(9),
        });
        dig(&mut store, [0, 0, 0], 3);
        assert_eq!(store.get(VoxelCoord([4, 0, 0])), Some(Material(9)));
        // 200 Voxels out is inside the Planet's core, but not inside this store's.
        let undo = dig(&mut store, [200, 0, 0], 3);
        assert_eq!(undo.bricks().count(), 0);
        assert_eq!(store.get(VoxelCoord([200, 1, 0])), None);
    }

    #[test]
    fn without_a_source_a_brick_that_is_not_held_is_air_and_a_dig_leaves_it_so() {
        let mut store = BrickStore::new();
        let undo = dig(&mut store, [0, 0, 0], 3);
        assert_eq!(undo.bricks().count(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn digging_into_the_core_stores_the_rock_that_is_there_and_digs_it() {
        let mut store = core_store();
        dig(&mut store, [0, 0, 0], 3);
        assert_eq!(store.get(VoxelCoord([0, 0, 0])), None);
        assert_eq!(store.get(VoxelCoord([3, 0, 0])), None);
        assert_eq!(store.get(VoxelCoord([4, 0, 0])), Some(ROCK));
        assert_eq!(store.get(VoxelCoord([-1, -1, -1])), None);
        assert_eq!(store.get(VoxelCoord([-2, -2, -2])), Some(ROCK));
        // The eight bricks around the centre, and no more.
        assert_eq!(store.len(), 8);
    }

    #[test]
    fn a_dig_that_only_grazes_a_core_brick_stores_no_extra_bricks() {
        let mut store = core_store();
        // The sphere's bounding cube reaches brick (1, 1, 1) at (8, 8, 8), which is outside it.
        dig(&mut store, [7, 7, 7], 1);
        assert_eq!(
            store.sorted_coords(),
            [
                BrickCoord([0, 0, 0]),
                BrickCoord([0, 0, 1]),
                BrickCoord([0, 1, 0]),
                BrickCoord([1, 0, 0]),
            ]
        );
    }

    #[test]
    fn an_edit_at_the_edge_of_the_coordinate_range_does_not_overflow() {
        let mut store = core_store();
        for centre in [[i32::MAX; 3], [i32::MIN; 3]] {
            dig(&mut store, centre, 20);
        }
        assert!(store.is_empty());
        // Nor does any other kind, though a Brush there places what fits in the range.
        for op in every_kind() {
            for centre in [[i32::MAX; 3], [i32::MIN; 3], [i32::MAX, 0, i32::MIN]] {
                apply(
                    &mut store,
                    &EditOp {
                        centre: VoxelCoord(centre),
                        ..op
                    },
                );
            }
        }
    }

    #[test]
    fn the_same_edits_in_the_same_order_give_the_same_bricks() {
        let run = || {
            let mut store = cube([-20; 3], [20; 3]);
            for centre in [[0, 0, 0], [5, -3, 9], [-8, -8, 8], [0, 0, 1]] {
                dig(&mut store, centre, 7);
            }
            store.content_hash()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn validate_accepts_up_to_the_largest_brush_and_names_the_reason_otherwise() {
        let op = |centre, radius| EditOp::dig_sphere(VoxelCoord(centre), radius);
        assert_eq!(validate(&op([0; 3], MAX_BRUSH_RADIUS)), Ok(()));
        assert_eq!(
            validate(&op([0; 3], MAX_BRUSH_RADIUS + 1)),
            Err(RejectReason::BrushTooLarge)
        );
        let edge = MAX_COORD;
        assert_eq!(validate(&op([edge, -edge, edge], 20)), Ok(()));
        for centre in [[edge + 1, 0, 0], [0, -edge - 1, 0], [0, 0, i32::MAX]] {
            assert_eq!(validate(&op(centre, 20)), Err(RejectReason::OutOfRange));
        }
        let turned = |rotation| EditOp {
            rotation,
            ..op([0; 3], 5)
        };
        assert_eq!(validate(&turned(7)), Ok(()));
        assert_eq!(validate(&turned(8)), Err(RejectReason::BadRotation));
    }

    #[test]
    fn validate_turns_down_a_place_or_flatten_of_a_material_the_table_does_not_define() {
        let place = |mode, material| EditOp {
            mode,
            material: Material(material),
            ..EditOp::dig_sphere(VoxelCoord([0; 3]), 3)
        };
        // 1 is rock and 55 the last light, both in the table; 0 and 56 are not.
        for mode in [EditMode::Place, EditMode::Flatten] {
            assert_eq!(validate(&place(mode, 1)), Ok(()));
            assert_eq!(validate(&place(mode, 55)), Ok(()));
            assert_eq!(
                validate(&place(mode, 0)),
                Err(RejectReason::UnknownMaterial)
            );
            assert_eq!(
                validate(&place(mode, 56)),
                Err(RejectReason::UnknownMaterial)
            );
        }
        // A dig and a smooth fill with nothing, so what the field says is ignored.
        for mode in [EditMode::Dig, EditMode::Smooth] {
            assert_eq!(validate(&place(mode, 60_000)), Ok(()));
        }
    }

    #[test]
    fn the_budget_takes_20_edits_a_burst_of_voxels_and_of_work_and_gives_them_back_by_the_tick() {
        let mut budget = EditBudget::full();
        for _ in 0..OPS_PER_SECOND {
            assert!(budget.has_op() && budget.take_op());
        }
        assert!(!budget.take_op() && !budget.has_op());
        // Three ticks bring back an Edit.
        for _ in 0..TICK_RATE_HZ / OPS_PER_SECOND {
            budget.refill();
        }
        assert!(budget.take_op() && !budget.take_op());

        // Voxels changed: the burst at once, and 64 000 (8 m^3) a second.
        let mut budget = EditBudget::full();
        assert!(budget.has_volume(u64::from(VOXEL_BURST)));
        assert!(!budget.has_volume(u64::from(VOXEL_BURST) + 1));
        assert!(!budget.has_volume(u64::MAX));
        assert!(budget.take_volume(u64::from(VOXEL_BURST) - 2_000));
        assert!(
            !budget.take_volume(2_001),
            "only 2 000 left, and nothing was taken"
        );
        assert!(budget.take_volume(2_000));
        assert!(!budget.take_volume(1));
        for _ in 0..TICK_RATE_HZ {
            budget.refill();
        }
        assert!(budget.take_volume(u64::from(VOXELS_PER_SECOND)) && !budget.take_volume(1));

        // Work: a burst that is more than any one Edit can ask, and the rate a second.
        let mut budget = EditBudget::full();
        assert!(budget.has_work(WORK_BURST) && !budget.has_work(WORK_BURST + 1));
        assert!(!budget.has_work(u64::MAX));
        assert!(budget.take_work(WORK_BURST - 5));
        assert!(!budget.take_work(6) && budget.take_work(5));
        // A tick brings back its share of the rate, and a second all of the burst, and no more.
        budget.refill();
        assert!(
            budget.take_work(WORK_PER_SECOND / u64::from(TICK_RATE_HZ)) && !budget.take_work(1)
        );
        for _ in 0..TICK_RATE_HZ {
            budget.refill();
        }
        assert!(budget.has_work(WORK_BURST) && !budget.has_work(WORK_BURST + 1));
        // The three do not draw on each other.
        let mut budget = EditBudget::full();
        budget.take_work(WORK_BURST);
        assert!(budget.take_op() && budget.take_volume(1));
    }

    #[test]
    fn what_an_edit_asks_of_the_server_is_its_box_and_a_smooth_asks_six_times_as_much() {
        let at = |mode, centre, radius| EditOp {
            mode,
            ..EditOp::dig_sphere(VoxelCoord(centre), radius)
        };
        let dig = at(EditMode::Dig, [1500, 0, 0], 10);
        assert_eq!(work(&dig), 21 * 21 * 21);
        assert_eq!(work(&at(EditMode::Place, [1500, 0, 0], 10)), 21 * 21 * 21);
        assert_eq!(
            work(&at(EditMode::Smooth, [1500, 0, 0], 10)),
            SMOOTH_WEIGHT * 21 * 21 * 21
        );
        assert_eq!(
            work(&at(EditMode::Dig, [MAX_COORD + 1, 0, 0], 10)),
            u64::MAX
        );
    }

    /// Every Edit that [`validate`] allows, of the biggest Brushes at the places that lean them the
    /// most, asks for no more than the bursts hold, so that each can be used from rest.
    #[test]
    fn no_edit_that_is_allowed_asks_for_more_than_the_bursts_and_a_leaning_cube_asks_a_lot() {
        let mut most = 0;
        for mode in [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ] {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                for rotation in 0..8 {
                    // The centres that lean the most from the grid, and some that do not.
                    for centre in [
                        [1100, 700, 700],
                        [1000, 1000, 1000],
                        [3, 4, 12],
                        [-870, 870, 870],
                        [1500, 0, 0],
                        [999_983, 777_777, -555_555],
                        [MAX_COORD, MAX_COORD, MAX_COORD],
                    ] {
                        let radius = if matches!(mode, EditMode::Smooth | EditMode::Flatten) {
                            MAX_SMOOTH_RADIUS
                        } else {
                            MAX_BRUSH_RADIUS
                        };
                        let op = EditOp {
                            mode,
                            brush: Brush { shape, radius },
                            centre: VoxelCoord(centre),
                            material: STONE,
                            alignment: Alignment::Local,
                            rotation,
                        };
                        assert_eq!(validate(&op), Ok(()), "{op:?}");
                        assert!(work(&op) <= WORK_BURST, "{op:?}: {}", work(&op));
                        let cube = (2 * u64::from(radius) + 1).pow(3);
                        assert!(cube <= u64::from(VOXEL_BURST), "{op:?}: {cube}");
                        most = most.max(work(&op));
                    }
                }
            }
        }
        // A tilted cube of 2 m across is most of it.
        assert!(most > 300_000, "{most}");
    }

    #[test]
    fn the_biggest_brushes_are_metric_and_the_bursts_just_afford_them() {
        // 2 m and 1 m in radius: the Edit rate is in metres, so the bursts keep up with the Brush.
        assert_eq!((MAX_BRUSH_RADIUS, MAX_SMOOTH_RADIUS), (40, 20));
        assert_eq!((VOXELS_PER_SECOND, VOXEL_BURST), (64_000, 576_000));
        // One more Voxel of radius and a cube would be more than the bursts hold.
        let side = 2 * u64::from(MAX_BRUSH_RADIUS) + 3;
        let volume = side.pow(3);
        assert!(volume > u64::from(VOXEL_BURST) || 51 * volume > 10 * WORK_BURST);
        // A Smooth's is a metre because that is its limit; the bursts would afford a little more.
        assert!(affordable_radius(SMOOTH_WEIGHT) >= MAX_SMOOTH_RADIUS);
    }

    #[test]
    fn a_smooth_or_a_flatten_is_no_bigger_than_1_m_and_a_dig_or_a_place_than_2_m() {
        let with = |mode, radius| EditOp {
            mode,
            ..EditOp::dig_sphere(VoxelCoord([5, 5, 5]), radius)
        };
        let material = |op: EditOp| EditOp {
            material: STONE,
            ..op
        };
        for mode in [EditMode::Smooth, EditMode::Flatten] {
            assert_eq!(validate(&material(with(mode, MAX_SMOOTH_RADIUS))), Ok(()));
            assert_eq!(
                validate(&material(with(mode, MAX_SMOOTH_RADIUS + 1))),
                Err(RejectReason::BrushTooLarge)
            );
        }
        for mode in [EditMode::Dig, EditMode::Place] {
            assert_eq!(validate(&material(with(mode, MAX_BRUSH_RADIUS))), Ok(()));
            assert_eq!(
                validate(&material(with(mode, MAX_BRUSH_RADIUS + 1))),
                Err(RejectReason::BrushTooLarge)
            );
        }
    }

    fn hardness(name: &str) -> f32 {
        MaterialTable::built_in().by_name(name).unwrap().hardness
    }

    #[test]
    fn the_time_of_a_dig_scales_with_the_hardness_and_is_counted_in_whole_ticks() {
        let seconds = |name| dig_seconds(hardness(name));
        assert!((seconds("sand") - 0.15).abs() < 1e-9);
        assert!((seconds("obsidian") - 1.2).abs() < 1e-9);
        assert!((dig_seconds(4.0) - 2.0 * dig_seconds(2.0)).abs() < 1e-12);
        assert_eq!(dig_ticks(hardness("sand")), 9);
        assert_eq!(dig_ticks(hardness("obsidian")), 72);
        // Water has none, and still takes the least time.
        assert_eq!(dig_seconds(hardness("water")), MIN_DIG_SECONDS);
        assert_eq!(dig_seconds(0.0), MIN_DIG_SECONDS);
        assert_eq!(dig_ticks(0.0), 3);
    }

    #[test]
    fn reach_is_8_m_from_the_eyes_to_the_middle_of_the_centre_voxel() {
        let eye = [10.0, 20.0, 30.0];
        assert!(within_reach(eye, VoxelCoord([169, 19, 29])));
        assert!(!within_reach(eye, VoxelCoord([170, 19, 29])));
        assert!(!within_reach([f64::NAN; 3], VoxelCoord([0; 3])));
    }

    /// Every kind of Edit, at places that are in the air, in solid rock and across the two.
    fn every_kind() -> Vec<EditOp> {
        let mut ops = Vec::new();
        for mode in [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ] {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                for alignment in [Alignment::Local, Alignment::World] {
                    for centre in [[1, 2, 3], [-9, 4, -5], [300, 280, 3], [-7, -7, -7]] {
                        ops.push(EditOp {
                            mode,
                            brush: Brush { shape, radius: 6 },
                            centre: VoxelCoord(centre),
                            material: STONE,
                            alignment,
                            rotation: 0,
                        });
                    }
                }
            }
        }
        ops
    }

    #[test]
    fn the_voxels_an_edit_changes_are_exactly_the_ones_the_preview_lists() {
        // The core with a hole dug at the origin: rock, air and the surface between.
        let mut base = core_store();
        dig(&mut base, [0, 0, 0], 10);
        for op in every_kind() {
            let mut store = base.clone();
            let listed = crate::plan(&store, &op);
            apply(&mut store, &op);
            let reach = 14;
            let mut seen = 0;
            for x in -reach..=reach {
                for y in -reach..=reach {
                    for z in -reach..=reach {
                        let voxel = VoxelCoord([
                            op.centre.0[0] + x,
                            op.centre.0[1] + y,
                            op.centre.0[2] + z,
                        ]);
                        let (was, now) = (base.voxel_at(0, voxel), store.voxel_at(0, voxel));
                        let change = listed.iter().find(|c| c.voxel == voxel);
                        assert_eq!(was != now, change.is_some(), "{op:?} at {voxel:?}");
                        if let Some(change) = change {
                            assert_eq!(now, change.to, "{op:?} at {voxel:?}");
                            seen += 1;
                        }
                    }
                }
            }
            assert_eq!(seen, listed.len(), "{op:?} lists Voxels beyond its reach");
        }
    }

    /// Solid STONE bricks from brick `lo` to brick `hi` inclusive, with `levels` coarse levels.
    fn bricks_with_pyramid(lo: i32, hi: i32, levels: usize) -> BrickStore {
        let mut store = BrickStore::new();
        for x in lo..=hi {
            for y in lo..=hi {
                for z in lo..=hi {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        store.build_lod(levels);
        store
    }

    /// The pyramid that `build_lod` makes of `store`'s level 0 bricks and source.
    fn rebuilt(store: &BrickStore) -> BrickStore {
        let mut again = store.clone();
        again.build_lod(store.level_count() - 1);
        again
    }

    /// The coarse bricks, as `(level, coordinate)`, that read differently in the two stores.
    fn differing(before: &BrickStore, after: &BrickStore) -> Vec<(usize, BrickCoord)> {
        let mut differing = Vec::new();
        for level in 1..before.level_count() {
            let mut coords = before.level(level).unwrap().sorted_coords();
            coords.extend(after.level(level).unwrap().sorted_coords());
            coords.sort();
            coords.dedup();
            differing.extend(
                coords
                    .into_iter()
                    .filter(|&c| before.brick_at(level, c) != after.brick_at(level, c))
                    .map(|c| (level, c)),
            );
        }
        differing
    }

    /// Xorshift64*: the same "random" inputs on every machine.
    struct Dice(u64);

    impl Dice {
        fn below(&mut self, n: u32) -> i32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32 % n as i32
        }
    }

    #[test]
    fn a_crater_shows_at_the_coarsest_level_that_can_resolve_it() {
        // Stone from Voxel -16 to 47 on each axis, around level 5's Voxel 0.
        let mut store = bricks_with_pyramid(-2, 5, 6);
        // A sphere of radius 14 about (8, 8, 8) holds the 16³ cube of Voxels from 0 to 15
        // (its far corner is 8^2 * 3 = 192 <= 196 away), and no larger aligned cube.
        dig(&mut store, [8, 8, 8], 14);
        // Levels 1 to 4 have a coarse Voxel that is all dug out: the cube, at 8, 4, 2 and 1 wide.
        for (n, width) in [(1, 8), (2, 4), (3, 2), (4, 1)] {
            assert_eq!(
                store.voxel_at(n, VoxelCoord([width - 1; 3])),
                None,
                "level {n}"
            );
            assert_eq!(store.voxel_at(n, VoxelCoord([0; 3])), None, "level {n}");
        }
        // Level 5's Voxel is 32 wide, and only one of its eight children is dug out: it is
        // still stone.
        assert_eq!(store.voxel_at(5, VoxelCoord([0; 3])), Some(STONE));
        // The levels see the stone around the crater, too.
        assert_eq!(store.voxel_at(4, VoxelCoord([-1, 0, 0])), Some(STONE));
        assert_eq!(store.voxel_at(4, VoxelCoord([1, 1, 1])), Some(STONE));
    }

    #[test]
    fn every_level_after_an_edit_is_the_one_a_rebuild_from_level_0_would_make() {
        let mut store = bricks_with_pyramid(-2, 2, 5);
        for centre in [[0, 0, 0], [7, -3, 9], [-8, -8, 8], [0, 0, 1], [19, 19, 19]] {
            dig(&mut store, centre, 17);
            assert_eq!(
                store.lod_hash(),
                rebuilt(&store).lod_hash(),
                "after {centre:?}"
            );
        }
    }

    #[test]
    fn an_edit_remakes_only_the_ancestors_of_the_bricks_it_changed() {
        let mut store = bricks_with_pyramid(-3, 3, 4);
        // Stone far from the Edits, whose coarse bricks must be left alone.
        for far in [[30, 0, 0], [-40, 25, -9]] {
            store.insert(BrickCoord(far), Brick::solid(STONE));
        }
        store.build_lod(4);

        // Inside one brick: its ancestor on level 1, which reads differently, and the three
        // others that are over bricks beside it (those on its low sides). They are remade
        // again on level 2, where the hole is too small to show and so nothing changes: that
        // is the end of it.
        let before = store.clone();
        let undo = dig(&mut store, [4, 4, 4], 3);
        assert_eq!(undo.bricks().count(), 1);
        assert_eq!(undo.lod().remade, 2 * 4);
        assert_eq!(undo.lod().changed, differing(&before, &store));
        assert!(undo.lod().changed.contains(&(1, BrickCoord([0; 3]))));
        assert!(
            undo.lod()
                .changed
                .iter()
                .all(|&(_, c)| c == BrickCoord([0; 3]))
        );

        // On the corner where eight bricks meet: bricks -1 and 0 never share a parent, so level
        // 1 has eight ancestors. Four of them read differently, and the coarse bricks over those
        // and the ones beside them are seven. The crater is too small to show on level 2.
        let before = store.clone();
        let undo = dig(&mut store, [0, 0, 0], 2);
        assert_eq!(undo.bricks().count(), 8);
        assert_eq!(undo.lod().remade, 8 + 7);
        let changed = differing(&before, &store);
        assert!(!changed.is_empty());
        assert_eq!(undo.lod().changed, changed);
        let over = |&(_, c): &(usize, BrickCoord)| c.0.iter().all(|&a| a == 0 || a == -1);
        assert!(changed.iter().all(over), "{changed:?}");
    }

    #[test]
    fn reverting_reports_the_coarse_bricks_it_put_back() {
        let mut store = bricks_with_pyramid(-2, 2, 4);
        let before = store.clone();
        let undo = dig(&mut store, [3, 3, 3], 6);
        let dug = undo.lod().clone();
        assert!(!dug.changed.is_empty());
        let reverted = undo.revert(&mut store);
        assert_eq!(reverted.remade, dug.remade);
        assert_eq!(reverted.changed, dug.changed);
        assert!(differing(&before, &store).is_empty());
    }

    /// A rock planet of 72 Voxels' radius, with a core of rock to 40 that is not held unless
    /// `hold_core`. Every brick not wholly inside the core is held, so its crust is.
    fn rock_planet(hold_core: bool) -> BrickStore {
        let core = Interior {
            radius: 40,
            material: ROCK,
        };
        let mut store = BrickStore::new();
        for i in 0..18 * 18 * 18 {
            let coord = BrickCoord([i % 18 - 9, i / 18 % 18 - 9, i / 324 - 9]);
            if coord.within_ball(72) && (hold_core || !coord.within_ball(core.radius)) {
                store.insert(coord, Brick::solid(ROCK));
            }
        }
        if !hold_core {
            store.set_source(core);
        }
        store.build_lod(8);
        store
    }

    #[test]
    fn digging_into_an_implied_core_makes_the_same_levels_as_digging_a_planet_that_holds_it() {
        let (mut implied, mut whole) = (rock_planet(false), rock_planet(true));
        assert!(whole.len() > implied.len() + 50);
        let digs = [
            ([0, 0, 0], 20),
            ([38, 3, -2], 15),
            ([-41, 41, 0], 12),
            ([0, 0, 57], 20),
            ([0, 0, 0], 1),
            ([60, 0, 0], 20),
        ];
        for (centre, radius) in digs {
            dig(&mut implied, centre, radius);
            dig(&mut whole, centre, radius);
            for level in 0..whole.level_count() {
                for coord in whole.level(level).unwrap().sorted_coords() {
                    assert_eq!(
                        implied.brick_at(level, coord),
                        whole.brick_at(level, coord),
                        "after {centre:?} level {level} {coord:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_edit_in_the_core_is_carried_up_through_the_rock_it_implies() {
        let mut store = core_store();
        store.build_lod(3);
        dig(&mut store, [0, 0, 0], 12);
        // The crater is 25 wide, and so shows at level 2, whose Voxels are 4 wide.
        assert_eq!(store.voxel_at(2, VoxelCoord([0, 0, 0])), None);
        assert_eq!(store.voxel_at(2, VoxelCoord([-1, -1, -1])), None);
        assert_eq!(store.voxel_at(2, VoxelCoord([7, 0, 0])), Some(ROCK));
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
    }

    #[test]
    fn reverting_an_edit_puts_every_level_back() {
        let mut store = bricks_with_pyramid(-2, 2, 5);
        let hash = store.lod_hash();
        let first = dig(&mut store, [0, 0, 0], 17);
        let second = dig(&mut store, [9, 9, 9], 12);
        assert_ne!(store.lod_hash(), hash);
        second.revert(&mut store);
        first.revert(&mut store);
        assert_eq!(store.lod_hash(), hash);
    }

    #[test]
    fn random_edits_and_reverts_over_an_interior_always_leave_the_levels_a_rebuild_would() {
        // A ball of radius 90, so every coarse brick from level 4 up is over its edge.
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 90,
            material: ROCK,
        });
        store.build_lod(9);
        let bare = (store.content_hash(), store.lod_hash());

        let mut dice = Dice(0x9E37_79B9_7F4A_7C15);
        // Each Edit with the hashes from before it was applied.
        let mut done: Vec<(Undo, (u64, u64))> = Vec::new();
        let (mut digs, mut reverts) = (0, 0);
        for step in 0..120 {
            if !done.is_empty() && dice.below(3) == 0 {
                let (undo, before) = done.pop().unwrap();
                undo.revert(&mut store);
                assert_eq!(
                    (store.content_hash(), store.lod_hash()),
                    before,
                    "revert at step {step}"
                );
                reverts += 1;
            } else {
                let before = (store.content_hash(), store.lod_hash());
                let centre = [0; 3].map(|_| dice.below(221) - 110);
                let undo = dig(&mut store, centre, dice.below(21) as u16);
                done.push((undo, before));
                digs += 1;
            }
            assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash(), "step {step}");
        }
        assert!(digs > 60 && reverts > 20, "{digs} digs, {reverts} reverts");

        // Taking back what is left gives the bare ball again.
        while let Some((undo, before)) = done.pop() {
            undo.revert(&mut store);
            assert_eq!((store.content_hash(), store.lod_hash()), before);
        }
        assert_eq!((store.content_hash(), store.lod_hash()), bare);
    }

    /// Ground 48 Voxels across, up to z = 20 (so that its layers are not on the boundaries of
    /// coarse Voxels): rock, with soil on it for the top 4 Voxels (the test Planet's), and its
    /// pyramid. The Materials differ, so how a coarse Voxel is made
    /// depends on which of its Voxels are exposed to air, and so on the bricks beside it.
    fn soil_over_rock(levels: usize) -> BrickStore {
        let mut store = BrickStore::new();
        for x in -24..24 {
            for y in -24..24 {
                for z in -16..21 {
                    store.set(
                        VoxelCoord([x, y, z]),
                        Some(if z >= 17 { SOIL } else { ROCK }),
                    );
                }
            }
        }
        store.build_lod(levels);
        store
    }

    #[test]
    fn random_digs_beside_brick_borders_always_leave_the_levels_a_rebuild_would() {
        let mut store = soil_over_rock(5);
        let mut dice = Dice(0x2545_F491_4F6C_DD1D);
        let mut edits: Vec<(Undo, (u64, u64))> = Vec::new();
        let (mut digs, mut reverts, mut changed) = (0, 0, 0);
        for step in 0..150 {
            if !edits.is_empty() && dice.below(3) == 0 {
                let (undo, before) = edits.pop().unwrap();
                undo.revert(&mut store);
                assert_eq!(
                    (store.content_hash(), store.lod_hash()),
                    before,
                    "revert at step {step}"
                );
                reverts += 1;
            } else {
                let before = (store.content_hash(), store.lod_hash());
                // Along each axis the centre is on a brick border (a multiple of 8) or a Voxel
                // from it, or the sphere ends flush with one, from either side. Then Voxels
                // change at a border and not past it, and the coarse Voxels over the bricks that
                // did not change have neighbours that did, in the next brick over.
                let radius = dice.below(15);
                let along = |dice: &mut Dice, up: i32| {
                    let border = 8 * (dice.below(7) - 3) + up;
                    match dice.below(3) {
                        0 => border + dice.below(3) - 1,
                        1 => border - 1 - radius + dice.below(2),
                        _ => border + radius - dice.below(2),
                    }
                };
                let centre = [
                    along(&mut dice, 0),
                    along(&mut dice, 0),
                    along(&mut dice, 16),
                ];
                let undo = dig(&mut store, centre, radius as u16);
                changed += undo.bricks().count();
                edits.push((undo, before));
                digs += 1;
            }
            assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash(), "step {step}");
        }
        assert!(digs > 70 && reverts > 20, "{digs} digs, {reverts} reverts");
        assert!(changed > 100, "{changed} bricks changed");
        while let Some((undo, before)) = edits.pop() {
            undo.revert(&mut store);
            assert_eq!((store.content_hash(), store.lod_hash()), before);
        }
        assert_eq!(store.lod_hash(), soil_over_rock(5).lod_hash());
    }

    #[test]
    fn an_edit_on_a_store_without_a_pyramid_reports_no_coarse_bricks() {
        let mut store = cube([0; 3], [8; 3]);
        let undo = dig(&mut store, [4, 4, 4], 2);
        assert!(undo.bricks().count() > 0);
        assert_eq!(*undo.lod(), LodRefresh::default());
        assert_eq!(store.level_count(), 1);
        assert_eq!(undo.revert(&mut store), LodRefresh::default());
    }

    /// A dig into bricks that the loader left out because they are buried reads, at level 0 and in
    /// the coarse levels, as it does in a store that was given every brick the source makes.
    #[test]
    fn a_dig_into_buried_bricks_that_were_left_out_is_what_a_dig_into_generated_ones_is() {
        use brick_store::children_to_generate;
        use planet_gen::planet::{Planet, tangents};
        use planet_gen::world_map::WorldMap;

        let planet = Planet::new(WorldMap::generate(0xB4A7_A9AC, 512));
        let ground = planet.spawn(100);
        let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
        let up = ground.map(|c| c / length);
        let (ahead, _) = tangents(up);
        let point: [f64; 3] = std::array::from_fn(|a| ground[a] + ahead[a] * 300.0);
        let radius = point.iter().map(|c| c * c).sum::<f64>().sqrt();
        let at = |r: f64| VoxelCoord(point.map(|c| (c / radius * r).round() as i32));

        let mut left_out = Vec::new();
        let mut skipping = planet.crust_around(ground, 100, 2);
        skipping.stream_from(1);
        let mut everything = skipping.clone();
        let source = skipping.source().unwrap();
        // Down the column under the point, the first brick whose children have one that is left
        // out (some are, a little under the ground and away from caves).
        let parent = (0..80)
            .map(|d| at(radius + 100.0 - f64::from(d) * 25.0).brick().parent())
            .find(|&parent| {
                let kept = children_to_generate(&*source, 1, parent, |_| false);
                left_out = parent
                    .children()
                    .into_iter()
                    .filter(|c| !kept.contains(c) && source.solid_below(0, *c))
                    .collect();
                !left_out.is_empty()
            })
            .expect("a buried brick under the ground");
        let held = brick_store::children_from(&*source, 1, parent, |_| false);
        assert!(held.len() < 8);
        skipping.hold_children(1, parent, held);
        // The other store has all of them, the way a load that did not skip would.
        let all = parent
            .children()
            .into_iter()
            .map(|c| (c, source.brick(0, c)))
            .filter(|(_, brick)| !brick.is_air())
            .collect();
        everything.hold_children(1, parent, all);
        for &c in &left_out {
            assert!(
                skipping.reads_full_from_source(0, c)
                    && !skipping.holds(0, c)
                    && everything.holds(0, c)
            );
            assert_eq!(skipping.brick_at(0, c), everything.brick_at(0, c));
        }

        // Dig into the middle of one of them, a ball that reaches into its neighbours.
        let into = left_out[0].origin().0.map(|v| v + 4);
        let op = EditOp::dig_sphere(VoxelCoord(into), 14);
        apply(&mut skipping, &op);
        apply(&mut everything, &op);
        assert!(skipping.holds(0, left_out[0]), "the dig held it");

        let first = VoxelCoord(into).brick();
        for level in 0..3 {
            let reach = if level == 0 { 3 } else { 2 };
            let base = if level == 0 {
                first
            } else {
                ancestor(first, level)
            };
            for dx in -reach..=reach {
                for dy in -reach..=reach {
                    for dz in -reach..=reach {
                        let c = BrickCoord([base.0[0] + dx, base.0[1] + dy, base.0[2] + dz]);
                        assert_eq!(
                            skipping.brick_at(level, c),
                            everything.brick_at(level, c),
                            "level {level} {c:?}"
                        );
                        assert_eq!(
                            skipping.solid_below(level, c),
                            everything.solid_below(level, c),
                            "solid below, level {level} {c:?}"
                        );
                    }
                }
            }
        }
    }

    /// The client streams the Planet in and the server does not; an Edit taken back must leave
    /// them reading the same Voxels, whether or not the bricks it touched have been resident.
    #[test]
    fn an_edit_taken_back_after_its_bricks_became_resident_leaves_client_and_server_agreeing() {
        use planet_gen::planet::{Planet, tangents};
        use planet_gen::world_map::WorldMap;

        let planet = Planet::new(WorldMap::generate(0xB4A7_A9AC, 512));
        let ground = planet.spawn(100);
        let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
        let up = ground.map(|c| c / length);
        let (ahead, _) = tangents(up);
        // Ground 300 Voxels from the spawn, outside the ring of 100 that is generated.
        let point: [f64; 3] = std::array::from_fn(|a| ground[a] + ahead[a] * 300.0);
        let radius = point.iter().map(|c| c * c).sum::<f64>().sqrt();
        let at = |r: f64| VoxelCoord(point.map(|c| (c / radius * r).round() as i32));
        let surface = (0..4000)
            .map(|d| radius + 800.0 - f64::from(d) * 0.5)
            .find(|&r| planet.voxel(at(r)).is_some())
            .expect("ground under the point");
        let centre = at(surface).0;

        let mut client = planet.crust_around(ground, 100, 2);
        client.stream_from(1);
        let mut server = planet.crust_around(ground, 100, 0);

        let op = EditOp::dig_sphere(VoxelCoord(centre), 6);
        // The Edit comes before the bricks under its parent are resident...
        let undo_client = apply(&mut client, &op);
        let undo_server = apply(&mut server, &op);
        // ...then they are, a brick's children at a time, as the loader does (never over a held brick).
        let source = client.source().unwrap();
        for brick in undo_client.bricks().collect::<Vec<_>>() {
            let parent = brick.parent();
            let bricks = brick_store::children_from(&*source, 1, parent, |c| client.holds(0, c));
            client.hold_children(1, parent, bricks);
            assert!(client.children_resident(1, parent));
        }
        // Then it is taken back.
        undo_client.revert(&mut client);
        undo_server.revert(&mut server);

        let first = VoxelCoord(centre).brick();
        for dx in -2..=2 {
            for dy in -2..=2 {
                for dz in -2..=2 {
                    let brick = BrickCoord([first.0[0] + dx, first.0[1] + dy, first.0[2] + dz]);
                    for i in 0..512 {
                        let v = brick.voxel([(i & 7) as u8, (i >> 3 & 7) as u8, (i >> 6) as u8]);
                        assert_eq!(
                            client.voxel_at(0, v),
                            server.voxel_at(0, v),
                            "{v:?} in {brick:?}"
                        );
                    }
                }
            }
        }
    }
}
