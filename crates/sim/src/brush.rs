//! What an Edit changes: the Voxels of its Brush, worked out from the op and the Planet's bricks
//! by integer maths in a fixed order, so that the client, the server and the ghost preview all
//! get the same list (ADR 0003, and `clippy.toml` bans what is not exact).
//!
//! # Orientation
//!
//! A Brush's axes are three mutually perpendicular integer vectors, each of whatever length is
//! convenient, so that no angle is ever turned into a number. The up axis is the local up at the
//! centre (the centre's own direction from the Planet's centre, which is at the origin) snapped to
//! the nearest of the 26 directions of the grid's cube (its faces, edges and corners: 45 degrees
//! apart, give or take), or the grid's +y in [`Alignment::World`]. The other two axes are the
//! cross products of up with the grid axis it is least along. A Voxel is then in a cube or a
//! cylinder when its offset from the centre, dotted with each axis, is within the radius times
//! that axis's length, which is compared squared and so stays in integers.

use brick_store::{BRICK_SIZE, BRICK_VOXELS, BrickCoord, BrickStore, Material, VoxelCoord};
use net::{Alignment, Brush, EditMode, EditOp, Shape};

type Vec3 = [i64; 3];

/// Up for [`Alignment::World`], and for a centre that has no direction (the Planet's own).
const GRID_UP: Vec3 = [0, 1, 0];

fn dot(a: Vec3, b: Vec3) -> i64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// One Voxel that an Edit changes: what it becomes (`None` for air), which is never what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub voxel: VoxelCoord,
    pub to: Option<Material>,
}

/// The direction among the 26 of the grid's cube that is nearest to `direction`, which must not be
/// zero. Compares the cosines exactly, as signed squares, and takes the first of a tie.
fn snapped(direction: Vec3) -> Vec3 {
    let mut best: Option<(Vec3, i128, i128)> = None;
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                let candidate = [x, y, z];
                let length2 = i128::from(dot(candidate, candidate));
                if length2 == 0 {
                    continue;
                }
                // The cosine is `along / (|direction| * sqrt(length2))`: the signed square of
                // `along` over `length2` orders them as it does, as `|direction|` is common.
                let along = i128::from(dot(direction, candidate));
                let signed_square = along.signum() * along * along;
                let better = best.is_none_or(|(_, best_square, best_length2)| {
                    signed_square * best_length2 > best_square * length2
                });
                if better {
                    best = Some((candidate, signed_square, length2));
                }
            }
        }
    }
    best.expect("there are 26 candidates").0
}

/// The direction up is for a Brush centred on `centre`: its axis, in the grid's frame, as the
/// smallest integer vector that points that way.
pub fn up(centre: VoxelCoord, alignment: Alignment) -> [i32; 3] {
    up_vector(centre, alignment).map(|c| c as i32)
}

fn up_vector(centre: VoxelCoord, alignment: Alignment) -> Vec3 {
    let radial = centre.0.map(i64::from);
    match alignment {
        Alignment::Local if radial != [0; 3] => snapped(radial),
        _ => GRID_UP,
    }
}

/// The Voxels a Brush covers, as a test of an offset from its centre.
struct Footprint {
    shape: Shape,
    radius: i64,
    /// Perpendicular, and not of length 1: the two across, then up.
    axes: [Vec3; 3],
    /// What each axis dotted with itself is.
    norms: [i64; 3],
}

impl Footprint {
    fn new(brush: Brush, up: Vec3) -> Self {
        // The grid axis that up is least along (the first of a tie), which cannot be parallel to
        // it, so the cross product is not zero.
        let least = (0..3).min_by_key(|&a| up[a].abs()).expect("three axes");
        let mut grid = [0; 3];
        grid[least] = 1;
        let across = cross(up, grid);
        let other = cross(up, across);
        let axes = [across, other, up];
        Self {
            shape: brush.shape,
            radius: i64::from(brush.radius),
            axes,
            norms: axes.map(|a| dot(a, a)),
        }
    }

    /// The farthest in Voxels along any grid axis from the centre that a covered Voxel can be:
    /// the sphere that the Brush's corners are on.
    fn bound(&self) -> i64 {
        let r2 = self.radius * self.radius;
        match self.shape {
            Shape::Sphere => self.radius,
            Shape::Cube => (3 * r2).isqrt(),
            Shape::Cylinder => (2 * r2).isqrt(),
        }
    }

    /// Whether the Voxel `d` from the centre is covered.
    fn contains(&self, d: Vec3) -> bool {
        let r2 = self.radius * self.radius;
        match self.shape {
            Shape::Sphere => dot(d, d) <= r2,
            Shape::Cube => (0..3).all(|i| {
                let along = dot(d, self.axes[i]);
                along * along <= r2 * self.norms[i]
            }),
            Shape::Cylinder => {
                let along = dot(d, self.axes[2]);
                let norm = self.norms[2];
                // The distance from the axis squared, times the axis's length squared.
                let across = dot(d, d) * norm - along * along;
                along * along <= r2 * norm && across <= r2 * norm
            }
        }
    }
}

/// How far from its centre a Brush can change a Voxel, along any grid axis, whatever its
/// orientation.
pub(crate) fn extent(brush: Brush) -> i64 {
    Footprint::new(brush, GRID_UP).bound()
}

/// The Voxels of a cube of bricks as they were before the Edit, read once.
struct Snapshot {
    first: Vec3,
    side: usize,
    cells: Vec<Option<Material>>,
}

impl Snapshot {
    /// The Voxels within `reach` of `centre` along each axis.
    fn new(store: &BrickStore, centre: Vec3, reach: i64) -> Self {
        let first = centre.map(|c| c - reach);
        let side = (2 * reach + 1) as usize;
        let mut snapshot = Self {
            first,
            side,
            cells: vec![None; side * side * side],
        };
        let (low, high) = bricks_around(centre, reach);
        for x in low[0]..=high[0] {
            for y in low[1]..=high[1] {
                for z in low[2]..=high[2] {
                    let coord = BrickCoord([x, y, z]);
                    let brick = store.brick_at(0, coord);
                    if brick.is_air() {
                        continue;
                    }
                    for i in 0..BRICK_VOXELS {
                        let local = [(i & 7) as u8, (i >> 3 & 7) as u8, (i >> 6) as u8];
                        let voxel = coord.voxel(local).0.map(i64::from);
                        if let (Some(material), Some(at)) =
                            (brick.get(local), snapshot.index(voxel))
                        {
                            snapshot.cells[at] = Some(material);
                        }
                    }
                }
            }
        }
        snapshot
    }

    fn index(&self, voxel: Vec3) -> Option<usize> {
        let [x, y, z] = std::array::from_fn(|a| voxel[a] - self.first[a]);
        let range = 0..self.side as i64;
        (range.contains(&x) && range.contains(&y) && range.contains(&z))
            .then(|| (x as usize) + self.side * ((y as usize) + self.side * (z as usize)))
    }

    fn get(&self, voxel: Vec3) -> Option<Material> {
        self.index(voxel).and_then(|at| self.cells[at])
    }
}

/// The first and last brick, clamped to the Voxel range, that the Voxels within `reach` of
/// `centre` along each axis are in.
fn bricks_around(centre: Vec3, reach: i64) -> ([i32; 3], [i32; 3]) {
    let brick = |sign: i64| {
        std::array::from_fn(|a| {
            let voxel = (centre[a] + sign * reach).clamp(i64::from(i32::MIN), i64::from(i32::MAX));
            (voxel as i32).div_euclid(BRICK_SIZE)
        })
    };
    (brick(-1), brick(1))
}

/// A Voxel becomes solid when at least this many of the 27 around it (itself included) are.
const SMOOTH_SOLID: u32 = 14;

/// What a Voxel becomes under [`EditMode::Smooth`]: `None` for air, and if it is solid now and
/// stays so, its own Material, which smoothing never changes. A Voxel that becomes solid takes the
/// Material most of its solid neighbours have, the lowest id of a tie.
fn smoothed(snapshot: &Snapshot, voxel: Vec3, now: Option<Material>) -> Option<Material> {
    let mut tally: Vec<(Material, u32)> = Vec::new();
    let mut solid = 0;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let near = [voxel[0] + dx, voxel[1] + dy, voxel[2] + dz];
                if let Some(material) = snapshot.get(near) {
                    solid += 1;
                    match tally.iter_mut().find(|(m, _)| *m == material) {
                        Some((_, count)) => *count += 1,
                        None => tally.push((material, 1)),
                    }
                }
            }
        }
    }
    if solid < SMOOTH_SOLID {
        return None;
    }
    now.or_else(|| {
        tally
            .into_iter()
            .max_by_key(|&(material, count)| (count, std::cmp::Reverse(material.0)))
            .map(|(material, _)| material)
    })
}

/// The Voxels that `op` changes in `store`, in ascending brick order and within a brick by index,
/// with what each becomes. Every one of them differs from what it is now: a dig lists only the
/// solid Voxels it covers, a place only the air ones. This is the one function that says what an
/// Edit does: [`crate::apply`] writes exactly this list, and the ghost preview shows it.
///
/// The op should have passed [`crate::validate`].
pub fn plan(store: &BrickStore, op: &EditOp) -> Vec<Change> {
    let centre = op.centre.0.map(i64::from);
    let up = up_vector(op.centre, op.alignment);
    let footprint = Footprint::new(op.brush, up);
    let reach = footprint.bound();
    // Flatten's plane is the tangent one, which is the exact radial direction and not the
    // snapped one.
    let normal = match op.alignment {
        Alignment::Local if centre != [0; 3] => centre,
        _ => GRID_UP,
    };
    let snapshot = Snapshot::new(store, centre, reach + 1);

    let mut changes = Vec::new();
    let (low, high) = bricks_around(centre, reach);
    for x in low[0]..=high[0] {
        for y in low[1]..=high[1] {
            for z in low[2]..=high[2] {
                let brick = BrickCoord([x, y, z]);
                for i in 0..BRICK_VOXELS {
                    let local = [(i & 7) as u8, (i >> 3 & 7) as u8, (i >> 6) as u8];
                    let voxel = brick.voxel(local);
                    let at = voxel.0.map(i64::from);
                    let d: Vec3 = std::array::from_fn(|a| at[a] - centre[a]);
                    if !footprint.contains(d) {
                        continue;
                    }
                    let now = snapshot.get(at);
                    let to = match op.mode {
                        EditMode::Dig => now.is_some().then_some(None),
                        EditMode::Place => now.is_none().then_some(Some(op.material)),
                        EditMode::Flatten if dot(normal, d) > 0 => now.is_some().then_some(None),
                        EditMode::Flatten => now.is_none().then_some(Some(op.material)),
                        EditMode::Smooth => {
                            let to = smoothed(&snapshot, at, now);
                            (to.is_some() != now.is_some()).then_some(to)
                        }
                    };
                    if let Some(to) = to {
                        changes.push(Change { voxel, to });
                    }
                }
            }
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::Shape;

    const STONE: Material = Material(7);
    const SAND: Material = Material(6);

    fn op(mode: EditMode, shape: Shape, radius: u16, centre: [i32; 3]) -> EditOp {
        EditOp {
            mode,
            brush: Brush { shape, radius },
            centre: VoxelCoord(centre),
            material: STONE,
            alignment: Alignment::Local,
        }
    }

    fn placed(shape: Shape, radius: u16, centre: [i32; 3], alignment: Alignment) -> Vec<Change> {
        let op = EditOp {
            alignment,
            ..op(EditMode::Place, shape, radius, centre)
        };
        plan(&BrickStore::new(), &op)
    }

    #[test]
    fn a_world_aligned_cube_is_the_box_that_reaches_radius_along_each_axis() {
        for radius in [0u16, 1, 3, 10] {
            let r = u64::from(radius);
            let cells = placed(Shape::Cube, radius, [5, -9, 3], Alignment::World);
            assert_eq!(cells.len() as u64, (2 * r + 1).pow(3), "radius {radius}");
        }
        // And it is where it should be: the corner is in, the Voxel past it is not.
        let cells = placed(Shape::Cube, 2, [0, 0, 0], Alignment::World);
        let has = |v: [i32; 3]| cells.iter().any(|c| c.voxel == VoxelCoord(v));
        assert!(has([2, -2, 2]) && has([-2, 2, -2]));
        assert!(!has([3, 0, 0]) && !has([0, -3, 0]) && !has([0, 0, 3]));
    }

    #[test]
    fn a_world_aligned_cylinder_is_a_disc_in_x_and_z_as_tall_as_it_is_wide() {
        // The integer points of a disc: 5, 13, 81 and 317 for radius 1, 2, 5 and 10.
        for (radius, disc) in [(1u16, 5usize), (2, 13), (5, 81), (10, 317)] {
            let cells = placed(Shape::Cylinder, radius, [0, 0, 0], Alignment::World);
            assert_eq!(
                cells.len(),
                disc * (2 * usize::from(radius) + 1),
                "{radius}"
            );
            assert!(
                cells
                    .iter()
                    .all(|c| c.voxel.0[1].abs() <= i32::from(radius))
            );
        }
    }

    #[test]
    fn a_cube_tilted_to_a_face_diagonal_is_counted_exactly() {
        // Up snaps to (1, 1, 0). With radius 2 the Voxels covered are those with |x + y| <= 2
        // (as 2 * sqrt 2 is 2.83), |x - y| <= 2 and |z| <= 2. Of the (x + y, x - y) pairs, 9 of
        // the 25 are both even and 4 both odd, 13 in all, for each of the 5 values of z.
        assert_eq!(up(VoxelCoord([100, 100, 3]), Alignment::Local), [1, 1, 0]);
        let cells = placed(Shape::Cube, 2, [100, 100, 3], Alignment::Local);
        assert_eq!(cells.len(), 13 * 5);
    }

    #[test]
    fn a_cylinder_tilted_to_a_face_diagonal_is_counted_exactly() {
        // The axis is (1, 1, 0): |x + y| <= 2 along it, and across it (x - y)^2 + 2 z^2 <= 8.
        // Of the (x + y, x - y) pairs, 13 have |x - y| <= 2 for z = 0 and for each of z = +-1,
        // and 3 have x - y = 0 for each of z = +-2: 13 + 26 + 6.
        let cells = placed(Shape::Cylinder, 2, [100, 100, 3], Alignment::Local);
        assert_eq!(cells.len(), 45);
    }

    #[test]
    fn up_is_the_nearest_of_the_26_directions_of_the_cube_and_exact_on_the_axes() {
        let up = |c: [i32; 3]| up(VoxelCoord(c), Alignment::Local);
        assert_eq!(up([1500, 0, 0]), [1, 0, 0]);
        assert_eq!(up([0, 0, -1500]), [0, 0, -1]);
        assert_eq!(up([1000, 1000, 5]), [1, 1, 0]);
        assert_eq!(up([-1000, 1000, 1000]), [-1, 1, 1]);
        // 22.5 degrees from the x axis towards y is the middle of the two.
        assert_eq!(up([1000, 400, 0]), [1, 0, 0]);
        assert_eq!(up([1000, 430, 0]), [1, 1, 0]);
        // Huge coordinates do not overflow, and the Planet's centre has the grid's up.
        assert_eq!(up([i32::MAX, i32::MIN, 0]), [1, -1, 0]);
        assert_eq!(up([0, 0, 0]), [0, 1, 0]);
        assert_eq!(
            super::up(VoxelCoord([1000, 5, 5]), Alignment::World),
            [0, 1, 0]
        );
    }

    /// The square of the cosine of the angle between two vectors, signed by the cosine (a test's
    /// own arithmetic, not the sim's; it avoids the inverse cosine, which the sim bans).
    fn cos2(a: [f64; 3], b: [f64; 3]) -> f64 {
        let dot: f64 = (0..3).map(|i| a[i] * b[i]).sum();
        let norm2 = |v: [f64; 3]| v.iter().map(|c| c * c).sum::<f64>();
        dot * dot.abs() / (norm2(a) * norm2(b))
    }

    /// cos^2 of 28 degrees, and of 20.
    const WITHIN_28: f64 = 0.7796;
    const WITHIN_20: f64 = 0.8830;

    #[test]
    fn up_is_never_far_from_the_radial_direction() {
        // A sweep of directions over the sphere, in a few thousand steps.
        let mut worst = 1.0f64;
        for i in -20..=20 {
            for j in -20..=20 {
                for k in -20..=20 {
                    if (i, j, k) == (0, 0, 0) {
                        continue;
                    }
                    let radial = [i * 1000 + 7, j * 1000 - 3, k * 1000 + 1];
                    let up = up(VoxelCoord(radial), Alignment::Local);
                    let aligned = cos2(radial.map(f64::from), up.map(f64::from));
                    worst = worst.min(aligned);
                }
            }
        }
        // Nearest of 26 directions: the worst is between a face and a corner, 27.4 degrees, and
        // the sweep finds directions at least 20 degrees off.
        assert!((WITHIN_28..WITHIN_20).contains(&worst), "{worst}");
    }

    #[test]
    fn a_cylinder_placed_on_the_side_of_the_planet_stands_upright_at_several_points() {
        // Points on a Planet of radius about 1500 Voxels, round it: on the axes, on the diagonals
        // and between them, with negative coordinates.
        let points = [
            [1500, 0, 0],
            [0, 1500, 0],
            [0, 0, -1500],
            [1060, 1060, 0],
            [-870, 870, 870],
            [1000, -700, 1000],
            [-1300, 400, -600],
            [300, -1400, 400],
        ];
        for centre in points {
            let cells = placed(Shape::Cylinder, 8, centre, Alignment::Local);
            // The axis of the voxels, as the line between the middles of the two halves on either
            // side of the plane through the centre that is square to the radial direction.
            let radial = centre.map(f64::from);
            let (mut top, mut bottom) = (([0.0; 3], 0.0), ([0.0; 3], 0.0));
            for cell in &cells {
                let d: [f64; 3] = std::array::from_fn(|a| f64::from(cell.voxel.0[a] - centre[a]));
                let side = if (0..3).map(|a| d[a] * radial[a]).sum::<f64>() > 0.0 {
                    &mut top
                } else {
                    &mut bottom
                };
                (0..3).for_each(|a| side.0[a] += d[a]);
                side.1 += 1.0;
            }
            let axis: [f64; 3] = std::array::from_fn(|a| top.0[a] / top.1 - bottom.0[a] / bottom.1);
            let upright = cos2(axis, radial);
            assert!(
                upright >= WITHIN_28,
                "{centre:?}: the cylinder leans, cos^2 {upright}"
            );
        }
    }

    #[test]
    fn a_world_aligned_brush_does_not_depend_on_where_it_is() {
        let at = |c: [i32; 3]| -> Vec<[i32; 3]> {
            placed(Shape::Cylinder, 6, c, Alignment::World)
                .iter()
                .map(|cell| std::array::from_fn(|a| cell.voxel.0[a] - c[a]))
                .collect()
        };
        let (mut here, mut there) = (at([0, 0, 0]), at([-777, 1500, 2_000_000]));
        // Listed by brick, which depends on where the centre is, so compare as sets.
        here.sort();
        there.sort();
        assert_eq!(here, there);
    }

    #[test]
    fn a_brush_at_the_edge_of_the_voxel_range_lists_only_voxels_that_exist() {
        for centre in [[i32::MAX, i32::MAX, i32::MAX], [i32::MIN, 0, i32::MIN]] {
            let cells = placed(Shape::Cube, 3, centre, Alignment::World);
            // Some of the cube is past the range and is not there.
            assert!(!cells.is_empty() && cells.len() < 343, "{}", cells.len());
        }
    }

    /// A store with a solid slab of `STONE` below y = 0, from -6 to 5 in x and z.
    fn slab() -> BrickStore {
        let mut store = BrickStore::new();
        for x in -6..6 {
            for z in -6..6 {
                for y in -6..0 {
                    store.set(VoxelCoord([x, y, z]), Some(STONE));
                }
            }
        }
        store
    }

    #[test]
    fn a_dig_lists_only_the_solid_voxels_and_a_place_only_the_air_ones() {
        let store = slab();
        // Centred on the surface layer: half of a radius 2 sphere is in the slab.
        let dig = plan(&store, &op(EditMode::Dig, Shape::Sphere, 2, [0, -1, 0]));
        assert!(dig.iter().all(|c| c.to.is_none() && c.voxel.0[1] < 0));
        let place = plan(&store, &op(EditMode::Place, Shape::Sphere, 2, [0, -1, 0]));
        assert!(
            place
                .iter()
                .all(|c| c.to == Some(STONE) && c.voxel.0[1] >= 0)
        );
        // Together they are the sphere's 33 Voxels, none twice.
        assert_eq!(dig.len() + place.len(), 33);
    }

    #[test]
    fn smoothing_leaves_a_flat_surface_alone_and_shaves_a_spike_and_fills_a_pit() {
        let mut store = slab();
        let flat = plan(&store, &op(EditMode::Smooth, Shape::Sphere, 4, [0, -1, 0]));
        assert!(flat.is_empty(), "{flat:?}");

        // One voxel standing on the surface, and one missing from it.
        store.set(VoxelCoord([0, 0, 0]), Some(SAND));
        store.set(VoxelCoord([2, -1, 2]), None);
        let changes = plan(&store, &op(EditMode::Smooth, Shape::Sphere, 4, [0, -1, 0]));
        assert!(changes.contains(&Change {
            voxel: VoxelCoord([0, 0, 0]),
            to: None
        }));
        // The pit is filled with what is round it, not with the spike's sand.
        assert!(changes.contains(&Change {
            voxel: VoxelCoord([2, -1, 2]),
            to: Some(STONE)
        }));
        assert_eq!(changes.len(), 2, "{changes:?}");
    }

    #[test]
    fn a_smoothed_voxel_takes_the_commonest_material_round_it_and_the_lowest_id_of_a_tie() {
        // A pit at the origin with the 26 Voxels round it: the layer below is all STONE and the
        // one above all SAND, and the 8 beside it are split. 13 of each is a tie.
        let mut store = BrickStore::new();
        fn around(store: &mut BrickStore, stone_beside: usize) {
            let mut beside = 0;
            for x in -1..=1 {
                for y in -1..=1 {
                    for z in -1..=1 {
                        let material = match (y, (x, z) == (0, 0)) {
                            (_, true) if y == 0 => continue,
                            (-1, _) => STONE,
                            (1, _) => SAND,
                            _ => {
                                beside += 1;
                                if beside <= stone_beside { STONE } else { SAND }
                            }
                        };
                        store.set(VoxelCoord([x, y, z]), Some(material));
                    }
                }
            }
        }
        let pit = |store: &BrickStore| {
            plan(store, &op(EditMode::Smooth, Shape::Sphere, 0, [0, 0, 0]))
                .first()
                .map(|c| c.to)
        };
        around(&mut store, 4);
        assert_eq!(pit(&store), Some(Some(SAND)), "tied: the lower id");
        around(&mut store, 5);
        assert_eq!(
            pit(&store),
            Some(Some(STONE)),
            "one more STONE: the commoner"
        );
    }

    #[test]
    fn flatten_cuts_what_is_above_the_tangent_plane_and_fills_what_is_below() {
        let mut store = slab();
        // A bump above y = 0, and a pit in the slab. Flatten at the centre of the surface, in
        // world alignment, so the plane is y = -1: the bump and the top layer go, and the pit,
        // which is under the plane, is filled.
        store.set(VoxelCoord([1, 0, 1]), Some(SAND));
        store.set(VoxelCoord([-2, -3, 2]), None);
        let flatten = EditOp {
            alignment: Alignment::World,
            ..op(EditMode::Flatten, Shape::Cube, 4, [0, -1, 0])
        };
        let changes = plan(&store, &flatten);
        let cut = |v: [i32; 3]| Change {
            voxel: VoxelCoord(v),
            to: None,
        };
        assert!(changes.contains(&cut([1, 0, 1])));
        assert!(changes.contains(&Change {
            voxel: VoxelCoord([-2, -3, 2]),
            to: Some(STONE)
        }));
        // Nothing at or below the plane is cut, and everything above it that is solid is.
        assert!(changes.iter().all(|c| match c.to {
            None => c.voxel.0[1] > -1,
            Some(_) => c.voxel.0[1] <= -1,
        }));
        assert_eq!(changes.len(), 2);

        // Tangent to the Planet at (1500, 0, 0): the plane is x = 1500, and what is past it goes.
        let mut wall = BrickStore::new();
        for x in 1496..=1504 {
            wall.set(VoxelCoord([x, 0, 0]), Some(STONE));
        }
        let changes = plan(
            &wall,
            &op(EditMode::Flatten, Shape::Sphere, 6, [1500, 0, 0]),
        );
        let cut: Vec<_> = changes.iter().filter(|c| c.to.is_none()).collect();
        assert_eq!(cut.len(), 4, "x = 1501..=1504");
        assert!(cut.iter().all(|c| c.voxel.0[0] > 1500));
    }
}
