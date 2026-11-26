//! PROTOTYPE (#246): `client lookdev --landform --out DIR`. The look-dev shots of the landform
//! model, each **searched for on the world map** and framed from where it is found, so that they
//! follow the terrain whatever the model makes:
//!
//! 1. the whole Planet from orbit, 2. 30 km up over the biggest continent, 3. the greatest range
//! from 5 km, 4. the same at dusk, 5. a valley floor looking up at the peaks, 6. a plateau's edge,
//! 7. a river's mouth, 8. a canyon's rim.
//!
//! A feature the search cannot find (or finds only a weak one) is reported, on stderr and on the
//! sheet. All at 1280x720 and noon (the sun 60 degrees up) unless noted.

use std::error::Error;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use brick_store::{BrickStore, VOXELS_PER_METRE};
use planet_gen::planet::Planet;
use planet_gen::world_map::{WORLD_MAP_SIZE, WORLD_RADIUS, WorldMap};
use render::lod::Bands;
use render::atmosphere::Atmosphere;
use render::sun::DEFAULT_ANGULAR_RADIUS;
use render::{Camera, Image, Look, Offscreen, Resolution, Stages, Sun};

use super::run::{Options, settle_frames, still_camera, sun_at};
use super::{Key, Shot};
use crate::bench::write_png;

/// The sun's elevation at noon and at dusk, in degrees.
const NOON: f64 = 60.0;
const DUSK: f64 = 1.0;

/// Voxels round a shot's place that the level 0 ring holds: 30 m.
const RING: i32 = (30.0 * VOXELS_PER_METRE) as i32;

type V3 = [f64; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    mul(a, 1.0 / dot(a, a).sqrt())
}

/// Metres to Voxels.
fn vox(m: f64) -> f64 {
    m * VOXELS_PER_METRE
}

/// The planet seen through the world map, in metres.
struct Ground<'a> {
    map: &'a WorldMap,
}

impl Ground<'_> {
    /// Height above sea level, metres, at a unit direction.
    fn height(&self, d: V3) -> f64 {
        self.map.elevation_at(d) / VOXELS_PER_METRE
    }

    /// A point `alt` metres above the ground of direction `d`, in Voxels from the centre.
    fn above_ground(&self, d: V3, alt: f64) -> V3 {
        mul(
            norm(d),
            f64::from(WORLD_RADIUS) + vox(self.height(norm(d)) + alt),
        )
    }

    /// A point `alt` metres above sea level.
    fn above_sea(&self, d: V3, alt: f64) -> V3 {
        mul(norm(d), f64::from(WORLD_RADIUS) + vox(alt))
    }
}

/// East and north at a unit direction (the pole axis is y).
fn frame(d: V3) -> (V3, V3) {
    let reference = if d[1].abs() > 0.99 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let east = norm(cross(reference, d));
    (east, cross(d, east))
}

/// The direction `km` kilometres from `d` along `bearing` (radians clockwise from north).
fn walk(d: V3, bearing: f64, km: f64) -> V3 {
    let (east, north) = frame(d);
    let t = add(mul(north, bearing.cos()), mul(east, bearing.sin()));
    let a = km * 1000.0 * VOXELS_PER_METRE / f64::from(WORLD_RADIUS);
    norm(add(mul(d, a.cos()), mul(t, a.sin())))
}

fn lat_lon(d: V3) -> (f64, f64) {
    (d[1].asin().to_degrees(), d[2].atan2(d[0]).to_degrees())
}

/// A shot's place, found by the search.
struct Found {
    name: &'static str,
    subject: String,
    /// Where the camera is and where it looks, in Voxels from the Planet's centre.
    position: V3,
    target: V3,
    /// The place the level 0 ring is built round, on the ground under the camera.
    site: V3,
    sun: f64,
    /// The sun's direction where the camera looks straight down (the orbit), where an elevation
    /// from the horizon means nothing.
    sun_dir: Option<V3>,
    /// How much of the game's haze the look keeps: the game's 1/300 per metre (and the hero tint's
    /// own, of the same order, over the distance in air) makes everything past a kilometre fog,
    /// which is right for a forest floor and wrong for a range 40 km away.
    haze: f32,
    /// What the search found, for the sheet.
    note: String,
}

/// Nodes of a Fibonacci lattice on the sphere, 2 km apart, with the height of the ground at each.
struct Survey {
    dirs: Vec<V3>,
    height: Vec<f32>,
    /// Cells of 3 km, for the nodes near a point.
    cells: std::collections::HashMap<(i32, i32, i32), Vec<u32>>,
}

const CELL_KM: f64 = 3.0;

impl Survey {
    fn new(ground: &Ground) -> Survey {
        let n = 300_000usize;
        let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
        let dirs: Vec<V3> = (0..n)
            .map(|k| {
                let y = 1.0 - 2.0 * (k as f64 + 0.5) / n as f64;
                let r = (1.0 - y * y).sqrt();
                let a = golden * k as f64;
                [r * a.cos(), y, r * a.sin()]
            })
            .collect();
        let threads = std::thread::available_parallelism().map_or(1, |t| t.get());
        let mut height = vec![0f32; n];
        let part = n.div_ceil(threads);
        std::thread::scope(|scope| {
            for (c, chunk) in height.chunks_mut(part).enumerate() {
                let dirs = &dirs;
                scope.spawn(move || {
                    for (k, h) in chunk.iter_mut().enumerate() {
                        *h = ground.height(dirs[c * part + k]) as f32;
                    }
                });
            }
        });
        let mut cells: std::collections::HashMap<(i32, i32, i32), Vec<u32>> = Default::default();
        for (k, d) in dirs.iter().enumerate() {
            cells.entry(cell_of(*d)).or_default().push(k as u32);
        }
        Survey {
            dirs,
            height,
            cells,
        }
    }

    /// The nodes within `km` of `d`.
    fn near(&self, d: V3, km: f64) -> Vec<usize> {
        let reach = (km / CELL_KM).ceil() as i32 + 1;
        let (cx, cy, cz) = cell_of(d);
        let limit = km * 1000.0 * VOXELS_PER_METRE / f64::from(WORLD_RADIUS);
        let min_dot = limit.min(3.0).cos();
        let mut out = Vec::new();
        for x in cx - reach..=cx + reach {
            for y in cy - reach..=cy + reach {
                for z in cz - reach..=cz + reach {
                    if let Some(list) = self.cells.get(&(x, y, z)) {
                        for &k in list {
                            if dot(self.dirs[k as usize], d) >= min_dot {
                                out.push(k as usize);
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

fn cell_of(d: V3) -> (i32, i32, i32) {
    let s = f64::from(WORLD_RADIUS) / VOXELS_PER_METRE / 1000.0 / CELL_KM;
    ((d[0] * s).floor() as i32, (d[1] * s).floor() as i32, (d[2] * s).floor() as i32)
}

/// The biggest land mass: its nodes' centre, and its share of the nodes.
fn biggest_continent(survey: &Survey) -> Option<(V3, usize, V3)> {
    let n = survey.dirs.len();
    let mut comp = vec![u32::MAX; n];
    let mut best: Option<(usize, u32)> = None;
    let mut count = 0u32;
    for s in 0..n {
        if survey.height[s] <= 0.0 || comp[s] != u32::MAX {
            continue;
        }
        let id = count;
        count += 1;
        comp[s] = id;
        let mut stack = vec![s];
        let mut size = 0;
        while let Some(k) = stack.pop() {
            size += 1;
            for m in survey.near(survey.dirs[k], 3.2) {
                if survey.height[m] > 0.0 && comp[m] == u32::MAX {
                    comp[m] = id;
                    stack.push(m);
                }
            }
        }
        if best.is_none_or(|b| size > b.0) {
            best = Some((size, id));
        }
    }
    let (size, id) = best?;
    let mut sum = [0.0; 3];
    for k in 0..n {
        if comp[k] == id {
            sum = add(sum, survey.dirs[k]);
        }
    }
    let centre = norm(sum);
    // The member nearest the centre, so that the camera is over land.
    let nearest = (0..n)
        .filter(|&k| comp[k] == id)
        .max_by(|&a, &b| dot(survey.dirs[a], centre).total_cmp(&dot(survey.dirs[b], centre)))?;
    Some((survey.dirs[nearest], size, centre))
}

/// Every shot the search finds, and a line for each feature it does not.
fn find(ground: &Ground) -> (Vec<Found>, Vec<String>) {
    let t = Instant::now();
    let survey = Survey::new(ground);
    eprintln!(
        "surveyed {} nodes in {:.1} s",
        survey.dirs.len(),
        t.elapsed().as_secs_f64()
    );
    let mut found = Vec::new();
    let mut missing = Vec::new();
    let sea = f64::from(WORLD_RADIUS);

    // 1. The whole Planet: a camera 2.6 radii out, over the biggest continent.
    let continent = biggest_continent(&survey);
    let over = continent.map_or([0.0, 0.0, 1.0], |c| c.2);
    let orbit = mul(over, sea * 2.3);
    found.push(Found {
        haze: 0.0,
        name: "orbit",
        subject: "the whole Planet from 2.3 radii, the biggest continent facing the camera".into(),
        position: orbit,
        target: [0.0; 3],
        site: ground.above_ground(over, 0.0),
        sun: NOON,
        sun_dir: Some(norm(add(mul(over, 0.85), mul(frame(over).0, 0.5)))),
        note: continent.map_or("no land".into(), |c| {
            let (la, lo) = lat_lon(c.2);
            format!("facing {la:.0} N {lo:.0} E")
        }),
    });

    // 2. 30 km up over the biggest continent, looking north across it.
    match continent {
        Some((centre, nodes, _)) => {
            let target = ground.above_ground(walk(centre, 0.0, 40.0), 0.0);
            let camera_dir = walk(centre, std::f64::consts::PI, 45.0);
            let camera = ground.above_ground(camera_dir, 30_000.0);
            let (la, lo) = lat_lon(centre);
            found.push(Found {
                haze: 0.005,
                name: "30 km over the continent",
                subject: "30 km up over the biggest continent, looking north across it".into(),
                position: camera,
                target,
                site: ground.above_ground(camera_dir, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!(
                    "the biggest land mass has {:.1}% of the survey's nodes (about {:.0} km2), at {la:.0} N {lo:.0} E",
                    nodes as f64 / survey.dirs.len() as f64 * 100.0,
                    nodes as f64 * 4.3
                ),
            });
        }
        None => missing.push("30 km over the biggest continent: there is no land".into()),
    }

    // 3 and 4. The greatest range: the highest node, refined on the map's own texels.
    let top = (0..survey.dirs.len())
        .max_by(|&a, &b| survey.height[a].total_cmp(&survey.height[b]))
        .unwrap();
    let mut peak = (survey.dirs[top], f64::from(survey.height[top]));
    for i in -48..=48 {
        for j in -48..=48 {
            let d = walk(
                walk(survey.dirs[top], std::f64::consts::FRAC_PI_2, f64::from(i) * 0.25),
                0.0,
                f64::from(j) * 0.25,
            );
            let h = ground.height(d);
            if h > peak.1 {
                peak = (d, h);
            }
        }
    }
    let (peak_dir, peak_h) = peak;
    let (la, lo) = lat_lon(peak_dir);
    // The side to look from: the lowest ground 40 km out, which leaves the range in view.
    let side = (0..16)
        .map(|b| f64::from(b) * std::f64::consts::TAU / 16.0)
        .min_by(|&a, &b| {
            ground
                .height(walk(peak_dir, a, 40.0))
                .total_cmp(&ground.height(walk(peak_dir, b, 40.0)))
        })
        .unwrap();
    let camera_dir = walk(peak_dir, side, 40.0);
    let camera_alt = 5_000f64.max(ground.height(camera_dir) + 800.0);
    let camera = ground.above_sea(camera_dir, camera_alt);
    let massif = survey
        .near(peak_dir, 80.0)
        .iter()
        .filter(|&&k| survey.height[k] > 6000.0)
        .count();
    for (name, sun, subject) in [
        (
            "greatest range, noon",
            NOON,
            "the greatest range from 5 km up, 40 km out",
        ),
        (
            "greatest range, dusk",
            DUSK,
            "the same view at dusk, the sun 1 degree up",
        ),
    ] {
        found.push(Found {
            haze: 0.0075,
            name,
            subject: subject.into(),
            position: camera,
            target: ground.above_sea(peak_dir, peak_h),
            site: ground.above_ground(camera_dir, 0.0),
            sun,
            sun_dir: None,
            note: format!(
                "the highest peak is {peak_h:.0} m at {la:.1} N {lo:.1} E; {massif} survey nodes above 6 km within 80 km; the camera is {camera_alt:.0} m up"
            ),
        });
    }

    // 5. A valley floor looking up at the peaks: near the greatest range and the other high ones,
    // the low ground with the most relief round it.
    let mut high_nodes: Vec<usize> = (0..survey.dirs.len())
        .filter(|&k| survey.height[k] > 4500.0)
        .collect();
    high_nodes.sort_by(|&a, &b| survey.height[b].total_cmp(&survey.height[a]));
    let mut centres: Vec<V3> = vec![peak_dir];
    for &k in &high_nodes {
        let d = survey.dirs[k];
        if centres.len() < 4
            && centres
                .iter()
                .all(|&c| dot(c, d).clamp(-1.0, 1.0).acos() * sea / VOXELS_PER_METRE > 150_000.0)
        {
            centres.push(d);
        }
    }
    let mut valley: Option<(usize, f64, usize)> = None; // node, relief, top
    for &c in &centres {
        let around = survey.near(c, 70.0);
        for &k in &around {
            let h = f64::from(survey.height[k]);
            if !(30.0..2600.0).contains(&h) {
                continue;
            }
            let (mut top_h, mut top_k) = (f64::MIN, k);
            for m in survey.near(survey.dirs[k], 22.0) {
                if f64::from(survey.height[m]) > top_h {
                    top_h = f64::from(survey.height[m]);
                    top_k = m;
                }
            }
            let relief = top_h - h;
            if valley.is_none_or(|v| relief > v.1) {
                valley = Some((k, relief, top_k));
            }
        }
    }
    match valley {
        Some((k, relief, top_k)) => {
            let d = survey.dirs[k];
            let (la, lo) = lat_lon(d);
            found.push(Found {
                haze: 0.015,
                name: "valley floor",
                subject: "a valley floor, eye height, looking up at the peaks".into(),
                position: ground.above_ground(d, 1.7),
                target: ground.above_sea(survey.dirs[top_k], f64::from(survey.height[top_k])),
                site: ground.above_ground(d, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!(
                    "floor at {:.0} m, {la:.1} N {lo:.1} E; the top within 22 km is {relief:.0} m above it",
                    survey.height[k]
                ),
            });
            if relief < 2500.0 {
                missing.push(format!(
                    "valley floor: the best has only {relief:.0} m of relief to the top within 22 km"
                ));
            }
        }
        None => missing.push("valley floor: no low land near the high ranges".into()),
    }

    // 6. A plateau's edge: high, flat ground that ends in a drop.
    let mut plateau: Option<(V3, f64, f64, f64)> = None; // node, bearing, top, drop
    for k in 0..survey.dirs.len() {
        let h = f64::from(survey.height[k]);
        if !(700.0..4800.0).contains(&h) || k % 2 == 1 {
            continue;
        }
        let d = survey.dirs[k];
        for b in 0..8 {
            let bearing = f64::from(b) * std::f64::consts::TAU / 8.0;
            let behind = bearing + std::f64::consts::PI;
            let flat = [4.0, 8.0, 14.0, 20.0]
                .iter()
                .all(|&km| (ground.height(walk(d, behind, km)) - h).abs() < 150.0);
            if !flat {
                continue;
            }
            let drop = h - ground.height(walk(d, bearing, 6.0));
            let far = h - ground.height(walk(d, bearing, 12.0));
            // The drop ends in lowland, not in the sea.
            let foot = ground.height(walk(d, bearing, 12.0));
            if drop > 500.0 && far > 800.0 && foot > 30.0 && ground.height(walk(d, bearing, 6.0)) > 30.0
                && plateau.is_none_or(|p| far > p.3)
            {
                plateau = Some((d, bearing, h, far));
            }
        }
    }
    match plateau {
        Some((d, bearing, h, drop)) => {
            let (la, lo) = lat_lon(d);
            let foot = walk(d, bearing, 9.0);
            let camera_dir = walk(d, bearing, 12.0);
            found.push(Found {
                haze: 0.01,
                name: "plateau edge",
                subject: "the edge of a plateau seen from the lowland, 12 km out".into(),
                position: ground.above_ground(camera_dir, 300.0),
                target: ground.above_ground(walk(d, bearing, 0.5), 0.0),
                site: ground.above_ground(camera_dir, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!(
                    "top at {h:.0} m, {la:.1} N {lo:.1} E, flat for 20 km behind, {drop:.0} m lower 12 km ahead (foot near {:.0} m)",
                    ground.height(foot)
                ),
            });
        }
        None => missing.push(
            "plateau edge: no ground 700 m to 4.8 km up that is flat for 20 km and drops 800 m within 12 km".into(),
        ),
    }

    // 7. A river's mouth: the coast node with the most water draining through it.
    let mut mouth: Option<(V3, f64)> = None;
    for k in 0..survey.dirs.len() {
        let h = f64::from(survey.height[k]);
        if !(0.0..60.0).contains(&h) {
            continue;
        }
        let d = survey.dirs[k];
        let sea_near = (0..8).any(|b| {
            ground.height(walk(d, f64::from(b) * std::f64::consts::TAU / 8.0, 2.0)) < -2.0
        });
        if !sea_near {
            continue;
        }
        // The best drainage within the node's own cell of the survey, on the map's texels.
        for i in -4..=4 {
            for j in -4..=4 {
                let p = walk(walk(d, std::f64::consts::FRAC_PI_2, f64::from(i) * 0.25), 0.0, f64::from(j) * 0.25);
                let t = ground.map.sample(p);
                let at = f64::from(t.drainage);
                if t.elevation >= 0
                    && ground.height(p) < 80.0
                    && mouth.is_none_or(|m| at > m.1)
                {
                    mouth = Some((p, at));
                }
            }
        }
    }
    match mouth {
        Some((d, drain)) => {
            let (la, lo) = lat_lon(d);
            let out = (0..16)
                .map(|b| f64::from(b) * std::f64::consts::TAU / 16.0)
                .min_by(|&a, &b| {
                    ground
                        .height(walk(d, a, 3.0))
                        .total_cmp(&ground.height(walk(d, b, 3.0)))
                })
                .unwrap();
            let camera_dir = walk(d, out, 2.5);
            let up_river = walk(d, out + std::f64::consts::PI, 3.0);
            found.push(Found {
                haze: 0.01,
                name: "river mouth",
                subject: "the biggest drainage reaching the sea, from 2.5 km offshore, 250 m up (no water is drawn in rivers yet)".into(),
                position: ground.above_sea(camera_dir, 250.0),
                target: ground.above_ground(up_river, 0.0),
                site: ground.above_sea(camera_dir, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!("drainage {drain:.0} of 255 (16 per doubling of the texels draining in) at {la:.1} N {lo:.1} E"),
            });
        }
        None => missing.push("river mouth: no coast with drainage".into()),
    }

    // 8. A canyon's rim: a channel with walls on both sides.
    let mut canyon: Option<(V3, f64, f64, f64)> = None; // channel node, wall bearing, wall height, depth
    for k in 0..survey.dirs.len() {
        let h = f64::from(survey.height[k]);
        if !(300.0..3800.0).contains(&h) || k % 2 == 1 {
            continue;
        }
        let d = survey.dirs[k];
        for b in 0..4 {
            let wall = f64::from(b) * std::f64::consts::PI / 4.0;
            let (a, c) = (
                ground.height(walk(d, wall, 2.5)),
                ground.height(walk(d, wall + std::f64::consts::PI, 2.5)),
            );
            let depth = a.min(c) - h;
            if depth < 500.0 {
                continue;
            }
            // The channel runs on: the ground along it is not much above the floor.
            let along = wall + std::f64::consts::FRAC_PI_2;
            let (f, g) = (
                ground.height(walk(d, along, 3.0)),
                ground.height(walk(d, along + std::f64::consts::PI, 3.0)),
            );
            if f.max(g) - h < 250.0 && canyon.is_none_or(|p| depth > p.3) {
                canyon = Some((d, wall, a.max(c), depth));
            }
        }
    }
    match canyon {
        Some((d, wall, rim, depth)) => {
            let (la, lo) = lat_lon(d);
            let rim_dir = walk(d, wall, 2.5);
            let along = wall + std::f64::consts::FRAC_PI_2;
            found.push(Found {
                haze: 0.015,
                name: "canyon rim",
                subject: "standing on the rim of the deepest gorge found, looking down it".into(),
                position: ground.above_ground(rim_dir, 4.0),
                target: ground.above_ground(walk(d, along, 4.0), 0.0),
                site: ground.above_ground(rim_dir, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!(
                    "floor {:.0} m, wall {rim:.0} m, {depth:.0} m deep within 2.5 km, at {la:.1} N {lo:.1} E",
                    ground.height(d)
                ),
            });
            if depth < 800.0 {
                missing.push(format!(
                    "canyon rim: the deepest gorge found is only {depth:.0} m deep within 2.5 km, which is no canyon"
                ));
            }
        }
        None => missing.push(
            "canyon rim: no channel 300 m to 3.8 km up with walls 500 m above it within 2.5 km on both sides".into(),
        ),
    }
    (found, missing)
}

/// A [`Shot`] of one still camera, for the shared camera and sun code.
fn shot_of(f: &Found) -> Shot {
    let key = Key {
        position: f.position,
        target: f.target,
        roll: 0.0,
    };
    Shot {
        name: f.name,
        subject: "",
        keys: Box::leak(Box::new([key, key])),
        frames: 3,
        places: &[],
        cave: None,
    }
}

fn envf(name: &str, default: f64) -> f64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The images side by side in rows of two, each at half size, for one look at all of them.
fn contact_sheet(images: &[Image]) -> Image {
    let (w, h) = (images[0].width as usize / 2, images[0].height as usize / 2);
    let rows = images.len().div_ceil(2);
    let mut sheet = Image {
        width: (2 * w) as u32,
        height: (rows * h) as u32,
        rgba: vec![0; 2 * w * rows * h * 4],
    };
    for (n, image) in images.iter().enumerate() {
        let (ox, oy) = ((n % 2) * w, (n / 2) * h);
        let stride = image.width as usize * 4;
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: usize, dy: usize| {
                        u32::from(image.rgba[(2 * y + dy) * stride + (2 * x + dx) * 4 + c])
                    };
                    let mean = (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4;
                    sheet.rgba[((oy + y) * 2 * w + ox + x) * 4 + c] = mean as u8;
                }
            }
        }
    }
    sheet
}

/// Runs the landform shots.
pub fn run(options: &Options) -> Result<PathBuf, Box<dyn Error>> {
    let began = Instant::now();
    std::fs::create_dir_all(&options.out)?;
    eprintln!("making the 320 km world map from the landform model...");
    let t = Instant::now();
    // LOOKDEV_MAP_SIZE: a smaller map, for trying the shots out quickly.
    let size = std::env::var("LOOKDEV_MAP_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(WORLD_MAP_SIZE);
    let planet = Planet::new(WorldMap::generate(crate::SEED, size));
    let map_s = t.elapsed().as_secs_f64();
    eprintln!("world map in {map_s:.1} s");
    let (found, missing) = find(&Ground { map: planet.map() });
    let mut report = String::new();
    for f in &found {
        let line = format!("{}: {}", f.name, f.note);
        eprintln!("found {line}");
        report += &line;
        report.push('\n');
    }
    for m in &missing {
        eprintln!("MISSING {m}");
        report += &format!("MISSING {m}\n");
    }
    std::fs::write(options.out.join("features.txt"), &report)?;

    let (w, h) = options.size;
    let d0 = f64::from(h) / (2.0 * 35f64.to_radians().tan());
    let mut html = String::new();
    let mut images = Vec::new();
    let mut shown = Vec::new();
    for (n, f) in found.iter().enumerate() {
        if !options.only.is_empty() && !options.only.iter().any(|o| f.name.contains(o.as_str())) {
            continue;
        }
        let t = Instant::now();
        eprintln!("[{n}] {}: building the place", f.name);
        let shot = shot_of(f);
        let mut store: BrickStore = planet.crust_around(f.site, RING, crate::RING_LEVELS - 1);
        crate::loader::hold_guard(&mut store, &planet, crate::loader::GUARD_FIRST);
        let mut scene =
            crate::planet_scene(&store, f.site, Bands::with_d0(d0), planet.max_radius())?;
        crate::bench::caves::fill(&mut scene, &mut store, f.site)?;
        eprintln!(
            "    bricks by level: {:?}",
            (0..crate::RING_LEVELS).map(|l| scene.level_bricks(l)).collect::<Vec<_>>()
        );
        let mut gpu = Offscreen::with_materials(
            Resolution::native(w, h),
            &scene,
            super::stamps::table(),
        )?;
        gpu.set_flat_ambient(false);
        gpu.set_ground_level(f64::from(WORLD_RADIUS));
        let mut air = if options.untinted { Atmosphere::PHYSICAL } else { Atmosphere::HERO };
        for key in &mut air.tint.keys {
            key.haze_per_metre *= f.haze * envf("LOOKDEV_HAZE", 0.3) as f32;
        }
        // The fixed exposure of the orbit shot wants a dimmer sun than the metered ones.
        let light = if f.sun_dir.is_some() { envf("LOOKDEV_ORBIT_LIGHT", 0.2) } else { 1.0 };
        air.sun_light = air.sun_light.map(|c| c * light as f32);
        // The game's air is a 35 m layer of heavy Mie haze, which is opaque to a ground-level ray
        // within about 400 m: thinned for the vistas.
        if f.haze > 0.0 {
            let mie = (f.haze * envf("LOOKDEV_MIE", 0.7) as f32).min(1.0);
            air.mie_scattering = air.mie_scattering.map(|c| c * mie);
            air.mie_absorption = air.mie_absorption.map(|c| c * mie);
        }
        gpu.set_look(Look {
            haze_per_metre: Look::HERO_B.haze_per_metre * f.haze * envf("LOOKDEV_HAZE", 0.3) as f32,
            sun: Look::HERO_B.sun.map(|c| c * light as f32),
            atmosphere: Some(air),
            ..Look::HERO_B
        });
        eprintln!(
            "[{n}] {}: ready in {:.0} s; {}",
            f.name,
            t.elapsed().as_secs_f64(),
            gpu.gpu_info().name
        );
        let camera: Camera = still_camera(&shot);
        {
            let ground = Ground { map: planet.map() };
            let (a, b) = (norm(f.position), norm(f.target));
            let profile: Vec<String> = (0..=12)
                .map(|i| {
                    let t = f64::from(i) / 12.0;
                    format!("{:.0}", ground.height(norm(add(mul(a, 1.0 - t), mul(b, t)))))
                })
                .collect();
            eprintln!("    ground along the view (m): {}", profile.join(" "));
        }
        {
            let len = |v: V3| dot(v, v).sqrt();
            let up = norm(camera.position);
            eprintln!(
                "    camera {:.0} m above sea, {:.0} m above the ground; looks {:.0} degrees {} the horizon; target {:.0} km away",
                (len(camera.position) - f64::from(WORLD_RADIUS)) / VOXELS_PER_METRE,
                (len(camera.position) - planet.sea_radius()) / VOXELS_PER_METRE
                    - planet.ground_height(camera.position) / VOXELS_PER_METRE,
                dot(camera.forward(), up).asin().to_degrees().abs(),
                if dot(camera.forward(), up) < 0.0 { "below" } else { "above" },
                len([0, 1, 2].map(|a| f.target[a] - f.position[a])) / VOXELS_PER_METRE / 1000.0
            );
        }
        // From orbit most of the frame is black space, which the auto-exposure would meter as dark
        // and open up until the whole Planet is white: the orbit is shot at a fixed exposure.
        gpu.set_stages(Stages {
            exposure: f.sun_dir.is_none(),
            bloom: f.sun_dir.is_none(),
            ..Stages::default()
        });
        gpu.set_sun(match f.sun_dir {
            Some(_) if std::env::var("LOOKDEV_SUN_ANGLE").is_ok() => {
                let a: f64 = std::env::var("LOOKDEV_SUN_ANGLE").unwrap().parse().unwrap();
                let up = norm(camera.position);
                let e = frame(up).0;
                let d = add(mul(up, a.to_radians().cos()), mul(e, a.to_radians().sin()));
                Sun::new(d.map(|c| c as f32), DEFAULT_ANGULAR_RADIUS).expect("a direction")
            }
            Some(d) => Sun::new(d.map(|c| c as f32), DEFAULT_ANGULAR_RADIUS).expect("a direction"),
            None => sun_at(
                &camera,
                std::env::var("LOOKDEV_SUN").ok().and_then(|v| v.parse().ok()).unwrap_or(f.sun),
            ),
        });
        let dir = format!("{n:02}-{}", slug(f.name));
        std::fs::create_dir_all(options.out.join(&dir))?;
        gpu.reset_accumulation();
        let mut settled = gpu.render_upscaled(&camera);
        let settle = std::env::var("LOOKDEV_SETTLE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| settle_frames(options));
        for _ in 1..settle {
            settled = gpu.render_upscaled(&camera);
        }
        let primary = format!("{dir}/primary-settled.png");
        write_png(&options.out.join(&primary), &settled)?;
        let reference = gpu.render_reference(&camera, options.spp);
        let reference_file = format!("{dir}/reference.png");
        write_png(&options.out.join(&reference_file), &reference)?;
        let _ = write!(
            html,
            "<tr><td class=name><b>{}</b><br>{}<br><i>{}</i></td>\
             <td><a href=\"{primary}\"><img src=\"{primary}\"></a></td>\
             <td><a href=\"{reference_file}\"><img src=\"{reference_file}\"></a></td></tr>",
            f.name, f.subject, f.note
        );
        shown.push(f.name);
        images.push(settled);
        eprintln!("[{n}] {}: done in {:.0} s", f.name, t.elapsed().as_secs_f64());
    }
    if !images.is_empty() {
        write_png(&options.out.join("contact-sheet.png"), &contact_sheet(&images))?;
    }
    let mut page = String::from(
        "<!doctype html><meta charset=utf-8><title>landform look-dev (#246)</title>\
         <style>body{font:14px system-ui;margin:1.5em;background:#16171a;color:#ddd}\
         table{border-collapse:collapse}td{vertical-align:top;padding:4px 6px;border-top:1px solid #444}\
         td.name{max-width:260px;font-size:12px;color:#aaa}td.name b{color:#ddd;font-size:15px}\
         img{display:block;width:560px;height:auto}.m{color:#f90}</style>",
    );
    let _ = write!(
        page,
        "<h1>Landform look-dev (#246): PROTOTYPE</h1><p>World map made in {map_s:.0} s from the landform model; \
         {}x{}; the Primary image settled over {} frames and the Reference at {} samples a pixel; noon (the sun {NOON} degrees up) \
         unless it says dusk. Took {:.0} s.</p>",
        options.size.0,
        options.size.1,
        settle_frames(options),
        options.spp,
        began.elapsed().as_secs_f64()
    );
    for m in &missing {
        let _ = write!(page, "<p class=m>MISSING {m}</p>");
    }
    page.push_str("<table><tr><td>shot</td><td>Primary, settled</td><td>Reference</td></tr>");
    page.push_str(&html);
    page.push_str("</table>");
    std::fs::write(options.out.join("index.html"), page)?;
    let sheet = options.out.join("index.html");
    eprintln!("wrote {} ({:.0} s)", sheet.display(), began.elapsed().as_secs_f64());
    Ok(sheet)
}
