mod bench;
mod fly;
mod world;

use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use brick_store::BrickStore;
use fly::{Action, Fly};
use planet_gen::planet::Planet;
use planet_gen::world_map::{WORLD_MAP_SIZE, WORLD_RADIUS, WorldMap};
use render::overlay::{self, Controls, SceneMemory};
use render::{Camera, Mode, Renderer, RingsFull, Scene, SceneUpdate, Sun};
use sim::{Predictor, ServerHandle};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};
use world::World;

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
/// Voxels from the spawn point that the client generates the Planet's ground to, along the
/// ground (see [`Planet::region_around`]). Beyond it the rings draw the ground only where it is
/// generated on demand, a ring's width from the camera, and otherwise only the deep rock and the
/// core, until the Planet can be streamed (#47, #49): the test Planet's 2.6 million bricks of
/// ground took 20 seconds, and this Planet has ten thousand times the ground.
const SPAWN_REACH: i32 = 640;

/// Radians the sun turns per arrow key press (and per repeat while it is held).
const SUN_STEP: f32 = 0.05;

/// Mip levels the client draws the Planet in: rings of them, each twice as coarse as the one
/// inside it. The outermost is 65536 Voxels across, which is room to zoom out to ten times the
/// distance of the spawn point.
const RING_LEVELS: usize = 7;

/// A Planet's bricks packed for the GPU as rings around `camera`. The store must have its
/// pyramid ([`RING_LEVELS`] of it); its source gives the bricks it does not hold.
fn planet_scene(store: &BrickStore, camera: [f64; 3]) -> Result<Scene, RingsFull> {
    Scene::rings(store, camera, RING_LEVELS)
}

/// The Planet as the client and the in-process server each hold it, the client's packed copy of
/// it, and where the client starts.
struct Loaded {
    store: BrickStore,
    server_store: BrickStore,
    scene: Scene,
    camera: Camera,
}

/// The Planet's ground as the client and the in-process server each hold it, before it is
/// packed for the GPU.
struct Ground {
    store: BrickStore,
    server_store: BrickStore,
    camera: Camera,
}

/// Generates the ground around a spawn on gentle land of the Planet of `map`, with a pyramid of
/// `levels` levels for the client's copy of it.
fn generate_ground(map: WorldMap, reach: i32, levels: usize) -> Ground {
    let started = Instant::now();
    let planet = Planet::new(map);
    let ground = planet.spawn(reach);
    let mut store = planet.region_around(ground, reach);
    eprintln!(
        "generated {} bricks around the spawn in {:.1?}",
        store.len(),
        started.elapsed()
    );
    // Generation is deterministic, so the server's Planet is what generating it again would
    // give; copying it is quicker. The server draws nothing, so it needs no coarse levels.
    let server_store = store.clone();
    store.build_lod(levels - 1);
    Ground {
        store,
        server_store,
        camera: spawn_camera(ground),
    }
}

/// Makes the Planet of `map` into what the client starts with: the ground around a spawn on
/// gentle land, its pyramid, and the rings around the spawn.
fn load(map: WorldMap, reach: i32) -> Result<Loaded, RingsFull> {
    let started = Instant::now();
    let Ground {
        store,
        server_store,
        camera,
    } = generate_ground(map, reach, RING_LEVELS);
    let scene = planet_scene(&store, camera.position)?;
    eprintln!("planet ready after {:.1?}", started.elapsed());
    Ok(Loaded {
        store,
        server_store,
        scene,
        camera,
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
    camera: Camera,
    fly: Fly,
    /// The sun as an azimuth and an elevation, which the arrow keys turn.
    sun: (f32, f32),
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
            // Until the Planet arrives there is only sky to look at, wherever from.
            camera: spawn_camera([0.0, 0.0, f64::from(WORLD_RADIUS)]),
            fly: Fly::default(),
            sun: Sun::default().angles(),
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
                self.world = Some(World::new(
                    loaded.scene,
                    Predictor::new(loaded.store),
                    client_end,
                ));
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

    /// Moves the rings of the Planet to the camera, and copies what that changed to the GPU.
    fn recentre(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let update = world.recentre(self.camera.position)?;
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
                self.fly.update(&mut self.camera, dt);
                if let Err(err) = self.recentre() {
                    self.error = Some(err);
                    event_loop.exit();
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

    /// Walks 200 m over the full-size Planet in steps of 1 m, moving the rings at each as the
    /// client does, and reports how long each move took. Takes about 40 seconds in a release
    /// build: `cargo test --release -p client -- --ignored --nocapture full_size`.
    #[test]
    #[ignore = "generates the full-size world map"]
    fn the_full_size_planet_loads_into_the_rings_and_walking_over_it_does_not_stall() {
        let started = Instant::now();
        let mut loaded = load_planet();
        eprintln!(
            "{} bricks held, {} bytes in the rings, camera at {:?}, {:.1?} in all",
            loaded.store.len(),
            loaded.scene.used_bytes(),
            loaded.camera.position,
            started.elapsed()
        );
        assert!(loaded.scene.is_rings());
        let (heading, _) = planet_gen::planet::tangents(loaded.camera.horizon_up);
        let mut position = loaded.camera.position;
        let (mut worst, mut total, mut moves) =
            (std::time::Duration::ZERO, std::time::Duration::ZERO, 0);
        for _ in 0..200 {
            position = std::array::from_fn(|a| position[a] + heading[a] * 10.0);
            let started = Instant::now();
            let moved = loaded.scene.recentre(&loaded.store, position).unwrap();
            let took = started.elapsed();
            if moved.is_some() {
                (worst, total, moves) = (worst.max(took), total + took, moves + 1);
            }
        }
        eprintln!(
            "200 steps of 1 m: {moves} moves of the rings, longest {worst:.1?}, in all {total:.1?}"
        );
        assert!(worst < std::time::Duration::from_millis(500), "{worst:?}");
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
