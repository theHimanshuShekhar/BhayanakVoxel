//! The crystal goldens (#204): two fixed views of the crystal clusters that the generator makes,
//! rendered offscreen on lavapipe like the landscape's (see `landscape`) and compared with the
//! approved PNGs in `bins/client/golden/`: a cluster on a sunlit slope, and a crystal cave.
//!
//! The places are found, not recorded: the generator's own placement rules decide where the
//! clusters are (`Planet::crystal_sites`), and the views are of the longest cluster on lit ground
//! and the cave cluster with the most others round it, from a few metres off, looking at it. A
//! change to the placement therefore moves the views and the goldens are approved again, with
//! `UPDATE_GOLDEN=1 cargo test -p client --bin client crystals`.

use std::path::{Path, PathBuf};

use brick_store::VOXELS_PER_METRE;
use planet_gen::planet::{CrystalSite, Place, Planet, tangents};
use planet_gen::world_map::{Regions, TEST_MAP_SIZE, TEST_RADIUS, WorldMap};
use render::lod::Bands;
use render::{Image, Offscreen};

use super::caves::fill;
use super::path::{CameraPath, Keyframe};
use super::write_png;

const WIDTH: u32 = 512;
const HEIGHT: u32 = 288;
/// Voxels at which a Voxel of level 0 covers a pixel of a 512x288 view at 70 degrees: 10.3 m.
const D0: f64 = 205.0;
/// The ring of level 0 about a view: 40 m.
const RING: i32 = (40.0 * VOXELS_PER_METRE) as i32;
const GUARD_FIRST: usize = crate::loader::GUARD_FIRST + 2;
/// The mean squared error, over the colour channels, that a Mesa version's rounding may add.
const MAX_MSE: f64 = 4.0;

fn length(a: [f64; 3]) -> f64 {
    a.iter().map(|c| c * c).sum::<f64>().sqrt()
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    a.map(|c| c / length(a))
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    length(std::array::from_fn(|k| a[k] - b[k]))
}

fn plus(a: [f64; 3], b: [f64; 3], by: f64) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + b[k] * by)
}

/// The sites in boxes of 128 m round the lit texels of the crystal country on land.
fn sites(planet: &Planet) -> Vec<CrystalSite> {
    let map = planet.map();
    let mut found = Vec::new();
    let mut lit = 0;
    for face in 0..6 {
        for j in 0..map.size() {
            for i in 0..map.size() {
                let t = map.texel(face, i, j);
                if t.elevation < 20 || !t.regions.contains(Regions::CRYSTAL_CAVES) {
                    continue;
                }
                let d = map.texel_direction(face, i, j);
                if !planet.sunlit_at(d).is_lit() {
                    continue;
                }
                lit += 1;
                if lit % 5 != 0 {
                    continue;
                }
                let up = unit(d);
                let at = up.map(|u| u * (planet.sea_radius() + planet.ground_height(up)));
                let half = 64.0 * VOXELS_PER_METRE;
                found.extend(planet.crystal_sites(at.map(|c| c - half), at.map(|c| c + half)));
            }
        }
    }
    found
}

/// A point `length` Voxels along the principal prism of `site`.
fn along(site: &CrystalSite, length: f64) -> [f64; 3] {
    plus(site.anchor, site.normal, length)
}

/// The view of the cluster on lit ground: the longest of them, from 3.5 m downhill of it (the way
/// its normal leans) at the height of a crouching person's eyes, looking at the middle of it.
fn slope(planet: &Planet, sites: &[CrystalSite]) -> Keyframe {
    let site = sites
        .iter()
        .filter(|s| s.place == Place::Ground)
        .max_by(|a, b| {
            a.length
                .total_cmp(&b.length)
                .then(a.anchor[0].total_cmp(&b.anchor[0]))
        })
        .expect("a cluster on lit ground");
    let up = unit(site.anchor);
    let lean = std::array::from_fn::<f64, 3, _>(|k| {
        site.normal[k] - up[k] * site.normal.iter().zip(up).map(|(n, u)| n * u).sum::<f64>()
    });
    let (ahead, _) = tangents(up);
    let away = if length(lean) > 0.05 {
        unit(lean)
    } else {
        ahead
    };
    let off = plus(site.anchor, away, 3.5 * VOXELS_PER_METRE);
    let ground = planet.sea_radius() + planet.ground_height(off);
    let eye = unit(off).map(|u| u * (ground + 1.2 * VOXELS_PER_METRE));
    Keyframe {
        position: eye,
        target: along(site, site.length * 0.6),
    }
}

/// The view of a crystal cave: the cave cluster of 1 m or more with most others within 8 m, from the air of the
/// cave beside it, looking at the middle of it.
fn cave(planet: &Planet, sites: &[CrystalSite]) -> Keyframe {
    let reach = 8.0 * VOXELS_PER_METRE;
    let caves: Vec<&CrystalSite> = sites
        .iter()
        .filter(|s| matches!(s.place, Place::Cave(_)))
        .collect();
    let mut ranked: Vec<(usize, &CrystalSite)> = caves
        .iter()
        .map(|s| {
            (
                caves
                    .iter()
                    .filter(|o| dist(o.anchor, s.anchor) < reach)
                    .count(),
                *s,
            )
        })
        .collect();
    // Big enough to be the subject of a view: 1 m or more (the longest are 1.6 m).
    ranked.retain(|(_, s)| s.length >= 20.0);
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.length.total_cmp(&a.1.length))
            .then(a.1.anchor[0].total_cmp(&b.1.anchor[0]))
    });
    for (_, site) in ranked {
        // From the side, so that the prisms show in profile: a metre and a half out from the
        // wall and as far to the side, in the first of four ways that has air all the way to the
        // middle of the cluster, to within 20% of it.
        let target = along(site, site.length * 0.5);
        let (t1, t2) = tangents(site.normal);
        let air = |at: [f64; 3]| {
            planet
                .voxel(brick_store::VoxelCoord(at.map(|c| c.round() as i32)))
                .is_none()
        };
        let mut best = None;
        for (side, back) in [(1.0, 1.0), (-1.0, 1.0), (1.0, 0.7), (-1.0, 0.7)]
            .into_iter()
            .flat_map(|(s, b)| [(t1, s, b), (t2, s, b)])
            .map(|(t, s, b)| (plus([0.0; 3], t, s), b))
        {
            let out = back * 1.5 * VOXELS_PER_METRE;
            let eye = plus(
                plus(along(site, site.length), side, out),
                site.normal,
                out * 0.5,
            );
            let clear = (0..=16).all(|n| {
                let at: [f64; 3] =
                    std::array::from_fn(|k| eye[k] + (target[k] - eye[k]) * f64::from(n) / 20.0);
                air(at)
            });
            if clear {
                best = Some(eye);
                break;
            }
        }
        if let Some(eye) = best {
            return Keyframe {
                position: eye,
                target: along(site, site.length * 0.5),
            };
        }
    }
    panic!("no cave cluster with air in front of it");
}

fn render(planet: &Planet, views: [(&'static str, Keyframe); 2]) -> Vec<(&'static str, Image)> {
    views
        .into_iter()
        .map(|(name, key)| {
            let mut store = planet.crust_around(key.target, RING, crate::RING_LEVELS - 1);
            crate::loader::hold_guard(&mut store, planet, GUARD_FIRST);
            let bands = Bands::with_d0(D0);
            let mut scene =
                crate::planet_scene(&store, key.position, bands, planet.max_radius()).unwrap();
            fill(&mut scene, &mut store, key.position).unwrap();
            let camera = CameraPath::new(vec![key, key]).radial().camera_at(0, 1);
            let mut gpu = Offscreen::new(WIDTH, HEIGHT, &scene).unwrap();
            (name, gpu.render(&camera))
        })
        .collect()
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("golden")
        .join(format!("crystals_{name}_{WIDTH}x{HEIGHT}.png"))
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

/// Whether the middle of the view, where the camera looks (at the middle of the cluster), is
/// glowing: some pixel within 8 of the centre has a channel at 200 or more, which an emissive
/// crystal reaches and the ground and the rock of the views (their brightest channel is under
/// 170) do not.
fn glows_at_the_centre(image: &Image) -> bool {
    let (cx, cy) = (image.width as i32 / 2, image.height as i32 / 2);
    (-8..=8).any(|dy| {
        (-8..=8).any(|dx| {
            let at = ((cy + dy) as usize * image.width as usize + (cx + dx) as usize) * 4;
            image.rgba[at..at + 3].iter().any(|&c| c >= 200)
        })
    })
}

#[test]
fn a_cluster_on_a_sunlit_slope_and_a_crystal_cave_match_the_approved_images() {
    let planet = Planet::new(WorldMap::with_radius(
        crate::SEED,
        TEST_MAP_SIZE,
        TEST_RADIUS,
    ));
    let sites = sites(&planet);
    let images = render(
        &planet,
        [
            ("slope", slope(&planet, &sites)),
            ("cave", cave(&planet, &sites)),
        ],
    );
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let out = std::env::temp_dir().join(format!("crystals-{}", std::process::id()));
    let mut failures = Vec::new();
    for (name, image) in &images {
        let path = golden_path(name);
        if update {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_png(&path, image).unwrap();
            eprintln!("wrote {}", path.display());
            continue;
        }
        // The cluster is in the view, in the middle, and glows.
        assert!(
            glows_at_the_centre(image),
            "{name}: no crystal at the centre"
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

/// The view along a cave wall (for the look comparison below): the cave cluster with most others
/// in a box that runs 7 m ahead along the wall and 2.5 m each side of the line, seen from 0.8 m
/// out from the wall and 2.5 m behind the cluster, looking along the wall and a little into it,
/// so that the prisms, their tips and their hues show in profile. Only a way that has air all
/// along the sight line is taken.
fn cave_along(planet: &Planet, sites: &[CrystalSite]) -> Keyframe {
    let m = VOXELS_PER_METRE;
    let caves: Vec<&CrystalSite> = sites
        .iter()
        .filter(|s| matches!(s.place, Place::Cave(_)))
        .collect();
    let air = |at: [f64; 3]| {
        planet
            .voxel(brick_store::VoxelCoord(at.map(|c| c.round() as i32)))
            .is_none()
    };
    let mut best: Option<(usize, Keyframe)> = None;
    for site in caves.iter().filter(|s| s.length >= 10.0) {
        let (t1, t2) = tangents(site.normal);
        for k in 0..8 {
            let angle = f64::from(k) * std::f64::consts::FRAC_PI_4;
            let (c, s_) = (angle.cos(), angle.sin());
            let along_wall: [f64; 3] = std::array::from_fn(|a| t1[a] * c + t2[a] * s_);
            let eye = plus(
                plus(site.anchor, along_wall, -2.5 * m),
                site.normal,
                0.8 * m,
            );
            let target = plus(plus(eye, along_wall, 6.0 * m), site.normal, -0.3 * m);
            let clear = (0..=24).all(|n| {
                let at: [f64; 3] =
                    std::array::from_fn(|a| eye[a] + (target[a] - eye[a]) * f64::from(n) / 24.0);
                air(at)
            });
            if !clear {
                continue;
            }
            let seen = caves
                .iter()
                .filter(|o| {
                    let d: [f64; 3] = std::array::from_fn(|a| o.anchor[a] - site.anchor[a]);
                    let ahead: f64 = d.iter().zip(along_wall).map(|(x, y)| x * y).sum();
                    let lateral = length(plus(d, along_wall, -ahead));
                    (0.0..7.0 * m).contains(&ahead) && lateral < 2.5 * m
                })
                .count();
            if best.as_ref().is_none_or(|(n, _)| seen > *n) {
                best = Some((
                    seen,
                    Keyframe {
                        position: eye,
                        target,
                    },
                ));
            }
        }
    }
    best.expect("a cave cluster with air along its wall").1
}

/// The look of the crystals at several emission strengths and tone curves, for the owner to
/// choose from (#204): the slope and a view along a cave wall, at an `emissive_intensity` of 1.0,
/// 1.5 and 3.0 (the table's), with the game's tone curve and with Neutral (which keeps a
/// saturated colour's hue where the AgX-like one takes it towards white). The PNGs are in
/// `target/crystal-look/`, not committed: `cargo test -p client --bin client crystal_look --
/// --ignored`.
#[test]
#[ignore = "renders twelve images for a decision; writes PNGs"]
fn crystal_look_comparison() {
    use render::{Resolution, Stages, ToneCurve};
    let planet = Planet::new(WorldMap::with_radius(
        crate::SEED,
        TEST_MAP_SIZE,
        TEST_RADIUS,
    ));
    let sites = sites(&planet);
    let views = [
        ("slope", slope(&planet, &sites)),
        ("cave", cave_along(&planet, &sites)),
    ];
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crystal-look");
    std::fs::create_dir_all(&out).unwrap();
    let toml = include_str!("../../../../crates/materials/materials.toml");
    let with_intensity = |intensity: f32| {
        let text: Vec<String> = toml
            .split("[[material]]")
            .map(|block| {
                if block.contains("name = \"crystal ") {
                    block
                        .lines()
                        .map(|l| {
                            if l.starts_with("emissive_intensity") {
                                format!("emissive_intensity = {intensity}")
                            } else {
                                l.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    block.to_string()
                }
            })
            .collect();
        materials::MaterialTable::parse(&text.join("[[material]]")).unwrap()
    };
    for intensity in [1.0f32, 1.5, 3.0] {
        let table = with_intensity(intensity);
        for (curve_name, curve) in [("agx", ToneCurve::Agx), ("neutral", ToneCurve::Neutral)] {
            for (name, key) in views {
                let path = out.join(format!("{name}_{intensity}_{curve_name}.png"));
                // A run that was cut short is carried on with `LOOK_KEEP=1`.
                if std::env::var_os("LOOK_KEEP").is_some() && path.exists() {
                    continue;
                }
                let mut store = planet.crust_around(key.target, RING, crate::RING_LEVELS - 1);
                crate::loader::hold_guard(&mut store, &planet, GUARD_FIRST);
                let bands = Bands::with_d0(D0 * 2.0);
                let mut scene =
                    crate::planet_scene(&store, key.position, bands, planet.max_radius()).unwrap();
                fill(&mut scene, &mut store, key.position).unwrap();
                let camera = CameraPath::new(vec![key, key]).radial().camera_at(0, 1);
                let mut gpu = Offscreen::with_materials(
                    Resolution::native(WIDTH * 2, HEIGHT * 2),
                    &scene,
                    &table,
                )
                .unwrap();
                gpu.set_stages(Stages {
                    curve,
                    ..Stages::default()
                });
                let image = gpu.render(&camera);
                write_png(&path, &image).unwrap();
                eprintln!("wrote {}", path.canonicalize().unwrap().display());
            }
        }
    }
}
