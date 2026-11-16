//! Rendering to a window surface.

use std::time::Instant;

use crate::camera::Camera;
use crate::composite::Composite;
use crate::cpu::RenderCpu;
use crate::debug_view::DebugView;
use crate::look::Look;
use crate::post::Stages;
use crate::profiler::Profiler;
use crate::scene::{Scene, SceneUpdate};
use crate::still::Plan;
use crate::sun::Sun;
use crate::timing::Timings;
use crate::tracer::{Target, Tracer, View};
use crate::upscale::{DEFAULT_SCALE, Resolution};
use crate::{Result, create_instance, request_gpu};

/// How the Planet is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Primary visibility, shaded by normal and albedo: cheap and noise-free. It is traced at
    /// a share of the window's size and upscaled over the frames.
    #[default]
    Primary,
    /// The Reference view when the view is Still (ADR 0005): primary visibility while the camera,
    /// the sun or the window change, and once nothing has changed for 2 frames the reference path
    /// tracer, at the output resolution, taking as many samples a frame as fit in 12 ms and
    /// fading in over the last Primary image. Any change cuts to Primary at once.
    Reference,
}

/// Traces the Planet into an image and presents it.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    tracer: Tracer,
    profiler: Profiler,
    /// What the CPU spent drawing the last frame.
    last_cpu: RenderCpu,
    /// Paints egui's output over the image.
    gui: egui_wgpu::Renderer,
    target: Target,
    /// The share of the window's width and height that primary visibility traces.
    scale: f32,
    mode: Mode,
    sun: Sun,
    /// If set, the sun is at these angles (azimuth, elevation, angular radius) in the camera's local
    /// frame, so it follows the Planet's up (see [`Sun::local`]); `sun` is then not used.
    local_sun: Option<(f32, f32, f32)>,
    /// Copies the images to the surface, mixing the Reference view over Primary's as it fades in.
    composite: Composite,
    /// What `composite` reads: the upscaled image and the reference image.
    composite_group: wgpu::BindGroup,
    /// Time zero of the clock that the fade runs on.
    began: Instant,
    /// What the last frame drew.
    plan: Plan,
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
        let (adapter, device, queue) = request_gpu(&instance, Some(&surface), true)?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("surface is not supported by the adapter")?;
        // The traced image already holds display-encoded values (AgX-like by default; the true sRGB
        // curve for Neutral and with the transform off), so present it to a surface that does not
        // encode them a second time.
        let formats = surface.get_capabilities(&adapter).formats;
        if let Some(format) = formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = format;
        }
        surface.configure(&device, &config);

        let tracer = Tracer::new(&device, scene)?;
        let profiler = Profiler::new(&device, &queue);
        let gui = egui_wgpu::Renderer::new(&device, config.format, Default::default());
        let composite = Composite::new(&device, config.format);
        let (target, composite_group) = Self::create_target(
            &device,
            &composite,
            Resolution::scaled((config.width, config.height), DEFAULT_SCALE),
        );
        Ok(Self {
            surface,
            config,
            device,
            queue,
            tracer,
            profiler,
            last_cpu: RenderCpu::default(),
            gui,
            target,
            scale: DEFAULT_SCALE,
            mode: Mode::default(),
            sun: Sun::default(),
            local_sun: Some((
                crate::sun::DEFAULT_AZIMUTH,
                crate::sun::DEFAULT_ELEVATION,
                crate::sun::DEFAULT_ANGULAR_RADIUS,
            )),
            composite,
            composite_group,
            began: Instant::now(),
            plan: Plan::Primary,
        })
    }

    /// The images for `resolution`, with the Reference view at the output resolution, and the
    /// bind group of the copy that reads them.
    fn create_target(
        device: &wgpu::Device,
        composite: &Composite,
        resolution: Resolution,
    ) -> (Target, wgpu::BindGroup) {
        let target = Target::with_full_reference(device, resolution);
        let group = composite.group(device, target.image(), target.reference());
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

    /// Bytes of GPU memory that the Planet's bricks take.
    pub fn scene_bytes(&self) -> u64 {
        self.tracer.scene_bytes()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switches how the Planet is drawn.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn debug_view(&self) -> DebugView {
        self.tracer.debug_view()
    }

    /// Chooses what is drawn: the lit image, or one of the debug views. The reference image
    /// keeps accumulating whichever is chosen.
    pub fn set_debug_view(&mut self, view: DebugView) {
        self.tracer.set_debug_view(view);
    }

    /// The GPU time of each pass, as of a frame or two ago. Where the device has no timestamp
    /// queries there are none ([`Renderer::timers_supported`]).
    pub fn timings(&self) -> &Timings {
        self.profiler.timings()
    }

    /// Bytes of the Planet written to the GPU so far: what the buffers were made from, and what
    /// each [`Renderer::update_scene`] wrote.
    pub fn uploaded_bytes(&self) -> u64 {
        self.tracer.uploaded_bytes()
    }

    /// The most words a buffer of the scene may have on this device (see
    /// [`Scene::set_buffer_limit`](crate::Scene::set_buffer_limit)).
    pub fn buffer_limit_words(&self) -> usize {
        (self.device.limits().max_storage_buffer_binding_size / 4) as usize
    }

    /// The most bytes of GPU memory the scene's buffers have taken at once while a buffer grew
    /// (the old buffer and the new one both counted), and while a rebuilt scene was made; each 0
    /// if it has not happened.
    pub fn peak_growth_bytes(&self) -> u64 {
        self.tracer.peak_growth_bytes()
    }

    pub fn peak_rebuild_bytes(&self) -> u64 {
        self.tracer.peak_rebuild_bytes()
    }

    /// How frames are presented, which says whether acquiring one waits for vertical sync.
    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.config.present_mode
    }

    /// What the CPU spent in the last call to [`Renderer::render`], by part.
    pub fn last_cpu(&self) -> RenderCpu {
        self.last_cpu
    }

    /// Whether the device can time passes.
    pub fn timers_supported(&self) -> bool {
        self.profiler.supported()
    }

    /// Keeps every pass time from now on, for a report over many frames.
    pub fn set_keep_history(&mut self, keep: bool) {
        self.profiler.set_keep_history(keep);
    }

    /// Changes the sun. In reference mode this starts the image over.
    pub fn set_sun(&mut self, sun: Sun) {
        self.sun = sun;
        self.local_sun = None;
    }

    /// Puts the sun at `azimuth` and `elevation` radians in the camera's local frame, `radius`
    /// radians wide: it follows the local up, so it stays at the same height in the sky over the
    /// whole Planet ([`Sun::local`]). In reference mode this starts the image over when it moves.
    pub fn set_local_sun(&mut self, azimuth: f32, elevation: f32, radius: f32) {
        self.local_sun = Some((azimuth, elevation, radius));
    }

    /// The sun for a frame seen from `camera`.
    fn sun_for(&self, camera: &Camera) -> Sun {
        match self.local_sun {
            Some((azimuth, elevation, radius)) => Sun::local(camera, azimuth, elevation, radius),
            None => self.sun,
        }
    }

    /// Sets the light's palette (see [`Look`]).
    pub fn set_look(&mut self, look: Look) {
        self.tracer.set_look(look);
    }

    /// Switches the stages of the art direction (see [`Stages`]).
    pub fn set_stages(&mut self, stages: Stages) {
        self.tracer.set_stages(stages);
    }

    pub fn stages(&self) -> Stages {
        self.tracer.stages()
    }

    /// Samples in the reference image so far, to show on screen.
    pub fn samples(&self) -> u32 {
        self.tracer.samples()
    }

    /// Samples that the last frame added to the reference image, for the overlay: 0 for a frame
    /// that showed Primary.
    pub fn samples_per_frame(&self) -> u32 {
        self.plan.samples()
    }

    /// What the reference image would be of with the view from `camera`: it starts over when this
    /// changes (see [`View`]), so comparing it from frame to frame tells whether the view is still.
    pub fn view(&self, camera: &Camera) -> View {
        View::new(camera, &self.sun_for(camera), &self.target)
    }

    /// Reconfigures the surface and the images. Zero-sized windows (minimised) are ignored.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.recreate_target();
    }

    /// The share of the window's width and height that primary visibility traces.
    pub fn render_scale(&self) -> f32 {
        self.scale
    }

    /// Traces primary visibility at `scale` of the window's width and height (0.5 is the default,
    /// 1.0 traces every pixel); the upscaler fills in the rest. The reference path tracer traces
    /// every pixel of the window, whatever the scale is.
    pub fn set_render_scale(&mut self, scale: f32) {
        if scale.is_finite() && scale > 0.0 && scale != self.scale {
            self.scale = scale;
            self.recreate_target();
        }
    }

    fn recreate_target(&mut self) {
        let resolution = Resolution::scaled((self.config.width, self.config.height), self.scale);
        (self.target, self.composite_group) =
            Self::create_target(&self.device, &self.composite, resolution);
        self.tracer.reset_accumulation();
    }

    /// Traces the view from `camera` and presents it: in reference mode, one more sample of the
    /// image, which starts over if the camera moved. A frame that cannot be acquired is
    /// skipped. A suboptimal surface is reconfigured after its frame is presented.
    ///
    /// `gui` is what egui drew this frame, with the context that drew it, to paint over the
    /// image. Pass `None` to draw no overlay at all: nothing of egui is run or painted.
    pub fn render(&mut self, camera: &Camera, mut gui: Option<(&egui::Context, egui::FullOutput)>) {
        // egui sends each texture change once, so it is applied even if the frame is skipped.
        let freed = gui.as_mut().map(|(_, output)| {
            for (id, deltas) in std::mem::take(&mut output.textures_delta.set) {
                for delta in deltas {
                    self.gui
                        .update_texture(&self.device, &self.queue, id, &delta);
                }
            }
            std::mem::take(&mut output.textures_delta.free)
        });
        self.draw(camera, gui);
        for id in freed.into_iter().flatten() {
            self.gui.free_texture(&id);
        }
    }

    fn draw(&mut self, camera: &Camera, gui: Option<(&egui::Context, egui::FullOutput)>) {
        let acquiring = Instant::now();
        let texture = self.surface.get_current_texture();
        self.last_cpu = RenderCpu {
            acquire_ms: acquiring.elapsed().as_secs_f64() * 1e3,
            ..RenderCpu::default()
        };
        let (frame, suboptimal) = match texture {
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
        let encoding = Instant::now();
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        let sun = self.sun_for(camera);
        self.plan = self.tracer.encode_frame(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun,
            &self.target,
            self.mode == Mode::Reference,
            self.began.elapsed(),
        );
        self.composite.set_fade(&self.queue, self.plan.fade());
        {
            let mut pass = self.profiler.render_pass(
                &mut encoder,
                "blit",
                &color_attachment(&view, wgpu::LoadOp::Clear(wgpu::Color::BLACK)),
            );
            self.composite.draw(&mut pass, &self.composite_group);
        }
        let mut commands = Vec::new();
        if let Some((ctx, output)) = gui {
            let paint_jobs = ctx.tessellate(output.shapes, output.pixels_per_point);
            let screen = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [self.config.width, self.config.height],
                pixels_per_point: output.pixels_per_point,
            };
            commands = self.gui.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &paint_jobs,
                &screen,
            );
            let pass = self.profiler.render_pass(
                &mut encoder,
                "overlay",
                &color_attachment(&view, wgpu::LoadOp::Load),
            );
            self.gui
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen);
        }
        self.profiler.resolve(&mut encoder);
        commands.push(encoder.finish());
        self.queue.submit(commands);
        self.profiler.collect(&self.device, false);
        self.last_cpu.encode_ms = encoding.elapsed().as_secs_f64() * 1e3;
        let presenting = Instant::now();
        self.queue.present(frame);
        self.last_cpu.present_ms = presenting.elapsed().as_secs_f64() * 1e3;
        // The surface cannot be reconfigured while a frame is alive, so this waits until
        // the frame has been presented.
        if suboptimal {
            self.surface.configure(&self.device, &self.config);
        }
    }
}

/// The attachment of a render pass that draws to `view` and keeps the result.
fn color_attachment(
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> [Option<wgpu::RenderPassColorAttachment<'_>>; 1] {
    [Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load,
            store: wgpu::StoreOp::Store,
        },
    })]
}
