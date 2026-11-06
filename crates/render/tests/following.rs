//! The incremental selection against a fresh one (#124): along seeded random camera paths over a
//! streamed Planet, with jumps, arrivals, drops, Edits and Undos, what the selection that follows
//! the camera draws is what a selection made from scratch at the same camera and store draws,
//! except for bricks within the hysteresis of a boundary, which stay as they were. Nothing else
//! guards that a faster way of following the camera changes nothing that is drawn.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use brick_store::{Brick, BrickCoord, BrickStore, Material, VoxelCoord, children_with_solid};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::lod::{Bands, HYSTERESIS, brick_distance};
use render::scene::select::{self, Key, Selection};

const SEED: u64 = 0xB4A7_A9AC;
const PYRAMID: usize = 12;
const GUARD: std::ops::RangeInclusive<usize> = 10..=12;
const FLOOR: usize = 10;
const RING: i32 = 160;
const D0: f64 = 64.0;
/// Frames between the looks at a selection from scratch, and the frames and seeds of a run: a few
/// seconds in a debug build.
const CHECK_EVERY: usize = 8;
const STEPS: usize = 120;
const SEEDS: u64 = 2;

struct World {
    planet: Planet,
    ground: [f64; 3],
    guard: Vec<(usize, BrickCoord, Brick, bool)>,
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let planet = Planet::new(WorldMap::generate(SEED, 512));
        let ground = planet.spawn(100);
        let guard = planet.guard(GUARD);
        World {
            planet,
            ground,
            guard,
        }
    })
}

fn spawned_store() -> BrickStore {
    let w = world();
    let mut store = w.planet.crust_around(w.ground, RING, PYRAMID);
    for (level, coord, brick, solid) in &w.guard {
        store.hold(*level, *coord, brick.clone(), *solid);
    }
    store.stream_from(FLOOR);
    store
}

/// A small xorshift generator, so that a seed is a path.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The bricks an Edit changed, and what each was before, to take it back.
type Undo = Vec<(BrickCoord, Option<Brick>)>;

/// Sets the Voxels of a ball to `to` (`None` digs).
fn edit(store: &mut BrickStore, centre: [i32; 3], radius: i32, to: Option<Material>) -> Undo {
    let mut voxels = Vec::new();
    for dx in -radius..=radius {
        for dy in -radius..=radius {
            for dz in -radius..=radius {
                if dx * dx + dy * dy + dz * dz <= radius * radius {
                    voxels.push(VoxelCoord([centre[0] + dx, centre[1] + dy, centre[2] + dz]));
                }
            }
        }
    }
    voxels.sort_by_key(|v| (v.brick(), v.0));
    let mut undo: Undo = Vec::new();
    for group in voxels.chunk_by(|a, b| a.brick() == b.brick()) {
        let brick = group[0].brick();
        let before = store.brick(brick).cloned();
        if before.is_none() {
            let implied = store.brick_at(0, brick).into_owned();
            if !implied.is_air() {
                store.insert(brick, implied);
            }
        }
        for v in group {
            store.set(*v, to);
        }
        undo.push((brick, before));
    }
    store.refresh_lod(undo.iter().map(|(c, _)| *c));
    undo
}

fn revert(store: &mut BrickStore, undo: Undo) -> Vec<BrickCoord> {
    let changed: Vec<_> = undo.iter().map(|(c, _)| *c).collect();
    for (coord, before) in undo.into_iter().rev() {
        match before {
            Some(b) => store.insert(coord, b),
            None => {
                store.remove(coord);
            }
        }
    }
    store.refresh_lod(changed.iter().copied());
    changed
}

/// Fails if what `followed` draws is not what a selection made now draws, apart from the bricks
/// within the hysteresis of a boundary (the brick, or one over it), and a few of those.
fn compare(
    step: usize,
    what: &str,
    store: &BrickStore,
    followed: &Selection,
    roots: &[BrickCoord],
) -> Result<(), String> {
    let bands = followed.bands();
    let camera = followed.camera();
    let (fresh, _) = Selection::new(store, camera, bands, followed.top(), roots.to_vec());
    let a: BTreeSet<Key> = followed.bricks().collect();
    let b: BTreeSet<Key> = fresh.bricks().collect();
    let mut differ = 0;
    for &(level, coord) in a.symmetric_difference(&b) {
        differ += 1;
        let mut near = false;
        let mut c = coord;
        for l in level..=followed.top() {
            let d = brick_distance(camera, l, c);
            let floor = bands.floor(l);
            near |= floor > 0.0 && (d - floor).abs() <= HYSTERESIS * floor + 1e-6;
            c = c.parent();
        }
        if !near {
            return Err(format!(
                "step {step} after {what}: ({level}, {coord:?}) is drawn by only the {} selection",
                if a.contains(&(level, coord)) {
                    "followed"
                } else {
                    "fresh"
                }
            ));
        }
    }
    if differ * 4 > a.len().max(8) {
        return Err(format!(
            "step {step} after {what}: {differ} of {} differ",
            a.len()
        ));
    }
    Ok(())
}

/// One seeded path of `steps` frames. Returns how many bricks' children arrived, how many were
/// dropped, and how many Edits were made.
fn run(seed: u64, steps: usize) -> Result<(usize, usize, usize), String> {
    let w = world();
    let mut rng = Rng(seed | 1);
    let mut store = spawned_store();
    let bands = Bands::with_d0(D0);
    let top = select::top_level(&store);
    let roots = select::top_bricks(&store, top, Some(([0; 3], planet_gen::planet::MAX_RADIUS)));
    let length = w.ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = w.ground.map(|c| c / length);
    let (ahead, aside) = tangents(up);
    let (mut along, mut side, mut height) = (0.0f64, 0.0f64, 40.0f64);
    let at = |along: f64, side: f64, height: f64| -> [f64; 3] {
        std::array::from_fn(|a| w.ground[a] + up[a] * height + ahead[a] * along + aside[a] * side)
    };
    let (mut selection, _) =
        Selection::new(&store, at(along, side, height), bands, top, roots.clone());
    let mut pending: Vec<Key> = Vec::new();
    let mut undos: Vec<Undo> = Vec::new();
    let (mut arrived, mut dropped, mut edits) = (0, 0, 0);
    for step in 1..=steps {
        // Mostly walking, sometimes a jump, sometimes up or down, sometimes still.
        let r = rng.f();
        if r < 0.05 {
            along += (rng.f() - 0.5) * 1600.0;
            side += (rng.f() - 0.5) * 1600.0;
        } else if r < 0.12 {
            height = (height + (rng.f() - 0.3) * 400.0).clamp(20.0, 3000.0);
        } else if r >= 0.20 {
            let speed = if rng.f() < 0.5 { 1.0 } else { 14.0 };
            along += speed * (0.5 + rng.f());
            side += speed * (rng.f() - 0.5);
        }
        let camera = at(along, side, height);
        selection.update(&store, camera);

        for k in selection.take_wanted() {
            if !pending.contains(&k) {
                pending.push(k);
            }
        }
        let cancelled = selection.take_cancelled();
        pending.retain(|k| !cancelled.contains(k));
        // Deliver some, nearest first; drop the ones the camera has left, as a worker does.
        pending.sort_by(|a, b| {
            brick_distance(camera, b.0, b.1).total_cmp(&brick_distance(camera, a.0, a.1))
        });
        let source = store.source().unwrap();
        let mut resident = Vec::new();
        for _ in 0..rng.below(60) {
            let Some(key) = pending.pop() else { break };
            let (level, coord) = key;
            if !bands.split(level, brick_distance(camera, level, coord), Some(true)) {
                selection.unwant(key);
                dropped += 1;
                continue;
            }
            let bricks = children_with_solid(&*source, level, coord, |c| store.holds(level - 1, c));
            store.hold_children_solid(level, coord, bricks);
            resident.push(key);
        }
        arrived += resident.len();
        selection.arrived(&store, resident.iter().copied());
        // A selection from scratch takes a while to make, so it is looked at every few frames.
        if step % CHECK_EVERY == 0 {
            compare(step, "update and arrivals", &store, &selection, &roots)?;
        }

        // Edits now and then, near the camera or far from it, and one is taken back.
        if step % 23 == 0 {
            let reach = if rng.f() < 0.4 { 900.0 } else { 40.0 };
            let (ea, es) = (
                along + (rng.f() - 0.5) * reach,
                side + (rng.f() - 0.5) * reach,
            );
            let point = at(ea, es, 0.0).map(|c| c.round() as i32);
            let place = rng.f() < 0.4;
            let centre = if place {
                std::array::from_fn(|a| point[a] + (up[a] * 6.0) as i32)
            } else {
                point
            };
            let radius = 3 + rng.below(10) as i32;
            let to = place.then_some(planet_gen::planet::GRANITE);
            let undo = edit(&mut store, centre, radius, to);
            let changed: Vec<_> = undo.iter().map(|(c, _)| *c).collect();
            selection.refresh(&store, &changed);
            edits += 1;
            compare(step, "edit", &store, &selection, &roots)?;
            undos.push(undo);
        }
        if step % 37 == 0
            && let Some(undo) = undos.pop()
        {
            let changed = revert(&mut store, undo);
            selection.refresh(&store, &changed);
            compare(step, "undo", &store, &selection, &roots)?;
        }
    }
    Ok((arrived, dropped, edits))
}

#[test]
fn following_the_camera_draws_what_a_fresh_selection_does_along_random_paths() {
    let mut failures = Vec::new();
    for seed in 1..=SEEDS {
        match run(seed * 0x9E37_79B9, STEPS) {
            Ok((arrived, dropped, edits)) => {
                eprintln!("seed {seed}: {arrived} arrived, {dropped} dropped, {edits} edits")
            }
            Err(e) => failures.push(format!("seed {seed}: {e}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
