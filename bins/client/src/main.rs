mod bench;
mod clock;
mod dig;
mod editor;
mod fly;
mod ghost;
mod growth;
mod loader;
mod priority;
mod scene_worker;
#[cfg(test)]
mod streaming_tests;
mod tool;
mod world;

use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use brick_store::{BrickStore, Budget, VOXELS_PER_METRE};
use dig::Dig;
use editor::{Editor, Grab, Input};
use fly::{Action, Fly};
use growth::GrowthWatch;
use loader::{GUARD_FIRST, Loader};
use net::{EditMode, Leash};
use planet_gen::planet::Planet;
use planet_gen::world_map::{WORLD_MAP_SIZE, WORLD_RADIUS, WorldMap};
use render::cpu::{CpuBreakdown, Row};
use render::lod::{self, Bands};
use render::overlay::{self, Controls, Hud, HudEvent, LodInfo, SceneMemory};
use render::scene::Bound;
use render::tracer::View;
use render::{Camera, Mode, Renderer, RingsFull, Scene, SceneUpdate, Sun};
use sim::{FixedTimestep, Predictor, ServerHandle, TICK_RATE_HZ};
use winit::application::ApplicationHandler;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};
use world::{SceneWork, Stillness, World};

const SEED: u64 = 0xB4A7_A9AC;

/// Longest time step the camera takes, so that a long stall (such as uploading the Planet)
/// does not make it jump.
const MAX_FRAME_TIME: f64 = 0.1;

const TITLE: &str = "BhayanakVoxel";
/// Degrees below the horizon the client spawns looking: well short of the pitch limit, so that
/// turning left and right does not spin the view about its middle.
const SPAWN_PITCH: f64 = -25.0;
/// How far above the ground the client spawns, in Voxels: 4 m.
const SPAWN_HEIGHT: f64 = 4.0 * VOXELS_PER_METRE;
/// Voxels from the spawn point that the client generates the full crust to, along the ground (see
/// [`Planet::crust_around`]): the spawn ring, 40 m, which takes about 7.7 s on eight cores at 5 cm (it was 1.5 s at 10 cm). The
/// loader generates the rest of what the LOD bands draw (#86). The character does not walk on
/// the ring: it walks on the ground that the predictor makes around it (see [`sim::Ground`]).
const SPAWN_REACH: i32 = (40.0 * VOXELS_PER_METRE) as i32;

/// How far above the ground the character is set down, in Voxels (30 cm): it drops the last few.
const DROP_HEIGHT: f64 = 0.3 * VOXELS_PER_METRE;
/// After a stall, run at most this many character ticks to catch up and drop the rest.
const MAX_TICKS_PER_FRAME: u32 = 5;

/// Radians the sun turns per arrow key press (and per repeat while it is held).
const SUN_STEP: f32 = 0.05;

/// Mip levels the client draws the Planet in: the LOD bands pick one for each brick from its
/// distance (see [`render::lod`]), 0 to 13, which reaches orbit at 50 Planet radii.
const RING_LEVELS: usize = lod::LEVELS;

/// Pixels of a touchpad's scrolling that make one line of a wheel.
const PIXELS_PER_LINE: f32 = 20.0;

/// The LOD bias the client starts with: output pixels one Voxel may cover. The overlay changes it.
const DEFAULT_BIAS: f32 = 1.0;

/// A Planet's bricks, packed for the GPU as the bricks the LOD `bands` select from `camera`. The
/// store must have its pyramid ([`RING_LEVELS`] of it); its source gives the bricks it does not
/// hold. Nothing of the Planet is further from the centre than `radius`, the Planet's maximum
/// radius (the full-size Planet's is [`planet_gen::planet::MAX_RADIUS`], the test one's
/// [`planet_gen::MAX_RADIUS`]), so no ray need be walked beyond it.
fn planet_scene(
    store: &BrickStore,
    camera: [f64; 3],
    bands: Bands,
    radius: i32,
) -> Result<Scene, RingsFull> {
    // The scene starts as the Guard and grows by an order of magnitude as the resident set fills
    // in, and packing it again for room is a stall of seconds: so it is given the room that the
    // bands are expected to fill to begin with.
    let (refs, words) = expected_words(bands);
    Scene::rings_reserving(
        store,
        camera,
        bands,
        Bound {
            centre: [0; 3],
            radius,
        },
        refs,
        words,
    )
}

/// How much more room than the most a table held in the measured fill the tables and the scene are
/// made with, for what the measurement did not see (the camera going elsewhere on the Planet, a
/// table that is full of deleted entries and cleans itself out).
///
/// It is also how far over its constant a value that the streaming bench measures on the test
/// Planet may be (the tests of `bench::stream`).
const HEADROOM: f64 = 1.25;

/// The measurements that `room_for` and `expected_words` are sized from: the full-size Planet's
/// fill with the camera still (118 s, 9.41 million bricks generated, at 5 cm Voxels) and then a
/// 150 m walk, at a bias of 1 (`d0` of 1028.2 Voxels), on the 8-core dev box on 2026-11-09, with
/// the loader generating only the children that can be drawn (#142). They were taken before that
/// (2026-11-07, 160 s, 23.9 million bricks generated, 29.3 million counted against the budget).
/// The full-size test prints them: `cargo test --release -p client -- --ignored --nocapture
/// full_size`.
///
/// Bricks that the bands draw: 3 439 848, which is 3.25 d0² (a level's bricks are the surface
/// within its band, so they go with d0²), 0.35 d0² for each of the levels 1 to 8 and a few for
/// the levels above. The Guard's 178 000 bricks that are drawn first are in that count, as they
/// are split. (It was 3 560 809 before #142: the bricks of the deep sea, which no ray reaches,
/// were drawn while a neighbour was not known to be full.)
///
/// Checked against `streaming.sizing.measured.bricks_per_d0_squared` of the bench report (`client
/// bench --streaming`): the bricks drawn at the levels below the Guard on the test Planet when its
/// resident set is complete, per `d0` squared. It must not be over this times [`HEADROOM`], 4.06.
///
/// That is not 1.25 times what the constant counted: it counted the Guard's 178 000 bricks, which
/// the measurement leaves out (they do not scale with `d0`), so the like-for-like value at full
/// size is (3 439 848 - 178 000) / 1028.2² = 3.09, and the limit is 1.32 times it. The test Planet
/// measures 3.51: 8% over the constant, 14% over the full-size Rings' 3.09, and 86% of the limit.
const BRICKS_PER_D0_SQUARED: f64 = 3.25;
/// Words of `brick_refs` and of `bricks` per brick drawn when the resident set was complete: 3.44
/// million and 117.6 million words of them. The scene's first reservation, 28 words a brick and a
/// tenth over, ran out 3.09 million bricks into the fill at 10 cm: the 10.4 s frame of #119 was
/// the scene packed again with the words that were left in pieces too small.
///
/// Checked against `streaming.sizing.measured.refs_per_brick` and `.words_per_brick` of the bench
/// report (`client bench --streaming`): the words in use per brick drawn below the Guard on the
/// test Planet when its resident set is complete, the Guard's counted apart and exactly (the
/// scene's `words_by_level`). Each must not be over its constant times [`HEADROOM`]: 1.25 and
/// 42.75.
///
/// The constants counted the Guard's bricks too, so the limits are not exactly 1.25 times what the
/// Rings alone came to. A Guard brick takes one reference and 53.8 words on the test Planet (6 627
/// bricks, 356 601 words), so the 178 000 of full size, if they weigh the same there (they are the
/// same levels, but another Planet: #162 measures at full size), take 9.6 of the 117.6 million
/// words, and the Rings alone were 33.1 words a brick: the limit of 42.75 is 1.29 times that, and
/// the references' limit is 1.25 times 1.0. The test Planet measures 1.000 and 39.78 words, 2.97
/// words (6.9% of the limit) under it, and its Rings take 20% more words a brick than the full-size
/// Planet's did.
const REFS_PER_BRICK: f64 = 1.0;
const WORDS_PER_BRICK: f64 = 34.2;

/// What the tables that hold the Planet are made roomy for at the start, for the full bands at a
/// bias of 1: the most each held in the measurement of [`BRICKS_PER_D0_SQUARED`], with
/// [`HEADROOM`], and scaled with `d0` squared like the bricks drawn. A table that grows in the
/// middle of a frame moves every entry: 45 to 190 ms for the store's and the resident set, 0.4 and
/// 0.9 s for the selection's nodes, and the fill before this grew 16 of them.
///
/// Reserving is not free: allocating a table commits only its control bytes on Linux, but hashing
/// spreads the entries over every page, so a table is in memory in full once a few percent of it
/// is used; and on Windows the allocation commits all of it at startup. The startup log gives the
/// total.
///
/// The measurement had the coarse bricks that read as air and are not proven to be in the tree
/// (see `BrickStore::may_have_ground_below`): they are split and read like any other, and make
/// the tree, the resident set and the bricks waiting 11 to 26% larger than without them.
struct Room {
    /// Bricks held at each level, from level 0.
    held: Vec<usize>,
    /// Bricks whose children are resident.
    resident: usize,
    /// Nodes of the selection, and bricks waiting, stuck, and asked of the loader at once.
    nodes: usize,
    waiting: usize,
    stuck: usize,
}

fn room_for(bands: Bands) -> Room {
    let s = (bands.d0() / 1028.2).powi(2);
    let scaled = |n: f64| (n * s) as usize + 1024;
    // The most each level held in the fill and the walk, from level 1, with `HEADROOM` (the table
    // of which are solid below is reserved the same): 1.20, 1.03, 1.06, 1.09, 1.09, 0.99, 0.91
    // and 0.84 million bricks.
    const HELD_AT_LEVELS_1_TO_8: [f64; 8] = [
        1_500_000.0,
        1_290_000.0,
        1_320_000.0,
        1_360_000.0,
        1_360_000.0,
        1_240_000.0,
        1_140_000.0,
        1_050_000.0,
    ];
    // The Guard's levels hold what they hold whatever the bands are: level 9 577 000 (all of it
    // the Guard), level 10 85 000, levels 11 to 13 14 000 and fewer.
    const GUARD_SECOND: usize = GUARD_FIRST + 1;
    // Level 0 held 6.99 million bricks, of which 5.63 million are the spawn ring, held already when
    // this is reserved and not scaled, so more is made room for than is held: `HEADROOM` over the
    // ring is 1.41 million more (1.5 million here), and 1.25 times the 1.36 million that scale is
    // 1.70 million.
    let mut held = vec![1_500_000 + scaled(1_700_000.0)];
    held.extend((1..RING_LEVELS).map(|level| match level {
        1..GUARD_FIRST => scaled(HELD_AT_LEVELS_1_TO_8[level - 1]),
        GUARD_FIRST => 720_000,
        GUARD_SECOND => 110_000,
        _ => 20_000,
    }));
    Room {
        held,
        // 3.15 million bricks had their children resident at the end of the fill with the margins
        // (#143; 2.18 million without), and 3.70 million after the walk, with the lead
        // (2.53 million without both): the store's budget counts them (see `budget_for`).
        resident: scaled(4_400_000.0),
        // The tree held 9.44 million nodes.
        nodes: scaled(11_800_000.0),
        // Waiting, and asked of the loader, were 884 000 at the most with the margins (#143: the
        // first request is 296 000 bricks, and the margin's children, a brick at the edge of
        // every band, are asked for as the fill goes outward; 219 000 without them), and stuck
        // 116 000.
        waiting: scaled(1_100_000.0),
        stuck: scaled(150_000.0),
    }
}

/// What the store may hold (`BrickStore::streamed_len`: the bricks held under the Guard, and an
/// entry for each unit whose children are resident), per d0 squared. The complete resident set
/// counted 20.90 million with the margins of ADR 0004 at a `d0` of 1028.2 Voxels (#143: 18.43
/// million bricks held, 0.68 million of them the Guard's, which are not counted, and 3.15 million
/// units resident), which is 19.77 per d0 squared. Without the margins it was 16.13 million
/// (15.26 per d0 squared, 27.7 before #142 left the air and the buried bricks out of the units:
/// they are not held, and so not counted): the 1.25 radial margin and the 20 m vertical one make
/// 30% more of the store resident, and the loader generates 45% more bricks to fill it (13.67
/// million against 9.41). The complete shell, margins included, is what eviction keeps (ADR 0004;
/// the prefetch simulator of #87 sizes its budget at the shell), so a camera that stays or walks
/// never evicts; flying, which leaves a trail of the shells it passed through, does.
///
/// #87's 227 to 259 MB per scene copy at 136 bytes a brick is the bricks that are *drawn*, which
/// the bands bound (3.44 million, 474 MiB in the scene, at 34 words a brick) whatever the store
/// holds. The host store holds about four times as many bricks as are drawn: the ancestors of
/// every drawn brick, and the bricks beside and under them that the bands do not draw but split.
/// That is what is budgeted here, since it is what grows.
///
/// Checked against `streaming.sizing.measured.streamed_per_d0_squared` of the bench report
/// (`client bench --streaming`): what the store counts on the test Planet when its resident set is
/// complete, less the spawn ring it started with (which is not scaled with `d0`), per `d0`
/// squared. It must not be over this times [`HEADROOM`], once the constant has the spawn ring's
/// and level 0's share beyond the ring taken out (below): 16.49, not 24.75.
const STREAMED_PER_D0_SQUARED: f64 = 19.8;
/// The spawn ring's share of what the store counted at full size: 5.63 million of the 20.90 million
/// (level 0's bricks round the spawn, held before the fill starts), at the `d0` of 1028.2 Voxels
/// that the constant was measured at. It does not scale with `d0`.
const SPAWN_RING_STREAMED_AT_FULL_SIZE: f64 = 5_630_000.0;
/// Level 0's bricks beyond the spawn ring at full size: 1.36 million of the 6.99 million that level
/// 0 held (6.99 less the ring's 5.63), at the same `d0`. They scale with `d0`, but on the test
/// Planet level 0 (0 to 128 Voxels, 160 with the radial margin) is the spawn ring and no more, so
/// the measurement has none.
const LEVEL_0_BEYOND_THE_RING_AT_FULL_SIZE: f64 = 1_360_000.0;
const D0_OF_THE_MEASUREMENT: f64 = 1028.2;
/// The spawn ring's share of [`STREAMED_PER_D0_SQUARED`], 5.33. The bench's measurement takes the
/// ring out (it is held at the start, and is 160 Voxels whatever `d0` is): the store at the start
/// is its level 0 and nothing else under the Guard, 226 964 bricks, which are the 5.63 million's
/// counterpart, level 0 only on both sides. So the measurement is held to what is left of the
/// constant, and this is one of the two shares that are taken out of it (see
/// [`STREAMED_LEVEL_0_BEYOND_THE_RING_PER_D0_SQUARED`]).
const STREAMED_SPAWN_RING_PER_D0_SQUARED: f64 =
    SPAWN_RING_STREAMED_AT_FULL_SIZE / (D0_OF_THE_MEASUREMENT * D0_OF_THE_MEASUREMENT);
/// Level 0's bricks beyond the ring, 1.29 per `d0` squared, the other share that is taken out. The
/// measurement is held to [`STREAMED_PER_D0_SQUARED`] less both, 19.8 − 5.33 − 1.29 = 13.19, times
/// [`HEADROOM`]: 16.49, against the 13.09 measured (99% of that part, 79% of the limit). Held to
/// the whole constant the limit would be 24.75, 1.88 times what is measured and 1.71 times what
/// the measurement can show of the constant without the ring (14.47), and not 1.25.
const STREAMED_LEVEL_0_BEYOND_THE_RING_PER_D0_SQUARED: f64 =
    LEVEL_0_BEYOND_THE_RING_AT_FULL_SIZE / (D0_OF_THE_MEASUREMENT * D0_OF_THE_MEASUREMENT);
/// How far over the budget the store may go before eviction starts (ADR 0004 says 10 to 20%).
/// A flight leaves a trail of the shells it passed through, and takes the store over the complete
/// set by 12% in 840 m, 28% in 2.5 km and 57% in 5 km, measured with nothing evicted (#142, with
/// no margins), so 10% is evicted from within a kilometre. At the cap, when everything left is
/// wanted, the farthest bands are drawn coarser instead (`COARSENING` in `scene_worker`).
const BUDGET_OVERFLOW: f64 = 0.1;

/// How much the store may hold, for the full bands at a bias of 1 scaled with `d0` squared like
/// `room_for`: the tables are reserved for the most the complete fill held with [`HEADROOM`], and
/// eviction brings the store's total back to the budget. The budget bounds the total, not each
/// level: a flight holds a different mix of levels than the fill, and a table that is near the
/// size of its hash table can double once in a long flight.
fn budget_for(bands: Bands) -> Budget {
    Budget {
        bricks: (STREAMED_PER_D0_SQUARED * bands.d0() * bands.d0()) as usize,
        overflow: BUDGET_OVERFLOW,
    }
}

/// Words of `brick_refs` and `bricks` to reserve for a scene that is going to fill to what the
/// `bands` draw: what that is expected to take, with [`HEADROOM`].
fn expected_words(bands: Bands) -> (usize, usize) {
    let bricks = BRICKS_PER_D0_SQUARED * bands.d0() * bands.d0() * HEADROOM;
    (
        (bricks * REFS_PER_BRICK) as usize,
        (bricks * WORDS_PER_BRICK) as usize,
    )
}

/// The Planet as the client and the in-process server each hold it, the client's packed copy of
/// it, and where the client starts.
struct Loaded {
    store: BrickStore,
    server_store: BrickStore,
    scene: Scene,
    /// Generates what the scene is waiting for, on threads of its own.
    loader: Loader,
    camera: Camera,
    /// A point on the ground to put the character down on.
    ground: [f64; 3],
}

/// The Planet's spawn ring as the client and the in-process server each hold it, before the
/// Guard and before it is packed for the GPU.
struct Ground {
    planet: Planet,
    store: BrickStore,
    server_store: BrickStore,
    camera: Camera,
    ground: [f64; 3],
}

/// Generates the spawn ring on gentle land of the Planet of `map`: the full crust at level 0
/// within `reach` Voxels, on all the cores, in a store with a pyramid of `levels` levels that
/// reads the Planet itself where it holds nothing ([`Planet::crust_around`]).
fn generate_ground(map: WorldMap, reach: i32, levels: usize) -> Ground {
    let planet = Planet::new(map);
    let ground = planet.spawn(reach);
    let started = Instant::now();
    let store = planet.crust_around(ground, reach, levels - 1);
    eprintln!(
        "spawn ring ({} m): {} bricks in {:.2?}",
        f64::from(reach) / VOXELS_PER_METRE,
        store.len(),
        started.elapsed()
    );
    // Generation is deterministic, so the server's Planet is what generating it again would
    // give; copying it is quicker. The server draws nothing, so it has no pyramid (an Edit there
    // would remake its coarse bricks for nothing, on the simulation thread), and it reads the
    // Planet itself beyond the ring, which the character cannot leave.
    let mut server_store = store.clone();
    server_store.build_lod(0);
    Ground {
        planet,
        store,
        server_store,
        camera: spawn_camera(ground),
        ground,
    }
}

/// Where the character is set down: [`DROP_HEIGHT`] above `ground`, along the radial up.
fn drop_point(ground: [f64; 3]) -> [f64; 3] {
    let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    std::array::from_fn(|a| ground[a] + ground[a] / length * DROP_HEIGHT)
}

/// Where to put the character down when it leaves noclip with the camera at `eye`: under the
/// camera, on the ground, if the camera is within or near the character's `leash` (the ball it
/// last walked in), else at the leash's centre. Before it has walked, the camera is near enough
/// if it is within [`SPAWN_REACH`] of the spawn's line through the Planet's centre, and else the
/// character goes back to the spawn.
fn landing(eye: [f64; 3], leash: Option<Leash>, ground: [f64; 3]) -> [f64; 3] {
    let distance = |a: [f64; 3], b: [f64; 3]| {
        (0..3)
            .map(|k| (a[k] - b[k]) * (a[k] - b[k]))
            .sum::<f64>()
            .sqrt()
    };
    let near = f64::from(SPAWN_REACH) - 2.0 * sim::Movement::default().radius;
    if let Some(leash) = leash {
        return if distance(eye, leash.centre) < leash.radius + near {
            eye
        } else {
            leash.centre
        };
    }
    let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    let axis = ground.map(|c| c / length);
    let along: f64 = (0..3).map(|a| eye[a] * axis[a]).sum();
    let square: f64 = eye.iter().map(|c| c * c).sum::<f64>() - along * along;
    if square.is_nan() || square >= near * near {
        drop_point(ground)
    } else {
        eye
    }
}

/// The residency margins: ADR 0004's. The full-size test can run without them to measure what they
/// buy (`FULLSIZE_MARGINS=0`).
fn margins() -> render::lod::Margins {
    #[cfg(test)]
    if std::env::var_os("FULLSIZE_MARGINS").is_some_and(|v| v == "0") {
        return render::lod::Margins::NONE;
    }
    render::lod::Margins::standard()
}

/// Makes the Planet of `map` into what the client starts with: the spawn ring on gentle land,
/// then the Guard, and the scene of the bands drawn from them, with a loader to make the resident set complete
/// (#86). The ring and the Guard are timed, and reported, apart.
fn load(map: WorldMap, reach: i32) -> Result<Loaded, RingsFull> {
    let started = Instant::now();
    let Ground {
        planet,
        mut store,
        server_store,
        camera,
        ground,
    } = generate_ground(map, reach, RING_LEVELS);
    let bands = Bands::standard(f64::from(DEFAULT_BIAS));
    let room = room_for(bands);
    store.reserve(&room.held);
    store.reserve_resident(room.resident);
    store.set_budget(budget_for(bands));
    let guarded = Instant::now();
    let held = loader::hold_guard(&mut store, &planet, GUARD_FIRST);
    eprintln!(
        "Guard (levels {GUARD_FIRST} to {}, the whole Planet): {held} bricks in {:.2?}",
        RING_LEVELS - 1,
        guarded.elapsed()
    );
    let selected = Instant::now();
    let mut scene = planet_scene(
        &store,
        camera.position,
        bands,
        planet_gen::planet::MAX_RADIUS,
    )?;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    scene.reserve(room.nodes, room.waiting, room.stuck);
    // The margins of ADR 0004, on from the start: the selection is only the Guard and the spawn
    // ring now, so looking at its bricks again is cheap.
    scene.set_margins(&store, margins());
    let mut loader = Loader::new(
        store.source().expect("the ring's store has a source"),
        camera.position,
        bands,
        loader::workers_for(cores),
    );
    loader.reserve(room.waiting);
    // The bricks the Guard leaves to split are asked for here, as load time, and not in the first
    // frame: there are 178 thousand.
    let asked = Instant::now();
    let first = scene.take_wanted();
    let first_count = first.len();
    loader.request(&store, first, camera.position);
    loader.sync(camera.position, bands);
    // What the tables and the scene's buffers allocated, from the tables themselves.
    let (mut tables, mut buffers) = (0, 0);
    for table in store
        .table_sizes()
        .into_iter()
        .chain(scene.table_sizes())
        .chain(loader.table_sizes())
    {
        if table.name.ends_with(" words") {
            buffers += table.bytes;
        } else {
            tables += table.bytes;
        }
    }
    let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    eprintln!(
        "asked for the children of {first_count} bricks in {:.2?}; tables made roomy for the \
         full bands: {:.0} MiB, which is in memory in full once a few percent of each is used \
         (hashing touches every page), and on Windows is committed now; and the scene's host \
         buffers {:.0} MiB, with a GPU copy of the same size",
        asked.elapsed(),
        mib(tables),
        mib(buffers)
    );
    let bricks: Vec<usize> = (0..RING_LEVELS).map(|l| scene.level_bricks(l)).collect();
    eprintln!(
        "selected and packed {} bricks in {:.1?} (per level {bricks:?}, {:.1} MiB)",
        bricks.iter().sum::<usize>(),
        selected.elapsed(),
        scene.used_bytes() as f64 / (1024.0 * 1024.0)
    );
    eprintln!("planet ready after {:.1?}", started.elapsed());
    Ok(Loaded {
        store,
        server_store,
        scene,
        loader,
        camera,
        ground,
    })
}

/// Generates the Planet and packs it for the GPU. The world map takes about 25 seconds on eight
/// cores in a release build (several times that in debug), so this runs on its own threads while
/// the window is already up.
fn load_planet() -> Loaded {
    let started = Instant::now();
    let map = WorldMap::generate(SEED, WORLD_MAP_SIZE);
    eprintln!("world map generated in {:.1?}", started.elapsed());
    load(map, SPAWN_REACH).expect("the ground around the spawn fits the rings' buffers")
}

/// The overlay's egui state. The hotbar is always shown, so it runs every frame; F1 only hides
/// the debug panel.
struct Gui {
    ctx: egui::Context,
    state: egui_winit::State,
    visible: bool,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    gui: Option<Gui>,
    /// The Planet, while it is still being generated.
    loading: Option<Receiver<Loaded>>,
    /// The Planet and the link to the server, once loaded.
    world: Option<World>,
    /// The in-process server, which runs the same simulation as the dedicated one.
    server: Option<ServerHandle>,
    /// When the Planet was handed over, until the bands are first complete.
    filling_since: Option<Instant>,
    /// The frame thread's time on the scene, frame by frame.
    scene_work: SceneWork,
    /// Logs a table that grew or a scene that was packed again (see [`growth`]).
    growth: GrowthWatch,
    /// Where the CPU time of each frame goes, for the overlay and the log of slow frames.
    cpu: CpuBreakdown,
    camera: Camera,
    fly: Fly,
    /// Walking as the character (the default), or flying the camera through everything as a
    /// debug aid (noclip, `N`).
    walking: bool,
    /// Runs the character at the server's tick rate, whatever the frame rate.
    clock: FixedTimestep,
    /// Where the character was set down, once the Planet is loaded.
    spawn: Option<[f64; 3]>,
    /// The eyes after the previous and the latest character tick, which the camera is drawn
    /// between so that it moves smoothly at any frame rate.
    eyes: Option<([f64; 3], [f64; 3])>,
    /// The tool and Material that a click applies, and the palette that picks them.
    editor: Editor,
    /// The button is held on a Dig, which is made once the hold has lasted as long as the
    /// Material's hardness says.
    digging: bool,
    dig: Dig,
    /// How long the Dig being held takes, for the bar that shows it.
    dig_seconds: f64,
    /// Whether `Alt` is held, which makes the wheel size the Brush.
    alt: bool,
    /// The sun as an azimuth and an elevation, which the arrow keys turn.
    sun: (f32, f32),
    /// The LOD bias in use: output pixels one Voxel may cover. The overlay changes it.
    bias: f32,
    /// How long the view the reference image is of (and the bias) has been still: see
    /// [`world::loading_paused_for`].
    still: Stillness<(View, f32)>,
    /// The title last set, so that it is only set when it changes.
    title: String,
    grabbed: bool,
    last_frame: Instant,
    /// Set when startup fails inside the event loop, which cannot return errors itself.
    error: Option<Box<dyn Error>>,
}

/// A camera [`SPAWN_HEIGHT`] above `ground`, a point on the Planet's ground, with the horizon
/// level to the Planet's radial up, heading along the ground the way
/// [`planet_gen::planet::tangents`] says, and pitched down to [`SPAWN_PITCH`].
fn spawn_camera(ground: [f64; 3]) -> Camera {
    let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = ground.map(|c| c / length);
    let position: [f64; 3] = std::array::from_fn(|a| ground[a] + up[a] * SPAWN_HEIGHT);
    let (heading, _) = planet_gen::planet::tangents(up);
    let ahead: [f64; 3] = std::array::from_fn(|a| position[a] + heading[a]);
    let mut camera = Camera::looking_at(position, ahead);
    camera.carry_up(position);
    camera.turn(0.0, SPAWN_PITCH.to_radians() - camera.pitch);
    camera
}

/// Whether a copy that wrote `written` bytes made the scene's buffers again, which `scene_bytes`
/// is the size of: it wrote all of them. A copy that wrote nothing did not.
fn is_rebuild(written: u64, scene_bytes: u64) -> bool {
    written > 0 && written >= scene_bytes
}

/// Copies what changed in the Planet to the GPU, and counts the time and the bytes it took in
/// `cpu`. A copy that wrote as much as the buffers hold made them again.
fn upload(
    growth: &mut GrowthWatch,
    cpu: &mut CpuBreakdown,
    renderer: &mut Renderer,
    scene: &Scene,
    update: &SceneUpdate,
) -> Result<(), Box<dyn Error>> {
    if matches!(update, SceneUpdate::Rebuilt) {
        growth.rebuilt(scene.rebuild_reason(), Instant::now());
    }
    let (before, started) = (renderer.uploaded_bytes(), Instant::now());
    let result = renderer.update_scene(scene, update);
    cpu.add(Row::UpdateScene, started.elapsed().as_secs_f64() * 1e3);
    let written = renderer.uploaded_bytes() - before;
    cpu.add(Row::SceneBytes, written as f64);
    if is_rebuild(written, renderer.scene_bytes()) {
        cpu.add(Row::Rebuilds, 1.0);
    }
    result
}

impl App {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            // The window may be gone before the Planet is ready; that is fine.
            let _ = sender.send(load_planet());
        });
        Self {
            window: None,
            renderer: None,
            gui: None,
            loading: Some(receiver),
            world: None,
            server: None,
            filling_since: None,
            scene_work: SceneWork::default(),
            growth: GrowthWatch::default(),
            cpu: CpuBreakdown::default(),
            // Until the Planet arrives there is only sky to look at, wherever from.
            camera: spawn_camera([0.0, 0.0, f64::from(WORLD_RADIUS)]),
            fly: Fly::default(),
            walking: true,
            clock: FixedTimestep::new(Duration::from_secs(1) / TICK_RATE_HZ, MAX_TICKS_PER_FRAME),
            spawn: None,
            eyes: None,
            editor: Editor::default(),
            digging: false,
            dig: Dig::default(),
            dig_seconds: 0.0,
            alt: false,
            sun: Sun::default().angles(),
            bias: DEFAULT_BIAS,
            still: Stillness::default(),
            title: String::new(),
            grabbed: false,
            last_frame: Instant::now(),
            error: None,
        }
    }

    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let window = Arc::new(event_loop.create_window(
            Window::default_attributes().with_title(format!("{TITLE} (generating the Planet...)")),
        )?);
        let size = window.inner_size();
        // The empty scene shows only sky until the Planet arrives.
        self.renderer = Some(Renderer::new(
            Arc::clone(&window),
            size.width,
            size.height,
            &Scene::default(),
        )?);
        let ctx = egui::Context::default();
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        self.gui = Some(Gui {
            ctx,
            state,
            visible: true,
        });
        self.window = Some(window);
        Ok(())
    }

    /// Hands the Planet to the renderer and starts the server once the loading thread has
    /// finished it.
    fn poll_loading(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(receiver) = &self.loading else {
            return Ok(());
        };
        match receiver.try_recv() {
            Ok(loaded) => {
                self.loading = None;
                self.camera = loaded.camera;
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_scene(&loaded.scene)?;
                }
                let (client_end, server_end) = net::loopback();
                let server_store = loaded.server_store;
                // The single-player client may land its character anywhere (noclip), so it has
                // debug rights.
                let spawn_point = drop_point(loaded.ground);
                self.server = Some(sim::spawn(
                    move || sim::Server::new(server_store, spawn_point).with_debug_client(0),
                    vec![server_end],
                ));
                let mut world = World::new(loaded.scene, Predictor::new(loaded.store), client_end)
                    .with_loader(loaded.loader);
                world.spawn(spawn_point)?;
                self.spawn = Some(loaded.ground);
                self.world = Some(world);
                self.filling_since = Some(Instant::now());
                self.clock =
                    FixedTimestep::new(Duration::from_secs(1) / TICK_RATE_HZ, MAX_TICKS_PER_FRAME);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Err("the Planet failed to generate".into()),
        }
        Ok(())
    }

    /// Takes in the server's answers to our Edits.
    fn poll_server(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let (span, fills) = (self.cpu.start(), world.full_fills());
        let update = world.poll()?;
        self.cpu
            .add(Row::FullFills, (world.full_fills() - fills) as f64);
        let resim = world.take_resim().as_secs_f64() * 1e3;
        self.cpu.add(Row::Resim, resim);
        let shown = self.show(update);
        self.cpu.finish(Row::Poll, span, resim);
        shown
    }

    /// Applies the tool where the camera looks. The Edit is drawn at once and confirmed by the
    /// server. Only the character edits: the server holds an Edit to the reach of its eyes, and the
    /// noclip camera is not where they are.
    fn edit(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        if !self.walking {
            return Ok(());
        }
        let update = world.edit(&self.camera, &self.editor.tool)?;
        self.show(update)
    }

    /// Holds a Dig for `dt` seconds, while the button is down on one: it is made, as an ordinary
    /// Edit, once the Material aimed at has been held on for as long as its hardness says.
    fn dig_step(&mut self, dt: f64) -> Result<(), Box<dyn Error>> {
        let holding = self.digging && self.walking && self.grabbed && !self.editor.palette_open;
        let aim = self
            .world
            .as_ref()
            .filter(|_| holding)
            .and_then(|world| world.dig_aim(&self.camera, &self.editor.tool));
        if let Some((_, seconds)) = aim {
            self.dig_seconds = seconds;
        }
        if self.dig.hold(dt, aim) {
            self.edit()?;
        }
        Ok(())
    }

    /// Captures or releases the mouse as the editor asks, and lets go of a Dig.
    fn apply_grab(&mut self, grab: Option<Grab>) {
        if let Some(grab) = grab {
            self.grab(grab == Grab::Capture);
        }
    }

    /// Shows what a click would change, while the character has the mouse. It is the Edit's own
    /// list of Voxels, so it is what the click does.
    fn aim(&mut self) {
        let aimed = self.walking && self.grabbed;
        match &mut self.world {
            Some(world) if aimed => world.show_ghost(&self.camera, &self.editor.tool),
            Some(world) => world.hide_ghost(),
            None => {}
        }
    }

    /// Runs the character for the time that has passed, as ticks at the server's rate, and puts
    /// the camera at its eyes: between the last two ticks, so that it is smooth. The camera's
    /// horizon is kept level to the Planet's radial up.
    fn walk(&mut self, dt: f64) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let (span, generated) = (self.cpu.start(), world.ground_generated());
        let ticks = self.clock.advance(Duration::from_secs_f64(dt));
        self.cpu.add(Row::Ticks, f64::from(ticks));
        for _ in 0..ticks {
            world.walk(self.fly.move_input(&self.camera))?;
            if let Some(eye) = world.eye() {
                let before = self.eyes.map_or(eye, |(_, latest)| latest);
                self.eyes = Some((before, eye));
            }
        }
        if let Some((before, latest)) = self.eyes {
            let step = Duration::from_secs(1) / TICK_RATE_HZ;
            let alpha = 1.0 - self.clock.until_next_tick().as_secs_f64() / step.as_secs_f64();
            self.camera.position =
                std::array::from_fn(|a| before[a] + (latest[a] - before[a]) * alpha);
            self.camera.carry_up(self.camera.position);
        }
        self.cpu.add(
            Row::GroundBricks,
            (world.ground_generated() - generated) as f64,
        );
        let fill = world.take_fill_time().as_secs_f64() * 1e3;
        self.cpu.add(Row::GroundFill, fill);
        self.cpu.finish(Row::SimTicks, span, fill);
        Ok(())
    }

    /// Switches between walking as the character and flying the camera through everything. On
    /// the way back the character is set down where the camera is.
    fn toggle_noclip(&mut self) -> Result<(), Box<dyn Error>> {
        self.walking = !self.walking;
        self.eyes = None;
        if let (true, Some(world), Some(ground)) = (self.walking, &mut self.world, &self.spawn) {
            let leash = world.leash();
            world.spawn(landing(self.camera.position, leash, *ground))?;
        }
        Ok(())
    }

    /// Moves the rings of the Planet to the camera, and copies what that changed to the GPU.
    fn recentre(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let update = world.recentre(self.camera.position)?;
        self.show(update)
    }

    /// Copies to the GPU what the scene thread made since the last frame (it takes in the bricks the
    /// loader has generated). Reports once, when the resident set is first complete for the bands:
    /// nothing waits for bricks and nothing is queued.
    fn stream(&mut self) -> Result<(), Box<dyn Error>> {
        let (Some(world), Some(renderer)) = (&mut self.world, &mut self.renderer) else {
            return Ok(());
        };
        // Every arrival is a scene update, which restarts the reference image: so arrivals are
        // held back while it accumulates a view that is still, and taken in the frame that
        // anything changes.
        let still_frames = self.still.observe((renderer.view(&self.camera), self.bias));
        world.pause_loading(world::loading_paused_for(renderer.mode(), still_frames));
        let (cpu, growth) = (&mut self.cpu, &mut self.growth);
        world.stream(self.camera.position, |scene, update| {
            upload(growth, cpu, renderer, scene, update)
        })?;
        self.growth.check(world, Instant::now());
        if let (Some(since), Some(world)) = (self.filling_since, &self.world)
            && world.waiting() == 0
            && world.loader().is_some_and(|l| l.pending == 0)
        {
            eprintln!(
                "resident set complete for the bands {:.1?} after the first frame: {} bricks generated",
                since.elapsed(),
                world.loader().map_or(0, |l| l.generated)
            );
            self.filling_since = None;
        }
        Ok(())
    }

    /// Selects the bricks again for a LOD bias of `bias`, and copies the result to the GPU.
    fn set_bias(&mut self, bias: f32) -> Result<(), Box<dyn Error>> {
        self.bias = bias;
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let update = world.set_bands(Bands::standard(f64::from(bias)))?;
        self.show(update)
    }

    /// Copies what changed in the Planet to the GPU.
    fn show(&mut self, update: Option<SceneUpdate>) -> Result<(), Box<dyn Error>> {
        if let (Some(update), Some(world), Some(renderer)) =
            (update, &self.world, &mut self.renderer)
        {
            upload(
                &mut self.growth,
                &mut self.cpu,
                renderer,
                world.scene(),
                &update,
            )?;
        }
        Ok(())
    }

    /// Shows in the window title whether the Planet is still being generated. The rest of the
    /// state (mode, sample count, timings) is in the overlay.
    fn update_title(&mut self) {
        let Some(window) = &self.window else { return };
        let title = if self.loading.is_some() {
            format!("{TITLE} (generating the Planet...)")
        } else if !self.walking {
            format!("{TITLE} (noclip)")
        } else {
            format!("{TITLE} [{}]", self.editor.tool.describe())
        };
        if title != self.title {
            window.set_title(&title);
            self.title = title;
        }
    }

    /// Turns the sun by `azimuth` and `elevation` radians, keeping it from going past the poles.
    fn turn_sun(&mut self, azimuth: f32, elevation: f32) {
        self.set_sun(self.sun.0 + azimuth, self.sun.1 + elevation);
    }

    /// Points the sun at `azimuth` and `elevation` radians, keeping it from going past the poles.
    fn set_sun(&mut self, azimuth: f32, elevation: f32) {
        self.sun = normalize_sun(azimuth, elevation);
        if let Some(renderer) = &mut self.renderer {
            renderer.set_sun(Sun::from_angles(
                self.sun.0,
                self.sun.1,
                Sun::default().angular_radius(),
            ));
        }
    }

    /// Hands `event` to the overlay and says whether the overlay used it, in which case the
    /// camera and the dig must ignore it. While the mouse is captured for looking around, the
    /// overlay gets no pointer events (the cursor is not on the screen) except button releases, so
    /// that it never believes a button is still down.
    fn overlay_event(&mut self, event: &WindowEvent) -> bool {
        let (Some(window), Some(gui)) = (&self.window, &mut self.gui) else {
            return false;
        };
        if matches!(event, WindowEvent::RedrawRequested) {
            return false;
        }
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_)
        );
        let release = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            }
        );
        if pointer && self.grabbed && !release {
            return false;
        }
        gui.state.on_window_event(window, event).consumed
    }

    /// Lays out the overlay for this frame (the hotbar always, the debug panel if it is shown) and
    /// applies what the user changed in it.
    fn run_overlay(&mut self) -> Option<egui::FullOutput> {
        let (Some(window), Some(renderer), Some(gui)) =
            (&self.window, &mut self.renderer, &mut self.gui)
        else {
            return None;
        };
        // The ghost and the hotbar are painted by the same pass, so a hidden debug panel still
        // runs it.
        let ghost = self.world.as_ref().and_then(World::ghost);
        let before = Controls {
            mode: renderer.mode(),
            debug_view: renderer.debug_view(),
            sun: self.sun,
            bias: self.bias,
        };
        let mut controls = before;
        let info = overlay::Info {
            timings: renderer.timings(),
            cpu: &self.cpu,
            present_mode: renderer.present_mode(),
            timers_supported: renderer.timers_supported(),
            samples: renderer.samples(),
            // Only said while there is something that waits.
            loading_paused: self
                .world
                .as_ref()
                .is_some_and(|w| w.loading_paused() && w.loader().is_some_and(|l| l.pending > 0)),
            scene_memory: SceneMemory {
                used: self.world.as_ref().map_or(0, World::used_bytes),
                allocated: renderer.scene_bytes(),
            },
            lod: self.world.as_ref().and_then(lod_info),
        };
        let input = gui.state.take_egui_input(window);
        let camera = &self.camera;
        let slots = tool::slot_labels();
        let status = self.editor.tool.describe();
        let (selected, material) = (self.editor.tool.slot(), self.editor.tool.material.0);
        let dig = self.dig.progress(self.dig_seconds);
        let mut palette = self.editor.palette_open.then_some(&mut self.editor.query);
        let mut events = Vec::new();
        let mut output = gui.ctx.run_pass(input, |ctx| {
            if gui.visible {
                overlay::show(ctx, &info, &mut controls);
            }
            if let Some(ghost) = ghost {
                ghost.paint(ctx, camera);
            }
            events = overlay::hud(
                ctx,
                Hud {
                    slots: &slots,
                    selected,
                    status: &status,
                    material,
                    dig,
                    palette: palette.as_deref_mut(),
                },
            );
        });
        gui.state
            .handle_platform_output(window, std::mem::take(&mut output.platform_output));
        for event in events {
            match event {
                HudEvent::Slot(slot) => self.editor.tool.select(slot),
                HudEvent::Size(steps) => self.editor.tool.resize(steps),
                HudEvent::Material(id) => self.editor.pick_material(id),
            }
        }
        if controls.mode != before.mode {
            renderer.set_mode(controls.mode);
        }
        if controls.debug_view != before.debug_view {
            renderer.set_debug_view(controls.debug_view);
        }
        if controls.sun != before.sun {
            self.set_sun(controls.sun.0, controls.sun.1);
        }
        if controls.bias != before.bias
            && let Err(err) = self.set_bias(controls.bias)
        {
            self.error = Some(err);
        }
        Some(output)
    }

    /// Captures or releases the mouse for looking around.
    fn grab(&mut self, grab: bool) {
        let Some(window) = &self.window else { return };
        if grab {
            // The cursor leaves the overlay's sight.
            if let Some(gui) = &mut self.gui {
                gui.state
                    .egui_input_mut()
                    .events
                    .push(egui::Event::PointerGone);
            }
            // Not every platform supports locking the cursor; confining it also works for look.
            let result = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            self.grabbed = result.is_ok();
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            self.grabbed = false;
            self.digging = false;
        }
        window.set_cursor_visible(!self.grabbed);
    }
}

/// What the overlay says of the LOD bands: where level 0 ends, in metres, and the bricks drawn at
/// each level.
fn lod_info(world: &World) -> Option<LodInfo> {
    let bands = world.bands()?;
    Some(LodInfo {
        // Level 0 owns [0, 2 * d0) Voxels.
        switch_m: bands.floor(1) / VOXELS_PER_METRE,
        bricks: (0..RING_LEVELS).map(|l| world.level_bricks(l)).collect(),
    })
}

/// The azimuth wrapped into (-pi, pi], which is the range of the overlay's slider, and the
/// elevation kept from going past the poles. A slider clamps what it is given, so an azimuth
/// outside its range would be snapped back, restarting the reference image.
fn normalize_sun(azimuth: f32, elevation: f32) -> (f32, f32) {
    use std::f32::consts::{FRAC_PI_2, PI, TAU};
    let azimuth = if azimuth > -PI && azimuth <= PI {
        // Left alone, so that arithmetic cannot nudge a value that was already in range.
        azimuth
    } else {
        let wrapped = (azimuth + PI).rem_euclid(TAU) - PI;
        // The shift above gives [-pi, pi); -pi is the same direction as pi.
        if wrapped <= -PI { PI } else { wrapped }
    };
    (azimuth, elevation.clamp(-FRAC_PI_2, FRAC_PI_2))
}

fn action_for(code: KeyCode) -> Option<Action> {
    Some(match code {
        KeyCode::KeyW => Action::Forward,
        KeyCode::KeyS => Action::Back,
        KeyCode::KeyA => Action::Left,
        KeyCode::KeyD => Action::Right,
        KeyCode::Space => Action::Up,
        KeyCode::ControlLeft | KeyCode::KeyC => Action::Down,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Action::Fast,
        _ => return None,
    })
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(err) = self.init(event_loop)
        {
            self.error = Some(err);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Whether the overlay took this event (a click on it, typing into a field of it).
        let used_by_overlay = self.overlay_event(&event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    if code == KeyCode::F1 {
                        if pressed
                            && !event.repeat
                            && let Some(gui) = &mut self.gui
                        {
                            gui.visible = !gui.visible;
                        }
                    } else if pressed
                        && !event.repeat
                        && (code == KeyCode::Tab
                            || (code == KeyCode::Escape && self.editor.palette_open))
                    {
                        // The palette's own keys, which its search box would otherwise take.
                        let grab = self.editor.input(Input::Key(code), self.grabbed);
                        self.apply_grab(grab);
                    } else if let Some(action) = action_for(code) {
                        // A key the overlay is typing with does not move the camera, but its
                        // release always counts, or the camera would keep going.
                        if !(pressed && used_by_overlay) {
                            self.fly.set(action, pressed);
                        }
                    } else if pressed && !used_by_overlay && !self.editor.palette_open {
                        match code {
                            KeyCode::Escape if !event.repeat => self.grab(false),
                            // Switches between walking and the debug noclip camera.
                            KeyCode::KeyN if !event.repeat => {
                                if let Err(err) = self.toggle_noclip() {
                                    self.error = Some(err);
                                    event_loop.exit();
                                }
                            }
                            // Cycles through the debug views.
                            KeyCode::KeyV if !event.repeat => {
                                if let Some(renderer) = &mut self.renderer {
                                    renderer.set_debug_view(renderer.debug_view().next());
                                }
                            }
                            // Switches between primary visibility and the reference path tracer.
                            KeyCode::KeyR if !event.repeat => {
                                if let Some(renderer) = &mut self.renderer {
                                    renderer.set_mode(match renderer.mode() {
                                        Mode::Primary => Mode::Reference,
                                        Mode::Reference => Mode::Primary,
                                    });
                                }
                            }
                            KeyCode::ArrowLeft => self.turn_sun(-SUN_STEP, 0.0),
                            KeyCode::ArrowRight => self.turn_sun(SUN_STEP, 0.0),
                            KeyCode::ArrowUp => self.turn_sun(0.0, SUN_STEP),
                            KeyCode::ArrowDown => self.turn_sun(0.0, -SUN_STEP),
                            // The editor's keys; the ones that size and pick may repeat.
                            code if !event.repeat || editor::repeats(code) => {
                                self.editor.input(Input::Key(code), self.grabbed);
                            }
                            _ => {}
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A click on the overlay is not for the camera. The click that captures the mouse
                // does not dig, and with the palette open there is nothing to capture it for. A
                // Dig is made when its hold has lasted, and the other tools at once.
                if !used_by_overlay && !self.editor.palette_open {
                    if !self.grabbed {
                        self.grab(true);
                    } else if self.editor.tool.mode == EditMode::Dig {
                        self.digging = true;
                    } else if let Err(err) = self.edit() {
                        self.error = Some(err);
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.digging = false;
                self.dig.release();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.alt = modifiers.state().alt_key(),
            WindowEvent::MouseWheel { delta, .. } if !used_by_overlay => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / PIXELS_PER_LINE,
                };
                let wheel = Input::Wheel {
                    lines,
                    alt: self.alt,
                };
                let grab = self.editor.input(wheel, self.grabbed);
                self.apply_grab(grab);
            }
            WindowEvent::Focused(false) => {
                self.grab(false);
                // Key releases that happen while unfocused never arrive.
                self.fly = Fly::default();
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = now
                    .duration_since(self.last_frame)
                    .as_secs_f64()
                    .min(MAX_FRAME_TIME);
                self.last_frame = now;
                if self.walking {
                    if let Err(err) = self.walk(dt) {
                        self.error = Some(err);
                        event_loop.exit();
                    }
                } else {
                    self.fly.update(&mut self.camera, dt);
                }
                let span = self.cpu.start();
                if let Err(err) = self.dig_step(dt) {
                    self.error = Some(err);
                    event_loop.exit();
                }
                self.cpu.finish(Row::Dig, span, 0.0);
                let span = self.cpu.start();
                self.aim();
                self.cpu.finish(Row::Aim, span, 0.0);
                // Before the scene is brought up to date, so that what the overlay changes (the
                // sun, the bias, the mode) is seen by the frame that follows it, not the next.
                // What it says of loading is as of the frame before.
                let span = self.cpu.start();
                let overlay = self.run_overlay();
                self.cpu.finish(Row::Egui, span, 0.0);
                let scene_started = Instant::now();
                let span = self.cpu.start();
                let recentred = self.recentre();
                self.cpu.finish(Row::Recentre, span, 0.0);
                let span = self.cpu.start();
                if let Err(err) = recentred.and_then(|()| self.stream()) {
                    self.error = Some(err);
                    event_loop.exit();
                }
                self.cpu.finish(Row::Stream, span, 0.0);
                if self.world.is_some() {
                    self.scene_work.record(scene_started.elapsed());
                }
                let ctx = self.gui.as_ref().map(|gui| gui.ctx.clone());
                if let Some(renderer) = &mut self.renderer {
                    renderer.render(&self.camera, overlay.zip(ctx.as_ref()).map(|(o, c)| (c, o)));
                    let drawn = renderer.last_cpu();
                    self.cpu.add(Row::Acquire, drawn.acquire_ms);
                    self.cpu.add(Row::Encode, drawn.encode_ms);
                    self.cpu.add(Row::Present, drawn.present_ms);
                }
                self.update_title();
                if let Some(line) = self.cpu.end_frame(Instant::now()) {
                    eprintln!("{line}");
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
            && self.grabbed
        {
            self.fly.look(&mut self.camera, dx, dy);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Err(err) = self.poll_loading().and_then(|()| self.poll_server()) {
            self.error = Some(err);
            event_loop.exit();
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Dropping the handle tells the server thread to stop; it is not waited for, since it
        // may still be starting up. Its end of the loopback goes when the thread does.
        if let Some(summary) = self.scene_work.summary() {
            eprintln!("{summary}");
        }
        self.world = None;
        self.server = None;
        // Drop the renderer (and its surface) before the window it draws to.
        self.gui = None;
        self.renderer = None;
        self.window = None;
    }
}

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("bench") {
        return Ok(bench::main(args));
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(ExitCode::SUCCESS), Err)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};

    use brick_store::VoxelCoord;

    use super::*;

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        (0..3).map(|i| a[i] * b[i]).sum()
    }

    #[test]
    fn the_client_spawns_above_the_ground_pitched_down_with_a_level_horizon() {
        for ground in [
            [0.0, 0.0, 160_300.0],
            [-93_000.0, 90_000.0, 94_000.0],
            [160_400.0, 0.0, 0.0],
        ] {
            let camera = spawn_camera(ground);
            let length = dot(ground, ground).sqrt();
            let radial = ground.map(|c| c / length);
            // Four metres up.
            let height = dot(camera.position, radial) - length;
            assert!((height - SPAWN_HEIGHT).abs() < 1e-6, "{height}");
            // Level: no roll relative to the Planet's horizon.
            assert!(dot(camera.right(), radial).abs() < 1e-9);
            // Pitched down, short of the 89 degree clamp.
            let pitch = dot(camera.forward(), radial).asin().to_degrees();
            assert!((pitch - SPAWN_PITCH).abs() < 1e-6, "{pitch}");
        }
    }

    #[test]
    fn the_character_is_set_down_just_above_the_ground_and_lands_under_the_camera_near_the_spawn() {
        let ground = [-93_000.0, 90_000.0, 94_000.0];
        let length = dot(ground, ground).sqrt();
        let up = ground.map(|c| c / length);
        let drop = drop_point(ground);
        assert!((dot(drop, up) - length - DROP_HEIGHT).abs() < 1e-6);
        // Out of noclip over the spawn, it lands under the camera; far from it, it goes back to
        // the spawn.
        let over = drop.map(|c| c + 1.0);
        assert_eq!(landing(over, None, ground), over);
        let far = std::array::from_fn(|a| drop[a] + up[a] * 5.0 + [1.0, 0.0, 0.0][a] * 1e4);
        assert_eq!(landing(far, None, ground), drop);
        assert_eq!(landing([f64::NAN; 3], None, ground), drop);
    }

    #[test]
    fn out_of_noclip_the_character_lands_under_the_camera_near_its_leash_and_at_its_centre_otherwise()
     {
        // A kilometre from the spawn, where it walked to.
        let ground = [-93_000.0, 90_000.0, 94_000.0];
        let leash = Leash {
            centre: [-92_000.0, 90_500.0, 94_200.0],
            radius: 48.0,
        };
        let over = leash.centre.map(|c| c + 30.0);
        assert_eq!(landing(over, Some(leash), ground), over);
        let past = leash.centre.map(|c| c + 600.0);
        assert_eq!(landing(past, Some(leash), ground), leash.centre);
        assert_eq!(landing([f64::NAN; 3], Some(leash), ground), leash.centre);
        // Not the spawn, which is far from it.
        assert_ne!(landing(past, Some(leash), ground), drop_point(ground));
    }

    #[test]
    fn the_client_generates_a_planet_it_stands_on_land_in_with_the_ground_around_it() {
        // Ring packing is the renderer's and is tested there; one level is the ground alone.
        let loaded = generate_ground(WorldMap::generate(SEED, 512), 100, 1);
        let [x, y, z] = loaded.camera.position;
        let up = loaded.camera.horizon_up;
        let feet = |down: f64| {
            VoxelCoord(
                [x - up[0] * down, y - up[1] * down, z - up[2] * down].map(|c| c.round() as i32),
            )
        };
        // Standing in air, over ground that is not water.
        assert_eq!(loaded.store.voxel_at(0, feet(0.0)), None);
        let below = loaded.store.voxel_at(0, feet(SPAWN_HEIGHT + 3.0));
        assert!(
            below.is_some_and(|m| m != planet_gen::planet::WATER),
            "{below:?}"
        );
        // The server holds the same ground.
        assert_eq!(
            loaded.server_store.content_hash(),
            loaded.store.content_hash()
        );
    }

    #[test]
    fn the_server_holds_the_ground_without_a_pyramid_so_an_edit_there_remakes_no_coarse_brick() {
        let loaded = generate_ground(WorldMap::generate(SEED, 512), 100, 3);
        assert_eq!(loaded.store.level_count(), 3);
        assert_eq!(loaded.server_store.level_count(), 1);
        assert_eq!(
            loaded.server_store.content_hash(),
            loaded.store.content_hash()
        );
    }

    #[test]
    fn the_ground_is_reachable_from_the_spawn() {
        // The full-size Planet reaches far beyond the test Planet's radius, so the sphere the
        // client clips rays to must be the full-size Planet's. With the test Planet's, every ray
        // from the spawn misses and there is only sky.
        let loaded = generate_ground(WorldMap::generate(SEED, 512), 100, 1);
        let (cell, origin) = render::camera::split(loaded.camera.position);
        let ray = render::reference::Ray {
            cell,
            origin,
            // Straight down at the ground under the spawn.
            dir: loaded.camera.horizon_up.map(|c| -c as f32),
        };
        let scene = planet_scene(
            &loaded.store,
            loaded.camera.position,
            Bands::standard(1.0),
            planet_gen::planet::MAX_RADIUS,
        )
        .expect("a patch of the Planet fits the rings");
        assert!(
            render::reference::trace(&scene, &ray).is_some(),
            "the ground under the spawn is inside the sphere rays are clipped to"
        );
        let clipped = planet_scene(
            &loaded.store,
            loaded.camera.position,
            Bands::standard(1.0),
            planet_gen::MAX_RADIUS,
        )
        .expect("a patch of the Planet fits the rings");
        assert!(
            render::reference::trace(&clipped, &ray).is_none(),
            "the test Planet's radius is far inside the spawn, so it clips the ground away"
        );
    }

    #[test]
    fn a_copy_that_wrote_the_whole_scene_made_the_buffers_again() {
        assert!(is_rebuild(4096, 4096));
        assert!(is_rebuild(8192, 4096));
        assert!(!is_rebuild(4095, 4096));
        assert!(!is_rebuild(0, 0));
    }

    #[test]
    fn an_azimuth_past_a_half_turn_wraps_round_instead_of_leaving_the_sliders_range() {
        let (azimuth, _) = normalize_sun(PI + 0.1, 0.0);
        assert!((azimuth - (-PI + 0.1)).abs() < 1e-5, "{azimuth}");
        let (azimuth, _) = normalize_sun(-PI - 0.1, 0.0);
        assert!((azimuth - (PI - 0.1)).abs() < 1e-5, "{azimuth}");
        let (azimuth, _) = normalize_sun(5.0 * PI + 0.2, 0.0);
        assert!((azimuth - (-PI + 0.2)).abs() < 1e-4, "{azimuth}");
    }

    #[test]
    fn an_azimuth_in_range_is_unchanged_and_the_range_is_open_at_minus_pi() {
        for azimuth in [0.0, 1.0, -1.0, PI] {
            assert_eq!(normalize_sun(azimuth, 0.0).0, azimuth);
        }
        assert_eq!(normalize_sun(-PI, 0.0).0, PI);
    }

    #[test]
    fn the_elevation_stops_at_the_poles() {
        assert_eq!(normalize_sun(0.0, 3.0).1, FRAC_PI_2);
        assert_eq!(normalize_sun(0.0, -3.0).1, -FRAC_PI_2);
        assert_eq!(normalize_sun(0.0, 0.3).1, 0.3);
    }
}
