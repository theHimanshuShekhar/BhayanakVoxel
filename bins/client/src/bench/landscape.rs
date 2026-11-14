//! The landscape golden (#166): three fixed views of the Planet's ground from the Spawn, on the
//! ground, 500 m up and 20 km up, rendered offscreen on lavapipe and compared with the approved
//! PNGs in `bins/client/golden/`. They are for judging the detail that the generator adds to the
//! map (ADR 0008): the ground that is made of it at the scale of a person, of a hill and of a
//! region.
//!
//! The views are from the steepest land near the Spawn, looking down the way it falls away, so
//! that there is relief to see (see `site`). The renderer shades a Voxel by the face of it that
//! the ray hits and the sun's angle to that face, and varies its colour from Voxel to Voxel: it has
//! no smooth normals, so a slope shows as steps of faces and as the changes of the ground's
//! Material, and gentle ground, whose Voxel tops all face up, shows as one tone.
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

fn unit_length(a: [f64; 3]) -> f64 {
    a.iter().map(|c| c * c).sum::<f64>().sqrt()
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    a.map(|c| c / unit_length(a))
}

/// The views: a name, how high above the ground the camera is in metres, and how far it looks
/// down, in degrees.
const VIEWS: [(&str, f64, f64); 3] = [
    ("ground", 2.0, 4.0),
    ("500m", 500.0, 22.0),
    ("20km", 20_000.0, 40.0),
];

/// How far the ground falls, in Voxels, in the steepest of four directions 20 m from `at`.
fn drop(planet: &Planet, at: [f64; 3]) -> f64 {
    let (ahead, aside) = tangents(unit(at));
    let step = 20.0 * VOXELS_PER_METRE;
    let height = planet.ground_height(at);
    [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
        .into_iter()
        .map(|(s, t)| {
            let to: [f64; 3] =
                std::array::from_fn(|a| at[a] + (ahead[a] * s + aside[a] * t) * step);
            height - planet.ground_height(to)
        })
        .fold(f64::MIN, f64::max)
}

/// Where the views are from: the steepest land of the Planet, and the way to look from there.
/// The site is picked from the ground itself, since a view of ground that is all one gentle slope
/// has nothing to show: of the map's texels that are land 40 m or more above the sea, the one whose
/// ground falls most in 20 m (taken at the texel's middle and at a lattice of 49 points 40 m
/// apart around it, since the detail's relief is finer than a texel), and then the steepest of
/// that lattice. The way is the one of sixteen along which the ground falls away from the site
/// (the lowest mean height over the next 0.4 to 3 km, with land the whole way), so that the ground
/// recedes below the camera to the horizon and the camera does not face the hill it is on.
fn site(planet: &Planet) -> ([f64; 3], [f64; 3]) {
    let map = planet.map();
    let mut best = (f64::MIN, [0.0; 3]);
    for face in 0..6 {
        for j in 0..map.size() {
            for i in 0..map.size() {
                let texel = map.texel(face, i, j);
                if i32::from(texel.elevation) < 400 {
                    continue;
                }
                let direction = map.texel_direction(face, i, j);
                let radius = planet.sea_radius() + planet.ground_height(direction);
                let at: [f64; 3] = direction.map(|d| d * radius / unit_length(direction));
                let steep = drop(planet, at);
                if steep > best.0 {
                    best = (steep, at);
                }
            }
        }
    }
    let (ahead, aside) = tangents(unit(best.1));
    let mut top = best;
    for i in -3..=3 {
        for j in -3..=3 {
            let step = 40.0 * VOXELS_PER_METRE;
            let around: [f64; 3] = std::array::from_fn(|a| {
                best.1[a] + (ahead[a] * f64::from(i) + aside[a] * f64::from(j)) * step
            });
            let height = planet.ground_height(around);
            let at = unit(around).map(|u| u * (planet.sea_radius() + height));
            let steep = drop(planet, at);
            if steep > top.0 {
                top = (steep, at);
            }
        }
    }
    eprintln!(
        "site: ground falls {:.1} Voxels in 20 m ({:.0} degrees)",
        top.0,
        (top.0 / (20.0 * VOXELS_PER_METRE)).atan().to_degrees()
    );
    let top = top.1;
    let (ahead, aside) = tangents(unit(top));
    let way = |k: i32| {
        let angle = f64::from(k) * std::f64::consts::FRAC_PI_8;
        let (s, c) = (angle.sin(), angle.cos());
        std::array::from_fn::<f64, 3, _>(|a| ahead[a] * c + aside[a] * s)
    };
    // The mean height over the next 0.4 to 3 km, or nothing (so, not chosen) if any of it is sea.
    let mean = |k: i32| -> f64 {
        let heights: Vec<f64> = (1..=13)
            .map(|n| {
                let metres = 400.0 + 200.0 * f64::from(n - 1);
                let point: [f64; 3] =
                    std::array::from_fn(|a| top[a] + way(k)[a] * metres * VOXELS_PER_METRE);
                planet.ground_height(point)
            })
            .collect();
        if heights.iter().any(|&h| h < 0.0) {
            f64::MAX
        } else {
            heights.iter().sum::<f64>() / heights.len() as f64
        }
    };
    let k = (0..16)
        .min_by(|&a, &b| mean(a).total_cmp(&mean(b)))
        .expect("sixteen ways");
    (top, way(k))
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
    let site = site(planet);
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

/// How many different colours there are in `rgba`, with each channel taken to 16 levels, of those
/// that are at least 1% of its pixels.
fn tones(rgba: &[u8]) -> usize {
    let mut counts = std::collections::HashMap::<[u8; 3], usize>::new();
    for p in rgba.chunks(4) {
        *counts.entry([p[0] >> 4, p[1] >> 4, p[2] >> 4]).or_default() += 1;
    }
    let pixels = rgba.len() / 4;
    counts.values().filter(|&&c| c * 100 >= pixels).count()
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
        // The ground has relief and more than one kind of ground in it: the lower half of the view,
        // which is ground, has at least five different tones (the share of the sun on the faces
        // that the ground's steps show, and its Materials), at 16 levels a channel.
        let tones = tones(&image.rgba[image.rgba.len() / 2..]);
        assert!(tones >= 5, "{name}: {tones} tones in the ground");
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
