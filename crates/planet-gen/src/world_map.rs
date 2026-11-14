//! The world map: the precomputed, per-seed shape of the whole Planet, which per-brick
//! generation (#45) samples instead of working out continents and climate itself.
//!
//! It is a cube-sphere: six faces of `size` x `size` texels (2048 for the real Planet, about
//! 240 m a texel at the 320 km radius of ADR 0008). Each texel holds its elevation, how much
//! erosion changed it, its biome and its fantasy-region masks. Sea level is the constant
//! [`SEA_LEVEL`], and rivers and lakes are not here yet (M1b). It keeps what is large:
//! continents, climate and biomes. What is finer than a texel is made per brick, from the map's
//! height (`crate::detail`).
//!
//! The Planet's radius is a value, not a constant read from inside: [`WorldMap::with_radius`]
//! makes the map of a Planet of any radius, and everything that depends on the radius is derived
//! from the map's. [`WORLD_RADIUS`] is the default, that of the one Planet of the Vertical
//! slice, and the only radius constant in this crate.
//!
//! The map is the same whatever the size of a Voxel (ADR 0007): sizes are in metres, and the
//! noise is sampled on a lattice of fixed 10 cm spacing ([`ELEVATION_UNITS_PER_METRE`]), not on
//! Voxels. Elevations are stored in the same 10 cm units, which an `i16` holds to 3.27 km (a 5 cm
//! Voxel would not hold the 2 km peak); [`WorldMap::elevation_at`] and
//! [`WorldMap::elevation_range`] give them in Voxels.
//!
//! It follows the crate's rules. Randomness is `value_noise` of an integer point on the sphere,
//! so the six faces meet without a seam, and the floats used are only `+ - * /`, `sqrt` and
//! conversions. Every texel is computed from read-only inputs, and the sums and the median are
//! over integers, so the map does not depend on the thread count. A change to the map's output
//! must bump [`GENERATOR_VERSION`](crate::GENERATOR_VERSION), which also keys the map's cache on
//! disk (with saves), and update the golden hash in `tests/golden.rs`.

use brick_store::VOXELS_PER_METRE;

use super::{corner_value, value_noise};

/// Elevations in the map's texels are whole numbers of this many to the metre: 10 cm, whatever
/// the size of a Voxel. An `i16` holds 3.27 km of them, enough for the 2 km peak.
pub const ELEVATION_UNITS_PER_METRE: i32 = 10;

/// Voxels in one elevation unit: 2 for 5 cm Voxels, a whole number so that converting is exact.
const VOXELS_PER_UNIT: f64 = VOXELS_PER_METRE / ELEVATION_UNITS_PER_METRE as f64;

/// Whole Voxels in `metres`, rounded to the nearest: the way the generator turns a size in metres
/// into the integer it works with.
pub(crate) const fn voxels(metres: f64) -> i32 {
    (metres * VOXELS_PER_METRE + 0.5) as i32
}

/// Whole elevation units in `metres`.
pub(crate) const fn units(metres: f64) -> i32 {
    (metres * ELEVATION_UNITS_PER_METRE as f64 + 0.5) as i32
}

/// Texels along one face's edge for the real Planet.
pub const WORLD_MAP_SIZE: u32 = 2048;

/// Distance of sea level from the Planet's centre, in Voxels: 320 km (ADR 0008). The default
/// radius of a [`WorldMap`], and the only radius constant in this crate: everything else derives
/// from it, or from the radius a map is made with.
pub const WORLD_RADIUS: i32 = voxels(320_000.0);

/// The radius of a test Planet: an eighth of [`WORLD_RADIUS`] (40 km, a sixty-fourth of its area),
/// which is a value derived from it and not a second radius. Tests of this and the other crates
/// that need a Planet's ground but not its size use it with a world map of [`TEST_MAP_SIZE`]
/// texels a face (an eighth of the real map's), so that a texel is the same 245 m as the real
/// Planet's: the same detail at the same sizes in metres, the same erosion limit, and a map made
/// in a fraction of a second. At 320 km the Guard alone (levels 9 to 13) is millions of bricks.
pub const TEST_RADIUS: i32 = WORLD_RADIUS / 8;

/// Texels along a face of a test Planet's world map: an eighth of [`WORLD_MAP_SIZE`].
pub const TEST_MAP_SIZE: u32 = WORLD_MAP_SIZE / 8;

/// Elevation of the sea surface. Texel elevations are in units of 10 cm above or below it.
pub const SEA_LEVEL: i16 = 0;

/// Height of the highest peak above sea level, in Voxels (2 km).
pub const PEAK: i32 = voxels(2_000.0);

/// Depth of the deepest trench below sea level, in Voxels (1 km).
pub const TRENCH: i32 = voxels(1_000.0);

/// [`PEAK`] and [`TRENCH`] in the map's elevation units.
const PEAK_UNITS: i32 = units(2_000.0);
const TRENCH_UNITS: i32 = units(1_000.0);

/// Radius of the sphere that the noise is sampled on, in lattice points, for a Planet of
/// `radius` Voxels: the radius in the lattice's 10 cm spacing. The cells of the noise are powers
/// of two of that spacing, so the map does not change with the size of a Voxel, and they are a
/// fixed size in metres, not a share of the radius: a bigger Planet has more continents, not
/// bigger ones.
const fn lattice_radius(radius: i32) -> i32 {
    (radius as f64 / VOXELS_PER_UNIT + 0.5) as i32
}

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
    /// Height above [`SEA_LEVEL`] after erosion, in units of 10 cm
    /// ([`ELEVATION_UNITS_PER_METRE`]); negative on the sea floor.
    pub elevation: i16,
    /// Height lost to erosion, in the same units: negative where eroded material settled.
    pub erosion: i16,
    pub biome: Biome,
    pub regions: Regions,
    /// How rough the ground is made between the texels, as a share of 255 of the roughest: from
    /// the slope here (steeper is rougher), the biome (volcanic is jagged, snow and sand are
    /// smoothed) and the drainage (smoother in a valley). See `detail`.
    pub rough: u8,
    /// How much water drains through this texel: 16 times the log base 2 of the texels that drain
    /// into it (itself included, flowing downhill to the lowest of four neighbours), 0 on the sea
    /// and where nothing drains in, at most 255 (about 60,000 texels). The ground is cut into
    /// gullies where it is high. See `detail`.
    pub drainage: u8,
}

/// The fields of the map that the detail is shaped by, at a point: interpolated between the
/// texels' centres like the elevation is, so that they are continuous.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fields {
    /// [`WorldMap::elevation_at`], in Voxels.
    pub elevation: f64,
    /// [`Texel::rough`], in `[0, 1]`.
    pub rough: f64,
    /// [`Texel::drainage`], in `[0, 1]`.
    pub drainage: f64,
}

/// Bounds on [`Fields`] over a box.
#[derive(Clone, Copy, Debug)]
pub struct FieldRange {
    pub elevation: (f64, f64),
    pub rough: (f64, f64),
    pub drainage: (f64, f64),
}

impl Texel {
    /// The texel's elevation in Voxels.
    pub fn elevation_voxels(&self) -> f64 {
        f64::from(self.elevation) * VOXELS_PER_UNIT
    }

    /// Whether the texel is below the sea surface. A texel exactly at [`SEA_LEVEL`] is land: it
    /// does not flood.
    pub fn is_ocean(&self) -> bool {
        self.elevation < SEA_LEVEL
    }
}

/// The four texels that the elevation is interpolated between, over the square of the map
/// between their centres: see [`WorldMap::elevation_cell`].
#[derive(Clone, Copy, Debug)]
pub struct ElevationCell {
    size: u32,
    face: usize,
    i: i32,
    j: i32,
    /// The fields at `(i, j)`, `(i + 1, j)`, `(i, j + 1)` and `(i + 1, j + 1)`.
    a: [[f64; 3]; 4],
}

impl ElevationCell {
    /// What [`WorldMap::fields_at`] says of `direction`, by the same sums, if it is in this cell;
    /// else `None`.
    pub fn fields_at(&self, direction: [f64; 3]) -> Option<Fields> {
        let (face, x, y) = position(self.size, direction);
        let (x, y) = (x - 0.5, y - 0.5);
        let (i, j) = (x.floor() as i32, y.floor() as i32);
        if (face, i, j) != (self.face, self.i, self.j) {
            return None;
        }
        let (fx, fy) = (x - f64::from(i), y - f64::from(j));
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        let [a00, a10, a01, a11] = self.a;
        let [elevation, rough, drainage] =
            std::array::from_fn(|k| lerp(lerp(a00[k], a10[k], fx), lerp(a01[k], a11[k], fx), fy));
        Some(Fields {
            elevation,
            rough,
            drainage,
        })
    }
}

/// The world map of one seed. See the module documentation.
pub struct WorldMap {
    seed: u64,
    size: u32,
    /// Distance of sea level from the Planet's centre, in Voxels.
    radius: i32,
    /// Face by face, each row by row (`i` fastest).
    texels: Vec<Texel>,
}

impl WorldMap {
    /// Computes the map of `seed` with `size` x `size` texels a face, on all available cores, for
    /// a Planet of [`WORLD_RADIUS`]. The Planet uses [`WORLD_MAP_SIZE`]; tests use a coarser map.
    /// Panics if `size` is 0.
    pub fn generate(seed: u64, size: u32) -> WorldMap {
        Self::with_radius(seed, size, WORLD_RADIUS)
    }

    /// The map of the test Planet of `seed`: [`TEST_MAP_SIZE`] texels a face, [`TEST_RADIUS`]
    /// Voxels from the centre to sea level.
    pub fn for_tests(seed: u64) -> WorldMap {
        Self::with_radius(seed, TEST_MAP_SIZE, TEST_RADIUS)
    }

    /// [`WorldMap::generate`] for a Planet of `radius` Voxels from the centre to sea level, which
    /// is a value passed to the generator, so that there can be Planets of different radii in one
    /// process. The size in metres of continents, peaks and everything else is the same
    /// whatever the radius; the climate's latitude bands and the erosion's slope limit are what
    /// scale with it. Panics if `size` is 0 or `radius` is too small to hold the 2 km peak or too
    /// big for the coordinates (`i32` Voxels, and 10 cm lattice points).
    pub fn with_radius(seed: u64, size: u32, radius: i32) -> WorldMap {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self::generate_on(seed, size, radius, threads)
    }

    fn generate_on(seed: u64, size: u32, radius: i32, threads: usize) -> WorldMap {
        assert!(size > 0, "a world map needs at least one texel");
        assert!(
            radius > PEAK + TRENCH && i64::from(radius) < i64::from(i32::MAX) / 2,
            "a Planet of {radius} Voxels does not fit its relief or the coordinates"
        );
        let noise = lattice_radius(radius);
        let count = 6 * (size as usize).pow(2);
        let mut raw = vec![0; count];
        par_fill(&mut raw, threads, |index| {
            let (face, i, j) = split(size, index);
            raw_elevation(seed, noise, centre(size, face, i, j))
        });
        let base = normalise(&raw, size);
        drop(raw);
        let mut eroded = base.clone();
        erode(&mut eroded, size, noise, threads);
        let blank = Texel {
            elevation: 0,
            erosion: 0,
            biome: Biome::Temperate,
            regions: Regions::default(),
            rough: 0,
            drainage: 0,
        };
        let drainage = drainage(&eroded, size);
        let mut texels = vec![blank; count];
        par_fill(&mut texels, threads, |index| {
            let (face, i, j) = split(size, index);
            let direction = centre(size, face, i, j);
            let elevation = eroded[index];
            let biome = biome(seed, noise, direction, elevation);
            Texel {
                elevation,
                erosion: (i32::from(base[index]) - i32::from(elevation)) as i16,
                biome,
                regions: regions(seed, noise, direction),
                rough: roughness(slope(&eroded, size, noise, index), biome, drainage[index]),
                drainage: drainage[index],
            }
        });
        WorldMap {
            seed,
            size,
            radius,
            texels,
        }
    }

    /// The seed the map was made from, which per-brick generation draws its own randomness from
    /// (it is not part of [`WorldMap::content_hash`]: the texels are).
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Texels along one face's edge.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The distance of sea level from the Planet's centre, in Voxels.
    pub fn radius(&self) -> i32 {
        self.radius
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
        self.fields_beyond(face, i, j)[0]
    }

    /// The elevation (in Voxels), roughness and drainage of texel `(i, j)` of `face`, where
    /// either may be one beyond the face's edge: across it, or at a cube corner the mean of the
    /// three around it.
    fn fields_beyond(&self, face: usize, i: i32, j: i32) -> [f64; 3] {
        let last = self.size as i32 - 1;
        let (inner_i, inner_j) = (i.clamp(0, last), j.clamp(0, last));
        let read = |(face, i, j): (usize, u32, u32)| {
            let texel = self.texel(face, i, j);
            [
                texel.elevation_voxels(),
                f64::from(texel.rough) / 255.0,
                f64::from(texel.drainage) / 255.0,
            ]
        };
        let (ci, cj) = (inner_i as u32, inner_j as u32);
        let inner = read((face, ci, cj));
        let across_i = || read(step(self.size, face, ci, cj, usize::from(i > 0)));
        let across_j = || read(step(self.size, face, ci, cj, 2 + usize::from(j > 0)));
        match (i != inner_i, j != inner_j) {
            (false, false) => inner,
            (true, false) => across_i(),
            (false, true) => across_j(),
            (true, true) => {
                let (a, b) = (across_i(), across_j());
                std::array::from_fn(|k| (inner[k] + a[k] + b[k]) / 3.0)
            }
        }
    }

    /// The fields at the ray from the Planet's centre along `direction`: the elevation of
    /// [`WorldMap::elevation_at`], and the roughness and drainage, interpolated the same way.
    pub fn fields_at(&self, direction: [f64; 3]) -> Fields {
        let (face, x, y) = position(self.size, direction);
        let (x, y) = (x - 0.5, y - 0.5);
        let (i, j) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - f64::from(i), y - f64::from(j));
        let at = |di, dj| self.fields_beyond(face, i + di, j + dj);
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        let (a00, a10, a01, a11) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
        let [elevation, rough, drainage] =
            std::array::from_fn(|k| lerp(lerp(a00[k], a10[k], fx), lerp(a01[k], a11[k], fx), fy));
        Fields {
            elevation,
            rough,
            drainage,
        }
    }

    /// The unit vector from the Planet's centre through the middle of texel `(i, j)` of `face`.
    /// Panics if one is out of range.
    pub fn texel_direction(&self, face: usize, i: u32, j: u32) -> [f64; 3] {
        assert!(face < 6 && i < self.size && j < self.size);
        centre(self.size, face, i, j)
    }

    /// Bounds `(lowest, highest)` on [`WorldMap::elevation_at`] over every direction in the hull
    /// of `directions`, as rays from the Planet's centre, which must not be empty.
    ///
    /// A face's cube coordinates map straight to its texels, so the hull is within the box of
    /// them that the directions are in. Elevation is interpolated between texel centres: if the
    /// box is in one cell, between four, the interpolation is bilinear there and so the extremes
    /// are at the box's corners, and the bounds are tight. If it is not, they are the extremes
    /// of all the texels the cells use. Directions on different faces give the whole map's
    /// bounds, since the hull is then not known to be on one face. Cheap, if they are near each
    /// other.
    pub fn elevation_range(&self, directions: &[[f64; 3]]) -> (f64, f64) {
        self.field_range(directions).elevation
    }

    /// [`WorldMap::elevation_range`] for all of the [`Fields`] at once: each is interpolated the
    /// same way, so each has its extremes where the elevation does.
    pub fn field_range(&self, directions: &[[f64; 3]]) -> FieldRange {
        // The face the directions are on, and the extremes of where they are on it.
        let mut places = directions.iter().map(|&d| position(self.size, d));
        let (face, x, y) = places.next().expect("directions must not be empty");
        let (mut xs, mut ys) = ((x, x), (y, y));
        for (f, x, y) in places {
            if f != face {
                return FieldRange {
                    elevation: (-f64::from(TRENCH), f64::from(PEAK)),
                    rough: (0.0, 1.0),
                    drainage: (0.0, 1.0),
                };
            }
            xs = (xs.0.min(x), xs.1.max(x));
            ys = (ys.0.min(y), ys.1.max(y));
        }
        // Texel centres are at whole numbers, half a texel in from the edges of `position`. A
        // point is interpolated between the texels from (floor(x), floor(y)) to one more along
        // each.
        let span = |(lo, hi): (f64, f64)| {
            let (lo, hi) = (lo - 0.5, hi - 0.5);
            (lo, hi, lo.floor() as i32, hi.floor() as i32 + 1)
        };
        let ((x0, x1, i0, i1), (y0, y1, j0, j1)) = (span(xs), span(ys));
        let texel = |i, j| self.fields_beyond(face, i, j);
        let mut low = [f64::INFINITY; 3];
        let mut high = [f64::NEG_INFINITY; 3];
        let mut take = |e: [f64; 3]| {
            for k in 0..3 {
                low[k] = low[k].min(e[k]);
                high[k] = high[k].max(e[k]);
            }
        };
        // Between the texels the fields are bilinear, so a rectangle of the face has its extremes
        // where the lines of the texels cut it, and at its corners. A rectangle across a few
        // texels is bounded that way, and not by all the texels that it is over, which would be
        // the whole range of a few hundred metres for a brick that only has a texel's edge in it.
        if i1 - i0 <= 4 && j1 - j0 <= 4 {
            let breaks = |lo: f64, hi: f64| {
                let mut at = vec![lo];
                at.extend(
                    ((lo.floor() as i32 + 1)..=(hi.floor() as i32))
                        .map(f64::from)
                        .filter(|&b| b < hi),
                );
                at.push(hi);
                at
            };
            let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
            for &y in &breaks(y0, y1) {
                for &x in &breaks(x0, x1) {
                    let (i, j) = (x.floor() as i32, y.floor() as i32);
                    let (fx, fy) = (x - f64::from(i), y - f64::from(j));
                    let [a00, a10, a01, a11] =
                        [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(di, dj)| texel(i + di, j + dj));
                    take(std::array::from_fn(|k| {
                        lerp(lerp(a00[k], a10[k], fx), lerp(a01[k], a11[k], fx), fy)
                    }));
                }
            }
        } else {
            for j in j0..=j1 {
                for i in i0..=i1 {
                    take(texel(i, j));
                }
            }
        }
        FieldRange {
            elevation: (low[0], high[0]),
            rough: (low[1], high[1]),
            drainage: (low[2], high[2]),
        }
    }

    /// The one cell of the bilinear interpolation of [`WorldMap::elevation_at`] that all of
    /// `directions` are in, if they are in one: so that the elevation in any direction between
    /// them can be had without reading the four texels again.
    pub fn elevation_cell(&self, directions: &[[f64; 3]]) -> Option<ElevationCell> {
        let mut places = directions.iter().map(|&d| position(self.size, d));
        let (face, x, y) = places.next().expect("directions must not be empty");
        let cell = |v: f64| (v - 0.5).floor() as i32;
        let ((mut i0, mut i1), (mut j0, mut j1)) = ((cell(x), cell(x)), (cell(y), cell(y)));
        for (f, x, y) in places {
            if f != face {
                return None;
            }
            (i0, i1) = (i0.min(cell(x)), i1.max(cell(x)));
            (j0, j1) = (j0.min(cell(y)), j1.max(cell(y)));
        }
        (i0 == i1 && j0 == j1).then(|| {
            let [a00, a10, a01, a11] = [(0, 0), (1, 0), (0, 1), (1, 1)]
                .map(|(di, dj)| self.fields_beyond(face, i0 + di, j0 + dj));
            ElevationCell {
                size: self.size,
                face,
                i: i0,
                j: j0,
                a: [a00, a10, a01, a11],
            }
        })
    }

    /// FNV-1a hash of the size and every texel, face by face, row by row.
    pub fn content_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let texels = self.texels.iter().flat_map(|texel| {
            let [e0, e1] = texel.elevation.to_le_bytes();
            let [r0, r1] = texel.erosion.to_le_bytes();
            [
                e0,
                e1,
                r0,
                r1,
                texel.biome as u8,
                texel.regions.0,
                texel.rough,
                texel.drainage,
            ]
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

/// Value noise summed over `octaves` octaves, from cells `1 << first` lattice points wide down,
/// each half the weight of the one before; in `[-1, 1)`. The noise is sampled at the lattice
/// point of the sphere of `noise` lattice points in radius (see [`lattice_radius`]) in
/// `direction`.
fn fractal(seed: u64, noise: i32, direction: [f64; 3], first: u32, octaves: u32) -> f64 {
    let point = direction.map(|d| (d * f64::from(noise)).round() as i32);
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

/// Where the noise's cells start, as the power of two of lattice points (10 cm) of the widest:
/// the sizes of what the map shows, fixed in kilometres whatever the radius.
///
/// Continents: sites in cubes of 2^19 points (52 km), see [`continents`]. Elevation's noise: 2^17
/// points, 13 km, down to 2^11 points (205 m, about a texel of the real map) in 7 octaves. Climate and biomes:
/// 13 km patches. Volcanic hot spots and the fantasy regions: 6.5 km patches.
const ELEVATION_CELL: u32 = 17;
const ELEVATION_OCTAVES: u32 = 7;
const CLIMATE_CELL: u32 = 17;
const PATCH_CELL: u32 = 16;

/// The continents' sites: one in each cube of `2^CONTINENT_CELL` lattice points (52 km), in 3D,
/// so that there is no seam on the sphere. A site is a ball, and a continent is where the ball
/// meets the sphere of the Planet: a ball that is far from the sphere's surface leaves a small
/// island or nothing, one that is on it a continent of up to `CONTINENT_REACH` of a cell's width
/// in radius. So the land is in separate masses of 30 to 60 km, and there are more of them on a
/// bigger Planet, where noise alone at the median would join half the land in one mass (it is the
/// threshold at which a random field's land percolates).
const CONTINENT_CELL: u32 = 19;
/// The most of a cell's width that a continent's radius is.
const CONTINENT_REACH: f64 = 0.55;
const CONTINENT: u64 = 0x3c6e_f372_fe94_f82b;

/// How far a site is from the middle of its cube, as a share of the cube's width (1 is anywhere in
/// it): the less, the more evenly the continents are spread, and the fewer touch.
const CONTINENT_JITTER: f64 = 0.9;

/// How much of the elevation is the continents' shape, against the noise's.
const CONTINENT_SHARE: f64 = 0.62;

/// The share of the Planet's area that is ocean. Not a half: land that is half of a random
/// field's area joins in one mass, and the continents are separate masses.
const OCEAN_SHARE: f64 = 0.54;

/// The continents' shape at a lattice point of the sphere: 1 at the middle of a continent, falling
/// smoothly to 0 at its edge and 0 where there is none; in `[0, 1]`. Deterministic: sums
/// and products of integers and hashes, so the same on every machine.
fn continents(seed: u64, point: [i32; 3]) -> f64 {
    let cell = point.map(|p| p >> CONTINENT_CELL);
    let width = f64::from(1u32 << CONTINENT_CELL);
    let mut best: f64 = 0.0;
    for k in 0..27 {
        let at = [0, 1, 2].map(|a| cell[a] + (k / 3i32.pow(a as u32)) % 3 - 1);
        // The site: three jitters in the cell, and the radius of its ball.
        let jitter = |tag: u64| corner_value(seed ^ CONTINENT ^ tag, at);
        let site = [1, 2, 3].map(|tag| 0.5 + CONTINENT_JITTER * (jitter(tag) - 0.5));
        let reach = width * CONTINENT_REACH * (0.45 + 0.55 * jitter(4));
        let d2: f64 = (0..3)
            .map(|a| {
                let d = (f64::from(at[a]) + site[a]) * width - f64::from(point[a]);
                d * d
            })
            .sum();
        if d2 < reach * reach {
            // 1 - (d / reach)^2, squared: smooth at the edge.
            let t = 1.0 - d2 / (reach * reach);
            best = best.max(t * t);
        }
    }
    best
}

/// The elevation noise at a point, in `[-2^20, 2^20)`, before it is scaled to elevation units:
/// the continents' shape, and the noise that is the land's relief and the coasts' shape.
fn raw_elevation(seed: u64, noise: i32, direction: [f64; 3]) -> i32 {
    let point = direction.map(|d| (d * f64::from(noise)).round() as i32);
    let relief = fractal(
        seed ^ ELEVATION,
        noise,
        direction,
        ELEVATION_CELL,
        ELEVATION_OCTAVES,
    );
    let shape = 2.0 * continents(seed, point) - 1.0;
    let total = CONTINENT_SHARE * shape + (1.0 - CONTINENT_SHARE) * relief;
    (total * 1_048_576.0) as i32
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

/// Scales the noise to elevation units so that sea level sits where [`OCEAN_SHARE`] of the area is
/// below it, which makes that share of the Planet ocean, the highest point [`PEAK`] above it and the lowest [`TRENCH`] below.
fn normalise(raw: &[i32], size: u32) -> Vec<i16> {
    const SHIFT: u32 = 5;
    const OFFSET: i32 = 1 << 20;
    let weights = area_weights(size);
    let mut area = vec![0u64; 1 << (21 - SHIFT)];
    for (index, &value) in raw.iter().enumerate() {
        area[((value + OFFSET) >> SHIFT) as usize] += u64::from(weights[index % weights.len()]);
    }
    let half = (area.iter().sum::<u64>() as f64 * OCEAN_SHARE) as u64;
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
                above * i64::from(PEAK_UNITS) / rise
            } else {
                above * i64::from(TRENCH_UNITS) / fall
            }) as i16
        })
        .collect()
}

/// Thermal erosion: each texel sheds an eighth of its height over the talus to each lower
/// neighbour, and gains the same from each higher one, so material only moves. Every step reads
/// the last step's heights and writes new ones, so the order of texels does not matter. The
/// number of steps and the talus scale with the texel size, so the slope limit is the same at
/// every size, but a coarse map is not a small copy of a fine one: material spreads about
/// `sqrt(size / 16)` texels, and the amounts moved differ.
fn erode(heights: &mut Vec<i16>, size: u32, noise: i32, threads: usize) {
    // A slope of 0.2 (11 degrees) across one texel, which is a quarter of the circumference of
    // sea level (pi / 2 of the radius, 251,327 units at 16 km), divided by `size`, wide.
    let quarter = i64::from(noise) * 1_570_796 / 1_000_000;
    let talus = (quarter / i64::from(size)) as i32 / 5;
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

/// The steepest slope from texel `index` to one of its four neighbours: the rise over the run,
/// where a texel is `noise * pi / 2 / size` lattice points wide (10 cm, as the elevation's units).
fn slope(heights: &[i16], size: u32, noise: i32, index: usize) -> f64 {
    let (face, i, j) = split(size, index);
    let here = i32::from(heights[index]);
    let rise = (0..4)
        .map(|side| {
            let (f, i, j) = step(size, face, i, j, side);
            (here - i32::from(heights[join(size, f, i, j)])).abs()
        })
        .max()
        .unwrap_or(0);
    let run = f64::from(noise) * std::f64::consts::FRAC_PI_2 / f64::from(size);
    f64::from(rise) / run
}

/// How rough the ground between the texels is made, from the slope (a share of the steepest the
/// erosion leaves, 0.2), the biome and the drainage: `[0, 255]` for `[0, 1]`. Only sums,
/// products and quotients, so the same on every machine.
fn roughness(slope: f64, biome: Biome, drainage: u8) -> u8 {
    let steep = (slope / 0.12).min(1.0);
    let biome = match biome {
        Biome::Volcanic => 1.0,
        Biome::Temperate => 0.85,
        Biome::Snow => 0.8,
        Biome::Desert => 0.65,
    };
    let valley = 1.0 - 0.5 * f64::from(drainage) / 255.0;
    ((0.3 + 0.7 * steep) * biome * valley * 255.0 + 0.5) as u8
}

/// How much water drains through each texel: land flows to its lowest neighbour (of four, the
/// first on a tie) if that is lower than it, so the texels' areas add up from the high ground
/// down to the sea or to a pit. The log of the area in texels, 16 to the doubling (the log is
/// from the integer's bits, not a function that machines differ on): 0 for a texel that nothing
/// drains into, and on the sea. A proxy for the large drainage, at the map's texel: the map has
/// no rivers (M1b-1).
fn drainage(heights: &[i16], size: u32) -> Vec<u8> {
    let count = heights.len();
    // Highest first, by counting sort over the height (a texel's order among equals is its index).
    let mut counts = vec![0u32; 1 << 16];
    for &h in heights {
        counts[(i32::from(h) + 32_768) as usize] += 1;
    }
    let mut start = 0u32;
    for c in counts.iter_mut().rev() {
        let n = *c;
        *c = start;
        start += n;
    }
    let mut order = vec![0u32; count];
    for (index, &h) in heights.iter().enumerate() {
        let slot = &mut counts[(i32::from(h) + 32_768) as usize];
        order[*slot as usize] = index as u32;
        *slot += 1;
    }
    let mut area = vec![1u32; count];
    for &index in &order {
        let index = index as usize;
        let here = heights[index];
        if here < SEA_LEVEL {
            continue;
        }
        let (face, i, j) = split(size, index);
        let mut lowest = (here, usize::MAX);
        for side in 0..4 {
            let (f, i, j) = step(size, face, i, j, side);
            let at = join(size, f, i, j);
            if heights[at] < lowest.0 {
                lowest = (heights[at], at);
            }
        }
        if lowest.1 != usize::MAX {
            area[lowest.1] = area[lowest.1].saturating_add(area[index]);
        }
    }
    area.iter()
        .zip(heights)
        .map(|(&a, &h)| {
            if h < SEA_LEVEL || a < 2 {
                return 0;
            }
            let shift = a.leading_zeros();
            let whole = 31 - shift;
            let part = ((a << shift) >> 27) & 15;
            (whole * 16 + part).min(255) as u8
        })
        .collect()
}

/// The biome at a texel: cold at the poles and on high ground, dry in a band of subtropical
/// latitudes, volcanic in rare hot spots.
fn biome(seed: u64, noise: i32, direction: [f64; 3], elevation: i16) -> Biome {
    let latitude = direction[1].abs();
    let land = elevation >= SEA_LEVEL;
    let height = f64::from(elevation.max(SEA_LEVEL)) / f64::from(PEAK_UNITS);
    let patch =
        |tag: u64, first: u32, octaves: u32| fractal(seed ^ tag, noise, direction, first, octaves);
    let warmth = 1.0 - latitude - 0.8 * height + 0.1 * patch(WARMTH, CLIMATE_CELL, 3);
    let subtropics = (1.0 - (latitude - 0.35).abs() / 0.35).max(0.0);
    let aridity = patch(ARIDITY, CLIMATE_CELL, 3) + 0.7 * subtropics;
    if land && patch(VOLCANISM, PATCH_CELL, 4) > 0.4 {
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
fn regions(seed: u64, noise: i32, direction: [f64; 3]) -> Regions {
    Regions(
        REGION
            .iter()
            .enumerate()
            .filter(|&(_, &tag)| fractal(seed ^ tag, noise, direction, PATCH_CELL, 3) > 0.35)
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
        MAP.get_or_init(|| WorldMap::with_radius(SEED, SIZE, TEST_RADIUS))
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
        let one = WorldMap::generate_on(SEED, 64, TEST_RADIUS, 1).content_hash();
        for threads in [2, 3, 8] {
            let many = WorldMap::generate_on(SEED, 64, TEST_RADIUS, threads);
            assert_eq!(one, many.content_hash());
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
        // Units of 10 cm: 20,000 is 2 km. Erosion shaves the highest peak a little.
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
        let here = map.texel(face, i, j).elevation_voxels();
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
        let mean = (here + map.texel(face, i + 1, j).elevation_voxels()) / 2.0;
        assert!((map.elevation_at(cube) - mean).abs() < 0.01);
    }

    #[test]
    fn an_elevation_cell_gives_the_bits_elevation_at_does_and_nothing_outside_it() {
        let map = map();
        let e = 1e-4;
        // Around a texel centre (its cell's corner), a face edge and a cube corner.
        for around in [
            map.texel_direction(1, 100, 60),
            map.texel_direction(2, 255, 3),
            map.texel_direction(4, 0, 0),
            [1.0, 1.0, 1.0],
        ] {
            let at = |s: f64, t: f64| [around[0] + e * s, around[1] + e * t, around[2] - e * s];
            let steps = [-3.0, -1.0, 0.0, 0.5, 2.0, 3.0];
            let mut inside = 0;
            for &(s, t) in &[(0.1, 0.1), (0.2, 0.3), (0.7, 0.1)] {
                let Some(cell) = map.elevation_cell(&[at(s, t), at(s + 0.01, t + 0.01)]) else {
                    continue;
                };
                for &a in &steps {
                    for &b in &steps {
                        let direction = at(a, b);
                        if let Some(fields) = cell.fields_at(direction) {
                            inside += 1;
                            assert_eq!(fields, map.fields_at(direction));
                            assert_eq!(
                                fields.elevation.to_bits(),
                                map.elevation_at(direction).to_bits()
                            );
                        }
                    }
                }
            }
            assert!(inside > 0, "{around:?}");
        }
        // Directions in different cells have no cell.
        assert!(
            map.elevation_cell(&[
                map.texel_direction(1, 100, 60),
                map.texel_direction(1, 103, 60)
            ])
            .is_none()
        );
    }

    #[test]
    fn a_texel_direction_is_in_its_own_texel() {
        let map = map();
        for (face, i, j) in [(0, 0, 0), (3, 255, 0), (5, 17, 200), (2, 128, 128)] {
            assert_eq!(map.locate(map.texel_direction(face, i, j)), (face, i, j));
        }
    }

    #[test]
    fn the_elevation_range_of_some_directions_holds_the_elevation_of_every_direction_between() {
        let map = map();
        let e = 1e-3;
        // Directions scattered around a texel centre, a face edge and a cube corner.
        for around in [
            map.texel_direction(1, 100, 60),
            map.texel_direction(2, 255, 3),
            map.texel_direction(4, 0, 0),
            [1.0, 1.0, 1.0],
        ] {
            let at = |s: f64, t: f64| [around[0] + e * s, around[1] + e * t, around[2] - e * s];
            let corners = [at(-2.0, -2.0), at(2.0, -2.0), at(-2.0, 2.0), at(2.0, 2.0)];
            let (low, high) = map.elevation_range(&corners);
            assert!(low <= high);
            for k in 0..25 {
                let p = at(f64::from(k % 5) - 2.0, f64::from(k / 5) - 2.0);
                let elevation = map.elevation_at(p);
                assert!(
                    (low..=high).contains(&elevation),
                    "{low} {elevation} {high}"
                );
            }
        }
        // Directions on different faces say nothing about where, and so cover the whole map.
        let (low, high) = map.elevation_range(&[[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]]);
        assert!(low <= -f64::from(TRENCH) && high >= f64::from(PEAK));
        // Within one cell the bounds are tight: a hair's breadth of directions is a hair's
        // breadth of elevations, though the texels around differ by much more.
        let near = |s: f64| {
            // The middle of the cell between four texel centres.
            let (p, q) = (
                map.texel_direction(3, 40, 40),
                map.texel_direction(3, 41, 41),
            );
            let c = [0, 1, 2].map(|k| p[k] + q[k]);
            [c[0] + 1e-7 * s, c[1], c[2] - 1e-7 * s]
        };
        let (low, high) = map.elevation_range(&[near(-1.0), near(1.0)]);
        assert!(high - low < 0.02, "{low} {high}");
        assert!(low <= map.elevation_at(near(0.0)) && map.elevation_at(near(0.0)) <= high);
        // At a texel's centre the range is that of a cell, which is among the texels around it.
        let centre = map.texel_direction(0, 100, 100);
        let (low, high) = map.elevation_range(&[centre]);
        let here = map.elevation_at(centre);
        assert!(low <= here + 1e-9 && here <= high + 1e-9);
        let all: Vec<f64> = (0..9)
            .map(|k| map.texel(0, 99 + k % 3, 99 + k / 3).elevation_voxels())
            .collect();
        assert!(low >= all.iter().copied().fold(f64::INFINITY, f64::min) - 1e-9);
        assert!(high <= all.iter().copied().fold(f64::NEG_INFINITY, f64::max) + 1e-9);
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
