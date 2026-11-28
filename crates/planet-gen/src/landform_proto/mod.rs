//! PROTOTYPE (#246): the landform model C2 of `examples/proto_landform`, run at the world map's
//! scale. Plates place the continents and the lines of the big ranges, warped noise draws the
//! coasts and the archipelagos, splines turn them into heights, a stream-power erosion carves
//! them, and a wind-band climate and a placeholder Biome table sit on top.
//!
//! Not deterministic across machines (it uses `sin`, `cos`, `exp` and `powf`, which clippy bans
//! in the rest of this crate) and with one fixed seed: it exists to look at, not to ship.
#![allow(clippy::all, clippy::disallowed_methods, dead_code, unused_variables)]

mod erosion;
mod grid;
mod landform;
mod model;
mod noise;

use grid::Grid;

/// What the world map needs of the prototype, per texel of its own cube-sphere grid.
pub struct Landform {
    pub grid: Grid,
    /// Height above sea level before erosion, metres.
    pub h0: Vec<f32>,
    /// Height above sea level after erosion, metres (the sea floor is negative).
    pub h: Vec<f32>,
    pub temp: Vec<f32>,
    pub moist: Vec<f32>,
    /// Index into [`landform::BIOMES`].
    pub biome: Vec<u8>,
    /// Drainage area, km^2 (land only).
    pub drainage_km2: Vec<f32>,
}

/// Runs the whole model at `res` x `res` texels a face (planet radius 320 km).
pub fn generate(res: usize) -> Landform {
    let t = std::time::Instant::now();
    let log = std::env::var("LANDFORM_LOG").is_ok();
    let g = Grid::new(res);
    let plates = model::make_plates(&g, true);
    let raw = model::raw_c2(&g, &plates);
    let shape = model::shape_spline(&g, &raw);
    drop(raw);
    if log {
        eprintln!("landform: land model {:.1} s", t.elapsed().as_secs_f64());
    }
    let h0 = shape.h.clone();
    let eroded = erosion::erode(
        &g,
        &h0,
        &shape.uplift,
        &erosion::Params {
            steps: 36,
            k: 0.02,
            u: 10.0,
            smax: 650.0,
        },
    );
    if log {
        eprintln!("landform: eroded {:.1} s", t.elapsed().as_secs_f64());
    }
    let h = eroded.h;
    let clim = landform::climate(&g, &h);
    let fam = landform::classify(&g, &shape, &h);
    let biome = landform::biomes(&g, &fam, &clim, &h);
    if log {
        eprintln!(
            "landform: climate and biomes {:.1} s",
            t.elapsed().as_secs_f64()
        );
    }
    Landform {
        grid: g,
        h0,
        h,
        temp: clim.temp,
        moist: clim.moist,
        biome,
        drainage_km2: eroded.area,
    }
}

impl Landform {
    /// Height after erosion, metres, bilinear, at a unit direction.
    pub fn height_at(&self, d: [f64; 3]) -> f32 {
        self.grid.sample(&self.h, d)
    }
    pub fn height0_at(&self, d: [f64; 3]) -> f32 {
        self.grid.sample(&self.h0, d)
    }
    pub fn temp_at(&self, d: [f64; 3]) -> f32 {
        self.grid.sample(&self.temp, d)
    }
    pub fn moist_at(&self, d: [f64; 3]) -> f32 {
        self.grid.sample(&self.moist, d)
    }
    /// The placeholder Biome table's entry at the nearest texel.
    pub fn biome_at(&self, d: [f64; 3]) -> &'static str {
        landform::BIOMES[self.biome[self.grid.nearest(d)] as usize].name
    }
    pub fn drainage_at(&self, d: [f64; 3]) -> f32 {
        self.drainage_km2[self.grid.nearest(d)]
    }
}
