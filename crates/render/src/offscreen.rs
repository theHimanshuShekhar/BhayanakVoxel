//! Rendering without a window, into a texture that is read back: for tests and tools.

use crate::camera::Camera;
use crate::composite::Composite;
use crate::debug_view::DebugView;
use crate::profiler::Profiler;
use crate::reference::{Hit, Ray};
use crate::scene::{Scene, SceneUpdate};
use crate::still::Plan;
use crate::sun::Sun;
use crate::timing::Timings;
use crate::tracer::{Plane, ReferenceOptions, Target, Tracer};
use crate::upscale::Resolution;
use crate::{Result, finish_and_read, request_gpu, shared_instance, staging_buffer};

/// A half-precision float's value.
fn half_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (exponent, fraction) = (i32::from((bits >> 10) & 0x1f), f32::from(bits & 0x3ff));
    match exponent {
        0 => sign * fraction * 2f32.powi(-24),
        31 if fraction == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    }
}

/// Copies a `width` x `height` texture of `bytes` per texel to the CPU after the work in
/// `encoder`, tightly packed, rows from the top.
fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut encoder: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    (width, height): (u32, u32),
    bytes: u32,
) -> Vec<u8> {
    // Rows in a texture-to-buffer copy start at multiples of 256 bytes.
    let row = (width * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let size = u64::from(row) * u64::from(height);
    let staging = staging_buffer(device, size);
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
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
    let padded = finish_and_read(device, queue, encoder, &staging, size);
    let tight = (width * bytes) as usize;
    if tight == row as usize {
        return padded;
    }
    let mut rows = Vec::with_capacity(tight * height as usize);
    for r in padded.chunks(row as usize) {
        rows.extend_from_slice(&r[..tight]);
    }
    rows
}

/// A finished frame: tightly packed RGBA8, rows from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What primary visibility wrote beside the image, at the traced resolution, rows from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct GBuffer {
    pub width: u32,
    pub height: u32,
    /// Distance along the ray to the first hit, in Voxels; negative where it hit nothing.
    pub depth: Vec<f32>,
    /// The first hit's face normal, a unit vector along an axis; zero where nothing was hit.
    pub normal: Vec<[f32; 3]>,
    /// The first hit's surface colour before it is lit, as RGBA8 like an [`Image`]; black where
    /// nothing was hit.
    pub albedo: Vec<u8>,
}

/// Which GPU and driver a device is on, for tools that report where they ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuInfo {
    pub name: String,
    /// `Discrete`, `Integrated`, `Virtual`, `Cpu` (software, such as lavapipe) or `Other`.
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub backend: String,
}

/// What the CPU spent on the last [`Offscreen::update_scene`], by part: for finding out where a slow
/// frame's time went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateTimes {
    /// Queuing the writes to the scene's buffers (a copy into a staging belt's chunk each, recorded
    /// in an encoder), the new buffers of one that grew and the copies into them (`grow` in the
    /// bench's report), or for a rebuild making the new buffers and filling them.
    pub write: std::time::Duration,
    /// Sending them: the submit with no commands that follows (not made if flushing is off).
    pub flush: std::time::Duration,
    /// Copies written to the staging belt: each is a copy on the GPU with a barrier of its own. 0
    /// for a rebuild.
    pub writes: u32,
}

/// The GPU's own time for the passes of one frame, from the timestamps the passes wrote (the
/// device's clock, not wall time).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuPasses {
    /// The primary pass and the upscale pass, in milliseconds each: None if the frame has no such
    /// time.
    pub primary_ms: Option<f64>,
    pub upscale_ms: Option<f64>,
    /// From the start of the frame's first pass to the end of its last, gaps between them included.
    pub total_ms: Option<f64>,
}

/// What the last [`Offscreen::draw_upscaled_timed`] spent, by part. The frame is over when the GPU
/// is done, which is `wait`; the CPU's share is `encode` and `submit`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DrawTimes {
    /// Making the commands: the passes' encoding, the timer queries' resolving, and finishing the
    /// command buffer.
    pub encode: std::time::Duration,
    /// `Queue::submit` of them.
    pub submit: std::time::Duration,
    /// Waiting for the GPU: the passes' times being read back, and then the device having nothing
    /// left to do. This is the GPU's time (the writes of the update before it, and the frame), and
    /// whatever else the GPU was doing.
    pub wait: std::time::Duration,
    /// What the GPU says its passes of **this frame** took (see [`Profiler::times_of_this_frame`]):
    /// None if the device has no timers, or this frame's own times did not come in; never an
    /// earlier frame's.
    pub gpu: Option<GpuPasses>,
}

/// What [`Offscreen::draw_frame`] drew.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameDrawn {
    /// What the still-frame policy said: Primary, or a batch of the Reference view and how far its
    /// fade is.
    pub plan: Plan,
    /// Samples in the reference image after the frame (0 after a Primary frame).
    pub samples: u32,
    /// The GPU's time for the Reference pass of this frame, in milliseconds: the whole batch.
    /// None for a Primary frame, or where the time did not come in or the device has no timers.
    pub reference_ms: Option<f64>,
    /// How long the frame took from the start of its encoding until the GPU was done, wall clock.
    pub wall: std::time::Duration,
}

/// The copy that a window does, to a texture that can be read back: see [`Offscreen::shown`].
struct Presenter {
    composite: Composite,
    texture: wgpu::Texture,
    group: wgpu::BindGroup,
}

/// The renderer with an image of its own instead of a window.
pub struct Offscreen {
    info: GpuInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    tracer: Tracer,
    profiler: Profiler,
    target: Target,
    sun: Sun,
    /// The sun's azimuth, elevation and radius in the camera's local frame, if it follows the
    /// camera's up ([`Offscreen::set_local_sun`]); it replaces `sun` then.
    local_sun: Option<(f32, f32, f32)>,
    /// Whether `update_scene` sends its writes at once; see `set_flush_scene_updates`.
    flush_scene_updates: bool,
    last_update: UpdateTimes,
    last_draw: DrawTimes,
    last_passes: Vec<(String, f64)>,
    presenter: Option<Presenter>,
}

impl Offscreen {
    /// Traces and presents at the same size. Needs a Vulkan device; CI and local tests use
    /// lavapipe.
    pub fn new(width: u32, height: u32, scene: &Scene) -> Result<Self> {
        Self::with_resolution(Resolution::native(width, height), scene)
    }

    /// Traces at `resolution.internal` and presents at `resolution.output`.
    pub fn with_resolution(resolution: Resolution, scene: &Scene) -> Result<Self> {
        Self::create(resolution, scene, true)
    }

    /// Like [`Offscreen::with_resolution`], with the Reference view traced at the **output**
    /// resolution whatever Primary's scale is, as a window does (ADR 0005). The other
    /// constructors trace it at the traced resolution, which is what the bench's flight times.
    pub fn with_full_reference(resolution: Resolution, scene: &Scene) -> Result<Self> {
        Self::build(resolution, scene, true, None, true)
    }

    /// Like [`Offscreen::new`], on a device without timestamp queries, as on a GPU that lacks
    /// them: passes are not timed. For testing that path wherever the driver has them.
    pub fn without_timers(width: u32, height: u32, scene: &Scene) -> Result<Self> {
        Self::create(Resolution::native(width, height), scene, false)
    }

    /// Like [`Offscreen::new`], with the Material table `table` in place of the shipped one: for
    /// tests of lighting that need Materials of an albedo or an emission it does not have.
    pub fn with_materials(
        resolution: Resolution,
        scene: &Scene,
        table: &materials::MaterialTable,
    ) -> Result<Self> {
        Self::build(resolution, scene, true, Some(table), false)
    }

    fn create(resolution: Resolution, scene: &Scene, timers: bool) -> Result<Self> {
        Self::build(resolution, scene, timers, None, false)
    }

    fn build(
        resolution: Resolution,
        scene: &Scene,
        timers: bool,
        table: Option<&materials::MaterialTable>,
        full_reference: bool,
    ) -> Result<Self> {
        let (adapter, device, queue) = request_gpu(shared_instance(), None, timers)?;
        let info = adapter.get_info();
        let info = GpuInfo {
            name: info.name,
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
            backend: format!("{:?}", info.backend),
        };
        let mut tracer = match table {
            Some(table) => Tracer::with_materials(&device, scene, table)?,
            None => Tracer::new(&device, scene)?,
        };
        // The flat ambient, not Primary's GI (#54), until a caller asks for GI: most of what
        // renders here is a test of geometry, colours or the sun, whose frames must be a function of
        // the scene alone (GI is a noisy sample that depends on the cache's history), and goldens
        // of them stay as they were. The bench and the look-dev sheet turn GI on.
        tracer.set_flat_ambient(true);
        let profiler = Profiler::new(&device, &queue);
        let target = if full_reference {
            Target::with_full_reference(&device, resolution)
        } else {
            Target::new(&device, resolution)
        };
        Ok(Self {
            info,
            device,
            queue,
            tracer,
            profiler,
            target,
            sun: Sun::default(),
            local_sun: None,
            flush_scene_updates: true,
            last_update: UpdateTimes::default(),
            last_draw: DrawTimes::default(),
            last_passes: Vec::new(),
            presenter: None,
        })
    }

    /// Brings the GPU copy of the Planet up to date with `scene`, after `update` (the result of
    /// [`Scene::update`]) changed it. This starts the reference image over.
    pub fn update_scene(&mut self, scene: &Scene, update: &SceneUpdate) -> Result<()> {
        let began = std::time::Instant::now();
        self.tracer
            .update_scene(&self.device, &self.queue, scene, update)?;
        let written = began.elapsed();
        // The writes are queued until the next submit: send them now, so that their cost is the
        // update's and not that of the frame drawn next.
        if self.flush_scene_updates {
            self.queue.submit(std::iter::empty());
        }
        self.last_update = UpdateTimes {
            write: written,
            flush: began.elapsed() - written,
            writes: self.tracer.last_writes(),
        };
        Ok(())
    }

    /// Says that an Edit changed `bricks`, so that the radiance cache follows the light round them
    /// faster (see [`Tracer::edit_made`]).
    pub fn edit_made(&mut self, bricks: &[brick_store::BrickCoord]) {
        self.tracer.edit_made(bricks);
    }

    /// The parts of the CPU's time in the last [`Offscreen::update_scene`].
    pub fn last_update_times(&self) -> UpdateTimes {
        self.last_update
    }

    /// The parts of the last [`Offscreen::draw_upscaled_timed`].
    pub fn last_draw_times(&self) -> DrawTimes {
        self.last_draw
    }

    /// The GPU's time of each pass of the last [`Offscreen::draw_upscaled_timed`], by the pass's
    /// name (`primary`, `normals`, `shadows`, `gi`, `shade`, `upscale`, ...), in milliseconds:
    /// empty where the device has no timers or this frame's own times did not come in.
    pub fn last_draw_pass_times(&self) -> &[(String, f64)] {
        &self.last_passes
    }

    /// Whether [`Offscreen::update_scene`] sends the writes to the GPU at once (the default), or
    /// leaves them to go with the next frame's commands. For finding out what the first costs.
    pub fn set_flush_scene_updates(&mut self, flush: bool) {
        self.flush_scene_updates = flush;
    }

    /// Changes the sun that both ways of rendering light the Planet with.
    pub fn set_sun(&mut self, sun: Sun) {
        if self.local_sun.is_some() || sun != self.sun {
            self.tracer.sun_changed();
        }
        self.sun = sun;
        self.local_sun = None;
    }

    /// Puts the sun at `azimuth` and `elevation` radians in the camera's local frame, `radius`
    /// radians wide, as the game's window does ([`Sun::local`]): it follows the local up, so a
    /// camera that is carried over a Planet has a sun whose direction moves a little on every
    /// frame. That is not a change of the sun, and keeps the upscaler's history.
    pub fn set_local_sun(&mut self, azimuth: f32, elevation: f32, radius: f32) {
        if self.local_sun != Some((azimuth, elevation, radius)) {
            self.tracer.sun_changed();
        }
        self.local_sun = Some((azimuth, elevation, radius));
    }

    /// Starts the upscaler's history over, and the Reference's image: the next frame is the first
    /// of a view, whatever came before it.
    pub fn reset_accumulation(&mut self) {
        self.tracer.reset_accumulation();
    }

    /// Sets the light's palette (see [`Look`](crate::Look)): hero light B by default.
    pub fn set_look(&mut self, look: crate::look::Look) {
        self.tracer.set_look(look);
    }

    /// Sets the height of the Planet's ground along the up, in Voxels, which the atmosphere
    /// measures the camera's height from (see [`Tracer::set_ground_level`]): 0 by default, for a
    /// world that is not a Planet.
    pub fn set_ground_level(&mut self, voxels: f64) {
        self.tracer.set_ground_level(voxels);
    }

    /// Switches the stages of the art direction (see [`Stages`](crate::Stages)); all on by default.
    pub fn set_stages(&mut self, stages: crate::post::Stages) {
        self.tracer.set_stages(stages);
    }

    /// The light and display the renderer had before #202: [`Look::LEGACY`](crate::Look::LEGACY),
    /// no stage of the art direction ([`Stages::NONE`](crate::Stages::NONE)) and the old default
    /// sun (high, 0.04 rad), so that the lit image is the sRGB encoding of the light of the old
    /// constants. For the tests whose answers are worked out from those constants, and the bench's
    /// convergence phase, whose numbers are compared from report to report. A sun set afterwards
    /// with [`Offscreen::set_sun`] replaces the old one.
    pub fn use_legacy_light(&mut self) {
        self.set_look(crate::look::Look::LEGACY);
        self.set_stages(crate::post::Stages::NONE);
        self.set_sun(Sun::legacy());
    }

    /// Draws the sun and the sky alone, in both views: no GI and no emitters. Primary has no
    /// ambient term and no emitted light, and the Reference follows no path past its first
    /// surface and adds no emitted light (see [`Tracer::set_sun_and_sky_only`]). The bench's
    /// shadows view uses it (#52).
    pub fn set_sun_and_sky_only(&mut self, only: bool) {
        self.tracer.set_sun_and_sky_only(only);
    }

    /// Puts the flat ambient of before #54 in place of Primary's GI, or takes it out (see
    /// [`Tracer::set_flat_ambient`]). **An `Offscreen` starts with it in**: a caller that looks at
    /// GI (the bench, the look-dev sheet, the tests of the cache) calls `set_flat_ambient(false)`.
    /// The window's tracer has GI from the start.
    pub fn set_flat_ambient(&mut self, flat: bool) {
        self.tracer.set_flat_ambient(flat);
    }

    /// Sets how many shadow rays a pixel of Primary casts towards the sun's disc (see
    /// [`Tracer::set_shadow_rays`]). The bench's `--shadow-rays` uses it (#52).
    pub fn set_shadow_rays(&mut self, rays: u32) {
        self.tracer.set_shadow_rays(rays);
    }

    /// Chooses what is drawn: the lit image, or one of the debug views.
    pub fn set_debug_view(&mut self, view: DebugView) {
        self.tracer.set_debug_view(view);
    }

    /// The GPU time of each pass, as of the last frame rendered. Unlike in a window, a frame's
    /// times are in by the time it has been read back. Without timestamp queries there are
    /// none (see [`Offscreen::timers_supported`]).
    pub fn timings(&self) -> &Timings {
        self.profiler.timings()
    }

    /// Bytes of GPU memory that the Planet's bricks take.
    pub fn scene_bytes(&self) -> u64 {
        self.tracer.scene_bytes()
    }

    /// Bytes of the Planet written to the GPU so far, at the start and by each update.
    pub fn uploaded_bytes(&self) -> u64 {
        self.tracer.uploaded_bytes()
    }

    /// Bytes that the last [`Offscreen::update_scene`] copied on the GPU into buffers that grew
    /// (the host wrote none of them; they are not in [`Offscreen::uploaded_bytes`]).
    pub fn last_grown_bytes(&self) -> u64 {
        self.tracer.last_grown_bytes()
    }

    /// Copies that the last [`Offscreen::update_scene`] wrote into the scene's buffers, counted as
    /// they were made (see [`Offscreen::uploaded_bytes`]); 0 for a rebuild.
    pub fn last_writes(&self) -> u32 {
        self.tracer.last_writes()
    }

    /// How long the last [`Offscreen::update_scene`] took, on the host, to make the buffers that
    /// grew and record the copies into them. Zero if none grew.
    pub fn last_grow_time(&self) -> std::time::Duration {
        self.tracer.last_grow_time()
    }

    /// How many times each of the scene's four buffers has been replaced by a bigger one (see
    /// [`Tracer::buffer_generations`]).
    pub fn buffer_generations(&self) -> [u32; 4] {
        self.tracer.buffer_generations()
    }

    /// The most bytes of GPU memory the scene's buffers have taken at once while a buffer grew,
    /// the old buffer and the new one both counted ([`Tracer::peak_growth_bytes`]), and while a
    /// rebuilt scene was made ([`Tracer::peak_rebuild_bytes`]); each 0 if it has not happened.
    pub fn peak_growth_bytes(&self) -> u64 {
        self.tracer.peak_growth_bytes()
    }

    pub fn peak_rebuild_bytes(&self) -> u64 {
        self.tracer.peak_rebuild_bytes()
    }

    /// The most words a buffer of the scene may have on this device (see
    /// [`Scene::set_buffer_limit`]).
    pub fn buffer_limit_words(&self) -> usize {
        (self.device.limits().max_storage_buffer_binding_size / 4) as usize
    }

    /// The GPU that this renders on.
    pub fn gpu_info(&self) -> &GpuInfo {
        &self.info
    }

    /// Whether the device can time passes.
    pub fn timers_supported(&self) -> bool {
        self.profiler.supported()
    }

    /// The exposure, in stops (log2), that the display transform has kept for the Reference view
    /// (`reference`) or for Primary, or `None` if it has not metered one. For tests.
    pub fn exposure_stops(&mut self, reference: bool) -> Option<f32> {
        let view = if reference {
            crate::post::Exposed::Reference
        } else {
            crate::post::Exposed::Primary
        };
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let bytes = crate::read_buffer(
            &self.device,
            &self.queue,
            encoder,
            self.tracer.exposure_state(view),
            16,
        );
        let word = |i: usize| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        (word(2) != 0.0).then(|| word(0))
    }

    /// Primary's last frame as linear light before the display transform, and the bloom the
    /// transform made of it at a quarter of the size: both as RGB, rows from the top, with their
    /// sizes. For tests of the bloom, which the 8-bit image shows only coarsely.
    #[allow(clippy::type_complexity)]
    pub fn read_hdr_and_bloom(&mut self) -> ((u32, u32, Vec<[f32; 3]>), (u32, u32, Vec<[f32; 3]>)) {
        let size = self.target.resolution().internal;
        let (hdr, (bloom, bloom_size)) = self.target.lit_hdr_and_bloom();
        let (hdr, bloom) = (hdr.texture.clone(), bloom.texture.clone());
        let read = |texture: &wgpu::Texture, size: (u32, u32)| {
            let encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            let bytes = read_texture(&self.device, &self.queue, encoder, texture, size, 8);
            let pixels = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|texel| {
                    std::array::from_fn(|c| {
                        half_to_f32(u16::from_le_bytes([texel[2 * c], texel[2 * c + 1]]))
                    })
                })
                .collect();
            (size.0, size.1, pixels)
        };
        (read(&hdr, size), read(&bloom, bloom_size))
    }

    /// A table of the atmosphere as it was last filled, as linear RGB, rows from the top: for tests.
    pub fn read_atmosphere_table(&mut self, id: crate::atmosphere::TableId) -> Vec<[f32; 3]> {
        let (texture, size) = self.tracer.atmosphere_tables().texture(id);
        let texture = texture.clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let bytes = read_texture(&self.device, &self.queue, encoder, &texture, size, 8);
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|texel| {
                std::array::from_fn(|c| {
                    half_to_f32(u16::from_le_bytes([texel[2 * c], texel[2 * c + 1]]))
                })
            })
            .collect()
    }

    /// The cumulative distribution of the sun-free sky's brightness, as the Reference samples it:
    /// the 32 rows' (up to and including each), then each row's 64 texels', then the total weight.
    /// For tests.
    pub fn read_sky_distribution(&mut self) -> Vec<f32> {
        let buffer = self.tracer.atmosphere_tables().cdf().clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let words = crate::atmosphere::SKY_CDF_WORDS;
        let bytes = crate::read_buffer(&self.device, &self.queue, encoder, &buffer, words * 4);
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| f32::from_le_bytes(*w))
            .collect()
    }

    /// The radiance cache's table as it stands, `GI_SLOT_WORDS` (12) words a slot, for tests of
    /// how the table is kept (`cache.wgsl` has the layout).
    pub fn read_radiance_cache(&mut self) -> Vec<u32> {
        let buffer = crate::tracer::radiance_cache_buffer(&self.tracer).clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let size = crate::tracer::GI_CACHE_SLOTS * 12 * 4;
        let bytes = crate::read_buffer(&self.device, &self.queue, encoder, &buffer, size);
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect()
    }

    /// Keeps every pass time from now on, for a report over many frames.
    pub fn set_keep_history(&mut self, keep: bool) {
        self.profiler.set_keep_history(keep);
    }

    /// Renders primary visibility from `camera` at the traced resolution and reads it back: one
    /// ray through the centre of each pixel, with nothing carried over from earlier frames and no
    /// upscaling. This is also what [`Offscreen::gbuffer`] then holds.
    pub fn render(&mut self, camera: &Camera) -> Image {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_primary(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun_for(self.local_sun, self.sun, camera),
            &self.target,
        );
        let size = self.target.resolution().internal;
        let texture = self.target.lit().texture.clone();
        self.read_image(encoder, &texture, size)
    }

    /// Renders one frame of what the window shows in primary mode, and reads back the image at
    /// the output resolution: primary visibility traced at the internal resolution, through a
    /// different point of each pixel every frame, upscaled together with the frames before. Call
    /// it every frame with the camera of that frame: a still camera converges. A new size or a
    /// debug view that is not the lit image or the history length starts the sequence over.
    pub fn render_upscaled(&mut self, camera: &Camera) -> Image {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_upscaled(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun_for(self.local_sun, self.sun, camera),
            &self.target,
        );
        let size = self.target.resolution().output;
        let texture = self.target.image().texture.clone();
        self.read_image(encoder, &texture, size)
    }

    /// Like [`Offscreen::render_upscaled`], without reading the image back: the frame is drawn and
    /// its pass times are in when this returns. For a run of frames that is timed and whose
    /// pictures are not looked at (the bench), where reading back every output image (15 MB at
    /// 1440p) would be most of the CPU time of a frame and leave the GPU idle between frames.
    pub fn draw_upscaled(&mut self, camera: &Camera) {
        self.draw_upscaled_timed(camera);
    }

    /// [`Offscreen::draw_upscaled`], and how long the CPU took to encode the frame and submit it,
    /// before it waited for the GPU: what the frame thread of a window spends on drawing (the wait
    /// is the GPU's time, which the window does not wait for on the CPU).
    pub fn draw_upscaled_timed(&mut self, camera: &Camera) -> std::time::Duration {
        let began = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_upscaled(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun_for(self.local_sun, self.sun, camera),
            &self.target,
        );
        self.profiler.resolve(&mut encoder);
        let commands = encoder.finish();
        let encoded = began.elapsed();
        // The uniforms of the next frame are written over these.
        self.queue.submit([commands]);
        let submitted = began.elapsed();
        self.profiler.collect(&self.device, true);
        // Without timers `collect` does not wait, and the frame is not over until it has.
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");
        let waited = began.elapsed() - submitted;
        self.last_passes = self
            .profiler
            .times_of_this_frame()
            .map(|frame| {
                let timings = self.profiler.timings();
                frame
                    .passes
                    .iter()
                    .map(|&(pass, ms)| (timings.passes()[pass].name.to_string(), ms))
                    .collect()
            })
            .unwrap_or_default();
        let gpu = self.profiler.times_of_this_frame().map(|frame| {
            let timings = self.profiler.timings();
            let named = |name: &str| {
                frame
                    .passes
                    .iter()
                    .find(|&&(pass, _)| timings.passes()[pass].name == name)
                    .map(|&(_, ms)| ms)
            };
            GpuPasses {
                primary_ms: named("primary"),
                upscale_ms: named("upscale"),
                total_ms: frame.total,
            }
        });
        self.last_draw = DrawTimes {
            encode: encoded,
            submit: submitted - encoded,
            wait: waited,
            gpu,
        };
        submitted
    }

    /// The G-buffer of the last frame of primary visibility ([`Offscreen::render`] or
    /// [`Offscreen::render_upscaled`]).
    pub fn gbuffer(&mut self) -> GBuffer {
        let (width, height) = self.target.resolution().internal;
        let read = |plane: &Plane, bytes: u32| {
            let encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            read_texture(
                &self.device,
                &self.queue,
                encoder,
                &plane.texture,
                (width, height),
                bytes,
            )
        };
        let words = read(self.target.gbuffer(), 8);
        let mut gbuffer = GBuffer {
            width,
            height,
            depth: Vec::new(),
            normal: Vec::new(),
            albedo: Vec::new(),
        };
        for texel in words.as_chunks::<8>().0 {
            let [d0, d1, d2, d3, r, g, b, face] = *texel;
            gbuffer.depth.push(f32::from_le_bytes([d0, d1, d2, d3]));
            gbuffer.albedo.extend_from_slice(&[r, g, b, 255]);
            gbuffer.normal.push(match face {
                0 => [0.0; 3],
                code => {
                    let mut n = [0.0; 3];
                    n[usize::from(code - 1) / 2] = if (code - 1) % 2 == 1 { 1.0 } else { -1.0 };
                    n
                }
            });
        }
        gbuffer
    }

    /// Adds `samples` samples of the reference path tracer to the image of `camera` and reads
    /// back the mean so far. The image starts over if `camera` or the sun is not the one of the
    /// previous call. With zero samples it just reads the image again.
    pub fn render_reference(&mut self, camera: &Camera, samples: u32) -> Image {
        for _ in 0..samples {
            self.reference_sample(camera);
        }
        let size = self.target.reference_size();
        let texture = self.target.reference().texture.clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.read_image(encoder, &texture, size)
    }

    /// Adds one sample of the reference path tracer to the image of `camera`, without reading
    /// anything back, and waits until the GPU has done it: the pass is timed (see
    /// [`Offscreen::timings`]) the same way as every sample of [`Offscreen::render_reference`].
    pub fn reference_sample(&mut self, camera: &Camera) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_reference(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun_for(self.local_sun, self.sun, camera),
            &self.target,
        );
        self.profiler.resolve(&mut encoder);
        // The uniforms of the next sample are written over these.
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
    }

    /// Shows the Reference view's mean so far again, under the current [`Stages`](crate::Stages)
    /// and with the exposure it has (held, not metered again), and reads it back. The mean is not
    /// touched: the tone curve is only a way of showing it, so one accumulation can be seen
    /// through every curve. `None` if the view has no samples (it has just started over), whose
    /// mean is not an image of this view.
    pub fn reshow_reference(&mut self) -> Option<Image> {
        if self.reference_samples() == 0 {
            return None;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_reference_post(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            &self.target,
            crate::post::REFERENCE_METERED_UP_TO,
        );
        self.profiler.resolve(&mut encoder);
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
        let size = self.target.reference_size();
        let texture = self.target.reference().texture.clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        Some(self.read_image(encoder, &texture, size))
    }

    /// The most samples one dispatch of the Reference view may take at the size it is traced at
    /// ([`crate::batch::max_samples`]).
    pub fn max_batch_samples(&self) -> u32 {
        self.target.max_batch()
    }

    /// Adds `samples` samples of the reference path tracer in **one dispatch**, without reading
    /// anything back, and waits until the GPU has done it. The accumulation is that of the same
    /// number of [`Offscreen::reference_sample`]s, bit for bit. Returns the samples taken, which
    /// is `samples` up to the cap ([`crate::batch::max_samples`]).
    pub fn reference_batch(&mut self, camera: &Camera, samples: u32) -> u32 {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        let taken = self.tracer.encode_reference_batch(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun_for(self.local_sun, self.sun, camera),
            &self.target,
            samples,
        );
        self.profiler.resolve(&mut encoder);
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
        taken
    }

    /// Draws one frame as a window in Reference mode (`reference`) or Primary mode does, by the
    /// still-frame policy of ADR 0005 (see [`Tracer::encode_frame`]): Primary until the view has
    /// been still for 2 frames, then a batch of the Reference view, which starts over from sample
    /// 0 whenever the camera, the sun, the size or the Planet change. `now` is the time of the
    /// frame on any one clock (it runs the fade). Nothing is read back; the frame is over when
    /// this returns, and its GPU time is in the result.
    pub fn draw_frame(
        &mut self,
        camera: &Camera,
        reference: bool,
        now: std::time::Duration,
    ) -> FrameDrawn {
        let began = std::time::Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        let plan = self.tracer.encode_frame(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &sun_for(self.local_sun, self.sun, camera),
            &self.target,
            reference,
            now,
        );
        // The copy to the "window", timed as the window's is, so that a frame costs what it
        // costs there.
        self.present(&mut encoder, plan.fade());
        self.profiler.resolve(&mut encoder);
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
        // Without timers `collect` does not wait, and the frame is not over until it has.
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");
        let wall = began.elapsed();
        let reference_ms = self.profiler.times_of_this_frame().and_then(|frame| {
            let timings = self.profiler.timings();
            frame
                .passes
                .iter()
                .find(|&&(pass, _)| timings.passes()[pass].name == "reference")
                .map(|&(_, ms)| ms)
        });
        FrameDrawn {
            plan,
            samples: self.tracer.samples(),
            reference_ms,
            wall,
        }
    }

    /// Records the copy that a window does after the passes of a frame, to a texture of its own:
    /// Primary's image, the Reference view's, or the one fading in over the other by `fade`. It
    /// reads the images and writes none of them, so it never changes the accumulation.
    fn present(&mut self, encoder: &mut wgpu::CommandEncoder, fade: f32) {
        let (width, height) = self.target.resolution().output;
        if self.presenter.is_none() {
            let composite = Composite::new(&self.device, crate::tracer::IMAGE_FORMAT);
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shown"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::tracer::IMAGE_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let group = composite.group(&self.device, self.target.image(), self.target.reference());
            self.presenter = Some(Presenter {
                composite,
                texture,
                group,
            });
        }
        let presenter = self.presenter.as_ref().expect("made above");
        presenter.composite.set_fade(&self.queue, fade);
        let view = presenter
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut pass = self.profiler.render_pass(
            encoder,
            "blit",
            &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
        );
        presenter.composite.draw(&mut pass, &presenter.group);
    }

    /// What the window would show for the last [`Offscreen::draw_frame`], at the output
    /// resolution: Primary's image, the Reference view's, or the one fading in over the other.
    ///
    /// # Panics
    /// If no frame has been drawn.
    pub fn shown(&mut self) -> Image {
        let size = self.target.resolution().output;
        let texture = self
            .presenter
            .as_ref()
            .expect("no frame has been drawn")
            .texture
            .clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let rgba = read_texture(&self.device, &self.queue, encoder, &texture, size, 4);
        Image {
            width: size.0,
            height: size.1,
            rgba,
        }
    }

    /// The image that Primary's last frame left for the copy pass (the upscaled image), at the
    /// output resolution.
    pub fn primary_image(&mut self) -> Image {
        let size = self.target.resolution().output;
        let texture = self.target.image().texture.clone();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let rgba = read_texture(&self.device, &self.queue, encoder, &texture, size, 4);
        Image {
            width: size.0,
            height: size.1,
            rgba,
        }
    }

    /// Sets the seed of the reference's random numbers (the default is
    /// [`crate::tracer::RNG_SEED`]) and starts its image over. Another seed gives an independent
    /// image of the same view, with the same expected value.
    pub fn set_reference_seed(&mut self, seed: u32) {
        self.tracer.set_seed(seed);
    }

    /// Builds the Reference view with `options` and starts its image over. For tests; the
    /// default is the player's.
    pub fn set_reference_options(&mut self, options: ReferenceOptions) {
        self.tracer.set_reference_options(&self.device, options);
    }

    /// The cuts of Still views since the renderer was made, by cause (#217): see
    /// [`crate::tracer::Tracer::cuts`].
    pub fn still_cuts(&self) -> crate::still::Cuts {
        self.tracer.cuts()
    }

    /// Samples in the reference image so far.
    pub fn reference_samples(&self) -> u32 {
        self.tracer.samples()
    }

    /// The reference's running mean as the accumulation buffer holds it, in linear light before
    /// the display encoding and the clamp to 0..1: RGB for each pixel of the traced image, rows
    /// from the top (`3 * width * height` floats).
    pub fn read_accumulation(&mut self) -> Vec<f32> {
        let (width, height) = self.target.reference_size();
        let size = u64::from(width) * u64::from(height) * 16;
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let bytes = crate::read_buffer(
            &self.device,
            &self.queue,
            encoder,
            self.target.accum(),
            size,
        );
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for texel in bytes.as_chunks::<16>().0 {
            for channel in texel[..12].as_chunks::<4>().0 {
                rgb.push(f32::from_le_bytes(*channel));
            }
        }
        rgb
    }

    /// Reads back `texture`, an image `(width, height)` big, after the work recorded in `encoder`,
    /// which ends the frame the profiler is timing (if any).
    fn read_image(
        &mut self,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        (width, height): (u32, u32),
    ) -> Image {
        self.profiler.resolve(&mut encoder);
        let rgba = read_texture(
            &self.device,
            &self.queue,
            encoder,
            texture,
            (width, height),
            4,
        );
        self.profiler.collect(&self.device, true);
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

/// The sun for a frame seen from `camera`: the local one if there is one.
fn sun_for(local: Option<(f32, f32, f32)>, sun: Sun, camera: &Camera) -> Sun {
    match local {
        Some((azimuth, elevation, radius)) => Sun::local(camera, azimuth, elevation, radius),
        None => sun,
    }
}
