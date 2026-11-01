mod bench;
mod fly;
mod loader;
mod priority;
#[cfg(test)]
mod streaming_tests;
mod world;

use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use brick_store::BrickStore;
use fly::{Action, Fly};
use loader::{GUARD_FIRST, Loader};
use net::Leash;
use planet_gen::planet::Planet;
use planet_gen::world_map::{WORLD_MAP_SIZE, WORLD_RADIUS, WorldMap};
use render::lod::{self, Bands};
use render::overlay::{self, Controls, LodInfo, SceneMemory};
use render::scene::Bound;
use render::{Camera, Mode, Renderer, RingsFull, Scene, SceneUpdate, Sun};
use sim::{FixedTimestep, Predictor, ServerHandle, TICK_RATE_HZ};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};
use world::{SceneWork, World};

const SEED: u64 = 0xB4A7_A9AC;

/// Longest time step the camera takes, so that a long stall (such as uploading the Planet)
/// does not make it jump.
const MAX_FRAME_TIME: f64 = 0.1;

const TITLE: &str = "BhayanakVoxel";
/// Degrees below the horizon the client spawns looking: well short of the pitch limit, so that
/// turning left and right does not spin the view about its middle.
const SPAWN_PITCH: f64 = -25.0;
/// How far above the ground the client spawns, in Voxels: 4 m.
const SPAWN_HEIGHT: f64 = 40.0;
/// Voxels from the spawn point that the client generates the full crust to, along the ground (see
/// [`Planet::crust_around`]): the spawn ring, 40 m, which takes about 1.5 s on eight cores. The
/// loader generates the rest of what the LOD bands draw (#86). The character does not walk on
/// the ring: it walks on the ground that the predictor makes around it (see [`sim::Ground`]).
const SPAWN_REACH: i32 = 400;

/// How far above the ground the character is set down, in Voxels: it drops the last few.
const DROP_HEIGHT: f64 = 3.0;
/// After a stall, run at most this many character ticks to catch up and drop the rest.
const MAX_TICKS_PER_FRAME: u32 = 5;

/// Radians the sun turns per arrow key press (and per repeat while it is held).
const SUN_STEP: f32 = 0.05;

/// Mip levels the client draws the Planet in: the LOD bands pick one for each brick from its
/// distance (see [`render::lod`]), 0 to 12, which reaches orbit at 50 Planet radii.
const RING_LEVELS: usize = lod::LEVELS;

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

/// Bricks that the bands draw, at most, at a bias of 1 (`d0` of 1028 Voxels): measured on the
/// full-size Planet, 3.1 million, 0.36 d0² for each of the levels 0 to 7 and the Guard's 176 000
/// of level 8. A level's bricks are the surface within its band, so they go with d0².
const BRICKS_PER_D0_SQUARED: f64 = 3.0;
/// Words of `brick_refs` and of `bricks` per brick drawn, as measured when the resident set
/// was complete.
const REFS_PER_BRICK: f64 = 1.0;
const WORDS_PER_BRICK: f64 = 28.0;

/// What the tables that hold the Planet are made roomy for at the start, for the full bands at a
/// bias of 1, measured on the full-size Planet (the bricks the store holds are more than are
/// drawn: the ones buried are held too). Scaled with `d0` squared like the bricks drawn. A table
/// that grows in the middle of a frame moves every entry, 25 to 60 ms for these; this is
/// address space until the resident set fills it.
struct Room {
    /// Bricks held at each level, from level 0.
    held: Vec<usize>,
    /// Bricks whose children are resident.
    resident: usize,
    /// Nodes of the selection, and bricks waiting and stuck, and asked of the loader at once.
    nodes: usize,
    waiting: usize,
}

fn room_for(bands: Bands) -> Room {
    let s = (bands.d0() / 1028.2).powi(2);
    let scaled = |n: f64| (n * s) as usize + 1024;
    // The Guard's levels hold what they hold whatever the bands are.
    let mut held = vec![800_000 + scaled(1_300_000.0)];
    held.extend((1..RING_LEVELS).map(|level| match level {
        1..GUARD_FIRST => scaled(750_000.0),
        GUARD_FIRST => 600_000 + scaled(100_000.0),
        _ => 100_000,
    }));
    Room {
        held,
        resident: scaled(1_200_000.0),
        nodes: scaled(5_000_000.0) + 400_000,
        waiting: scaled(1_200_000.0) + 200_000,
    }
}

/// Bytes of address space a table of `entries` entries of `size` bytes takes: its buckets are the
/// next power of two over eight sevenths of that, with a byte of control each.
fn table_bytes(entries: usize, size: usize) -> usize {
    (entries * 8 / 7).next_power_of_two() * (size + 1)
}

/// Words of `brick_refs` and `bricks` to reserve for a scene that is going to fill to what the
/// `bands` draw: what that is expected to take, a tenth over.
fn expected_words(bands: Bands) -> (usize, usize) {
    let bricks = BRICKS_PER_D0_SQUARED * bands.d0() * bands.d0() * 1.1 + 200_000.0;
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
        reach / 10,
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
    store.reserve(&room.held, room.resident);
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
    scene.reserve(room.nodes, room.waiting);
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
    eprintln!(
        "asked for the children of {first_count} bricks in {:.2?}; tables made roomy for the \
         full bands: about {:.0} MiB of address space",
        asked.elapsed(),
        (room.held.iter().map(|&n| table_bytes(n, 32)).sum::<usize>()
            + table_bytes(room.resident, 32)
            + table_bytes(room.nodes, 41)
            + 2 * table_bytes(room.waiting, 25)
            + table_bytes(room.waiting, 40)) as f64
            / (1024.0 * 1024.0)
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

/// The debug overlay's egui state. While it is hidden, none of it runs.
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
    /// The sun as an azimuth and an elevation, which the arrow keys turn.
    sun: (f32, f32),
    /// The LOD bias in use: output pixels one Voxel may cover. The overlay changes it.
    bias: f32,
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
            // Until the Planet arrives there is only sky to look at, wherever from.
            camera: spawn_camera([0.0, 0.0, f64::from(WORLD_RADIUS)]),
            fly: Fly::default(),
            walking: true,
            clock: FixedTimestep::new(Duration::from_secs(1) / TICK_RATE_HZ, MAX_TICKS_PER_FRAME),
            spawn: None,
            eyes: None,
            sun: Sun::default().angles(),
            bias: DEFAULT_BIAS,
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
                self.server = Some(sim::spawn(move || server_store, vec![server_end]));
                let mut world = World::new(loaded.scene, Predictor::new(loaded.store), client_end)
                    .with_loader(loaded.loader);
                world.spawn(drop_point(loaded.ground))?;
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
        let update = world.poll()?;
        self.show(update)
    }

    /// Digs where the camera looks. The hole is drawn at once and confirmed by the server.
    fn dig(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let update = world.dig(&self.camera)?;
        self.show(update)
    }

    /// Runs the character for the time that has passed, as ticks at the server's rate, and puts
    /// the camera at its eyes: between the last two ticks, so that it is smooth. The camera's
    /// horizon is kept level to the Planet's radial up.
    fn walk(&mut self, dt: f64) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        for _ in 0..self.clock.advance(Duration::from_secs_f64(dt)) {
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

    /// Takes in the bricks the loader has generated, for a bounded time a frame, and copies what
    /// that changed to the GPU. Reports once, when the resident set is first complete for the
    /// bands: nothing waits for bricks and nothing is queued.
    fn stream(&mut self) -> Result<(), Box<dyn Error>> {
        let (Some(world), Some(renderer)) = (&mut self.world, &mut self.renderer) else {
            return Ok(());
        };
        world.stream(self.camera.position, |scene, update| {
            renderer.update_scene(scene, update)
        })?;
        if let (Some(since), Some(world)) = (self.filling_since, &self.world)
            && world.waiting() == 0
            && world.loader().is_some_and(|l| l.pending() == 0)
        {
            eprintln!(
                "resident set complete for the bands {:.1?} after the first frame: {} bricks generated",
                since.elapsed(),
                world.loader().map_or(0, Loader::generated)
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
            renderer.update_scene(world.scene(), &update)?;
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
            TITLE.to_string()
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
    /// camera and the dig must ignore it. A hidden overlay sees nothing. While the mouse is
    /// captured for looking around, the overlay gets no pointer events (the cursor is not on the
    /// screen) except button releases, so that it never believes a button is still down.
    fn overlay_event(&mut self, event: &WindowEvent) -> bool {
        let (Some(window), Some(gui)) = (&self.window, &mut self.gui) else {
            return false;
        };
        if !gui.visible || matches!(event, WindowEvent::RedrawRequested) {
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

    /// Lays out the overlay for this frame, if it is shown, and applies what the user changed in
    /// it. Hidden, this does nothing: no layout, no paint.
    fn run_overlay(&mut self) -> Option<egui::FullOutput> {
        let (Some(window), Some(renderer), Some(gui)) =
            (&self.window, &mut self.renderer, &mut self.gui)
        else {
            return None;
        };
        if !gui.visible {
            return None;
        }
        let before = Controls {
            mode: renderer.mode(),
            debug_view: renderer.debug_view(),
            sun: self.sun,
            bias: self.bias,
        };
        let mut controls = before;
        let info = overlay::Info {
            timings: renderer.timings(),
            timers_supported: renderer.timers_supported(),
            samples: renderer.samples(),
            scene_memory: SceneMemory {
                used: self.world.as_ref().map_or(0, |w| w.scene().used_bytes()),
                allocated: renderer.scene_bytes(),
            },
            lod: self.world.as_ref().and_then(|w| lod_info(w.scene())),
        };
        let input = gui.state.take_egui_input(window);
        let mut output = gui
            .ctx
            .run_pass(input, |ctx| overlay::show(ctx, &info, &mut controls));
        gui.state
            .handle_platform_output(window, std::mem::take(&mut output.platform_output));
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
        }
        window.set_cursor_visible(!self.grabbed);
    }
}

/// What the overlay says of the LOD bands: where level 0 ends, in metres, and the bricks drawn at
/// each level.
fn lod_info(scene: &Scene) -> Option<LodInfo> {
    let bands = scene.bands()?;
    Some(LodInfo {
        // Level 0 owns [0, 2 * d0) Voxels of 10 cm.
        switch_m: bands.floor(1) * 0.1,
        bricks: (0..RING_LEVELS).map(|l| scene.level_bricks(l)).collect(),
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
                    } else if let Some(action) = action_for(code) {
                        // A key the overlay is typing with does not move the camera, but its
                        // release always counts, or the camera would keep going.
                        if !(pressed && used_by_overlay) {
                            self.fly.set(action, pressed);
                        }
                    } else if pressed && !used_by_overlay {
                        match code {
                            KeyCode::Escape => self.grab(false),
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
                // does not dig.
                if !used_by_overlay {
                    if !self.grabbed {
                        self.grab(true);
                    } else if let Err(err) = self.dig() {
                        self.error = Some(err);
                        event_loop.exit();
                    }
                }
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
                let scene_started = Instant::now();
                if let Err(err) = self.recentre().and_then(|()| self.stream()) {
                    self.error = Some(err);
                    event_loop.exit();
                }
                if self.world.is_some() {
                    self.scene_work.record(scene_started.elapsed());
                }
                let overlay = self.run_overlay();
                let ctx = self.gui.as_ref().map(|gui| gui.ctx.clone());
                if let Some(renderer) = &mut self.renderer {
                    renderer.render(&self.camera, overlay.zip(ctx.as_ref()).map(|(o, c)| (c, o)));
                }
                self.update_title();
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
        let past = leash.centre.map(|c| c + 400.0);
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
