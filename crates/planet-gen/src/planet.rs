//! The real Planet: 16 km in radius, made brick by brick from the [`WorldMap`].
//!
//! The world map gives the surface (an elevation and a biome per texel); this module gives what
//! lies above and below it. From the sky down, a Voxel is:
//! - **air**, or **water** if it is below sea level (static until Fluids);
//! - one of the biome's **strata** for [`STRATA_DEPTH`] Voxels below the ground, the topmost
//!   being the biome's surface Material (the sea floor has strata of its own);
//! - **deep rock**, below the strata, and
//! - the glowing **molten core**, within [`MOLTEN_RADIUS`] of the centre.
//!
//! The lowest the ground gets is [`TRENCH`] below sea level, so everything within
//! [`INTERIOR_RADIUS`] of the centre is core or deep rock whatever the world map says. That is
//! most of the Planet, and its bricks are uniform: one flag each, not memory. Only the bricks
//! near the ground need their Voxels evaluated, and most of those are decided by bounds on the
//! ground's height under them (see [`Planet::brick`]).
//!
//! Materials are named here by id constant, and a test checks each against the data file, which
//! is never read at run time: the output depends on these numbers and `GENERATOR_VERSION`, not
//! on what the data file calls a Material.

use std::fmt;
use std::sync::Arc;

use brick_store::{BRICK_SIZE, Brick, BrickCoord, BrickSource, BrickStore, Material, VoxelCoord};

pub use crate::SOIL;
use crate::world_map::{Biome, PEAK, TRENCH, WORLD_RADIUS, WorldMap};
use crate::{distance, par_bricks};

pub const GRASSY_SOIL: Material = Material(3);
pub const CLAY: Material = Material(4);
pub const GRAVEL: Material = Material(5);
pub const SAND: Material = Material(6);
pub const SANDSTONE: Material = Material(7);
pub const LIMESTONE: Material = Material(8);
pub const SHALE: Material = Material(9);
pub const GRANITE: Material = Material(10);
pub const BASALT: Material = Material(11);
pub const OBSIDIAN: Material = Material(12);
pub const DEEP_ROCK: Material = Material(13);
pub const SNOW: Material = Material(14);
pub const ICE: Material = Material(15);
/// The one Material of the terrain that gives off light.
pub const MOLTEN_CORE: Material = Material(16);
pub const WATER: Material = Material(27);

/// Radius of the molten core, in Voxels: 3.2 km.
pub const MOLTEN_RADIUS: i32 = 32_000;
/// Thickness of the strata, in Voxels: 48 m. Deep rock lies below.
pub const STRATA_DEPTH: i32 = 480;
/// Radius inside which a Voxel is core or deep rock whatever the world map says: below even the
/// lowest ground's strata.
pub const INTERIOR_RADIUS: i32 = WORLD_RADIUS - TRENCH - STRATA_DEPTH;

/// The interior's layers, innermost first, as (outer radius in Voxels, Material).
const INTERIOR: [(i32, Material); 2] = [(MOLTEN_RADIUS, MOLTEN_CORE), (INTERIOR_RADIUS, DEEP_ROCK)];

/// What the ground is made of, topmost first, as (depth at which the layer ends in Voxels,
/// Material). The last ends at [`STRATA_DEPTH`].
type Strata = [(i32, Material)];

const TEMPERATE: &Strata = &[
    (4, GRASSY_SOIL),
    (40, SOIL),
    (120, CLAY),
    (300, LIMESTONE),
    (STRATA_DEPTH, SHALE),
];
const DESERT: &Strata = &[
    (10, SAND),
    (120, SANDSTONE),
    (300, LIMESTONE),
    (STRATA_DEPTH, SHALE),
];
const SNOWY: &Strata = &[
    (6, SNOW),
    (40, ICE),
    (100, GRAVEL),
    (300, SHALE),
    (STRATA_DEPTH, GRANITE),
];
const VOLCANIC: &Strata = &[
    (6, BASALT),
    (30, OBSIDIAN),
    (300, BASALT),
    (STRATA_DEPTH, GRANITE),
];
const SEABED: &Strata = &[
    (4, SAND),
    (60, CLAY),
    (300, LIMESTONE),
    (STRATA_DEPTH, SHALE),
];

/// The strata of the ground in a biome, or of the sea floor.
fn strata(biome: Biome, seabed: bool) -> &'static Strata {
    match (seabed, biome) {
        (true, _) => SEABED,
        (_, Biome::Temperate) => TEMPERATE,
        (_, Biome::Desert) => DESERT,
        (_, Biome::Snow) => SNOWY,
        (_, Biome::Volcanic) => VOLCANIC,
    }
}

/// The Material at `depth` Voxels below the ground in `strata`: deep rock below them.
fn at_depth(strata: &Strata, depth: f64) -> Material {
    let layer = strata.iter().find(|&&(end, _)| depth < f64::from(end));
    layer.map_or(DEEP_ROCK, |&(_, material)| material)
}

/// The Material if everything from depth `shallow` to `deep` in `strata` is one.
fn one_layer(strata: &Strata, shallow: f64, deep: f64) -> Option<Material> {
    let material = at_depth(strata, shallow);
    (at_depth(strata, deep) == material
        && strata
            .iter()
            .all(|&(end, _)| !(shallow..deep).contains(&f64::from(end))))
    .then_some(material)
}

/// The interior's Material at `radius` Voxels from the centre, if it is within the interior.
fn interior(radius: f64) -> Option<Material> {
    INTERIOR
        .iter()
        .find(|&&(outer, _)| radius <= f64::from(outer))
        .map(|&(_, material)| material)
}

/// The sea's surface, as a distance in Voxels from the centre.
const SEA_RADIUS: f64 = WORLD_RADIUS as f64;

/// An upper bound on how far a Voxel of a brick is from the middle of its Voxels: the Voxels are
/// at their minimum corners, 0 to 7 from the brick's origin, so the true figure is
/// 3.5 * sqrt(3) = 6.06.
const REACH: f64 = 6.5;

/// Slack, in Voxels, so that rounding in the bounds on a brick's depths cannot flip a decision.
const SLACK: f64 = 1e-6;

/// Heights above sea level, in Voxels, a spawn's ground may be at: 20 to 100 m.
const SPAWN_ELEVATIONS: std::ops::RangeInclusive<i16> = 200..=1_000;

/// Most the ground may rise or fall across a spawn's region, in Voxels: 40 m. More would make
/// the region a lot of bricks.
const GENTLE: f64 = 400.0;

/// The Planet, as bricks. Cloning shares the world map, which carries the seed.
#[derive(Clone)]
pub struct Planet {
    map: Arc<WorldMap>,
}

impl fmt::Debug for Planet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Planet ({0}x{0} world map)", self.map.size())
    }
}

impl Planet {
    pub fn new(map: WorldMap) -> Planet {
        Planet { map: Arc::new(map) }
    }

    pub fn map(&self) -> &WorldMap {
        &self.map
    }

    /// The Voxel at `coord`: `None` for air.
    pub fn voxel(&self, coord: VoxelCoord) -> Option<Material> {
        let point = coord.0.map(f64::from);
        let radius = distance(point);
        if let Some(material) = interior(radius) {
            return Some(material);
        }
        let elevation = self.map.elevation_at(point);
        let depth = SEA_RADIUS + elevation - radius;
        if depth <= 0.0 {
            // Above the ground: water below sea level, else air.
            return (radius < SEA_RADIUS).then_some(WATER);
        }
        let strata = strata(self.map.sample(point).biome, elevation < 0.0);
        Some(at_depth(strata, depth))
    }

    /// Generates the brick at `coord`, from nothing but the world map and the coordinate.
    ///
    /// Most bricks are decided without looking at their Voxels: those in the interior by their
    /// distance from the centre, those above the highest peak, and the rest by bounds on the
    /// ground's height under the brick, from the few texels of the world map that it is over.
    /// Those say whether the brick is wholly sky, sea, deep rock or one stratum. A test checks
    /// that this agrees with [`Planet::voxel`] for every Voxel, which only a brick over a
    /// boundary between layers, or over the ground itself, needs.
    pub fn brick(&self, coord: BrickCoord) -> Brick {
        let origin = coord.origin().0.map(f64::from);
        let centre = distance(origin.map(|o| o + 3.5));
        let (near, far) = (centre - REACH, centre + REACH);
        if near >= f64::from(WORLD_RADIUS + PEAK) {
            return Brick::AIR;
        }
        if far <= f64::from(INTERIOR_RADIUS) {
            let mut inner = f64::NEG_INFINITY;
            for &(outer, material) in &INTERIOR {
                if near > inner && far <= f64::from(outer) {
                    return Brick::solid(material);
                }
                inner = f64::from(outer);
            }
            return self.evaluate(coord);
        }
        let corners: [[f64; 3]; 8] = std::array::from_fn(|i| {
            [0, 1, 2].map(|a| origin[a] + if i >> a & 1 == 1 { 7.0 } else { 0.0 })
        });
        // Every Voxel is in the hull of the corners, so the ground under it is within these.
        let (low, high) = self.map.elevation_range(&corners);
        // The brick's Voxels are between these many Voxels below the ground.
        let shallowest = SEA_RADIUS + low - far - SLACK;
        let deepest = SEA_RADIUS + high - near + SLACK;
        if deepest <= 0.0 {
            // All above the ground: sky, or sea if it is below sea level.
            if low >= 0.0 || near >= SEA_RADIUS {
                return Brick::AIR;
            }
            if far < SEA_RADIUS {
                return Brick::solid(WATER);
            }
        } else if shallowest >= f64::from(STRATA_DEPTH) {
            return Brick::solid(DEEP_ROCK);
        } else if shallowest > 0.0 && (low >= 0.0 || high < 0.0) {
            // Below the ground, all land or all sea floor, and one stratum if it is all in one
            // biome, which it is if the corners are in one texel.
            let texel = self.map.locate(corners[0]);
            if corners.iter().all(|&c| self.map.locate(c) == texel) {
                let strata = strata(self.map.sample(corners[0]).biome, high < 0.0);
                if let Some(material) = one_layer(strata, shallowest, deepest) {
                    return Brick::solid(material);
                }
            }
        }
        self.evaluate(coord)
    }

    /// The brick at `coord`, every Voxel worked out.
    fn evaluate(&self, coord: BrickCoord) -> Brick {
        let mut brick = Brick::AIR;
        for z in 0..8 {
            for y in 0..8 {
                for x in 0..8 {
                    let local = [x, y, z];
                    brick.set(local, self.voxel(coord.voxel(local)));
                }
            }
        }
        brick
    }

    /// Bounds `(lowest, highest)` on the ground's height above sea level within `reach` Voxels
    /// of the line from the centre through `point`, which is near the ground. The whole Planet's
    /// bounds if that is not over one face of the world map.
    fn heights_near(&self, point: [f64; 3], reach: i32) -> (f64, f64) {
        let up = point.map(|c| c / distance(point));
        let helper = if up[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let east = cross(up, helper);
        let east = east.map(|c| c / distance(east));
        let north = cross(up, east);
        // A square that holds the circle, with room for a brick, at the top of the ground,
        // which is a tenth wider than the bottom of the strata is.
        let half = f64::from(reach + 2 * BRICK_SIZE) * 1.1;
        let corners: Vec<[f64; 3]> = (0..4)
            .map(|i| {
                let s = if i & 1 == 0 { -half } else { half };
                let t = if i & 2 == 0 { -half } else { half };
                std::array::from_fn(|a| point[a] + east[a] * s + north[a] * t)
            })
            .collect();
        self.map.elevation_range(&corners)
    }

    /// A point on the ground of gentle, temperate land to spawn on: the first, going through the
    /// world map's texels on a lattice, where the ground is 20 to 100 m above the sea and
    /// varies by 40 m at most within `reach` Voxels (see [`Planet::region_around`]). The same
    /// for the same map.
    ///
    /// Panics if there is no such place on the Planet.
    pub fn spawn(&self, reach: i32) -> [f64; 3] {
        let size = self.map.size();
        let stride = (size / 32).max(1);
        for face in 0..6 {
            for j in (stride / 2..size).step_by(stride as usize) {
                for i in (stride / 2..size).step_by(stride as usize) {
                    let texel = self.map.texel(face, i, j);
                    if texel.biome != Biome::Temperate
                        || !SPAWN_ELEVATIONS.contains(&texel.elevation)
                    {
                        continue;
                    }
                    let direction = self.map.texel_direction(face, i, j);
                    let ground = direction.map(|d| d * (SEA_RADIUS + f64::from(texel.elevation)));
                    let (low, high) = self.heights_near(ground, reach);
                    if high - low <= GENTLE {
                        return ground;
                    }
                }
            }
        }
        panic!("no gentle temperate land on this Planet to spawn on");
    }

    /// A store of the ground within `reach` Voxels of the line from the centre through `spawn`
    /// (see [`Planet::spawn`]): every brick that is not air, from the bottom of the strata
    /// there to above the highest ground or the sea. The store's source is the Planet, so a
    /// brick it does not hold reads as the Planet's own at level 0, the deep rock under the
    /// region included, which costs a flag, not memory. Its coarse levels are not the Planet's
    /// beyond the region until they are generated directly (#47).
    pub fn region_around(&self, spawn: [f64; 3], reach: i32) -> BrickStore {
        let (low, high) = self.heights_near(spawn, reach);
        let up = spawn.map(|c| c / distance(spawn));
        // Along `up`, from a brick below the lowest strata to a brick above the highest ground
        // or the sea; across it, `reach` and a brick.
        let bottom = SEA_RADIUS + low - f64::from(STRATA_DEPTH + BRICK_SIZE);
        let top = SEA_RADIUS + high.max(0.0) + f64::from(BRICK_SIZE);
        let across = f64::from(reach + BRICK_SIZE);
        let ends = [bottom, top].map(|a| up.map(|u| u * a));
        let corner = |pick: fn(f64, f64) -> f64, margin: f64| {
            BrickCoord(std::array::from_fn(|a| {
                ((pick(ends[0][a], ends[1][a]) + margin) / f64::from(BRICK_SIZE)).floor() as i32
            }))
        };
        let (min, max) = (corner(f64::min, -across), corner(f64::max, across));
        let found = par_bricks(min, max, |coord| {
            let centre = coord.origin().0.map(|o| f64::from(o) + 3.5);
            let along: f64 = (0..3).map(|a| centre[a] * up[a]).sum();
            let sideways = centre.iter().map(|c| c * c).sum::<f64>() - along * along;
            if along < bottom || along > top || sideways > (across + REACH) * (across + REACH) {
                return None;
            }
            Some(self.brick(coord)).filter(|brick| !brick.is_air())
        });
        let mut store = BrickStore::new();
        store.set_source(self.clone());
        for (coord, brick) in found {
            store.insert(coord, brick);
        }
        store
    }
}

/// What a store reads where it holds no brick. At level 0 it is the Planet itself. At the coarse
/// levels it is only the interior, where a brick lies wholly within one of its layers, and air
/// elsewhere, until coarse bricks are generated directly (#47). A coarse brick is right where
/// it is made from held bricks.
impl BrickSource for Planet {
    fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
        if level == 0 {
            return Planet::brick(self, coord);
        }
        INTERIOR
            .iter()
            .find(|&&(outer, _)| coord.within_ball_at(level, outer))
            .map_or(Brick::AIR, |&(_, material)| Brick::solid(material))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_map::{Biome, SEA_LEVEL, Texel};
    use std::sync::OnceLock;

    const SEED: u64 = 0xB4A7_A9AC;
    const RADIUS: f64 = WORLD_RADIUS as f64;
    /// Texels along a face of the world map the tests share.
    const SIZE: u32 = 512;
    /// Voxels around the spawn the tests take: the world map of the tests is too coarse for more.
    const SPAWN_REACH: i32 = 100;

    fn planet() -> &'static Planet {
        static PLANET: OnceLock<Planet> = OnceLock::new();
        PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, SIZE)))
    }

    /// The Voxel `radius` Voxels from the centre along `direction`.
    fn along(direction: [f64; 3], radius: f64) -> VoxelCoord {
        let length = direction.iter().map(|d| d * d).sum::<f64>().sqrt();
        VoxelCoord(direction.map(|d| (d / length * radius).round() as i32))
    }

    /// The direction of the middle of the first texel for which `pick` holds.
    fn find(pick: impl Fn(&Texel) -> bool) -> [f64; 3] {
        let map = planet().map();
        (0..6)
            .flat_map(|f| {
                (0..map.size()).flat_map(move |j| (0..map.size()).map(move |i| (f, i, j)))
            })
            .find(|&(f, i, j)| pick(&map.texel(f, i, j)))
            .map(|(f, i, j)| map.texel_direction(f, i, j))
            .expect("the test map has such a texel")
    }

    /// The Voxels of a column along `direction`, from `from` to `to` Voxels above sea level, as
    /// runs of equal ones, bottom first, with the height where each starts. Rounding a point to
    /// a Voxel moves it up to a Voxel off the column, which can flip a Voxel next to a boundary
    /// between layers, so a single Voxel unlike both its neighbours is taken to be like them.
    fn column(direction: [f64; 3], from: f64, to: f64) -> Vec<(f64, Option<Material>)> {
        let heights: Vec<f64> = (0..)
            .map(|k| from + f64::from(k))
            .take_while(|&h| h <= to)
            .collect();
        let mut voxels: Vec<_> = heights
            .iter()
            .map(|&h| planet().voxel(along(direction, RADIUS + h)))
            .collect();
        for k in 1..voxels.len() - 1 {
            if voxels[k - 1] == voxels[k + 1] {
                voxels[k] = voxels[k - 1];
            }
        }
        let mut runs: Vec<(f64, Option<Material>)> = Vec::new();
        for (h, voxel) in heights.into_iter().zip(voxels) {
            if runs.last().map(|r| r.1) != Some(voxel) {
                runs.push((h, voxel));
            }
        }
        runs
    }

    fn voxels_of(coord: BrickCoord) -> impl Iterator<Item = ([u8; 3], VoxelCoord)> {
        (0..512usize).map(move |i| {
            let local = [(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8];
            (local, coord.voxel(local))
        })
    }

    /// Worlds already generated hold these ids; changing what they mean needs a
    /// `GENERATOR_VERSION` bump and new golden hashes.
    #[test]
    fn the_planet_constants_are_materials_of_the_shared_table() {
        let table = materials::MaterialTable::built_in();
        for (material, name) in [
            (SOIL, "soil"),
            (GRASSY_SOIL, "grassy soil"),
            (CLAY, "clay"),
            (GRAVEL, "gravel"),
            (SAND, "sand"),
            (SANDSTONE, "sandstone"),
            (LIMESTONE, "limestone"),
            (SHALE, "shale"),
            (GRANITE, "granite"),
            (BASALT, "basalt"),
            (OBSIDIAN, "obsidian"),
            (DEEP_ROCK, "deep rock"),
            (SNOW, "snow"),
            (ICE, "ice"),
            (MOLTEN_CORE, "molten core"),
            (WATER, "water"),
        ] {
            assert_eq!(table.get(material).map(|d| d.name.as_str()), Some(name));
        }
    }

    #[test]
    fn the_core_glows_and_the_ocean_is_water() {
        let table = materials::MaterialTable::built_in();
        assert!(table.get(MOLTEN_CORE).unwrap().emissive_intensity > 1.0);
        assert_eq!(table.get(WATER).unwrap().fluid, materials::FluidKind::Water);
    }

    #[test]
    fn every_biome_has_strata_that_end_where_the_deep_rock_begins() {
        for strata in [TEMPERATE, DESERT, SNOWY, VOLCANIC, SEABED] {
            assert!(strata.windows(2).all(|w| w[0].0 < w[1].0));
            assert_eq!(strata.last().unwrap().0, STRATA_DEPTH);
        }
        // Nothing the world map decides is inside the interior.
        assert_eq!(INTERIOR_RADIUS, 149_520);
        const { assert!(MOLTEN_RADIUS < INTERIOR_RADIUS) };
    }

    #[test]
    fn the_centre_is_molten_then_deep_rock_going_out() {
        let at = |x: i32| planet().voxel(VoxelCoord([x, 0, 0]));
        assert_eq!(at(0), Some(MOLTEN_CORE));
        assert_eq!(at(MOLTEN_RADIUS), Some(MOLTEN_CORE));
        assert_eq!(at(MOLTEN_RADIUS + 1), Some(DEEP_ROCK));
        assert_eq!(at(INTERIOR_RADIUS), Some(DEEP_ROCK));
    }

    #[test]
    fn deep_bricks_are_uniform_and_cost_a_flag() {
        for (c, want) in [
            ([0, 0, 0], MOLTEN_CORE),
            ([-1, -1, -1], MOLTEN_CORE),
            ([2000, 0, 0], MOLTEN_CORE),
            ([0, -6000, 0], DEEP_ROCK),
            ([9000, 9000, 9000], DEEP_ROCK),
            ([0, 0, 17_000], DEEP_ROCK),
        ] {
            let brick = planet().brick(BrickCoord(c));
            assert!(brick.is_uniform(), "{c:?}");
            assert_eq!(brick.get([3, 3, 3]), Some(want), "{c:?}");
            assert_eq!(brick.storage_bytes(), 0, "{c:?}");
        }
    }

    #[test]
    fn a_column_on_temperate_land_goes_deep_rock_shale_limestone_clay_soil_grass_then_air() {
        let d = find(|t| t.biome == Biome::Temperate && (300..600).contains(&t.elevation));
        let surface = planet().map().elevation_at(d);
        let runs = column(d, surface - 520.0, surface + 20.0);
        let found: Vec<_> = runs.iter().map(|r| r.1).collect();
        let want = [DEEP_ROCK, SHALE, LIMESTONE, CLAY, SOIL, GRASSY_SOIL].map(Some);
        assert_eq!(found, [want.as_slice(), &[None]].concat());
        // Where each begins, in Voxels below the surface.
        for (run, depth) in runs[1..].iter().zip([480.0, 300.0, 120.0, 40.0, 4.0, 0.0]) {
            assert!((surface - run.0 - depth).abs() <= 2.0, "{run:?} {depth}");
        }
    }

    #[test]
    fn the_surface_follows_the_biome() {
        let top = |pick: fn(&Texel) -> bool| {
            let d = find(pick);
            let surface = planet().map().elevation_at(d);
            column(d, surface - 2.0, surface + 3.0)[0].1
        };
        assert_eq!(
            top(|t| t.biome == Biome::Temperate && t.elevation >= 100),
            Some(GRASSY_SOIL)
        );
        assert_eq!(
            top(|t| t.biome == Biome::Desert && t.elevation >= 100),
            Some(SAND)
        );
        assert_eq!(
            top(|t| t.biome == Biome::Snow && t.elevation >= 100),
            Some(SNOW)
        );
        assert_eq!(
            top(|t| t.biome == Biome::Volcanic && t.elevation >= 100),
            Some(BASALT)
        );
    }

    #[test]
    fn the_strata_below_the_surface_differ_by_biome() {
        let below = |biome| {
            let d = find(|t| t.biome == biome && t.elevation >= 100);
            let surface = planet().map().elevation_at(d);
            column(d, surface - 520.0, surface - 1.0)
                .iter()
                .map(|r| r.1.unwrap())
                .collect::<Vec<_>>()
        };
        let all = [
            Biome::Temperate,
            Biome::Desert,
            Biome::Snow,
            Biome::Volcanic,
        ]
        .map(below);
        for (k, a) in all.iter().enumerate() {
            assert_eq!(a[0], DEEP_ROCK);
            assert!(a.len() >= 5, "{a:?}");
            assert!(all[k + 1..].iter().all(|b| b != a), "{a:?}");
        }
    }

    #[test]
    fn an_ocean_column_is_seabed_then_water_up_to_sea_level_then_air() {
        let d = find(|t| t.elevation < -400);
        let surface = planet().map().elevation_at(d);
        let runs = column(d, surface - 3.0, 30.0);
        let found: Vec<_> = runs.iter().map(|r| r.1).collect();
        assert_eq!(found, [Some(SAND), Some(WATER), None]);
        // The water reaches sea level: the first air is there.
        assert!((runs[2].0 - f64::from(SEA_LEVEL)).abs() <= 2.0, "{runs:?}");
    }

    #[test]
    fn land_is_not_flooded_and_the_sea_floor_is_under_water() {
        let map = planet().map();
        for face in 0..6 {
            for j in (0..map.size()).step_by(5) {
                for i in (0..map.size()).step_by(5) {
                    let texel = map.texel(face, i, j);
                    let d = map.texel_direction(face, i, j);
                    if texel.elevation >= 0 {
                        // Just under the surface, and at sea level itself where it is higher.
                        for h in [f64::from(texel.elevation) - 2.0, -2.0, 0.0] {
                            let voxel = planet().voxel(along(d, RADIUS + h));
                            assert!(voxel != Some(WATER), "{face} {i} {j} {h}");
                        }
                    } else if texel.elevation < -10 {
                        let sea = planet().voxel(along(d, RADIUS - 3.0));
                        assert_eq!(sea, Some(WATER), "{face} {i} {j}");
                    }
                }
            }
        }
    }

    #[test]
    fn ocean_bricks_below_sea_level_are_water() {
        let d = find(|t| t.elevation < -400);
        let coord = along(d, RADIUS - 100.0).brick();
        let brick = planet().brick(coord);
        assert!(brick.is_uniform());
        assert_eq!(brick.get([0, 0, 0]), Some(WATER));
    }

    #[test]
    fn a_brick_is_what_its_voxels_are_wherever_it_is() {
        // Down radial lines through every kind of country, to the granite below the strata.
        let mut lines = vec![[1.0, 0.0, 0.0], [0.3, -0.5, 0.8], [-0.2, -0.9, -0.4]];
        for pick in [
            (|t: &Texel| t.biome == Biome::Desert) as fn(&Texel) -> bool,
            |t| t.biome == Biome::Snow && t.elevation > 0,
            |t| t.biome == Biome::Volcanic,
            |t| t.elevation < -300,
            |t| (0..40).contains(&t.elevation),
            |t| t.elevation > 8000,
        ] {
            lines.push(find(pick));
        }
        for d in lines {
            let surface = planet().map().elevation_at(d);
            let mut h = surface - 560.0;
            while h < surface.max(0.0) + 40.0 {
                let coord = along(d, RADIUS + h).brick();
                let brick = planet().brick(coord);
                for (local, voxel) in voxels_of(coord) {
                    assert_eq!(
                        brick.get(local),
                        planet().voxel(voxel),
                        "{d:?} {coord:?} {local:?}"
                    );
                }
                h += 5.0;
            }
        }
    }

    #[test]
    fn bricks_over_the_steepest_ground_and_a_texel_edge_are_what_their_voxels_are() {
        // The pair of neighbouring texels with the largest rise between them, and the middle of
        // them, which is where one texel's biome ends and the next begins.
        let map = planet().map();
        let (mut steepest, mut at) = (0, [0.0; 3]);
        for face in 0..6 {
            for j in 0..map.size() - 1 {
                for i in 0..map.size() - 1 {
                    let (a, b) = (map.texel(face, i, j), map.texel(face, i + 1, j));
                    let rise = (a.elevation - b.elevation).abs();
                    if rise > steepest {
                        let (da, db) = (
                            map.texel_direction(face, i, j),
                            map.texel_direction(face, i + 1, j),
                        );
                        (steepest, at) = (rise, [0, 1, 2].map(|k| da[k] + db[k]));
                    }
                }
            }
        }
        assert!(steepest > 100, "{steepest}");
        let ground = planet().map().elevation_at(at);
        let middle = along(at, RADIUS + ground).brick();
        for dx in -3..=3 {
            for dy in -3..=3 {
                for dz in -12..=12 {
                    let coord = BrickCoord([middle.0[0] + dx, middle.0[1] + dy, middle.0[2] + dz]);
                    let brick = planet().brick(coord);
                    for (local, voxel) in voxels_of(coord) {
                        assert_eq!(brick.get(local), planet().voxel(voxel), "{coord:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn bricks_across_the_interiors_boundaries_are_what_their_voxels_are() {
        for r in [MOLTEN_RADIUS, INTERIOR_RADIUS, 5] {
            for d in [[1.0, 0.0, 0.0], [1.0, 1.0, 1.0], [-0.3, 0.2, -1.0]] {
                for k in -12..=12 {
                    let coord = along(d, f64::from(r + k)).brick();
                    let brick = planet().brick(coord);
                    for (local, voxel) in voxels_of(coord) {
                        assert_eq!(brick.get(local), planet().voxel(voxel), "{coord:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_brick_is_generated_alone_and_the_same_every_time() {
        let d = find(|t| t.elevation > 100);
        let coord = along(d, RADIUS + planet().map().elevation_at(d)).brick();
        let a = planet().brick(coord);
        let again = Planet::new(WorldMap::generate(SEED, SIZE)).brick(coord);
        assert_eq!(a.content_hash(), again.content_hash());
        assert!(!a.is_uniform());
        let other = Planet::new(WorldMap::generate(SEED + 1, SIZE)).brick(coord);
        assert_ne!(a.content_hash(), other.content_hash());
    }

    #[test]
    fn far_space_is_air() {
        assert!(planet().brick(BrickCoord([30_000, 0, 0])).is_air());
        assert!(
            planet()
                .brick(BrickCoord([i32::MIN / 8, i32::MAX / 8, 5]))
                .is_air()
        );
    }

    #[test]
    fn the_spawn_is_on_gentle_temperate_land_with_air_above_it() {
        let spawn = planet().spawn(SPAWN_REACH);
        assert!(planet().map().elevation_at(spawn) >= 100.0);
        assert_eq!(planet().map().sample(spawn).biome, Biome::Temperate);
        let radius = spawn.iter().map(|c| c * c).sum::<f64>().sqrt();
        let voxel = |dr| planet().voxel(along(spawn, radius + dr));
        assert!(voxel(-3.0).is_some_and(|m| m != WATER));
        assert_eq!(voxel(3.0), None);
    }

    #[test]
    fn the_region_around_the_spawn_holds_the_ground_and_reads_the_rest_as_the_planet() {
        let spawn = planet().spawn(SPAWN_REACH);
        let store = planet().region_around(spawn, SPAWN_REACH);
        assert!(!store.is_empty());
        for coord in store.sorted_coords() {
            let brick = store.brick(coord).unwrap();
            assert!(!brick.is_air());
            assert_eq!(brick.content_hash(), planet().brick(coord).content_hash());
        }
        // Down the line through the spawn and 300 Voxels to either side of it, from above the
        // ground to the centre, every brick reads as the Planet's own, held or not.
        let radius = spawn.iter().map(|c| c * c).sum::<f64>().sqrt();
        let up = spawn.map(|c| c / radius);
        let helper = if up[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let side = [
            up[1] * helper[2] - up[2] * helper[1],
            up[2] * helper[0] - up[0] * helper[2],
            up[0] * helper[1] - up[1] * helper[0],
        ];
        let length = side.iter().map(|c| c * c).sum::<f64>().sqrt();
        for offset in [0.0, 60.0, -60.0] {
            for r in (0..=radius as i32 + 60).rev().step_by(37) {
                let point: [f64; 3] =
                    std::array::from_fn(|a| up[a] * f64::from(r) + side[a] / length * offset);
                let coord = VoxelCoord(point.map(|c| c.round() as i32)).brick();
                assert_eq!(
                    store.brick_at(0, coord).content_hash(),
                    planet().brick(coord).content_hash(),
                    "{coord:?}"
                );
            }
        }
        // Deep bricks under the region are not held: they cost a source, not memory.
        let deep = along(spawn, radius - 12_000.0).brick();
        assert!(store.brick(deep).is_none());
        assert_eq!(*store.brick_at(0, deep), Brick::solid(DEEP_ROCK));
        assert_eq!(
            *store.brick_at(0, BrickCoord([0; 3])),
            Brick::solid(MOLTEN_CORE)
        );
    }

    #[test]
    fn coarse_bricks_are_the_interiors_layers_where_they_lie_wholly_in_one() {
        let at = |level, c| BrickSource::brick(planet(), level, BrickCoord(c));
        // Level 2 bricks are 32 Voxels across.
        assert_eq!(at(2, [100, 0, 0]), Brick::solid(MOLTEN_CORE));
        assert_eq!(at(2, [2000, 0, 0]), Brick::solid(DEEP_ROCK));
        // Over the core's edge (x = 32000 is brick 1000) it is the layer outside; over the
        // interior's edge, and beyond, it is air until coarse bricks are generated directly.
        assert_eq!(at(2, [1000, 0, 0]), Brick::solid(DEEP_ROCK));
        assert!(at(2, [4673, 0, 0]).is_air());
        assert!(at(2, [100_000, 0, 0]).is_air());
        assert_eq!(at(0, [0, 0, 0]), planet().brick(BrickCoord([0, 0, 0])));
    }
}
