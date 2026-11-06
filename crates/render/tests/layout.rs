//! Where the words of the bricks lie in the buffers of a scene of rings (#127): bricks that are
//! next to each other in space should be next to each other in memory, whatever order the camera's
//! flight happened to bring them in. The reference's rays bounce every which way, and read the
//! buffer better the more of what they pass through is close together.
//!
//! A scene that was just made packs the regions in order. A scene that followed a camera and took
//! in bricks has put each where the free space was at the time. These tests fly the camera over
//! the test Planet, loading what is wanted as they go, and look at how far apart in the buffer the
//! words of bricks that touch are, for the scene that followed and for the same bricks packed
//! again from scratch ([`Scene::rebuilt_rings`]).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use brick_store::{BrickCoord, BrickStore, children_with_solid};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::lod::{Bands, brick_distance};
use render::scene::{Bound, REGION_WORDS, SOLID_FLAG};
use render::{RingPatch, Scene, SceneUpdate};

const SEED: u64 = 0xB4A7_A9AC;
const PYRAMID: usize = 12;
const GUARD: std::ops::RangeInclusive<usize> = 10..=12;
const FLOOR: usize = 10;
const RING: i32 = 160;
const D0: f64 = 64.0;

struct World {
    planet: Planet,
    ground: [f64; 3],
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let planet = Planet::new(WorldMap::generate(SEED, 512));
        let ground = planet.spawn(100);
        World { planet, ground }
    })
}

fn spawned_store() -> BrickStore {
    let w = world();
    let mut store = w.planet.crust_around(w.ground, RING, PYRAMID);
    for (level, coord, brick, solid) in w.planet.guard(GUARD) {
        store.hold(level, coord, brick, solid);
    }
    store.stream_from(FLOOR);
    store
}

/// A scene that follows a camera, with its store, and two copies of it that are kept as a frame
/// thread keeps its copy of what a scene thread makes (see [`Scene::mirror`]): one brought up to
/// date with each change as it is made, and one with the changes merged, every few.
struct Flight {
    scene: Scene,
    store: BrickStore,
    eager: Scene,
    lazy: Scene,
    pending: Option<RingPatch>,
    notes: usize,
}

impl Flight {
    fn new(store: BrickStore, scene: Scene) -> Self {
        Self {
            eager: scene.mirror(),
            lazy: scene.mirror(),
            scene,
            store,
            pending: None,
            notes: 0,
        }
    }

    fn note(&mut self, update: Option<SceneUpdate>) {
        match update {
            None => {}
            Some(SceneUpdate::Rings(patch)) => {
                self.eager.paste(&self.scene.copy_patch(patch.clone()));
                match &mut self.pending {
                    Some(before) => before.merge(patch),
                    None => self.pending = Some(patch),
                }
                self.notes += 1;
                if self.notes.is_multiple_of(7) {
                    self.catch_up();
                }
            }
            Some(SceneUpdate::Rebuilt) => {
                (self.eager, self.lazy) = (self.scene.mirror(), self.scene.mirror());
                self.pending = None;
            }
            Some(SceneUpdate::Patched { .. }) => unreachable!("rings make rings"),
        }
    }

    /// The lazy copy takes the changes merged so far.
    fn catch_up(&mut self) {
        if let Some(patch) = self.pending.take() {
            self.lazy.paste(&self.scene.copy_patch(patch));
        }
    }

    fn recentre(&mut self, camera: [f64; 3]) {
        let update = self.scene.recentre(&self.store, camera).unwrap();
        self.note(update);
    }

    /// Loads, nearest first, what the scene waits for, until it waits for nothing.
    fn load_everything(&mut self, camera: [f64; 3]) {
        let source = self.store.source().unwrap();
        loop {
            let mut wanted = self.scene.take_wanted();
            if wanted.is_empty() {
                return;
            }
            wanted.sort_by(|a, b| {
                brick_distance(camera, a.0, a.1).total_cmp(&brick_distance(camera, b.0, b.1))
            });
            for chunk in wanted.chunks(64) {
                for &(level, coord) in chunk {
                    let store = &self.store;
                    let bricks =
                        children_with_solid(&*source, level, coord, |c| store.holds(level - 1, c));
                    self.store.hold_children_solid(level, coord, bricks);
                }
                let update = self.scene.arrive(&self.store, chunk).unwrap();
                self.note(update);
            }
        }
    }
}

/// The scene that follows a camera that walks over the ground in `steps` steps of `step` Voxels,
/// and takes in what it wants at every step.
fn flown(steps: usize, step: f64) -> Flight {
    let w = world();
    let store = spawned_store();
    let length = w.ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = w.ground.map(|c| c / length);
    let (ahead, aside) = tangents(up);
    let at = |along: f64, side: f64| -> [f64; 3] {
        std::array::from_fn(|a| w.ground[a] + up[a] * 40.0 + ahead[a] * along + aside[a] * side)
    };
    let bound = Bound {
        centre: [0; 3],
        radius: planet_gen::planet::MAX_RADIUS,
    };
    let scene = Scene::rings(&store, at(0.0, 0.0), Bands::with_d0(D0), bound).unwrap();
    let mut flight = Flight::new(store, scene);
    flight.load_everything(at(0.0, 0.0));
    for n in 1..=steps {
        // A path that bends, so that the rings move in every direction.
        let t = n as f64 * step;
        let camera = at(t, 0.35 * t * (t / 3000.0).sin());
        flight.recentre(camera);
        flight.load_everything(camera);
    }
    flight
}

/// The offset in `bricks` of every brick of the scene that has words there, by level and
/// coordinate.
fn words(scene: &Scene) -> BTreeMap<(usize, BrickCoord), usize> {
    let mut out = BTreeMap::new();
    for (level, l) in scene.levels.iter().enumerate() {
        for slot in l.table_base..=l.table_base + l.table_mask {
            let entry = &scene.table[slot as usize * 4..][..4];
            if entry[3] == 0 {
                continue;
            }
            let key = [entry[0] as i32, entry[1] as i32, entry[2] as i32];
            let record = &scene.regions[(entry[3] as usize - 1) * REGION_WORDS..][..REGION_WORDS];
            if record[16] & SOLID_FLAG != 0 {
                continue;
            }
            let mut rank = 0;
            for bit in 0..512usize {
                if record[bit / 32] >> (bit % 32) & 1 == 0 {
                    continue;
                }
                let entry = scene.brick_refs[record[16] as usize + rank];
                rank += 1;
                if entry & SOLID_FLAG == 0 {
                    let coord = BrickCoord([
                        key[0] * 8 + (bit & 7) as i32,
                        key[1] * 8 + (bit >> 3 & 7) as i32,
                        key[2] * 8 + (bit >> 6) as i32,
                    ]);
                    out.insert((level, coord), entry as usize);
                }
            }
        }
    }
    out
}

/// How far apart in the buffer bricks that touch are: for each pair of bricks of one level that
/// share a face and both have words, the distance between their words, in words. Returns the share
/// of the pairs that are within `block` words of each other, and the mean of the base 2 logarithm
/// of the distance.
fn closeness(scene: &Scene, levels: std::ops::Range<usize>, block: usize) -> (f64, f64) {
    let at = words(scene);
    let (mut pairs, mut near, mut log) = (0usize, 0usize, 0.0);
    for (&(level, coord), &offset) in &at {
        if !levels.contains(&level) {
            continue;
        }
        for axis in 0..3 {
            let mut next = coord.0;
            next[axis] += 1;
            if let Some(&other) = at.get(&(level, BrickCoord(next))) {
                let distance = offset.abs_diff(other);
                pairs += 1;
                near += usize::from(distance < block);
                log += ((distance + 1) as f64).log2();
            }
        }
    }
    (near as f64 / pairs.max(1) as f64, log / pairs.max(1) as f64)
}

/// Words that the mixed brick with `brick_refs` entry `entry` has in `bricks`.
fn brick_len(bricks: &[u32], entry: usize) -> usize {
    let header = bricks[entry + 16];
    17 + ((header >> 8) as usize).div_ceil(2) + 512 * (header & 0xff) as usize / 32
}

/// Fails unless `copy` shows what `scene` shows: the same header, regions and table, and the same
/// words for every brick that is drawn.
fn assert_same(what: &str, scene: &Scene, copy: &Scene) {
    assert_eq!(scene.levels, copy.levels, "{what}: header");
    assert_eq!(scene.bound, copy.bound, "{what}: bound");
    assert_eq!(scene.table, copy.table, "{what}: table");
    assert_eq!(scene.regions, copy.regions, "{what}: regions");
    assert_eq!(
        scene.clip_radius(),
        copy.clip_radius(),
        "{what}: clip radius"
    );
    assert_eq!(
        scene.walked_levels(),
        copy.walked_levels(),
        "{what}: levels walked"
    );
    let (a, b) = (words(scene), words(copy));
    assert_eq!(a, b, "{what}: where the bricks are");
    for (&(level, coord), &entry) in &a {
        let len = brick_len(&scene.bricks, entry);
        assert_eq!(
            scene.bricks[entry..entry + len],
            copy.bricks[entry..entry + len],
            "{what}: the words of ({level}, {coord:?})"
        );
    }
    for record in scene.regions.chunks(REGION_WORDS) {
        if record[16] & SOLID_FLAG == 0 {
            let (first, count) = (
                record[16] as usize,
                record[..16].iter().map(|w| w.count_ones()).sum::<u32>() as usize,
            );
            assert_eq!(
                scene.brick_refs[first..first + count],
                copy.brick_refs[first..first + count],
                "{what}: the entries of a region"
            );
        }
    }
    // The levels are the same to look at, from the table alone.
    let level_of = |scene: &Scene| -> Vec<Option<usize>> {
        a.keys()
            .step_by(7)
            .map(|&(level, coord)| scene.level_at(coord.0.map(|c| c << (level + 3))))
            .chain([None])
            .collect()
    };
    assert_eq!(level_of(scene), level_of(copy), "{what}: level_at");
}

#[test]
fn a_copy_that_is_written_what_the_scene_wrote_shows_what_the_scene_shows() {
    let mut flight = flown(100, 14.0);
    assert!(flight.notes > 20, "{} changes", flight.notes);
    assert_same(
        "brought up to date at every change",
        &flight.scene,
        &flight.eager,
    );
    // Merged: the words are those of the scene when the copy is made, so the changes that were
    // made since are in too.
    flight.catch_up();
    assert_same(
        "brought up to date with merged changes",
        &flight.scene,
        &flight.lazy,
    );
}

#[test]
fn after_a_flight_touching_bricks_lie_as_close_in_memory_as_when_packed_in_order() {
    let flight = flown(100, 14.0);
    let fresh = flight.scene.rebuilt_rings(&flight.store).unwrap();
    let (near, _) = closeness(&flight.scene, 0..3, 1024);
    let (far, _) = closeness(&flight.scene, 0..3, 16384);
    let (fresh_near, _) = closeness(&fresh, 0..3, 1024);
    let (fresh_far, _) = closeness(&fresh, 0..3, 16384);
    eprintln!(
        "followed: {:.1}% within 1024 words, {:.1}% within 16384; packed in order: {:.1}%, {:.1}%",
        near * 100.0,
        far * 100.0,
        fresh_near * 100.0,
        fresh_far * 100.0
    );
    // Words that were put wherever there was room (a brick at a time) put 11% of the pairs within
    // 1024 words and 44% within 16384 after such a flight, and packing in order puts 92% and 100%.
    assert!(
        near >= 0.8 * fresh_near,
        "{near} within 1024 words, {fresh_near} packed in order"
    );
    assert!(
        far >= 0.8 * fresh_far,
        "{far} within 16384 words, {fresh_far} packed in order"
    );
}

/// What a flight does to where the words lie, against the same bricks packed in order.
#[test]
#[ignore = "a measurement"]
fn layout_after_a_flight() {
    let flight = flown(240, 14.0);
    let fresh = flight.scene.rebuilt_rings(&flight.store).unwrap();
    for (what, scene) in [("followed", &flight.scene), ("packed again", &fresh)] {
        for block in [1024, 16384] {
            let (share, log) = closeness(scene, 0..3, block);
            eprintln!(
                "{what}: pairs within {block} words {:.1}%, mean log2 distance {log:.2}",
                share * 100.0
            );
        }
    }
}
