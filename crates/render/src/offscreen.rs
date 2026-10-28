//! Rendering without a window, into a texture that is read back: for tests and tools.

use crate::camera::Camera;
use crate::reference::{Hit, Ray};
use crate::scene::Scene;
use crate::tracer::Tracer;
use crate::{Result, create_instance, finish_and_read, request_gpu, staging_buffer};

/// A finished frame: tightly packed RGBA8, rows from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The renderer with an image of its own instead of a window.
pub struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    tracer: Tracer,
    image: wgpu::Texture,
    size: (u32, u32),
}

impl Offscreen {
    /// Needs a Vulkan device; CI and local tests use lavapipe.
    pub fn new(width: u32, height: u32, scene: &Scene) -> Result<Self> {
        let (_adapter, device, queue) = request_gpu(&create_instance(), None)?;
        let tracer = Tracer::new(&device, scene)?;
        let image = Tracer::create_image(&device, width, height);
        Ok(Self {
            device,
            queue,
            tracer,
            image,
            size: (width, height),
        })
    }

    /// Renders one frame from `camera` and reads it back.
    pub fn render(&self, camera: &Camera) -> Image {
        let (width, height) = self.size;
        let view = self
            .image
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.tracer.encode_primary(
            &self.device,
            &self.queue,
            &mut encoder,
            camera,
            &view,
            self.size,
        );
        // Rows in a texture-to-buffer copy start at multiples of 256 bytes.
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = u64::from(row) * u64::from(height);
        let staging = staging_buffer(&self.device, size);
        encoder.copy_texture_to_buffer(
            self.image.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let padded = finish_and_read(&self.device, &self.queue, encoder, &staging, size);
        let rgba = padded
            .chunks(row as usize)
            .flat_map(|r| &r[..width as usize * 4])
            .copied()
            .collect();
        Image {
            width,
            height,
            rgba,
        }
    }

    /// Casts `rays` through the GPU `trace()` and returns the hits, in order.
    pub fn trace_rays(&self, rays: &[Ray]) -> Vec<Option<Hit>> {
        self.tracer.trace_rays(&self.device, &self.queue, rays)
    }
}
