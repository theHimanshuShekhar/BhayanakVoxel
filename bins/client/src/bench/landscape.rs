//! The landscape golden (#166): three fixed views of the Planet's ground from the Spawn, on the
//! ground, 500 m up and 20 km up, rendered offscreen on lavapipe and compared with the approved
//! PNGs in `bins/client/golden/`. They are for judging the detail that the generator adds to the
//! map (ADR 0008): the ground that is made of it at the scale of a person, of a hill and of a
//! region.
//!
//! The Planet is the test Planet (`TEST_RADIUS`, 40 km, on a map of `TEST_MAP_SIZE` texels, which
//! has the real Planet's texel and so its detail). On a mismatch the test writes the new images
//! to the temp folder and says where; `UPDATE_GOLDEN=1 cargo test -p client --bin client
//! landscape` approves them.

use std::path::{Path, PathBuf};

use brick_store::VOXELS_PER_METRE;
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::{TEST_MAP_SIZE, TEST_RADIUS, WorldMap};
use render::lod::Bands;
use render::{Image, Offscreen};

use super::caves::fill;
use super::path::{CameraPath, Keyframe};
use super::write_png;

const WIDTH: u32 = 512;
const HEIGHT: u32 = 288;
/// Voxels at which a Voxel of level 0 covers a pixel of a 512x288 view at 70 degrees: 10.3 m.
const D0: f64 = 205.0;
/// The ring of level 0 about the Spawn: 40 m.
const RING: i32 = (40.0 * VOXELS_PER_METRE) as i32;
/// Where the Guard begins: the levels above are held, the ones below are loaded as asked.
const GUARD_FIRST: usize = crate::loader::GUARD_FIRST + 2;
/// The mean squared error, over the colour channels, that a Mesa version's rounding may add.
const MAX_MSE: f64 = 4.0;

fn unit(a: [f64; 3]) -> [f64; 3] {
    let length = a.iter().map(|c| c * c).sum::<f64>().sqrt();
    a.map(|c| c / length)
}

/// The views: a name, how high above the ground the camera is in metres, and how far it looks
/// down, in degrees.
const VIEWS: [(&str, f64, f64); 3] = [
    ("ground", 2.0, 2.0),
    ("500m", 500.0, 15.0),
    ("20km", 20_000.0, 40.0),
];

/// The highest ground within 8 km of the Spawn, on a lattice 250 m apart, with the way from it
/// to the Spawn (level with the ground there): the views are from the hill, looking at the
/// country below it, and so the ground has some relief in it.
fn hill(planet: &Planet, spawn: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let up = unit(spawn);
    let (ahead, aside) = tangents(up);
    let step = 250.0 * VOXELS_PER_METRE;
    let mut best = (f64::MIN, spawn);
    for i in -32..=32 {
        for j in -32..=32 {
            let at: [f64; 3] = std::array::from_fn(|a| {
                spawn[a] + ahead[a] * f64::from(i) * step + aside[a] * f64::from(j) * step
            });
            let height = planet.ground_height(at);
            if height > best.0 {
                best = (height, at);
            }
        }
    }
    // The way, of eight, with the most ground above the sea over the next 2 to 6 km.
    let at = unit(best.1);
    let (ahead, aside) = tangents(at);
    let way = |k: i32| {
        let angle = f64::from(k) * std::f64::consts::FRAC_PI_4;
        let (s, c) = (angle.sin(), angle.cos());
        std::array::from_fn(|a| ahead[a] * c + aside[a] * s)
    };
    let land = |k: i32| -> f64 {
        (2..=6)
            .map(|km| {
                let at: [f64; 3] = std::array::from_fn(|a| {
                    best.1[a] + way(k)[a] * f64::from(km) * 1000.0 * VOXELS_PER_METRE
                });
                planet.ground_height(at).max(0.0)
            })
            .sum()
    };
    let k = (0..8)
        .max_by(|&a, &b| land(a).total_cmp(&land(b)))
        .expect("eight ways");
    (best.1, way(k))
}

fn keyframe(planet: &Planet, (top, way): ([f64; 3], [f64; 3]), height: f64, down: f64) -> Keyframe {
    let up = unit(top);
    let ground = planet.sea_radius() + planet.ground_height(top);
    let eye = up.map(|u| u * (ground + height * VOXELS_PER_METRE));
    // Look along `way`, `down` degrees below the horizon: the ground's, not the Planet's.
    let (run, drop) = (down.to_radians().cos(), down.to_radians().sin());
    let reach = 10_000.0 * VOXELS_PER_METRE;
    let target = std::array::from_fn(|a| eye[a] + (way[a] * run - up[a] * drop) * reach);
    Keyframe {
        position: eye,
        target,
    }
}

fn render(planet: &Planet) -> Vec<(&'static str, Image)> {
    let spawn = planet.spawn(100);
    let site = hill(planet, spawn);
    let mut store = planet.crust_around(site.0, RING, crate::RING_LEVELS - 1);
    crate::loader::hold_guard(&mut store, planet, GUARD_FIRST);
    let bands = Bands::with_d0(D0);
    let mut scene: Option<render::Scene> = None;
    let mut images = Vec::new();
    for (name, height, down) in VIEWS {
        let key = keyframe(planet, site, height, down);
        let mut current = match scene.take() {
            Some(mut scene) => {
                scene.recentre(&store, key.position).unwrap();
                scene
            }
            None => crate::planet_scene(&store, key.position, bands, planet.max_radius()).unwrap(),
        };
        fill(&mut current, &mut store, key.position).unwrap();
        let camera = CameraPath::new(vec![key, key]).radial().camera_at(0, 1);
        images.push((
            name,
            Offscreen::new(WIDTH, HEIGHT, &current)
                .unwrap()
                .render(&camera),
        ));
        scene = Some(current);
    }
    images
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("golden")
        .join(format!("landscape_{name}_{WIDTH}x{HEIGHT}.png"))
}

fn read_png(path: &Path) -> Image {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ));
    let mut reader = decoder.read_info().unwrap();
    let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut rgba).unwrap();
    rgba.truncate(info.buffer_size());
    Image {
        width: info.width,
        height: info.height,
        rgba,
    }
}

/// The mean squared error over the colour channels.
fn mse(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: u64 = a
        .rgba
        .chunks(4)
        .zip(b.rgba.chunks(4))
        .map(|(p, q)| {
            (0..3)
                .map(|c| u64::from(p[c].abs_diff(q[c])).pow(2))
                .sum::<u64>()
        })
        .sum();
    sum as f64 / f64::from(3 * a.width * a.height)
}

#[test]
fn the_landscape_from_the_ground_500_m_and_20_km_matches_the_approved_images() {
    let planet = Planet::new(WorldMap::with_radius(
        crate::SEED,
        TEST_MAP_SIZE,
        TEST_RADIUS,
    ));
    let images = render(&planet);
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let out = std::env::temp_dir().join(format!("landscape-{}", std::process::id()));
    let mut failures = Vec::new();
    for (name, image) in &images {
        let path = golden_path(name);
        if update {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_png(&path, image).unwrap();
            eprintln!("wrote {}", path.display());
            continue;
        }
        // Not a blank image: sky and ground both show.
        let first = &image.rgba[..4];
        assert!(
            image.rgba.chunks(4).any(|p| p[..3] != first[..3]),
            "{name} is one colour"
        );
        let error = mse(image, &read_png(&path));
        if error > MAX_MSE {
            std::fs::create_dir_all(&out).unwrap();
            write_png(&out.join(format!("{name}.png")), image).unwrap();
            failures.push(format!("{name}: mse {error:.2} (limit {MAX_MSE})"));
        }
    }
    assert!(
        failures.is_empty(),
        "{failures:?}; the new images are in {}. If the change is intended, rerun with UPDATE_GOLDEN=1",
        out.display()
    );
}
