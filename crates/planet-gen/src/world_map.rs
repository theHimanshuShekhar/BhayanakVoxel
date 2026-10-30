//! The world map: the precomputed, per-seed shape of the whole Planet, which per-brick
//! generation (#45) samples instead of working out continents and climate itself.
//!
//! It is a cube-sphere: six faces of `size` x `size` texels (2048 for the real Planet, about
//! 12 m a texel at the 16 km radius). Each texel holds its elevation, how much erosion changed
//! it, its biome and its fantasy-region masks. Sea level is the constant [`SEA_LEVEL`], and
//! rivers and lakes are not here yet (M1b).
//!
//! It follows the crate's rules. Randomness is `value_noise` of an integer point on the sphere,
//! so the six faces meet without a seam, and the floats used are only `+ - * /`, `sqrt` and
//! conversions. Every texel is computed from read-only inputs, and the sums and the median are
//! over integers, so the map does not depend on the thread count. A change to the map's output
//! must bump [`GENERATOR_VERSION`](crate::GENERATOR_VERSION), which also keys the map's cache on
//! disk (with saves), and update the golden hash in `tests/golden.rs`.

use super::value_noise;

/// Texels along one face's edge for the real Planet.
pub const WORLD_MAP_SIZE: u32 = 2048;

/// Distance of sea level from the Planet's centre, in Voxels: 16 km at 10 cm Voxels.
pub const WORLD_RADIUS: i32 = 160_000;

/// Elevation of the sea surface. Elevations are in Voxels above or below it.
pub const SEA_LEVEL: i16 = 0;

/// Height of the highest peak above sea level, in Voxels (2 km).
const PEAK: i64 = 20_000;

/// Depth of the deepest trench below sea level, in Voxels (1 km).
const TRENCH: i64 = 10_000;

/// Faces as (normal, u axis, v axis), each a signed unit vector along one of x, y and z. A face
/// is the set of cube points `normal + a * u + b * v` for `a` and `b` in `[-1, 1]`. The order
/// is +x, -x, +y, -y, +z, -z, so `2 * axis + (is negative)` is the face of a direction.
const FACES: [[[i8; 3]; 3]; 6] = [
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
    [[0, 1, 0], [1, 0, 0], [0, 0, -1]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 0], [0, 1, 0]],
    [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
];

/// The kinds of country, chosen by latitude, elevation and noise. Oceans are only ever
/// [`Biome::Snow`] (ice) or [`Biome::Temperate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Biome {
    /// Forest and grassland.
    Temperate,
    Desert,
    /// Snow, ice and tundra.
    Snow,
    Volcanic,
}

/// Which fantasy features a texel may host. M1b decides what, if anything, grows in each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Regions(u8);

impl Regions {
    pub const CRYSTAL_CAVES: Regions = Regions(1);
    pub const FLOATING_ISLANDS: Regions = Regions(2);
    pub const ARCHES_AND_SPIRES: Regions = Regions(4);
    pub const BIOLUMINESCENT_FLORA: Regions = Regions(8);

    /// Every region, in the order of the constants above.
    pub const ALL: [Regions; 4] = [
        Self::CRYSTAL_CAVES,
        Self::FLOATING_ISLANDS,
        Self::ARCHES_AND_SPIRES,
        Self::BIOLUMINESCENT_FLORA,
    ];

    /// Whether every region in `other` is set.
    pub fn contains(self, other: Regions) -> bool {
        self.0 & other.0 == other.0
    }
}

/// One texel of the world map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texel {
    /// Height above [`SEA_LEVEL`] after erosion, in Voxels; negative on the sea floor.
    pub elevation: i16,
    /// Height lost to erosion, in Voxels: negative where eroded material settled.
    pub erosion: i16,
    pub biome: Biome,
    pub regions: Regions,
}

impl Texel {
    /// Whether the texel is below the sea surface. A texel exactly at [`SEA_LEVEL`] is land: it
    /// does not flood.
    pub fn is_ocean(&self) -> bool {
        self.elevation < SEA_LEVEL
    }
}

/// The world map of one seed. See the module documentation.
pub struct WorldMap {
    size: u32,
    /// Face by face, each row by row (`i` fastest).
    texels: Vec<Texel>,
}

impl WorldMap {
    /// Computes the map of `seed` with `size` x `size` texels a face, on all available cores.
    /// The Planet uses [`WORLD_MAP_SIZE`]; tests use a coarser map. Panics if `size` is 0.
    pub fn generate(seed: u64, size: u32) -> WorldMap {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self::generate_on(seed, size, threads)
    }

    fn generate_on(seed: u64, size: u32, threads: usize) -> WorldMap {
        assert!(size > 0, "a world map needs at least one texel");
        let count = 6 * (size as usize).pow(2);
        let mut raw = vec![0; count];
        par_fill(&mut raw, threads, |index| {
            let (face, i, j) = split(size, index);
            raw_elevation(seed, centre(size, face, i, j))
        });
        let base = normalise(&raw, size);
        drop(raw);
        let mut eroded = base.clone();
        erode(&mut eroded, size, threads);
        let blank = Texel {
            elevation: 0,
            erosion: 0,
            biome: Biome::Temperate,
            regions: Regions::default(),
        };
        let mut texels = vec![blank; count];
        par_fill(&mut texels, threads, |index| {
            let (face, i, j) = split(size, index);
            let direction = centre(size, face, i, j);
            let elevation = eroded[index];
            Texel {
                elevation,
                erosion: (i32::from(base[index]) - i32::from(elevation)) as i16,
                biome: biome(seed, direction, elevation),
                regions: regions(seed, direction),
            }
        });
        WorldMap { size, texels }
    }

    /// Texels along one face's edge.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The texel `(i, j)` of `face`: 0 to 5 in the order +x, -x, +y, -y, +z, -z, with `i`
    /// and `j` below [`WorldMap::size`]. Panics if one is out of range.
    pub fn texel(&self, face: usize, i: u32, j: u32) -> Texel {
        assert!(face < 6 && i < self.size && j < self.size);
        self.texels[join(self.size, face, i, j)]
    }

    /// The face and texel coordinates `(face, i, j)` containing the ray from the Planet's centre
    /// along `direction`, which need not be a unit vector but must not be zero. A direction on
    /// the edge between texels belongs to one of them.
    pub fn locate(&self, direction: [f64; 3]) -> (usize, u32, u32) {
        locate(self.size, direction)
    }

    /// The texel containing the ray from the Planet's centre along `direction`. Use it for the
    /// categorical layers (biome, regions); [`WorldMap::elevation_at`] is smooth.
    pub fn sample(&self, direction: [f64; 3]) -> Texel {
        let (face, i, j) = self.locate(direction);
        self.texel(face, i, j)
    }

    /// The elevation in Voxels of the ray from the Planet's centre along `direction`, bilinearly
    /// interpolated between the four nearest texel centres, so that terrain made from it has no
    /// plateaus at texel edges. It is continuous across face edges and corners: beyond a
    /// face's edge the texels of the adjacent face are used, and at a cube corner, where there
    /// is no fourth texel, the mean of the three around it.
    pub fn elevation_at(&self, direction: [f64; 3]) -> f64 {
        let (face, x, y) = position(self.size, direction);
        // Texel centres are at whole numbers, half a texel in from the edges of `position`.
        let (x, y) = (x - 0.5, y - 0.5);
        let (i, j) = (x.floor(), y.floor());
        let (i, j) = (i as i32, j as i32);
        let (fx, fy) = (x - f64::from(i), y - f64::from(j));
        let at = |di, dj| self.elevation_beyond(face, i + di, j + dj);
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        lerp(
            lerp(at(0, 0), at(1, 0), fx),
            lerp(at(0, 1), at(1, 1), fx),
            fy,
        )
    }

    /// The elevation of texel `(i, j)` of `face`, where either may be one beyond the face's edge.
    fn elevation_beyond(&self, face: usize, i: i32, j: i32) -> f64 {
        let last = self.size as i32 - 1;
        let (inner_i, inner_j) = (i.clamp(0, last), j.clamp(0, last));
        let read = |(face, i, j): (usize, u32, u32)| f64::from(self.texel(face, i, j).elevation);
        let (ci, cj) = (inner_i as u32, inner_j as u32);
        let inner = read((face, ci, cj));
        let across_i = || read(step(self.size, face, ci, cj, usize::from(i > 0)));
        let across_j = || read(step(self.size, face, ci, cj, 2 + usize::from(j > 0)));
        match (i != inner_i, j != inner_j) {
            (false, false) => inner,
            (true, false) => across_i(),
            (false, true) => across_j(),
            (true, true) => (inner + across_i() + across_j()) / 3.0,
        }
    }

    /// FNV-1a hash of the size and every texel, face by face, row by row.
    pub fn content_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let texels = self.texels.iter().flat_map(|texel| {
            let [e0, e1] = texel.elevation.to_le_bytes();
            let [r0, r1] = texel.erosion.to_le_bytes();
            [e0, e1, r0, r1, texel.biome as u8, texel.regions.0]
        });
        for byte in self.size.to_le_bytes().into_iter().chain(texels) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// Fills `out` with `f(index)`, split into one contiguous part per thread. `f` sees only its
/// index, so the result does not depend on the thread count.
fn par_fill<T: Send>(out: &mut [T], threads: usize, f: impl Fn(usize) -> T + Sync) {
    let part = out.len().div_ceil(threads.max(1)).max(1);
    std::thread::scope(|scope| {
        for (n, chunk) in out.chunks_mut(part).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (k, slot) in chunk.iter_mut().enumerate() {
                    *slot = f(n * part + k);
                }
            });
        }
    });
}

/// Splits a texel's index into (face, i, j).
fn split(size: u32, index: usize) -> (usize, u32, u32) {
    let size = size as usize;
    let (face, rest) = (index / (size * size), index % (size * size));
    (face, (rest % size) as u32, (rest / size) as u32)
}

/// The inverse of [`split`].
fn join(size: u32, face: usize, i: u32, j: u32) -> usize {
    (face * size as usize + j as usize) * size as usize + i as usize
}

/// Stretches a coordinate in `[-1, 1]` across a face so that texels cover nearly equal areas
/// of the sphere: the largest is about twice the smallest, where a plain cube projection has
/// five times.
/// A quadratic stands in for the equal-angle `tan`, which would need a transcendental.
fn warp(t: f64) -> f64 {
    t * (2.0 + t.abs()) / 3.0
}

/// The inverse of [`warp`].
fn unwarp(a: f64) -> f64 {
    ((1.0 + 3.0 * a.abs()).sqrt() - 1.0).copysign(a)
}

/// The cube coordinate in `[-1, 1]` of the centre of texel `k` along an axis.
fn coordinate(size: u32, k: u32) -> f64 {
    f64::from(2 * k + 1) / f64::from(size) - 1.0
}

/// The point of the unit sphere at the centre of a texel.
fn centre(size: u32, face: usize, i: u32, j: u32) -> [f64; 3] {
    let [n, u, v] = FACES[face].map(|axis| axis.map(f64::from));
    let (a, b) = (warp(coordinate(size, i)), warp(coordinate(size, j)));
    let cube: [f64; 3] = std::array::from_fn(|k| n[k] + a * u[k] + b * v[k]);
    let length = (cube[0] * cube[0] + cube[1] * cube[1] + cube[2] * cube[2]).sqrt();
    cube.map(|c| c / length)
}

/// The face containing `direction` and the position in it in texels, `(x, y)` in `[0, size]`:
/// texel `(i, j)` covers `i <= x < i + 1` and `j <= y < j + 1`.
fn position(size: u32, direction: [f64; 3]) -> (usize, f64, f64) {
    let abs = direction.map(f64::abs);
    let axis = if abs[0] >= abs[1] && abs[0] >= abs[2] {
        0
    } else if abs[1] >= abs[2] {
        1
    } else {
        2
    };
    let face = 2 * axis + usize::from(direction[axis] < 0.0);
    let [n, u, v] = FACES[face].map(|axis| axis.map(f64::from));
    let dot = |w: [f64; 3]| direction[0] * w[0] + direction[1] * w[1] + direction[2] * w[2];
    let depth = dot(n);
    let along = |axis: [f64; 3]| (unwarp(dot(axis) / depth) + 1.0) / 2.0 * f64::from(size);
    (face, along(u), along(v))
}

fn locate(size: u32, direction: [f64; 3]) -> (usize, u32, u32) {
    let (face, x, y) = position(size, direction);
    // The conversion floors a non-negative number, and gives 0 for a hair below zero.
    (face, (x as u32).min(size - 1), (y as u32).min(size - 1))
}

/// The texel one step from `(face, i, j)`: `side` 0 and 1 are `i - 1` and `i + 1`, 2 and 3 are
/// `j - 1` and `j + 1`. A step off a face's edge lands on the adjacent face, next to the same
/// point of the shared edge, and a step back returns to where it started.
fn step(size: u32, face: usize, i: u32, j: u32, side: usize) -> (usize, u32, u32) {
    let (along, kept_index) = if side < 2 { (i, j) } else { (j, i) };
    let delta = if side.is_multiple_of(2) { -1 } else { 1 };
    if let Some(m) = along.checked_add_signed(delta).filter(|&m| m < size) {
        return if side < 2 { (face, m, j) } else { (face, i, m) };
    }
    let [n, u, v] = FACES[face];
    // The way across the edge is the next face's normal; the other tangent runs along the edge.
    let (across, kept) = if side < 2 { (u, v) } else { (v, u) };
    let across = across.map(|a| a * delta as i8);
    let next = FACES.iter().position(|f| f[0] == across).unwrap();
    let dot = |a: [i8; 3], b: [i8; 3]| (0..3).map(|k| i32::from(a[k] * b[k])).sum::<i32>();
    // On the next face, the point keeps its place along the edge, and is at the end of the
    // tangent that points the way this face's normal does.
    let coord = |axis: [i8; 3]| match dot(axis, kept) {
        0 if dot(axis, n) > 0 => size - 1,
        0 => 0,
        d if d > 0 => kept_index,
        _ => size - 1 - kept_index,
    };
    (next, coord(FACES[next][1]), coord(FACES[next][2]))
}

/// Value noise summed over `octaves` octaves, from cells `1 << first` Voxels wide down, each
/// half the weight of the one before; in `[-1, 1)`. The noise is sampled at the point of the
/// sphere of [`WORLD_RADIUS`] in `direction`.
fn fractal(seed: u64, direction: [f64; 3], first: u32, octaves: u32) -> f64 {
    let point = direction.map(|d| (d * f64::from(WORLD_RADIUS)).round() as i32);
    let (mut total, mut weight, mut sum) = (0.0, 1.0, 0.0);
    for octave in 0..octaves {
        let salt = u64::from(octave).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        total += weight * value_noise(seed ^ salt, point, 1 << (first - octave), &mut None);
        sum += weight;
        weight /= 2.0;
    }
    total / sum
}

const ELEVATION: u64 = 0x1f83_d9ab_fb41_bd6b;
const WARMTH: u64 = 0x5be0_cd19_137e_2179;
const ARIDITY: u64 = 0xcbbb_9d5d_c105_9ed8;
const VOLCANISM: u64 = 0x629a_292a_367c_d507;
const REGION: [u64; 4] = [
    0x9159_015a_3070_dd17,
    0x152f_ecd8_f70e_5939,
    0x6733_2667_ffc0_0b31,
    0x8eb4_4a87_6858_1511,
];

/// The elevation noise at a point, in `[-2^20, 2^20)`, before it is scaled to Voxels.
fn raw_elevation(seed: u64, direction: [f64; 3]) -> i32 {
    (fractal(seed ^ ELEVATION, direction, 16, 9) * 1_048_576.0) as i32
}

/// The weight of each texel of a face, proportional to the area of sphere it covers.
fn area_weights(size: u32) -> Vec<u32> {
    let slope = |t: f64| (2.0 + 2.0 * t.abs()) / 3.0;
    (0..size * size)
        .map(|k| {
            let (ti, tj) = (coordinate(size, k % size), coordinate(size, k / size));
            let (a, b) = (warp(ti), warp(tj));
            let r = 1.0 + a * a + b * b;
            (slope(ti) * slope(tj) / (r * r.sqrt()) * 1_048_576.0) as u32
        })
        .collect()
}

/// Scales the noise to Voxels so that sea level sits at the median by area, which makes half
/// the Planet ocean, the highest point [`PEAK`] above it and the lowest [`TRENCH`] below.
fn normalise(raw: &[i32], size: u32) -> Vec<i16> {
    const SHIFT: u32 = 5;
    const OFFSET: i32 = 1 << 20;
    let weights = area_weights(size);
    let mut area = vec![0u64; 1 << (21 - SHIFT)];
    for (index, &value) in raw.iter().enumerate() {
        area[((value + OFFSET) >> SHIFT) as usize] += u64::from(weights[index % weights.len()]);
    }
    let half = area.iter().sum::<u64>() / 2;
    let mut below = 0;
    let median_bin = area
        .iter()
        .position(|&a| {
            below += a;
            below >= half
        })
        .unwrap();
    let median = ((median_bin as i32) << SHIFT) - OFFSET;
    let (min, max) = raw
        .iter()
        .fold((i32::MAX, i32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let rise = i64::from(max - median).max(1);
    let fall = i64::from(median - min).max(1);
    raw.iter()
        .map(|&v| {
            let above = i64::from(v - median);
            (if above >= 0 {
                above * PEAK / rise
            } else {
                above * TRENCH / fall
            }) as i16
        })
        .collect()
}

/// Thermal erosion: each texel sheds an eighth of its height over the talus to each lower
/// neighbour, and gains the same from each higher one, so material only moves. Every step reads
/// the last step's heights and writes new ones, so the order of texels does not matter. The
/// number of steps and the talus scale with the texel size, so the slope limit is the same at
/// every size, but a coarse map is not a small copy of a fine one: material spreads about
/// `sqrt(size / 16)` texels, and the amounts moved differ (on the test seed the largest change
/// is about 1,250 Voxels at 256 texels and 580 at 2048).
fn erode(heights: &mut Vec<i16>, size: u32, threads: usize) {
    // A slope of 0.2 (11 degrees) across one texel, which is 251,327 Voxels / `size` wide.
    let talus = (251_327 / size as i32) / 5;
    let mut next = vec![0i16; heights.len()];
    for _ in 0..size / 16 {
        par_fill(&mut next, threads, |index| {
            let (face, i, j) = split(size, index);
            let here = i32::from(heights[index]);
            let around = [0, 1, 2, 3].map(|side| {
                let (f, i, j) = step(size, face, i, j, side);
                i32::from(heights[join(size, f, i, j)])
            });
            let moved = |from: i32, to: i32| (from - to - talus).max(0) / 8;
            let shed: i32 = around.iter().map(|&n| moved(here, n)).sum();
            let gained: i32 = around.iter().map(|&n| moved(n, here)).sum();
            (here - shed + gained) as i16
        });
        std::mem::swap(heights, &mut next);
    }
}

/// The biome at a texel: cold at the poles and on high ground, dry in a band of subtropical
/// latitudes, volcanic in rare hot spots.
fn biome(seed: u64, direction: [f64; 3], elevation: i16) -> Biome {
    let latitude = direction[1].abs();
    let land = elevation >= SEA_LEVEL;
    let height = f64::from(elevation.max(SEA_LEVEL)) / PEAK as f64;
    let warmth = 1.0 - latitude - 0.8 * height + 0.1 * fractal(seed ^ WARMTH, direction, 15, 3);
    let subtropics = (1.0 - (latitude - 0.35).abs() / 0.35).max(0.0);
    let aridity = fractal(seed ^ ARIDITY, direction, 15, 3) + 0.7 * subtropics;
    if land && fractal(seed ^ VOLCANISM, direction, 15, 4) > 0.4 {
        Biome::Volcanic
    } else if warmth < 0.2 {
        Biome::Snow
    } else if land && warmth > 0.4 && aridity > 0.3 {
        Biome::Desert
    } else {
        Biome::Temperate
    }
}

/// The fantasy regions at a texel: each a rare patch of its own noise.
fn regions(seed: u64, direction: [f64; 3]) -> Regions {
    Regions(
        REGION
            .iter()
            .enumerate()
            .filter(|&(_, &tag)| fractal(seed ^ tag, direction, 15, 3) > 0.35)
            .fold(0, |bits, (k, _)| bits | 1 << k),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    const SEED: u64 = 0xB4A7_A9AC;
    /// Texels along a face of the map the tests share: 6 x 256^2 = 393,216 texels.
    const SIZE: u32 = 256;

    fn map() -> &'static WorldMap {
        static MAP: OnceLock<WorldMap> = OnceLock::new();
        MAP.get_or_init(|| WorldMap::generate(SEED, SIZE))
    }

    fn all_texels(map: &WorldMap) -> impl Iterator<Item = (usize, u32, u32, Texel)> + '_ {
        (0..6).flat_map(move |face| {
            (0..map.size()).flat_map(move |j| {
                (0..map.size()).map(move |i| (face, i, j, map.texel(face, i, j)))
            })
        })
    }

    #[test]
    fn the_same_seed_gives_the_same_map_and_another_seed_does_not() {
        let other = WorldMap::generate(SEED + 1, 32);
        let again = WorldMap::generate(SEED, 32);
        assert_eq!(
            again.content_hash(),
            WorldMap::generate(SEED, 32).content_hash()
        );
        assert_ne!(again.content_hash(), other.content_hash());
    }

    #[test]
    fn the_map_does_not_depend_on_the_thread_count() {
        let one = WorldMap::generate_on(SEED, 64, 1).content_hash();
        for threads in [2, 3, 8] {
            assert_eq!(one, WorldMap::generate_on(SEED, 64, threads).content_hash());
        }
    }

    #[test]
    fn about_half_the_planets_area_is_ocean() {
        let map = map();
        let weights = area_weights(SIZE);
        let (mut ocean, mut all) = (0u64, 0u64);
        for (_, i, j, texel) in all_texels(map) {
            let weight = u64::from(weights[(j * SIZE + i) as usize]);
            all += weight;
            if texel.is_ocean() {
                ocean += weight;
            }
        }
        let percent = ocean as f64 * 100.0 / all as f64;
        assert!((45.0..=55.0).contains(&percent), "{percent}% ocean");
    }

    #[test]
    fn peaks_reach_about_two_kilometres_and_trenches_about_one() {
        let elevations: Vec<i16> = all_texels(map()).map(|(.., t)| t.elevation).collect();
        let (low, high) = (
            *elevations.iter().min().unwrap(),
            *elevations.iter().max().unwrap(),
        );
        // 10 cm Voxels: 20,000 is 2 km. Erosion shaves the highest peak a little.
        assert!((17_000..=20_000).contains(&high), "highest {high}");
        assert!((-10_000..=-8_500).contains(&low), "lowest {low}");
    }

    #[test]
    fn erosion_only_moves_height_around() {
        let map = map();
        let moved: i64 = all_texels(map).map(|(.., t)| i64::from(t.erosion)).sum();
        assert_eq!(moved, 0, "erosion changed the Planet's total height");
        assert!(all_texels(map).any(|(.., t)| t.erosion > 0));
        assert!(all_texels(map).any(|(.., t)| t.erosion < 0));
    }

    #[test]
    fn every_biome_occurs_and_sits_where_its_climate_puts_it() {
        let map = map();
        let latitude = |face, i, j| centre(SIZE, face, i, j)[1].abs();
        let count = |biome| all_texels(map).filter(|(.., t)| t.biome == biome).count();
        for biome in [
            Biome::Temperate,
            Biome::Desert,
            Biome::Snow,
            Biome::Volcanic,
        ] {
            assert!(count(biome) > 1000, "{biome:?}: {}", count(biome));
        }
        for (face, i, j, texel) in all_texels(map) {
            let latitude = latitude(face, i, j);
            match texel.biome {
                Biome::Desert => assert!(!texel.is_ocean() && latitude < 0.75, "{latitude}"),
                Biome::Volcanic => assert!(!texel.is_ocean()),
                _ if latitude > 0.92 => assert_eq!(texel.biome, Biome::Snow, "pole {latitude}"),
                Biome::Snow => assert!(latitude > 0.4 || texel.elevation > 4_000),
                Biome::Temperate => {}
            }
            if latitude < 0.15 && texel.elevation < 5_000 {
                assert_ne!(texel.biome, Biome::Snow, "snow at the equator");
            }
        }
    }

    #[test]
    fn each_fantasy_region_is_rare_but_present() {
        let map = map();
        let total = 6 * (SIZE * SIZE) as usize;
        for region in Regions::ALL {
            let count = all_texels(map)
                .filter(|(.., t)| t.regions.contains(region))
                .count();
            assert!(
                (total / 200..total / 5).contains(&count),
                "{region:?}: {count}"
            );
        }
    }

    #[test]
    fn a_texel_centre_is_located_in_its_own_texel() {
        let size = 64;
        for face in 0..6 {
            for (i, j) in [(0, 0), (63, 0), (0, 63), (63, 63), (7, 40), (32, 31)] {
                let direction = centre(size, face, i, j);
                assert_eq!(locate(size, direction), (face, i, j));
                // Any length of the same ray is the same texel.
                assert_eq!(locate(size, direction.map(|d| d * 37.5)), (face, i, j));
            }
        }
    }

    #[test]
    fn a_direction_is_sampled_whatever_its_length_and_a_cube_corner_lands_on_a_face_corner() {
        let map = map();
        assert_eq!(map.sample([0.2, 0.9, -0.1]), map.sample([2.0, 9.0, -1.0]));
        assert_eq!(map.locate([0.2, 0.9, -0.1]).0, 2);
        // Ties go to the lowest face, which is +x, and the corner is its (0, size - 1) texel.
        let last = SIZE - 1;
        assert_eq!(map.locate([1.0, 1.0, 1.0]), (0, 0, last));
        // The two faces meeting it there each hold a corner texel as well.
        let beside = [0, 3].map(|side| step(SIZE, 0, 0, last, side));
        assert_ne!(beside[0].0, beside[1].0);
        for (face, i, j) in beside {
            assert!(face != 0 && [0, last].contains(&i) && [0, last].contains(&j));
        }
    }

    #[test]
    fn elevation_is_continuous_across_face_edges_and_cube_corners() {
        let map = map();
        // Pairs of cube points a hair either side of an edge between two faces, and of the
        // corner (1, 1, -1) where three faces meet, which are 1e-7 of a face apart.
        let e = 1e-7;
        let pairs = [
            ([1.0, 0.3, -1.0 + e], [1.0 - e, 0.3, -1.0]),
            ([1.0, -0.77, -1.0 + e], [1.0 - e, -0.77, -1.0]),
            ([0.4, 1.0, 1.0 - e], [0.4, 1.0 - e, 1.0]),
            ([1.0, 1.0 - e, -1.0 + e], [1.0 - e, 1.0, -1.0 + e]),
            ([1.0 - e, 1.0, -1.0 + e], [1.0 - e, 1.0 - e, -1.0]),
            ([1.0, 1.0 - e, -1.0 + e], [1.0 - e, 1.0 - e, -1.0]),
        ];
        for (a, b) in pairs {
            assert_ne!(map.locate(a).0, map.locate(b).0, "{a:?} {b:?} share a face");
            let (a, b) = (map.elevation_at(a), map.elevation_at(b));
            assert!((a - b).abs() < 0.5, "{a} {b}");
        }
    }

    #[test]
    fn interpolated_elevation_is_the_texel_elevation_at_its_centre_and_between_at_an_edge() {
        let map = map();
        let (face, i, j) = (2, 100, 37);
        let here = f64::from(map.texel(face, i, j).elevation);
        assert!((map.elevation_at(centre(SIZE, face, i, j)) - here).abs() < 0.01);
        // The point halfway between two texel centres, in the cube coordinates the texels
        // are evenly spaced in, is their mean.
        let [n, u, v] = FACES[face].map(|axis| axis.map(f64::from));
        let (a, b) = (
            coordinate(SIZE, i) + 1.0 / f64::from(SIZE),
            coordinate(SIZE, j),
        );
        let (a, b) = (warp(a), warp(b));
        let cube: [f64; 3] = std::array::from_fn(|k| n[k] + a * u[k] + b * v[k]);
        let mean = (here + f64::from(map.texel(face, i + 1, j).elevation)) / 2.0;
        assert!((map.elevation_at(cube) - mean).abs() < 0.01);
    }

    #[test]
    fn texels_are_within_a_factor_of_two_of_each_other_in_area() {
        let weights = area_weights(SIZE);
        let (min, max) = (weights.iter().min().unwrap(), weights.iter().max().unwrap());
        assert!(f64::from(*max) / f64::from(*min) < 2.2, "{min} {max}");
    }

    #[test]
    fn stepping_across_a_cube_edge_lands_beside_the_texel_and_can_be_undone() {
        let size = 16;
        let spacing = 2.0 / f64::from(size);
        for face in 0..6 {
            for i in 0..size {
                for j in 0..size {
                    for side in 0..4 {
                        let (f, a, b) = step(size, face, i, j, side);
                        let here = centre(size, face, i, j);
                        let there = centre(size, f, a, b);
                        let distance = (0..3)
                            .map(|k| {
                                let d = here[k] - there[k];
                                d * d
                            })
                            .sum::<f64>();
                        assert!(
                            distance.sqrt() < 2.0 * spacing,
                            "{face} {i},{j} side {side}: {f} {a},{b}"
                        );
                        assert!(
                            (0..4).any(|back| step(size, f, a, b, back) == (face, i, j)),
                            "{face} {i},{j} side {side} is not undone"
                        );
                    }
                }
            }
        }
    }
}
