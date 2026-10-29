mod fly;
mod world;

use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use brick_store::BrickStore;
use fly::{Action, Fly};
use render::scene::Core;
use render::{Camera, Mode, Renderer, Scene, SceneUpdate, Sun};
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

/// Radians the sun turns per arrow key press (and per repeat while it is held).
const SUN_STEP: f32 = 0.05;

/// The test Planet as the client and the in-process server each hold it, and the client's packed
/// copy of it.
struct Loaded {
    store: BrickStore,
    server_store: BrickStore,
    scene: Scene,
}

/// Generates the test Planet and packs it for the GPU. This takes about 25 seconds on eight
/// cores in a release build (several times that in debug), so it runs on its own threads while
/// the window is already up.
fn load_planet() -> Loaded {
    let started = Instant::now();
    let store = planet_gen::generate_planet(SEED);
    eprintln!(
        "generated {} bricks in {:.1?}",
        store.len(),
        started.elapsed()
    );
    let core = Core {
        radius: planet_gen::CORE_RADIUS,
        material: planet_gen::ROCK,
    };
    let scene = Scene::new(&store, Some(core));
    // Generation is deterministic, so the server's Planet is what generating it again would
    // give; copying it is quicker.
    let server_store = store.clone();
    eprintln!("planet ready after {:.1?}", started.elapsed());
    Loaded {
        store,
        server_store,
        scene,
    }
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
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
            loading: Some(receiver),
            world: None,
            server: None,
            // Far enough out to see the whole Planet (radius 1500 Voxels), looking at its centre.
            camera: Camera::looking_at([0.0, 400.0, 3400.0], [0.0; 3]),
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
        let update = world.poll().map_err(|_| "the server stopped")?;
        self.show(update)
    }

    /// Digs where the camera looks. The hole is drawn at once and confirmed by the server.
    fn dig(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(world) = &mut self.world else {
            return Ok(());
        };
        let update = world.dig(&self.camera).map_err(|_| "the server stopped")?;
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

    /// Shows the state in the window title: loading, or which way the Planet is drawn, with the
    /// reference image's sample count.
    fn update_title(&mut self) {
        let (Some(window), Some(renderer)) = (&self.window, &self.renderer) else {
            return;
        };
        let title = if self.loading.is_some() {
            format!("{TITLE} (generating the Planet...)")
        } else if renderer.mode() == Mode::Reference {
            format!("{TITLE} - reference, {} samples", renderer.samples())
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
        let half_turn = std::f32::consts::FRAC_PI_2;
        self.sun = (
            self.sun.0 + azimuth,
            (self.sun.1 + elevation).clamp(-half_turn, half_turn),
        );
        if let Some(renderer) = &mut self.renderer {
            renderer.set_sun(Sun::from_angles(
                self.sun.0,
                self.sun.1,
                Sun::default().angular_radius(),
            ));
        }
    }

    /// Captures or releases the mouse for looking around.
    fn grab(&mut self, grab: bool) {
        let Some(window) = &self.window else { return };
        if grab {
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
                    if let Some(action) = action_for(code) {
                        self.fly.set(action, pressed);
                    } else if pressed {
                        match code {
                            KeyCode::Escape => self.grab(false),
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
                // The click that captures the mouse does not dig.
                if !self.grabbed {
                    self.grab(true);
                } else if let Err(err) = self.dig() {
                    self.error = Some(err);
                    event_loop.exit();
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
                if let Some(renderer) = &mut self.renderer {
                    renderer.render(&self.camera);
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
        self.renderer = None;
        self.window = None;
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}
