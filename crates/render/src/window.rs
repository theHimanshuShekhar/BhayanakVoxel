//! Rendering to a window surface.

use std::time::Instant;

use crate::camera::Camera;
use crate::composite::Composite;
use crate::cpu::RenderCpu;
use crate::daylight::{DaySky, LocalFrame, Night};
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
use crate::{Result, RtBackend, RtChoice, RtDevice, create_instance, request_gpu_for};

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
    /// What the player asked for and what runs (#50): hardware ray queries, which are the default,
    /// or the compute walk (see [`Renderer::set_backend`]).
    rt: RtChoice,
    /// What the device has of ray queries and why (#260): its features are fixed when it is made,
    /// from the setting of that launch.
    rt_device: RtDevice,
    /// Where the camera was at the last frame: where the hardware backend's frame starts when the
    /// tracer is built again (a new Planet, or a change of backend).
    camera_position: [f64; 3],
    profiler: Profiler,
    /// What the CPU spent drawing the last frame.
    last_cpu: RenderCpu,
    /// Paints egui's output over the image.
    gui: egui_wgpu::Renderer,
    target: Target,
    /// The share of the window's width and height that primary visibility traces.
    scale: f32,
    mode: Mode,
    /// Primary is drawn though the mode is Reference: see [`Renderer::set_reference_wait`].
    reference_wait: bool,
    sun: Sun,
    /// If set, the sun is at these angles (azimuth, elevation, angular radius) in the camera's local
    /// frame, so it follows the Planet's up (see [`Sun::local`]); `sun` is then not used.
    local_sun: Option<(f32, f32, f32)>,
    /// The sky of the world clock's time of day (see [`Renderer::set_day_sky`]); it replaces the
    /// sun above while there is one.
    day_sky: Option<DaySky>,
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
    ///
    /// `backend` is how rays find the Planet (#50): the player's setting, and the default is
    /// [`RtBackend::Hardware`]. **The device is made with wgpu's experimental ray-query features
    /// only if `backend` is a hardware one, the adapter has them and the run is not held to none
    /// (`--profile low-end`)** (#260): the compute setting makes a device that cannot choke on
    /// them, and a change of the setting at run time ([`Renderer::set_backend`]) is a change of
    /// the next launch's device ([`Renderer::rt_note`] says so). If the device cannot be made with
    /// them it is made again without, and compute runs. An adapter without them runs the compute
    /// backend, with no error ([`Renderer::rt`] says why).
    pub fn new(
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
        scene: &Scene,
        backend: RtBackend,
    ) -> Result<Self> {
        let instance = create_instance();
        let surface = instance.create_surface(window)?;
        let (adapter, device, queue, rt, rt_device) =
            request_gpu_for(&instance, Some(&surface), true, backend, false)?;
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

        // Until a frame is drawn the camera is at the Planet's middle, which the hardware backend's
        // frame starts from.
        let camera_position = [0.0; 3];
        let tracer = Tracer::on_backend(&device, &queue, scene, camera_position, rt.active)?;
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
            rt,
            rt_device,
            camera_position,
            profiler,
            last_cpu: RenderCpu::default(),
            gui,
            target,
            scale: DEFAULT_SCALE,
            mode: Mode::default(),
            reference_wait: false,
            sun: Sun::default(),
            local_sun: Some((
                crate::sun::DEFAULT_AZIMUTH,
                crate::sun::DEFAULT_ELEVATION,
                crate::sun::DEFAULT_ANGULAR_RADIUS,
            )),
            day_sky: None,
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

    /// Replaces the Planet being drawn. A new Planet starts the reference image over. **The ray
    /// backend stays what it is** (#50): the tracer that replaces the old one is on the backend that
    /// [`Renderer::rt`] says, its acceleration structures made for the new Planet.
    pub fn set_scene(&mut self, scene: &Scene) -> Result<()> {
        self.tracer
            .replace_scene(&self.device, &self.queue, scene, self.camera_position)
    }

    /// Which way rays find the Planet: what the player asked for and what runs, and why not the
    /// one asked for if it is not (the adapter has no ray queries).
    pub fn rt(&self) -> &RtChoice {
        &self.rt
    }

    /// Why this adapter cannot run ray queries, if it cannot: the compute backend is then the only
    /// one, whatever the setting says.
    pub fn rt_unavailable(&self) -> Option<&str> {
        self.rt_device.unavailable.as_deref()
    }

    /// What the player should know about the device's ray-query features against the setting, if
    /// anything (#260): a restart is needed for hardware ray tracing on a device that was made
    /// without them, the next launch makes a device without them after a switch to compute, or the
    /// device could not be made with them. The overlay says it.
    pub fn rt_note(&self) -> Option<String> {
        self.rt_device.note(self.rt.requested)
    }

    /// Whether the device was made with the ray-query features.
    pub fn device_has_ray_queries(&self) -> bool {
        self.rt_device.on_device
    }

    /// Changes how rays find the Planet to `requested`, for `scene`, which is the Planet being drawn
    /// (what the window draws, and what [`Renderer::update_scene`] has kept up to date): the
    /// player's setting, changed at run time (#50). The tracer is made again for the scene, so this
    /// takes a moment, as a rebuild of the scene does; the sun, the look and the rest of its
    /// settings stay. On an adapter without ray queries the compute backend stays, with no error,
    /// and so it does on a device that was made without them (for the compute setting, #260, or
    /// because it could not be made with them): a switch to hardware is then kept as the setting
    /// and takes effect at the next launch ([`Renderer::rt_note`]).
    pub fn set_backend(&mut self, scene: &Scene, requested: RtBackend) -> Result<()> {
        let rt = self
            .rt_device
            .choose(requested, self.device.features(), &self.device.limits());
        if rt.active != self.tracer.backend() {
            self.tracer.switch_backend(
                &self.device,
                &self.queue,
                scene,
                self.camera_position,
                rt.active,
            )?;
        }
        self.rt = rt;
        Ok(())
    }

    /// Brings the GPU copy of the Planet up to date with `scene`, after `update` (the result of
    /// [`Scene::update`]) changed it. This starts the reference image over.
    pub fn update_scene(&mut self, scene: &Scene, update: &SceneUpdate) -> Result<()> {
        self.tracer
            .update_scene(&self.device, &self.queue, scene, update)
    }

    /// Says that the player's Edit changed `bricks` (when it was made, not when the scene thread's
    /// patch of it arrives), so that the radiance cache follows the light round them faster (see
    /// [`Tracer::edit_made`]); streaming loads and moves of the scene are not Edits.
    pub fn edit_made(&mut self, bricks: &[brick_store::BrickCoord]) {
        self.tracer.edit_made(bricks);
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

    /// Holds the Reference view back (or lets it start) in Reference mode: while the client waits
    /// for the scene to reach a camera that has stopped (#217), what is drawn is Primary, and the
    /// Reference view starts, from sample 0, in the frame after this is false.
    pub fn set_reference_wait(&mut self, wait: bool) {
        self.reference_wait = wait;
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
        if self.local_sun.is_some() || self.day_sky.is_some() || sun != self.sun {
            self.tracer.sun_changed();
        }
        self.sun = sun;
        self.local_sun = None;
        self.day_sky = None;
    }

    /// Puts the sun at `azimuth` and `elevation` radians in the camera's local frame, `radius`
    /// radians wide: it follows the local up, so it stays at the same height in the sky over the
    /// whole Planet ([`Sun::local`]). In reference mode this starts the image over when it moves.
    pub fn set_local_sun(&mut self, azimuth: f32, elevation: f32, radius: f32) {
        if self.day_sky.is_some() || self.local_sun != Some((azimuth, elevation, radius)) {
            self.tracer.sun_changed();
        }
        self.local_sun = Some((azimuth, elevation, radius));
        self.day_sky = None;
    }

    /// Puts the sky at the world clock's time of day (#203): the sun, the moon, the night's floor
    /// and the stars, in the camera's local frame ([`LocalFrame::of`]), so they keep their place in
    /// the sky over the whole Planet. It replaces [`Renderer::set_sun`] and
    /// [`Renderer::set_local_sun`]. Call it every frame with what `Daylight::step` said:
    /// **`jumped` is what tells the light's caches that the sun was put somewhere** (the clock was
    /// set, or the step was big) and makes them forget at once; a sky that only drifted on from the
    /// last frame's is followed by the caches' normal updates, and the upscaler's history goes on.
    /// The Reference view starts over whenever the sky changes at all.
    pub fn set_day_sky(&mut self, sky: DaySky, jumped: bool) {
        if jumped || self.day_sky.is_none() {
            self.tracer.sun_changed();
        }
        self.day_sky = Some(sky);
        self.local_sun = None;
    }

    /// The sun and the night for a frame seen from `camera`.
    fn sky_for(&self, camera: &Camera) -> (Sun, Night) {
        if let Some(sky) = &self.day_sky {
            let frame = LocalFrame::of(camera);
            return (sky.sun_in(&frame), sky.night_in(&frame));
        }
        let sun = match self.local_sun {
            Some((azimuth, elevation, radius)) => Sun::local(camera, azimuth, elevation, radius),
            None => self.sun,
        };
        (sun, Night::NONE)
    }

    /// How many times the sun has jumped: see [`Tracer::sun_epoch`](crate::tracer::Tracer::sun_epoch).
    pub fn sun_epoch(&self) -> u64 {
        self.tracer.sun_epoch()
    }

    /// Sets the light's palette (see [`Look`]).
    pub fn set_look(&mut self, look: Look) {
        self.tracer.set_look(look);
    }

    /// Sets the height of the Planet's ground along the up, in Voxels, which the atmosphere
    /// measures the camera's height from: the Planet's radius over the Planet (see
    /// [`Tracer::set_ground_level`]).
    pub fn set_ground_level(&mut self, voxels: f64) {
        self.tracer.set_ground_level(voxels);
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

    /// The size the reference image is traced at, which a resize changes (and so restarts it).
    pub fn reference_size(&self) -> (u32, u32) {
        self.target.reference_size()
    }

    /// What the reference image would be of with the view from `camera`: it starts over when this
    /// changes (see [`View`]), so comparing it from frame to frame tells whether the view is still.
    pub fn view(&self, camera: &Camera) -> View {
        let (sun, night) = self.sky_for(camera);
        View::new(camera, &sun, &night, &self.target)
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
        self.camera_position = camera.position;
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
        let (sun, night) = self.sky_for(camera);
        self.tracer.set_night(night);
        self.plan = self.tracer.encode_frame(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun,
            &self.target,
            self.mode == Mode::Reference && !self.reference_wait,
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
