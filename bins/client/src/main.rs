mod fly;

use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use fly::{Action, Fly};
use render::scene::Core;
use render::{Camera, Renderer, Scene};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

const SEED: u64 = 0xB4A7_A9AC;

/// Longest time step the camera takes, so that a long stall (such as uploading the Planet)
/// does not make it jump.
const MAX_FRAME_TIME: f64 = 0.1;

const TITLE: &str = "BhayanakVoxel";

/// Generates the test Planet and packs it for the GPU. This takes about 25 seconds on eight
/// cores in a release build (several times that in debug), so it runs on its own threads while
/// the window is already up.
fn load_planet() -> Scene {
    let started = Instant::now();
    let bricks = planet_gen::generate_planet(SEED);
    eprintln!(
        "generated {} bricks in {:.1?}",
        bricks.len(),
        started.elapsed()
    );
    let core = Core {
        radius: planet_gen::CORE_RADIUS,
        material: planet_gen::ROCK,
    };
    let scene = Scene::new(&bricks, Some(core));
    eprintln!("planet ready after {:.1?}", started.elapsed());
    scene
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// The Planet, while it is still being generated.
    loading: Option<Receiver<Scene>>,
    camera: Camera,
    fly: Fly,
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
            // Far enough out to see the whole Planet (radius 1500 Voxels), looking at its centre.
            camera: Camera::looking_at([0.0, 400.0, 3400.0], [0.0; 3]),
            fly: Fly::default(),
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

    /// Hands the Planet to the renderer once the loading thread has finished it.
    fn poll_loading(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(receiver) = &self.loading else {
            return Ok(());
        };
        match receiver.try_recv() {
            Ok(scene) => {
                self.loading = None;
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_scene(&scene)?;
                }
                if let Some(window) = &self.window {
                    window.set_title(TITLE);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Err("the Planet failed to generate".into()),
        }
        Ok(())
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
                    } else if code == KeyCode::Escape && pressed {
                        self.grab(false);
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.grab(true),
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
        if let Err(err) = self.poll_loading() {
            self.error = Some(err);
            event_loop.exit();
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
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
