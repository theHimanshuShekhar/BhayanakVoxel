//! Rendering to a window surface.

use crate::camera::Camera;
use crate::scene::{Scene, SceneUpdate};
use crate::sun::Sun;
use crate::tracer::{Target, Tracer};
use crate::{Result, create_instance, request_gpu, shaders};

/// How the Planet is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Primary visibility, shaded by normal and albedo: cheap and noise-free.
    #[default]
    Primary,
    /// The reference path tracer: while nothing changes, samples accumulate without limit and
    /// the image converges.
    Reference,
}

/// Traces the Planet into an image and presents it.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    tracer: Tracer,
    target: Target,
    mode: Mode,
    sun: Sun,
    blit: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_group: wgpu::BindGroup,
}

impl Renderer {
    /// `window` is anything wgpu can make a surface from, e.g. an `Arc<winit::window::Window>`.
    pub fn new(
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        scene: &Scene,
    ) -> Result<Self> {
        let instance = create_instance();
        let surface = instance.create_surface(window)?;
        let (adapter, device, queue) = request_gpu(&instance, Some(&surface))?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("surface is not supported by the adapter")?;
        // The traced image already holds sRGB-encoded values, so present it to a surface that
        // does not encode them a second time.
        let formats = surface.get_capabilities(&adapter).formats;
        if let Some(format) = formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = format;
        }
        surface.configure(&device, &config);

        let tracer = Tracer::new(&device, scene)?;
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(shaders::BLIT.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(config.format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let (target, blit_group) =
            Self::create_target(&device, &blit_layout, config.width, config.height);
        Ok(Self {
            surface,
            config,
            device,
            queue,
            tracer,
            target,
            mode: Mode::default(),
            sun: Sun::default(),
            blit,
            blit_layout,
            blit_group,
        })
    }

    fn create_target(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        width: u32,
        height: u32,
    ) -> (Target, wgpu::BindGroup) {
        let target = Target::new(device, width, height);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(target.view()),
            }],
        });
        (target, group)
    }

    /// Replaces the Planet being drawn. A new Planet starts the reference image over.
    pub fn set_scene(&mut self, scene: &Scene) -> Result<()> {
        self.tracer = Tracer::new(&self.device, scene)?;
        Ok(())
    }

    /// Brings the GPU copy of the Planet up to date with `scene`, after `update` (the result of
    /// [`Scene::update`]) changed it. This starts the reference image over.
    pub fn update_scene(&mut self, scene: &Scene, update: &SceneUpdate) -> Result<()> {
        self.tracer
            .update_scene(&self.device, &self.queue, scene, update)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switches how the Planet is drawn.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Changes the sun. In reference mode this starts the image over.
    pub fn set_sun(&mut self, sun: Sun) {
        self.sun = sun;
    }

    /// Samples in the reference image so far, to show on screen.
    pub fn samples(&self) -> u32 {
        self.tracer.samples()
    }

    /// Reconfigures the surface and the traced image. Zero-sized windows (minimised) are ignored.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        (self.target, self.blit_group) =
            Self::create_target(&self.device, &self.blit_layout, width, height);
        self.tracer.reset_accumulation();
    }

    /// Traces the view from `camera` and presents it: in reference mode, one more sample of the
    /// image, which starts over if the camera moved. A frame that cannot be acquired is
    /// skipped. A suboptimal surface is reconfigured after its frame is presented.
    pub fn render(&mut self, camera: &Camera) {
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
        match self.mode {
            Mode::Primary => self.tracer.encode_primary(
                &self.device,
                &self.queue,
                &mut encoder,
                camera,
                &self.sun,
                &self.target,
            ),
            Mode::Reference => {
                self.tracer.encode_reference(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    camera,
                    &self.sun,
                    &self.target,
                );
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.blit);
            pass.set_bind_group(0, &self.blit_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        // The surface cannot be reconfigured while a frame is alive, so this waits until
        // the frame has been presented.
        if suboptimal {
            self.surface.configure(&self.device, &self.config);
        }
    }
}
