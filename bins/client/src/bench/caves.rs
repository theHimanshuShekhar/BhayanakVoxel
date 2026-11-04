//! The cave stills of `client bench` (#46): still cameras at a cave mouth on a slope and inside a
//! tunnel, each timed over a few dozen frames and photographed.
//!
//! The flight is over the small test sphere, which has no caves (they are the real Planet's:
//! `planet_gen::planet`). So the stills are of the real Planet, on a coarse world map that takes
//! a second to make, around the cave that [`find_site`] picks: a ring of level 0 round it, the
//! Guard from level 10 and a loader that fills in what the bands ask for, on this thread. The
//! bands are for a `d0` of [`D0`], a tenth of the flight's, so that the view is made in seconds
//! and not minutes: what is timed is the cave, not the Planet's far ground.

use std::error::Error;
use std::path::Path;

use brick_store::{BrickCoord, BrickStore, VoxelCoord, children_from};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::{WORLD_RADIUS, WorldMap};
use render::lod::{Bands, brick_distance};
use render::{Mode, Offscreen, Resolution, Scene};

use super::budget::Budgets;
use super::path::{CameraPath, Keyframe};
use super::report::CaveStill;
use super::{Flight, ModePlan, draw, pass_stats, run_mode, write_png};

/// Texels along a face of the world map the stills use.
const MAP_SIZE: u32 = 512;
/// Voxels round the cave that the level 0 ring holds: 30 m.
const RING: i32 = 300;
/// Where the Guard begins.
const GUARD_FIRST: usize = 10;
/// Voxels at which a Voxel of level 0 covers one output pixel, for the stills: 10 m.
const D0: f64 = 100.0;
/// Samples of a reference screenshot, which is not timed.
const SHOT_SPP: u32 = 16;

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    a.map(|c| c / length(a))
}

/// A still camera.
pub struct Still {
    pub name: &'static str,
    pub key: Keyframe,
}

/// The cave the stills are of.
pub struct Site {
    /// Where it opens to the surface, on the ground.
    pub mouth: [f64; 3],
    /// A camera at the mouth, and one inside, in that order.
    pub stills: [Still; 2],
}

/// How much the ground rises across 60 Voxels at `point`, at the most along the two ways.
fn slope(planet: &Planet, point: [f64; 3]) -> f64 {
    let (ahead, aside) = tangents(unit(point));
    let rise = |way: [f64; 3]| {
        let at = |side: f64| {
            planet
                .map()
                .elevation_at(std::array::from_fn(|a| point[a] + way[a] * side))
        };
        (at(30.0) - at(-30.0)).abs()
    };
    rise(ahead).max(rise(aside)) / 60.0
}

/// The Voxels of air under the ground within 8 m or so of the mouth `point`, on a lattice 80 cm
/// apart: the cave that the mouth leads into.
fn air_under_the_ground(planet: &Planet, point: [f64; 3]) -> Vec<[f64; 3]> {
    let up = unit(point);
    let (ahead, aside) = tangents(up);
    let mut found = Vec::new();
    for i in -10..=10 {
        for j in -10..=10 {
            for k in -8..=0 {
                let at: [f64; 3] = std::array::from_fn(|a| {
                    point[a]
                        + ahead[a] * f64::from(i * 8)
                        + aside[a] * f64::from(j * 8)
                        + up[a] * f64::from(k * 8 - 8)
                });
                let voxel = at.map(f64::round);
                let ground = f64::from(WORLD_RADIUS) + planet.map().elevation_at(voxel);
                if planet.voxel(VoxelCoord(voxel.map(|c| c as i32))).is_none()
                    && length(voxel) < ground - 4.0
                {
                    found.push(voxel);
                }
            }
        }
    }
    found
}

/// The cave for the stills: of the steepest mouths of the Planet, the first with a good deal of
/// cave under the ground by it. The camera at the mouth is on the ground on the downhill side
/// (the cave goes into the hill), 9 m off and 2 m up, looking at the mouth; the one inside is in
/// the air nearest the middle of the cave, looking away from the mouth, down the tunnel.
pub fn find_site(planet: &Planet) -> Site {
    let mut mouths: Vec<([f64; 3], f64)> = planet
        .cave_mouths()
        .into_iter()
        .map(|m| (m, slope(planet, m)))
        .collect();
    // Steepest first; the sort is stable, so ties keep the order of the table.
    mouths.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (mouth, air) = mouths
        .iter()
        .take(60)
        .map(|&(m, _)| (m, air_under_the_ground(planet, m)))
        .find(|(_, air)| air.len() >= 150)
        .expect("a Planet with caves has a steep mouth with some cave by it");
    let middle: [f64; 3] =
        std::array::from_fn(|a| air.iter().map(|p| p[a]).sum::<f64>() / air.len() as f64);
    let inside = *air
        .iter()
        .min_by(|a, b| {
            let from_middle = |p: &[f64; 3]| length(std::array::from_fn(|k| p[k] - middle[k]));
            from_middle(a).total_cmp(&from_middle(b))
        })
        .expect("there is air");
    // The way into the hill: along the ground, from the mouth to the cave.
    let up = unit(mouth);
    let to_cave: [f64; 3] = std::array::from_fn(|a| inside[a] - mouth[a]);
    let level: [f64; 3] = std::array::from_fn(|a| to_cave[a] - up[a] * dot(to_cave, up));
    let inward = if length(level) < 1.0 {
        tangents(up).0
    } else {
        unit(level)
    };
    let outside: [f64; 3] = std::array::from_fn(|a| mouth[a] - inward[a] * 90.0);
    let ground = f64::from(WORLD_RADIUS) + planet.map().elevation_at(outside);
    let eye = unit(outside).map(|c| c * (ground + 20.0));
    Site {
        mouth,
        stills: [
            Still {
                name: "mouth",
                key: Keyframe {
                    position: eye,
                    target: std::array::from_fn(|a| mouth[a] - up[a] * 10.0),
                },
            },
            Still {
                name: "tunnel",
                key: Keyframe {
                    position: inside,
                    target: std::array::from_fn(|a| inside[a] - (mouth[a] - inside[a])),
                },
            },
        ],
    }
}

/// Loads what `scene` asks for, nearest `position` first, until it asks for nothing: what the
/// client's loader does, with this thread.
fn fill(
    scene: &mut Scene,
    store: &mut BrickStore,
    position: [f64; 3],
) -> Result<(), Box<dyn Error>> {
    let mut pending: Vec<(usize, BrickCoord)> = Vec::new();
    loop {
        pending.extend(scene.take_wanted());
        if pending.is_empty() && scene.waiting() == 0 {
            return Ok(());
        }
        pending.sort_by(|a, b| {
            brick_distance(position, b.0, b.1).total_cmp(&brick_distance(position, a.0, a.1))
        });
        let source = store.source().expect("the ring's store has a source");
        let mut done = Vec::new();
        for _ in 0..400 {
            let Some((level, coord)) = pending.pop() else {
                break;
            };
            let bricks = children_from(&*source, level, coord, |c| store.holds(level - 1, c));
            store.hold_children(level, coord, bricks);
            done.push((level, coord));
        }
        scene.arrive(store, &done)?;
    }
}

/// What the stills time: frames of each mode.
pub struct Shares {
    pub primary: ModePlan,
    pub reference: ModePlan,
}

/// Times and photographs the stills of `site`: both modes' passes over `shares` frames, with a
/// screenshot of each mode (`primary-cave-NAME.png`, `reference-cave-NAME.png`) in `out`, whose
/// names are returned with the times. Takes the report's width and height for the traced image.
pub fn run(
    planet: &Planet,
    site: &Site,
    size: (u32, u32),
    shares: &Shares,
    budgets: &Budgets,
    out: &Path,
) -> Result<(Vec<CaveStill>, Vec<String>), Box<dyn Error>> {
    let resolution = Resolution {
        internal: size,
        output: (size.0 * 2, size.1 * 2),
    };
    let bands = Bands::with_d0(D0);
    let mut store = planet.crust_around(site.mouth, RING, crate::RING_LEVELS - 1);
    crate::loader::hold_guard(&mut store, planet, GUARD_FIRST);
    let (mut stills, mut names) = (Vec::new(), Vec::new());
    let mut scene: Option<Scene> = None;
    for still in &site.stills {
        let mut current = match scene.take() {
            Some(mut scene) => {
                scene.recentre(&store, still.key.position)?;
                scene
            }
            None => crate::planet_scene(
                &store,
                still.key.position,
                bands,
                planet_gen::planet::MAX_RADIUS,
            )?,
        };
        fill(&mut current, &mut store, still.key.position)?;
        let mut gpu = Offscreen::with_resolution(resolution, &current)?;
        gpu.set_keep_history(true);
        let mut flight = Flight {
            scene: current,
            store: Some(store),
            diag: Default::default(),
        };
        let path = CameraPath::new(vec![still.key, still.key]).radial();
        eprintln!("cave still: {}", still.name);
        let primary = run_mode(
            &mut gpu,
            &mut flight,
            &path,
            shares.primary,
            Mode::Primary,
            budgets,
            None,
        )?;
        let bricks = (0..render::lod::LEVELS)
            .map(|level| flight.scene.level_bricks(level))
            .collect();
        let reference = run_mode(
            &mut gpu,
            &mut flight,
            &path,
            shares.reference,
            Mode::Reference,
            budgets,
            None,
        )?;
        let camera = path.camera_at(0, 1);
        for (mode, spp) in [(Mode::Primary, 1), (Mode::Reference, SHOT_SPP)] {
            let image =
                draw(&mut gpu, mode, &camera, spp, true).expect("a screenshot is read back");
            let kind = if matches!(mode, Mode::Primary) {
                "primary"
            } else {
                "reference"
            };
            let file = format!("{kind}-cave-{}.png", still.name);
            write_png(&out.join(&file), &image)?;
            names.push(file);
        }
        stills.push(CaveStill {
            name: still.name.to_string(),
            bricks,
            primary: pass_stats(&primary, "primary"),
            upscale: pass_stats(&primary, "upscale"),
            reference: pass_stats(&reference, "reference"),
        });
        store = flight.store.take().expect("the flight has the store");
        scene = Some(flight.scene);
    }
    Ok((stills, names))
}

/// The Planet and cave that the stills of a run are of.
pub fn site() -> (Planet, Site) {
    let planet = Planet::new(WorldMap::generate(crate::SEED, MAP_SIZE));
    let site = find_site(&planet);
    (planet, site)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stills_are_in_the_air_at_a_steep_mouth_and_inside_the_cave() {
        let (planet, site) = site();
        // The mouth is on a slope, and opens to the surface: sky just above it.
        assert!(
            slope(&planet, site.mouth) > 0.2,
            "{}",
            slope(&planet, site.mouth)
        );
        let up = unit(site.mouth);
        let above: [f64; 3] = std::array::from_fn(|a| site.mouth[a] + up[a] * 30.0);
        assert!(
            planet
                .voxel(VoxelCoord(above.map(|c| c.round() as i32)))
                .is_none()
        );
        for still in &site.stills {
            let at = still.key.position.map(|c| c.round() as i32);
            assert!(
                planet.voxel(VoxelCoord(at)).is_none(),
                "{}: not in air",
                still.name
            );
        }
        // The camera at the mouth is above ground, the other under it, near the mouth.
        let ground = |p: [f64; 3]| f64::from(WORLD_RADIUS) + planet.map().elevation_at(p);
        let [mouth, tunnel] = &site.stills;
        assert!(length(mouth.key.position) > ground(mouth.key.position));
        assert!(length(tunnel.key.position) < ground(tunnel.key.position) - 4.0);
        let near = |p: [f64; 3]| length(std::array::from_fn(|a| p[a] - site.mouth[a]));
        assert!(near(mouth.key.position) < 150.0 && near(tunnel.key.position) < 150.0);
    }

    #[test]
    fn the_stills_are_timed_and_photographed() {
        let (planet, site) = site();
        let out = std::env::temp_dir().join(format!("cave-stills-{}", std::process::id()));
        std::fs::create_dir_all(&out).unwrap();
        let share = |frames, spp| ModePlan {
            frames,
            warmup: 1,
            spp,
        };
        let shares = Shares {
            primary: share(2, 1),
            reference: share(1, 1),
        };
        let budgets = Budgets::parse(super::super::budget::EMBEDDED).unwrap();
        let (stills, names) = run(&planet, &site, (80, 45), &shares, &budgets, &out).unwrap();
        assert_eq!(
            stills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["mouth", "tunnel"]
        );
        assert_eq!(names.len(), 4);
        for name in &names {
            assert!(out.join(name).exists(), "{name}");
        }
        // Bricks are drawn at level 0, where the cave is.
        assert!(
            stills.iter().all(|s| s.bricks[0] > 100),
            "{:?}",
            stills[0].bricks
        );
        let _ = std::fs::remove_dir_all(&out);
    }
}
