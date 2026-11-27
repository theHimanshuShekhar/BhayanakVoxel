//! PROTOTYPE (#206), THROWAWAY, never merged: `client lookdev --proto-rivers d8|mfd --out DIR`.
//! The shots of the rivers-and-lakes prototype, each **found on the world map** like `--landform`'s:
//!
//! 1. a river at eye height, from its bank; 2. the same river from 500 m up; 3. the river network
//! from 20 km; 4. the biggest lake; 5. the biggest river's mouth at the sea; 6. a river meeting a
//! cave (the shallowest cave tube under a river, shown whatever it looks like); and the tour's
//! valley floor and river mouth. 1280x720 at noon.

use std::error::Error;
use std::path::PathBuf;
use std::time::Instant;

use brick_store::VOXELS_PER_METRE;
use planet_gen::planet::Planet;
use planet_gen::rivers_proto::{self, RiverMode};
use planet_gen::world_map::{WORLD_MAP_SIZE, WORLD_RADIUS, WorldMap};

use super::landform::{Found, Ground, V3, add, cross, dot, find, frame, mul, norm, render_shots, walk};
use super::run::Options;

const NOON: f64 = 60.0;

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn bearing(from: V3, to: V3) -> f64 {
    let (e, n) = frame(from);
    let t = sub(to, mul(from, dot(from, to)));
    dot(t, e).atan2(dot(t, n))
}

fn km_between(a: V3, b: V3) -> f64 {
    dot(a, b).clamp(-1.0, 1.0).acos() * f64::from(WORLD_RADIUS) / VOXELS_PER_METRE / 1000.0
}

struct World<'a> {
    planet: &'a Planet,
}

impl World<'_> {
    fn point(&self, d: V3) -> V3 {
        mul(norm(d), f64::from(WORLD_RADIUS))
    }
    /// Metres above sea level of the top (ground or water) in direction `d`.
    fn top(&self, d: V3) -> f64 {
        self.planet.surface_height(self.point(d)) / VOXELS_PER_METRE
    }
    fn ground(&self, d: V3) -> f64 {
        self.planet.ground_height(self.point(d)) / VOXELS_PER_METRE
    }
    /// `alt` metres above the top at `d`, in Voxels from the centre.
    fn above_top(&self, d: V3, alt: f64) -> V3 {
        mul(norm(d), f64::from(WORLD_RADIUS) + (self.top(d) + alt) * VOXELS_PER_METRE)
    }
    fn above_ground(&self, d: V3, alt: f64) -> V3 {
        mul(norm(d), f64::from(WORLD_RADIUS) + (self.ground(d) + alt) * VOXELS_PER_METRE)
    }
    fn above_sea(&self, d: V3, alt: f64) -> V3 {
        mul(norm(d), f64::from(WORLD_RADIUS) + alt * VOXELS_PER_METRE)
    }
    fn wet(&self, d: V3) -> bool {
        self.top(d) > self.ground(d) + 0.05
    }
    /// A dry place on the bank beside `d`, going `across` the river at `bearing` + 90 degrees, at
    /// least `from_m` out and as far as it takes.
    fn bank(&self, d: V3, along: f64, from_m: f64) -> V3 {
        for side in [1.0f64, -1.0] {
            let mut m = from_m;
            while m < from_m + 400.0 {
                let p = walk(d, along + side * std::f64::consts::FRAC_PI_2, m / 1000.0);
                if !self.wet(p) {
                    return p;
                }
                m += 2.0;
            }
        }
        walk(d, along + std::f64::consts::FRAC_PI_2, from_m / 1000.0)
    }
}

struct Node {
    face: usize,
    i: u32,
    j: u32,
    flow: u8,
}

fn nodes(map: &WorldMap) -> Vec<Node> {
    let rd = map.rivers().expect("rivers");
    let n = map.size() as usize;
    let mut out = Vec::new();
    for idx in 0..6 * n * n {
        if rd.is_river(idx) && rd.recv[idx] & rivers_proto::NO_RECEIVER == 0 {
            let (face, i, j) = rivers_proto::texel_index(map, idx);
            out.push(Node { face, i, j, flow: rd.flow[idx] });
        }
    }
    out
}

fn hash(n: &Node, salt: u64) -> u64 {
    let k = (n.face as u64) << 40 | u64::from(n.i) << 20 | u64::from(n.j);
    (k ^ salt).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 20
}

fn finds(planet: &Planet, missing: &mut Vec<String>) -> Vec<Found> {
    let map = planet.map();
    let rd = map.rivers().expect("rivers");
    let w = World { planet };
    let n = map.size() as i32;
    let interior = |nd: &Node| nd.i > 100 && nd.j > 100 && (nd.i as i32) < n - 100 && (nd.j as i32) < n - 100;
    let all = nodes(map);
    let dir_of = |nd: &Node| rivers_proto::node_point(map, nd.face, nd.i, nd.j);
    let recv_of = |nd: &Node| {
        let (f, i, j) = rivers_proto::receiver_of(map, nd.face, nd.i, nd.j).unwrap();
        (f, i, j, rivers_proto::node_point(map, f, i, j))
    };
    let mut found = Vec::new();

    // The biggest river that reaches the sea, away from a face's edge.
    let mouth = all
        .iter()
        .filter(|nd| interior(nd))
        .filter(|nd| {
            let (f, i, j, _) = recv_of(nd);
            map.texel(f, i, j).is_ocean()
        })
        .max_by_key(|nd| (nd.flow, hash(nd, 1)));

    // 1 and 2. A mid-size river: about 2500 to 6000 texels drain in, inland, low, in the middle of
    // a long reach (its receiver's receiver is a river too).
    let lo = 12 * 11 + 3;
    let hi = 12 * 12 + 7;
    let mid = all
        .iter()
        .filter(|nd| interior(nd) && (lo..=hi).contains(&i32::from(nd.flow)))
        .filter(|nd| {
            let e = f64::from(map.texel(nd.face, nd.i, nd.j).elevation) / 4.0;
            (20.0..250.0).contains(&e)
        })
        .filter(|nd| {
            // Two more river texels downstream, so the view has a river to look along.
            let (f, i, j, _) = recv_of(nd);
            rivers_proto::receiver_of(map, f, i, j).is_some_and(|(f2, i2, j2)| {
                let idx = (f2 * n as usize + j2 as usize) * n as usize + i2 as usize;
                rd.is_river(idx)
            })
        })
        .max_by_key(|nd| hash(nd, 2));
    match mid {
        Some(nd) => {
            let d = dir_of(nd);
            let (rf, ri, rj, rdir) = recv_of(nd);
            let _ = (rf, ri, rj);
            let b = bearing(d, rdir);
            let hw = rivers_proto::half_width(nd.flow);
            let e = f64::from(map.texel(nd.face, nd.i, nd.j).elevation) / 4.0;
            let bank = w.bank(d, b, hw + 3.0);
            let look = walk(d, b, 0.09);
            found.push(Found {
                haze: 0.02,
                name: "river at eye height",
                subject: "a river from its bank at eye height, looking downstream".into(),
                position: w.above_ground(bank, 1.7),
                target: w.above_top(look, 0.0),
                site: w.above_ground(bank, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!("flow code {} (about {:.0} texels, half-width {hw:.1} m), the texel {e:.0} m up", nd.flow, 2f64.powf(f64::from(nd.flow) / 12.0)),
            });
            let up = walk(d, b + std::f64::consts::PI, 0.5);
            found.push(Found {
                haze: 0.015,
                name: "river from 500 m",
                subject: "the same river from 500 m up, 500 m upstream, looking down it".into(),
                position: w.above_ground(up, 500.0),
                target: w.above_top(walk(d, b, 0.3), 0.0),
                site: w.above_ground(up, 0.0),
                sun: NOON,
                sun_dir: None,
                note: "the river of the eye-height shot".into(),
            });
        }
        None => missing.push("river at eye height / 500 m: no mid-size inland river with a reach".into()),
    }

    // 3. The network from 20 km: the centroid of the bigger rivers within 40 km of the biggest mouth.
    match mouth {
        Some(m) => {
            let md = dir_of(m);
            let code = 12 * 10;
            let near: Vec<V3> = all
                .iter()
                .filter(|nd| i32::from(nd.flow) >= code && km_between(dir_of(nd), md) < 40.0)
                .map(|nd| dir_of(nd))
                .collect();
            let mut sum = [0.0; 3];
            for d in &near {
                sum = add(sum, *d);
            }
            let target = if near.is_empty() { md } else { norm(sum) };
            let towards = if km_between(target, md) > 4.0 { bearing(target, md) } else { std::f64::consts::PI };
            let camera = walk(target, towards, 16.0);
            found.push(Found {
                haze: 0.008,
                name: "river network from 20 km",
                subject: "the biggest river's network from 20 km up, from the sea side".into(),
                position: w.above_sea(camera, 20_000.0),
                target: w.above_top(target, 0.0),
                site: w.above_ground(camera, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!(
                    "{} river texels of 1000+ texels within 40 km of the mouth, flow code {} at it",
                    near.len(),
                    m.flow
                ),
            });

            // 5. The mouth.
            let (_, _, _, odir) = recv_of(m);
            let sea = bearing(md, odir);
            let camera = walk(md, sea, 0.8);
            found.push(Found {
                haze: 0.01,
                name: "river mouth at the sea",
                subject: "the biggest river's mouth, from 800 m offshore, 60 m up".into(),
                position: w.above_sea(camera, 60.0),
                target: w.above_top(walk(md, sea + std::f64::consts::PI, 0.3), 0.0),
                site: w.above_sea(camera, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!("flow code {} (about {:.0} texels, half-width {:.0} m)", m.flow, 2f64.powf(f64::from(m.flow) / 12.0), rivers_proto::half_width(m.flow)),
            });
        }
        None => missing.push("river network / river mouth: no river reaches the sea".into()),
    }

    // 4. The biggest lake.
    let total = 6 * (n as usize).pow(2);
    let mut seen = vec![false; total];
    let mut lake: Option<(usize, usize, usize)> = None; // face, deepest idx, size
    for s in 0..total {
        if seen[s] || !rd.is_lake(s) {
            continue;
        }
        let mut group = vec![s];
        seen[s] = true;
        let mut at = 0;
        while at < group.len() {
            let (f, i, j) = rivers_proto::texel_index(map, group[at]);
            at += 1;
            for (di, dj) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (ni, nj) = (i as i32 + di, j as i32 + dj);
                if ni < 0 || nj < 0 || ni >= n || nj >= n {
                    continue;
                }
                let k = (f * n as usize + nj as usize) * n as usize + ni as usize;
                if !seen[k] && rd.is_lake(k) {
                    seen[k] = true;
                    group.push(k);
                }
            }
        }
        if lake.is_none_or(|l| group.len() > l.2) {
            let depth = |k: usize| {
                let (f, i, j) = rivers_proto::texel_index(map, k);
                f64::from(rd.water[k]) - f64::from(map.texel(f, i, j).elevation)
            };
            let deepest = *group.iter().max_by(|&&a, &&b| depth(a).total_cmp(&depth(b))).unwrap();
            lake = Some((0, deepest, group.len()));
        }
    }
    match lake {
        Some((_, idx, size)) => {
            let (f, i, j) = rivers_proto::texel_index(map, idx);
            let c = map.texel_direction(f, i, j);
            let level = f64::from(rd.water[idx]) * rivers_proto::WATER_UNIT;
            let r_km = (size as f64 * 0.0512 / std::f64::consts::PI).sqrt();
            let camera = walk(c, std::f64::consts::PI, 0.9 * r_km + 0.4);
            found.push(Found {
                haze: 0.012,
                name: "lake",
                subject: "the biggest lake, from its shore 90 m up, looking across it".into(),
                position: w.above_sea(camera, level.max(w.ground(camera)) + 90.0),
                target: w.above_sea(c, level),
                site: w.above_ground(camera, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!("{size} lake texels (about {:.0} km2), the surface {level:.0} m above the sea", size as f64 * 0.0512),
            });
        }
        None => missing.push("lake: the map has no closed basin that deep".into()),
    }

    // 6. A river meeting a cave: the cave tube whose top comes nearest the bed of a river.
    let began = Instant::now();
    let mut cands: Vec<&Node> = all
        .iter()
        .filter(|nd| interior(nd) && i32::from(nd.flow) >= 12 * 8 && f64::from(map.texel(nd.face, nd.i, nd.j).elevation) > 8.0)
        .collect();
    cands.sort_by_key(|nd| hash(nd, 3));
    let mut best: Option<(f64, V3, V3, f64, f64, usize)> = None; // gap m, node dir, tube point, hw, along, tried
    let mut tried = 0;
    for nd in cands.into_iter().take(500) {
        if began.elapsed().as_secs() > 120 {
            break;
        }
        tried += 1;
        let d = dir_of(nd);
        let hw = rivers_proto::half_width(nd.flow);
        let p = w.above_ground(d, 0.0);
        let r = 250.0 * VOXELS_PER_METRE;
        let (low, high) = (p.map(|c| c - r), p.map(|c| c + r));
        for tube in planet.cave_tubes(low, high) {
            for s in 0..=12 {
                let t = f64::from(s) / 12.0;
                let q: V3 = std::array::from_fn(|k| tube.a[k] + (tube.b[k] - tube.a[k]) * t);
                let radius_tube = tube.ra + (tube.rb - tube.ra) * t;
                let qd = norm(q);
                let horizontal = dot(sub(qd, d), sub(qd, d)).sqrt() * f64::from(WORLD_RADIUS) / VOXELS_PER_METRE;
                if horizontal > hw + 25.0 {
                    continue;
                }
                let up = dot(q, q).sqrt() - f64::from(WORLD_RADIUS);
                let depth_to_top = (w.ground(qd) * VOXELS_PER_METRE - up - radius_tube) / VOXELS_PER_METRE;
                if depth_to_top < 0.0 && depth_to_top < -2.0 * hw {
                    continue;
                }
                let gap = depth_to_top.max(0.0) + horizontal * 0.02;
                if best.as_ref().is_none_or(|b| gap < b.0) {
                    let (_, _, _, rdir) = recv_of(nd);
                    best = Some((gap, d, q, hw, bearing(d, rdir), tried));
                }
            }
        }
    }
    match best {
        Some((gap, d, q, hw, along, tried)) => {
            let bank = w.bank(d, along, hw + 6.0);
            found.push(Found {
                haze: 0.02,
                name: "river meets a cave",
                subject: "the shallowest cave tube under a river, from the bank 8 m up (static water over a hole is expected)".into(),
                position: w.above_ground(bank, 8.0),
                target: q,
                site: w.above_ground(bank, 0.0),
                sun: NOON,
                sun_dir: None,
                note: format!("the tube's top is {gap:.1} m under the bed, half-width {hw:.1} m; best of {tried} rivers tried"),
            });
            if gap > 12.0 {
                missing.push(format!("river meets a cave: the nearest tube is {gap:.0} m under the bed, so the cave does not break the river"));
            }
        }
        None => missing.push(format!("river meets a cave: none of {tried} rivers has a cave tube within 25 m")),
    }
    let _ = cross;
    found
}

/// Runs the rivers shots of `mode`.
pub fn run(options: &Options, mode: RiverMode) -> Result<PathBuf, Box<dyn Error>> {
    let began = Instant::now();
    std::fs::create_dir_all(&options.out)?;
    rivers_proto::set_default_mode(mode);
    eprintln!("making the 320 km world map from the landform model, with {mode:?} rivers...");
    let t = Instant::now();
    let size = std::env::var("LOOKDEV_MAP_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(WORLD_MAP_SIZE);
    let planet = Planet::new(WorldMap::generate(crate::SEED, size)).with_rivers(mode);
    let map_s = t.elapsed().as_secs_f64();
    eprintln!("world map in {map_s:.1} s");
    let (tour, mut missing) = find(&Ground { map: planet.map() });
    let mut found = finds(&planet, &mut missing);
    found.extend(tour.into_iter().filter(|f| f.name == "valley floor" || f.name == "river mouth"));
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
    render_shots(options, &planet, &found, &missing, map_s, began, &format!("rivers and lakes look-dev (#206), method {mode:?}"))
}
