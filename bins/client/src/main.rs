use std::error::Error;
use std::sync::Arc;

use render::Renderer;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    /// Set when startup fails inside the event loop, which cannot return errors itself.
    error: Option<Box<dyn Error>>,
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let window = Arc::new(
            event_loop.create_window(Window::default_attributes().with_title("BhayanakVoxel"))?,
        );
        let size = window.inner_size();
        self.renderer = Some(Renderer::new(Arc::clone(&window), size.width, size.height)?);
        self.window = Some(window);
        Ok(())
    }
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
            WindowEvent::RedrawRequested => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.render();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
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
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}
