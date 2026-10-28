//! wgpu rendering on the Vulkan backend.

use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.05,
    b: 0.12,
    a: 1.0,
};

/// Vulkan is the only supported backend (ADR 0001).
fn create_instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    wgpu::Instance::new(desc)
}

/// Requests a device on a Vulkan adapter, presentable to `surface` if one is given.
fn request_gpu(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface,
        ..Default::default()
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("bhayanak device"),
        ..Default::default()
    }))?;
    Ok((adapter, device, queue))
}

/// Draws to a window surface, currently just clearing it to a colour.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Renderer {
    /// `window` is anything wgpu can make a surface from, e.g. an `Arc<winit::window::Window>`.
    pub fn new(
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let instance = create_instance();
        let surface = instance.create_surface(window)?;
        let (adapter, device, queue) = request_gpu(&instance, Some(&surface))?;
        let config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("surface is not supported by the adapter")?;
        surface.configure(&device, &config);
        Ok(Self {
            surface,
            config,
            device,
            queue,
        })
    }

    /// Reconfigures the surface. Zero-sized windows (minimised) are ignored.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Clears the surface and presents it. A frame that cannot be acquired is skipped.
    /// A suboptimal surface is reconfigured after its frame is presented.
    pub fn render(&mut self) {
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        // The surface cannot be reconfigured while a frame is alive, so this waits until
        // the frame has been presented.
        if suboptimal {
            self.surface.configure(&self.device, &self.config);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a Vulkan driver; CI uses lavapipe (mesa-vulkan-drivers).
    #[test]
    fn vulkan_device_can_be_created_without_a_window() {
        let (adapter, _device, _queue) = request_gpu(&create_instance(), None).unwrap();
        assert_eq!(adapter.get_info().backend, wgpu::Backend::Vulkan);
    }
}
