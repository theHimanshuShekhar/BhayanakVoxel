//! The GPU side of tracing: the packed scene in buffers, and the compute pipelines over it.
//! Windowed and offscreen rendering share this, so the golden image test exercises the same
//! code path the window does.

use brick_store::BrickCoord;
use bytemuck::{Pod, Zeroable};
use materials::MaterialTable;
use wgpu::util::DeviceExt;

use crate::accumulation::Accumulator;
use crate::atmosphere::{AtmosphereTables, AtmosphereView};
use crate::batch::max_samples;
use crate::camera::{Camera, CameraUniform};
use crate::debug_view::DebugView;
use crate::emitters::{Applied, EmitterIndex, Emitters, emissive_materials};
use crate::gpu_materials;
use crate::look::{Look, LookUniform};
use crate::post::{self, Exposed, HDR_FORMAT, Metering, Post, Scratch, Stages};
use crate::profiler::Profiler;
use crate::reference::{Hit, Ray};
use crate::scene::{MAX_LEVELS, Part, REGION_WORDS, Scene, SceneUpdate, UploadCost, spare_words};
use crate::still::{Cuts, Plan, Still};
use crate::sun::{Sun, SunUniform};
use crate::upscale::{History, Resolution, UpscalePass};
use crate::{Result, shaders};

/// Format of the traced image that the upscaler and the copy to the window read: display-encoded
/// values (AgX-like by default; the true sRGB curve for Neutral and with the transform off), in a
/// plain unorm texture. The kernels draw linear light in [`HDR_FORMAT`]
/// images, and the display transform (`post.wgsl`) turns those into this.
pub const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Format of the G-buffer: two words a pixel, written in one store (see `primary.wgsl`). The
/// first is the distance to the hit, as the bits of an f32 (-1 for no hit). The second has the
/// albedo of the hit's surface in its first three bytes, encoded like the image, and the face
/// that was hit in the last (0 for no hit, else 1 + 2 * axis + 1 if it points along the positive
/// direction). The normals are along the axes, so a byte holds them exactly.
pub const GBUFFER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;

/// Format of what primary visibility writes beside the G-buffer: the absolute Voxel that was hit
/// (three i32s as bits) and its Material, four words a pixel, zeros where nothing was hit.
pub const SURFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Uint;

/// Format of the smooth normals (`shade.wgsl`): a unit vector, and 1 in w where there is one.
pub const NORMALS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The format of the sun's light at each hit (`sun_weights` in `shade.wgsl`): one float a pixel.
pub const SUN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

/// The format of the incident light that Primary's GI finds at each hit (`gi_light` in `gi.wgsl`):
/// linear light, three half floats a pixel (and one unused).
pub const GI_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The slots of Primary's radiance cache, a power of two, and the words of a slot (`cache.wgsl`
/// holds the layout; keep `CACHE_SLOTS` and `SLOT_WORDS` there in step): 24 MiB of storage
/// buffer for a table that a frame fills a tenth of at most.
pub const GI_CACHE_SLOTS: u64 = 1 << 19;
const GI_SLOT_WORDS: u64 = 12;

/// The frames after an Edit (or a change of the sun) in which the cache forgets faster, in the cells
/// of the boost's box (`frame.gi.y`, `frame.boost_min` and `boost_max`): half a second at 60 Hz,
/// which is the time that the lit result has to settle in after a dig.
const GI_BOOST_FRAMES: u32 = 30;
/// How far round the regions an Edit wrote the boost reaches, in Voxels: the light that an Edit lets
/// in or shuts out changes the cells about it most (two regions, 6.4 m).
const GI_BOOST_MARGIN: i32 = 128;
/// A box that holds every Voxel, for a change of the sun.
const EVERYWHERE: ([i32; 3], [i32; 3]) = ([i32::MIN / 2; 3], [i32::MAX / 2; 3]);

/// Format of the upscaler's history: encoded colour, and in alpha the frames it has been through.
pub const HISTORY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// `Level` in `trace.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LevelHeader {
    occupied_first: [i32; 3],
    table_mask: u32,
    occupied_end: [i32; 3],
    table_base: u32,
    hole_first: [i32; 3],
    pad0: u32,
    hole_end: [i32; 3],
    pad1: u32,
}

/// `SceneHeader` in `trace.wgsl`: the number of levels, then the first `level_count` of the
/// levels.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct SceneHeader {
    level_count: u32,
    bound_radius: f32,
    pad0: [u32; 2],
    bound_centre: [i32; 3],
    pad1: u32,
    levels: [LevelHeader; MAX_LEVELS],
}

impl SceneHeader {
    fn new(scene: &Scene) -> Self {
        let mut header = SceneHeader::zeroed();
        header.level_count = scene.walked_levels() as u32;
        if let (Some(bound), Some(radius)) = (scene.bound, scene.clip_radius()) {
            header.bound_radius = radius;
            header.bound_centre = bound.centre;
        }
        for (level, (slot, l)) in header.levels.iter_mut().zip(&scene.levels).enumerate() {
            let (hole_first, hole_end) = scene.hole(level);
            *slot = LevelHeader {
                occupied_first: l.occupied_first,
                table_mask: l.table_mask,
                occupied_end: l.occupied_end,
                table_base: l.table_base,
                hole_first,
                pad0: 0,
                hole_end,
                pad1: 0,
            };
        }
        header
    }
}

/// Seeds the reference path tracer's random numbers: a given view, sun and sample count always
/// give the same image.
pub const RNG_SEED: u32 = 0xB4A7_A9AC;

/// How many shadow rays a pixel of Primary casts towards the sun's disc by default (#52): the
/// owner's choice after the 4090 measured one ray's noise in the wide penumbrae at FLIP 0.074
/// against a target of 0.05. Two rays measured 0.062 there: what is left of the miss is mostly the
/// centre-cosine weight's bias, not noise (ADR 0005). The bench's `--shadow-rays` and
/// [`Tracer::set_shadow_rays`] change it.
pub const DEFAULT_SHADOW_RAYS: u32 = 2;

/// The most shadow rays a pixel can be asked for, for the bench's comparisons and not a setting for
/// a game. The shader's loop has no bound of its own: this is the clamp in
/// [`Tracer::set_shadow_rays`], which cuts a count to 1 through this.
pub const MAX_SHADOW_RAYS: u32 = 16;

/// How the Reference view is built. The default is the player's: Owen-scrambled Sobol sample
/// positions, the colour variation Primary has, and the estimator of ADR 0005. A test that needs
/// something else builds the pipeline with it (see [`Tracer::set_reference_options`]); nothing
/// else does, and the shader the player runs has no branch for any of it, because they are
/// pipeline constants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReferenceOptions {
    /// Every Voxel has its Material's base colour, with none of the variation Primary draws, so
    /// that a scene has an answer worked out from the Material table alone.
    pub flat_colours: bool,
    /// Hashed random numbers for the sample positions, in every dimension: white noise, the plain
    /// Monte Carlo baseline.
    pub white_noise: bool,
    /// A fault put into the estimator, to show that a test of it can fail.
    pub fault: Fault,
    /// The sun counts wherever it faces the surface, with no shadow ray: the estimator without
    /// its visibility. The bench's sun-and-sky view runs it beside the real one to find which
    /// pixels have a shadow's edge in them.
    pub no_shadow_rays: bool,
    /// How the atmosphere's sky is estimated (#53).
    pub sky: SkyEstimator,
    /// Haze on the bounce rays, as an upper bound of it: for the test that measures what ADR 0005's
    /// leaving it off is worth (#53). Never on in a view that is shown.
    pub bounce_haze: bool,
    /// How the emissive Voxels are estimated (#54).
    pub emitters: EmitterEstimator,
}

/// How the Reference estimates the light of emissive Voxels on a surface. The player's view is
/// [`EmitterEstimator::Both`]; the other two are for the test that shows that the weights of the two
/// estimators add up to 1, which is that all three have the same expected value (where every
/// emitter is in the list of every surface that sees it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmitterEstimator {
    /// One emitter chosen from the cell's list and a direction over the solid angle it covers, and
    /// the bounce ray that finds an emitter, weighed against each other (multiple importance
    /// sampling).
    #[default]
    Both,
    /// The bounce rays alone.
    BounceOnly,
    /// The emitters' own samples alone (an emitter that is not in the list of a surface is still
    /// found by the bounce ray that hits it).
    SamplesOnly,
}

/// How the Reference estimates the light of the atmosphere's sky on a surface. The player's view
/// is [`SkyEstimator::Both`]; the other two are for the test that shows that the weights of the two
/// estimators add up to 1, which is that all three have the same expected value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkyEstimator {
    /// A direction of the sky chosen in proportion to its brightness, and the bounce ray that
    /// escapes, weighed against each other (multiple importance sampling).
    #[default]
    Both,
    /// The bounce rays alone: the sky is what the ones that escape find.
    BounceOnly,
    /// The sky's own samples alone.
    SkySamplesOnly,
}

/// A fault that a test puts into the Reference view's estimator, to show that the tests that
/// guard it fail when it is wrong. The player's view has none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fault {
    #[default]
    None,
    /// The estimator that ADR 0005 replaced: no roulette, and a path stops after its fifth
    /// surface (the fixed 4-bounce cut).
    FourBounceCut,
    /// The roulette without the rescale of the paths that go on.
    NoRescale,
    /// A depth cap of 190 surfaces in place of 256.
    ShortCap,
    /// The sky counted in full by both of its estimators: the weights add up to 2, not 1.
    SkyDoubleCount,
    /// The emissive Voxels counted in full by both of theirs.
    EmitterDoubleCount,
}

impl ReferenceOptions {
    /// The pipeline constants these are, as `pathtrace.wgsl` declares them.
    fn constants(self) -> [(&'static str, f64); 7] {
        [
            ("NO_SHADOW_RAYS", f64::from(u8::from(self.no_shadow_rays))),
            ("FLAT_COLOURS", f64::from(u8::from(self.flat_colours))),
            ("WHITE_NOISE", f64::from(u8::from(self.white_noise))),
            (
                "FAULT",
                match self.fault {
                    Fault::None => 0.0,
                    Fault::FourBounceCut => 1.0,
                    Fault::NoRescale => 2.0,
                    Fault::ShortCap => 3.0,
                    Fault::SkyDoubleCount => 4.0,
                    Fault::EmitterDoubleCount => 5.0,
                },
            ),
            (
                "SKY_MODE",
                match self.sky {
                    SkyEstimator::Both => 0.0,
                    SkyEstimator::BounceOnly => 1.0,
                    SkyEstimator::SkySamplesOnly => 2.0,
                },
            ),
            ("BOUNCE_HAZE", f64::from(u8::from(self.bounce_haze))),
            (
                "EMITTER_MODE",
                match self.emitters {
                    EmitterEstimator::Both => 0.0,
                    EmitterEstimator::BounceOnly => 1.0,
                    EmitterEstimator::SamplesOnly => 2.0,
                },
            ),
        ]
    }
}

/// `Frame` in `common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    sun: SunUniform,
    /// The sample's index (a batch's first), the random seed, the debug view, then the number of
    /// samples in the batch (0 for a dispatch that has none, and for one is the same as 1).
    params: [u32; 4],
    /// Where in its pixel primary visibility traces, then two unused.
    jitter: [f32; 4],
    /// The local up of the camera (its `horizon_up`), that the sky is a gradient along, then one
    /// unused.
    look_up: [f32; 4],
    /// The light's palette (see [`Look`]).
    look: LookUniform,
    /// 1 if natural Materials are shaded with smooth computed normals; the end of the smooth range
    /// in Voxels and the face normal's weight in it (both an f32's bits); one unused.
    flags: [u32; 4],
    /// The frame of Primary's sun samples; whether the view has GI (0 no, 1 yes, 2 Primary's flat
    /// ambient, see [`Tracer::set_flat_ambient`]); 1 if it has emitted light; the number of shadow
    /// rays a pixel casts.
    light: [u32; 4],
    /// Primary's radiance cache: the frame's number, 1 if an Edit or the sun changed lately (in the
    /// cells of the box below), and two unused.
    gi: [u32; 4],
    /// The first Voxel of the box that the cache forgets faster in, and one past its last (the
    /// fourth word of each is unused).
    boost_min: [i32; 4],
    boost_max: [i32; 4],
}

/// Where primary visibility traces when nothing moves it: the centre of the pixel.
const CENTRE: [f32; 2] = [0.5; 2];

/// A texture and a view of all of it.
pub struct Plane {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl Plane {
    /// A 2D texture of `format` that kernels can write and read, and that can be copied from.
    pub(crate) fn new(
        device: &wgpu::Device,
        label: &str,
        (width, height): (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

/// A bind group entry for the whole of `plane`.
pub(crate) fn plane_entry(binding: u32, plane: &Plane) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(&plane.view),
    }
}

/// A bind group entry for the whole of `buffer`.
pub(crate) fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

/// A texture of `format` that a kernel writes.
pub(crate) fn storage_texture(format: wgpu::TextureFormat) -> wgpu::BindingType {
    wgpu::BindingType::StorageTexture {
        access: wgpu::StorageTextureAccess::WriteOnly,
        format,
        view_dimension: wgpu::TextureViewDimension::D2,
    }
}

/// What the kernels draw into.
///
/// At the traced (internal) resolution, primary visibility writes the G-buffer for the passes after
/// it (the distance to the hit, its face normal and its albedo) and the hit's Voxel and Material;
/// the smooth normals and the lit image, as linear light, are drawn from those; and the display
/// transform (`post.wgsl`) makes the encoded lit image, which the upscaler reads.
///
/// The reference path tracer draws an image of its own, with the float buffer that holds its
/// running mean, one `vec4<f32>` per pixel. Where that is traced is the target's *reference
/// size*: the traced resolution for [`Target::new`] (what the bench's flight times, and the tests
/// use: the cost of a sample is a unit that the budgets are written in), and the output
/// resolution for [`Target::with_full_reference`], which is how the window draws it (ADR 0005:
/// the Reference view traces at full output resolution, whatever Primary's render scale is). It
/// anti-aliases itself, so it needs no upscaling.
///
/// At the output resolution is the image that is presented, which the upscaler builds from the
/// lit image and from its own history (two images, which take turns).
pub struct Target {
    resolution: Resolution,
    /// The lit image as linear light, and the display transform's images for it.
    lit_hdr: Plane,
    lit_scratch: Scratch,
    lit: Plane,
    gbuffer: Plane,
    surface: Plane,
    normals: Plane,
    /// The sun's light at each hit (`shadows`), read by the lit image's pass.
    sun_weights: Plane,
    /// The light from the bounce, the sky and the emitters at each hit (`gi_trace`), read by the lit
    /// image's pass.
    gi_light: Plane,
    reference_size: (u32, u32),
    /// The reference path tracer's mean as linear light, and the display transform's images for it.
    reference_hdr: Plane,
    reference_scratch: Scratch,
    reference: Plane,
    accum: wgpu::Buffer,
    image: Plane,
    history: [Plane; 2],
}

impl Target {
    /// A target whose Reference view is traced at the traced (internal) resolution.
    pub fn new(device: &wgpu::Device, resolution: Resolution) -> Self {
        Self::build(device, resolution, resolution.internal)
    }

    /// A target whose Reference view is traced at the output resolution, as the window's is.
    pub fn with_full_reference(device: &wgpu::Device, resolution: Resolution) -> Self {
        Self::build(device, resolution, resolution.output)
    }

    fn build(device: &wgpu::Device, resolution: Resolution, reference_size: (u32, u32)) -> Self {
        let Resolution { internal, output } = resolution;
        let accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("accumulation"),
            size: u64::from(reference_size.0) * u64::from(reference_size.1) * 16,
            // COPY_SRC so that the bench's convergence phase can read the float mean back.
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        Self {
            resolution,
            lit_hdr: Plane::new(device, "lit image (linear)", internal, HDR_FORMAT),
            lit_scratch: Scratch::new(device, internal),
            lit: Plane::new(device, "lit image", internal, IMAGE_FORMAT),
            gbuffer: Plane::new(device, "G-buffer", internal, GBUFFER_FORMAT),
            surface: Plane::new(device, "surface", internal, SURFACE_FORMAT),
            normals: Plane::new(device, "smooth normals", internal, NORMALS_FORMAT),
            sun_weights: Plane::new(device, "sun weights", internal, SUN_FORMAT),
            gi_light: Plane::new(device, "gi light", internal, GI_FORMAT),
            reference_size,
            reference_hdr: Plane::new(
                device,
                "reference image (linear)",
                reference_size,
                HDR_FORMAT,
            ),
            reference_scratch: Scratch::new(device, reference_size),
            reference: Plane::new(device, "reference image", reference_size, IMAGE_FORMAT),
            accum,
            image: Plane::new(device, "upscaled image", output, IMAGE_FORMAT),
            history: [0, 1]
                .map(|i| Plane::new(device, &format!("history {i}"), output, HISTORY_FORMAT)),
        }
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// The lit image, drawn by primary visibility and the passes after it and encoded for display
    /// by the display transform, at the internal resolution.
    pub fn lit(&self) -> &Plane {
        &self.lit
    }

    /// Primary's lit image as linear light, before the display transform (`Rgba16Float`), and the
    /// bloom that the transform made of it, at a quarter of the size. For tests.
    pub(crate) fn lit_hdr_and_bloom(&self) -> (&Plane, (&Plane, (u32, u32))) {
        (&self.lit_hdr, self.lit_scratch.bloom())
    }

    /// The image of the reference path tracer: the mean so far, through the display transform and
    /// encoded for display, at [`Target::reference_size`].
    pub fn reference(&self) -> &Plane {
        &self.reference
    }

    /// The size the reference path tracer traces at, and of its image and its running mean.
    pub fn reference_size(&self) -> (u32, u32) {
        self.reference_size
    }

    /// The most samples one dispatch of the reference path tracer may take at this size
    /// ([`max_samples`]).
    pub fn max_batch(&self) -> u32 {
        max_samples(pixels(self.reference_size))
    }

    /// The image that the upscaler builds, at the output resolution.
    pub fn image(&self) -> &Plane {
        &self.image
    }

    /// The G-buffer (`GBUFFER_FORMAT`): the distance to the first hit, its albedo and the face of
    /// its normal.
    pub fn gbuffer(&self) -> &Plane {
        &self.gbuffer
    }

    /// The reference path tracer's running mean: one `vec4<f32>` a pixel (the fourth is 1), rows
    /// from the top, at the reference size.
    pub(crate) fn accum(&self) -> &wgpu::Buffer {
        &self.accum
    }

    /// The upscaler's history image `which` (0 or 1).
    pub(crate) fn history(&self, which: usize) -> &Plane {
        &self.history[which]
    }
}

/// Everything that decides what the reference image looks like, bar the scene (a new scene is a
/// new `Tracer`). Accumulation restarts when any of it changes.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    camera: Camera,
    sun: Sun,
    /// The size the reference image is traced at.
    size: (u32, u32),
}

impl View {
    /// The view of `camera` and `sun` into the reference image of `target`.
    pub(crate) fn new(camera: &Camera, sun: &Sun, target: &Target) -> Self {
        Self {
            camera: *camera,
            sun: *sun,
            size: target.reference_size,
        }
    }
}

pub struct Tracer {
    scene: SceneBuffers,
    scene_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    frame_buffer: wgpu::Buffer,
    /// The Material table (see [`gpu_materials`]), which nothing changes after the upload.
    materials: wgpu::Buffer,
    /// The emissive Voxels of the scene as lights (see [`crate::emitters`]): which Materials give
    /// off light, the emitters of the scene as they were last found, and their buffer.
    emissive: Vec<bool>,
    emitters: Emitters,
    emitter_buffer: wgpu::Buffer,
    camera_layout: wgpu::BindGroupLayout,
    primary: wgpu::ComputePipeline,
    reference: wgpu::ComputePipeline,
    /// The smooth normals, and the lit image drawn from the G-buffer and them (`shade.wgsl`), each
    /// with the layout of its own bind group.
    normals_layout: wgpu::BindGroupLayout,
    normals: wgpu::ComputePipeline,
    shadows_layout: wgpu::BindGroupLayout,
    shadows: wgpu::ComputePipeline,
    gi_layout: wgpu::BindGroupLayout,
    gi: wgpu::ComputePipeline,
    gi_resolve_layout: wgpu::BindGroupLayout,
    gi_resolve: wgpu::ComputePipeline,
    /// Primary's radiance cache (`cache.wgsl`): a hash table in world space of the light on the
    /// faces of the world, which `gi_trace` fills and reads.
    gi_cache: wgpu::Buffer,
    /// The number of the frame of Primary being drawn (from 1), which the cache ages by.
    gi_frame: u32,
    /// How many more frames the cache forgets fast in, after an Edit or the sun changed, and in
    /// which cells (the first Voxel and one past the last, of level 0).
    gi_boost: u32,
    /// Frames left in which the upscaler keeps a short history, after an Edit (not a new sun, which
    /// starts the history over anyway).
    edit_settling: u32,
    gi_boost_box: ([i32; 3], [i32; 3]),
    /// The emitters of level 0 by region, which an update of the scene takes in region by region.
    emitter_index: EmitterIndex,
    /// Whether Primary's light has the flat ambient of before #54 in place of its GI (see
    /// [`Tracer::set_flat_ambient`]).
    flat_ambient: bool,
    shade_layout: wgpu::BindGroupLayout,
    shade: wgpu::ComputePipeline,
    /// The display transform and the exposure it keeps from frame to frame.
    post: Post,
    /// Counts the sun's changes that were asked for ([`Tracer::sun_changed`]), and the count the
    /// upscaler's history has seen: a frame after a new one starts the history over, so that no
    /// shadow of the old sun is carried into the new (#52). A signal and not a comparison of the
    /// sun's value: the game's sun follows the camera's local up (`Sun::local`), so its direction
    /// moves a little in f32 on every frame the camera moves, and a comparison would reset the
    /// history of every frame of a walk.
    sun_epoch: u64,
    history_sun_epoch: u64,
    /// Whether the view has neither GI nor emitters (see [`Tracer::set_sun_and_sky_only`]).
    sun_and_sky_only: bool,
    /// The shadow rays a pixel casts (see [`Tracer::set_shadow_rays`]).
    shadow_rays: u32,
    /// The atmosphere's tables, and the height of the Planet's ground along the up: 0 in a world
    /// that is not a Planet (the camera's height is then its height above y = 0 along its
    /// up), the Planet's radius over the Planet ([`Tracer::set_ground_level`]).
    atmosphere: AtmosphereTables,
    ground_level: f64,
    /// The light's palette, and which stages of the art direction are on.
    look: Look,
    stages: Stages,
    /// The time of the last frame of [`Tracer::encode_frame`], for how far the exposure adapts.
    last_frame_at: Option<std::time::Duration>,
    /// Where the smooth normals end, in Voxels from the camera ([`smooth_end`]).
    smooth_end: f32,
    upscale: UpscalePass,
    /// Decides what each frame of [`Tracer::encode_frame`] draws.
    still: Still<(View, u64)>,
    /// Counts what changes the Planet or the images, which the key of `still` cannot see.
    scene_epoch: u64,
    /// The cuts of Still views so far (#217), and the last frame's view and epoch, to tell what a
    /// cut was of.
    cuts: Cuts,
    last_key: Option<(View, u64)>,
    history: History,
    accumulator: Accumulator<View>,
    /// The reference's random seed: [`RNG_SEED`] unless [`Tracer::set_seed`] changed it.
    seed: u32,
    debug_view: DebugView,
    /// Bytes of the Planet that have been written to the GPU, creating its buffers included.
    uploaded: u64,
    /// Copies that the last [`Tracer::update_scene`] made into the scene's buffers (each a copy on
    /// the GPU with a barrier of its own); 0 for a rebuild, which makes new buffers.
    last_writes: u32,
    /// Bytes of the buffers that the last [`Tracer::update_scene`] copied on the GPU into bigger
    /// ones (a buffer that grew), which the host wrote none of.
    last_grown: u64,
    /// How long the last [`Tracer::update_scene`] spent making the buffers that grew (making them
    /// and recording the copies), on the host.
    last_grow_time: std::time::Duration,
    /// The most bytes of GPU memory the scene's buffers have taken while a buffer grew: what they
    /// held before, and the new buffers made, with the old buffer and the new one both there while
    /// the copy into the bigger one is made. 0 if none has grown.
    peak_growth_bytes: u64,
    /// The same for a rebuild (new buffers for all of the scene, made while the old scene still
    /// exists): what the old held, and what the new does. 0 if there has been none.
    peak_rebuild_bytes: u64,
    /// The staging memory that patches of rings are written through: a few big chunks, handed back
    /// by the GPU and used again, instead of a staging buffer for each write.
    belt: wgpu::util::StagingBelt,
}

/// The size of a chunk of the staging belt, in bytes: a patch is written to a few of them.
const BELT_CHUNK: u64 = 4 << 20;

/// Where the emitters' buffer is bound, in group 1 of the kernels that sample them (`emitters` in
/// `emitters.wgsl`).
pub(crate) const EMITTERS_BINDING: u32 = 18;

/// The radiance cache's table, for tests (`Offscreen::read_radiance_cache`).
pub(crate) fn radiance_cache_buffer(tracer: &Tracer) -> &wgpu::Buffer {
    &tracer.gi_cache
}

/// Where the radiance cache is bound (`gi_cache` in `cache.wgsl`).
const GI_CACHE_BINDING: u32 = 19;

/// The GPU buffer for `emitters`.
fn emitter_buffer(device: &wgpu::Device, emitters: &Emitters) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("emitters"),
        contents: bytemuck::cast_slice(&emitters.words),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

pub(crate) fn layout_entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

pub(crate) fn buffer_binding(ty: wgpu::BufferBindingType) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

pub(crate) const STORAGE_READ: wgpu::BufferBindingType =
    wgpu::BufferBindingType::Storage { read_only: true };
const STORAGE_WRITE: wgpu::BufferBindingType =
    wgpu::BufferBindingType::Storage { read_only: false };

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    entry_point: &str,
    group_1: &wgpu::BindGroupLayout,
    group_0: &wgpu::BindGroupLayout,
    constants: &[(&str, f64)],
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(group_0), Some(group_1)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: &module,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions {
            constants,
            ..Default::default()
        },
        cache: None,
    })
}

/// What the scene's storage buffers are for: read by the walk, written to, and copied from when
/// one grows into a bigger buffer.
const STORAGE_USAGE: wgpu::BufferUsages = wgpu::BufferUsages::STORAGE
    .union(wgpu::BufferUsages::COPY_DST)
    .union(wgpu::BufferUsages::COPY_SRC);

/// A storage buffer holding `words`, with room for `spare` more words (as far as the device's
/// limit allows), padded to a non-empty size as bindings must be. Returns it and its size in
/// words. It can be written to with `COPY_DST`.
fn storage_buffer(
    device: &wgpu::Device,
    label: &str,
    words: &[u32],
    spare: usize,
) -> Result<(wgpu::Buffer, usize)> {
    let limit = device.limits().max_storage_buffer_binding_size;
    if words.len() as u64 * 4 > limit {
        return Err(format!(
            "{label} needs {} bytes, more than the device's {limit} limit",
            words.len() * 4
        )
        .into());
    }
    let capacity = (words.len() + spare).min((limit / 4) as usize).max(4);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: capacity as u64 * 4,
        usage: STORAGE_USAGE,
        mapped_at_creation: true,
    });
    {
        let mut view = buffer
            .get_mapped_range_mut(..)
            .expect("a buffer created mapped");
        view.slice(..words.len() * 4)
            .copy_from_slice(bytemuck::cast_slice(words));
    }
    buffer.unmap();
    Ok((buffer, capacity))
}

/// Writes `words` to `buffer`, starting at word `at`, and returns the bytes written.
fn write_words(queue: &wgpu::Queue, buffer: &wgpu::Buffer, words: &[u32], at: usize) -> u64 {
    queue.write_buffer(buffer, at as u64 * 4, bytemuck::cast_slice(words));
    words.len() as u64 * 4
}

/// The bytes that copying `update` to the GPU copy of `scene` writes, worked out on the CPU: what
/// [`Tracer::update_scene`] adds to [`Tracer::uploaded_bytes`], the same on every machine. A
/// rebuild writes all five buffers (the Planet's buffers have no room to spare for rings), and a
/// patch of rings the header, if the levels changed, and the words it lists. A `Patched` update is
/// counted as if it fits the buffers; it is not made for rings, which the bench streams.
pub fn upload_bytes(scene: &Scene, update: &SceneUpdate) -> u64 {
    upload_cost(scene, update).bytes
}

/// What copying `update` to the GPU copy of `scene` costs, worked out on the CPU: the bytes
/// ([`upload_bytes`]) and the copies, each a barrier of its own (0 for a rebuild, which makes new
/// buffers). A buffer that grew is not counted: the GPU copies what it had, and the host writes
/// none of it.
pub fn upload_cost(scene: &Scene, update: &SceneUpdate) -> UploadCost {
    let bytes_only = |bytes| UploadCost { bytes, writes: 0 };
    match update {
        SceneUpdate::Rebuilt => {
            let spare = |len: usize| {
                if scene.is_rings() {
                    0
                } else {
                    spare_words(len)
                }
            };
            let words = |len: usize, spare: usize| (len + spare).max(4) as u64 * 4;
            bytes_only(
                size_of::<SceneHeader>() as u64
                    + words(scene.table.len(), 0)
                    + words(scene.regions.len(), 0)
                    + words(scene.brick_refs.len(), spare(scene.brick_refs.len()))
                    + words(scene.bricks.len(), spare(scene.bricks.len())),
            )
        }
        SceneUpdate::Rings(patch) => patch.cost(),
        SceneUpdate::Patched {
            regions,
            brick_refs_from,
            bricks_from,
        } => {
            let tail = |words: &[u32], from: usize| words.len().saturating_sub(from) as u64 * 4;
            bytes_only(
                regions.len() as u64 * REGION_WORDS as u64 * 4
                    + tail(&scene.brick_refs, *brick_refs_from)
                    + tail(&scene.bricks, *bricks_from),
            )
        }
    }
}

/// The Planet's packed bricks on the GPU. The buffers of an ordinary scene have room to grow,
/// since an Edit appends to `brick_refs` and `bricks` (see [`Scene::update`]); those of rings
/// are as big as the scene's, and one that the scene makes longer is made bigger on its own
/// ([`SceneBuffers::grow`]).
struct SceneBuffers {
    header: wgpu::Buffer,
    table: wgpu::Buffer,
    regions: wgpu::Buffer,
    brick_refs: wgpu::Buffer,
    bricks: wgpu::Buffer,
    /// Sizes of `table`, `regions`, `brick_refs` and `bricks`, in words.
    capacity: [usize; 4],
    /// Bytes of GPU memory in the five buffers.
    bytes: u64,
    /// How many times each of `table`, `regions`, `brick_refs` and `bricks` has been replaced by a
    /// bigger buffer: a buffer that did not grow has the generation it had. **A full
    /// [`SceneBuffers::new`] (a rebuild) makes all of them again and starts at 0**: a count of
    /// growths is only meaningful between rebuilds.
    generation: [u32; 4],
    group: wgpu::BindGroup,
}

impl SceneBuffers {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, scene: &Scene) -> Result<Self> {
        let header = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene header"),
            contents: bytemuck::bytes_of(&SceneHeader::new(scene)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let spare = |len| {
            if scene.is_rings() {
                0
            } else {
                spare_words(len)
            }
        };
        let (table, table_capacity) = storage_buffer(device, "region table", &scene.table, 0)?;
        let (regions, regions_capacity) = storage_buffer(device, "regions", &scene.regions, 0)?;
        let (brick_refs, refs_capacity) = storage_buffer(
            device,
            "brick refs",
            &scene.brick_refs,
            spare(scene.brick_refs.len()),
        )?;
        let (bricks, bricks_capacity) =
            storage_buffer(device, "bricks", &scene.bricks, spare(scene.bricks.len()))?;
        let mut buffers = Self {
            group: Self::bind(
                device,
                layout,
                &header,
                &table,
                &regions,
                &brick_refs,
                &bricks,
            ),
            header,
            table,
            regions,
            brick_refs,
            bricks,
            capacity: [
                table_capacity,
                regions_capacity,
                refs_capacity,
                bricks_capacity,
            ],
            bytes: 0,
            generation: [0; 4],
        };
        buffers.bytes = buffers.measure();
        Ok(buffers)
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        header: &wgpu::Buffer,
        table: &wgpu::Buffer,
        regions: &wgpu::Buffer,
        brick_refs: &wgpu::Buffer,
        bricks: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let entries: Vec<_> = [header, table, regions, brick_refs, bricks]
            .into_iter()
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout,
            entries: &entries,
        })
    }

    fn measure(&self) -> u64 {
        [
            &self.header,
            &self.table,
            &self.regions,
            &self.brick_refs,
            &self.bricks,
        ]
        .iter()
        .map(|buffer| buffer.size())
        .sum()
    }

    /// Makes each buffer that is shorter than the scene's (it grew, see [`RingPatch::sizes`]) a
    /// new one of the scene's length, and copies what it had into it with `encoder`: on the GPU,
    /// the host writes none of it. The buffers that did not grow are the same buffers. Returns the
    /// bytes copied; the bind group is made again if any buffer was replaced. `peak_extra` is
    /// added the bytes of the new buffers, which exist along with the old ones until the copy is
    /// done.
    fn grow(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        peak_extra: &mut u64,
    ) -> Result<u64> {
        let limit = device.limits().max_storage_buffer_binding_size;
        let mut copied = 0;
        for (i, (label, len)) in [
            ("region table", scene.table.len()),
            ("regions", scene.regions.len()),
            ("brick refs", scene.brick_refs.len()),
            ("bricks", scene.bricks.len()),
        ]
        .into_iter()
        .enumerate()
        {
            if len <= self.capacity[i] {
                continue;
            }
            if len as u64 * 4 > limit {
                return Err(format!(
                    "{label} needs {} bytes, more than the device's {limit} limit",
                    len * 4
                )
                .into());
            }
            let bigger = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: len as u64 * 4,
                usage: STORAGE_USAGE,
                mapped_at_creation: false,
            });
            let old = match i {
                0 => &mut self.table,
                1 => &mut self.regions,
                2 => &mut self.brick_refs,
                _ => &mut self.bricks,
            };
            encoder.copy_buffer_to_buffer(old, 0, &bigger, 0, old.size());
            copied += old.size();
            // Until the copy is done, which the GPU does after this is submitted, both exist.
            *peak_extra += bigger.size();
            *old = bigger;
            self.capacity[i] = len;
            self.generation[i] += 1;
        }
        if copied > 0 {
            self.group = Self::bind(
                device,
                layout,
                &self.header,
                &self.table,
                &self.regions,
                &self.brick_refs,
                &self.bricks,
            );
            self.bytes = self.measure();
        }
        Ok(copied)
    }
}

impl Tracer {
    /// Uploads `scene` and the built-in Material table, and builds the pipelines.
    pub fn new(device: &wgpu::Device, scene: &Scene) -> Result<Self> {
        Self::with_materials(device, scene, MaterialTable::built_in())
    }

    /// Like [`Tracer::new`], with the Material table `table`: for tests of lighting that need
    /// Materials of an albedo or an emission that the shipped table does not have.
    pub fn with_materials(
        device: &wgpu::Device,
        scene: &Scene,
        table: &MaterialTable,
    ) -> Result<Self> {
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(1, buffer_binding(STORAGE_READ)),
                layout_entry(2, buffer_binding(STORAGE_READ)),
                layout_entry(3, buffer_binding(STORAGE_READ)),
                layout_entry(4, buffer_binding(STORAGE_READ)),
            ],
        });
        let scene_buffers = SceneBuffers::new(device, &scene_layout, scene)?;
        let scene_bytes = scene_buffers.bytes;
        let belt = wgpu::util::StagingBelt::new(device.clone(), BELT_CHUNK);

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera, image, accumulation, materials and G-buffer"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(1, storage_texture(HDR_FORMAT)),
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(
                    3,
                    buffer_binding(wgpu::BufferBindingType::Storage { read_only: false }),
                ),
                layout_entry(4, buffer_binding(STORAGE_READ)),
                layout_entry(5, storage_texture(GBUFFER_FORMAT)),
                layout_entry(6, storage_texture(SURFACE_FORMAT)),
                layout_entry(EMITTERS_BINDING, buffer_binding(STORAGE_READ)),
            ]
            .into_iter()
            .chain(AtmosphereTables::layout_entries())
            .collect::<Vec<_>>(),
        });
        let texture = |binding, sample_type| {
            layout_entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let uint = wgpu::TextureSampleType::Uint;
        let normals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("smooth normals"),
            entries: &[
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(4, buffer_binding(STORAGE_READ)),
                texture(5, uint),
                texture(6, uint),
                layout_entry(7, storage_texture(NORMALS_FORMAT)),
            ],
        });
        let loaded = wgpu::TextureSampleType::Float { filterable: false };
        let shadows_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sun shadows"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                texture(5, uint),
                texture(6, uint),
                texture(8, loaded),
                layout_entry(9, storage_texture(SUN_FORMAT)),
            ],
        });
        let gi_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gi"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(4, buffer_binding(STORAGE_READ)),
                texture(5, uint),
                texture(6, uint),
                texture(8, loaded),
                texture(10, loaded),
                layout_entry(EMITTERS_BINDING, buffer_binding(STORAGE_READ)),
                layout_entry(GI_CACHE_BINDING, buffer_binding(STORAGE_WRITE)),
                layout_entry(20, storage_texture(GI_FORMAT)),
            ]
            .into_iter()
            .chain(AtmosphereTables::layout_entries())
            .collect::<Vec<_>>(),
        });
        let gi_resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gi resolve"),
            entries: &[
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(GI_CACHE_BINDING, buffer_binding(STORAGE_WRITE)),
            ],
        });
        let shade_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shade"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(1, storage_texture(HDR_FORMAT)),
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(4, buffer_binding(STORAGE_READ)),
                texture(5, uint),
                texture(6, uint),
                texture(10, loaded),
                layout_entry(GI_CACHE_BINDING, buffer_binding(STORAGE_WRITE)),
                texture(21, loaded),
            ]
            .into_iter()
            .chain(AtmosphereTables::layout_entries())
            .collect::<Vec<_>>(),
        });
        let materials = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("materials"),
            contents: bytemuck::cast_slice(&gpu_materials::pack(table)),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame"),
            size: size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let primary = compute_pipeline(
            device,
            "primary",
            shaders::PRIMARY,
            "primary",
            &camera_layout,
            &scene_layout,
            &[],
        );
        let reference = compute_pipeline(
            device,
            "reference",
            shaders::PATHTRACE,
            "reference",
            &camera_layout,
            &scene_layout,
            &ReferenceOptions::default().constants(),
        );
        let normals = compute_pipeline(
            device,
            "normals",
            shaders::SHADE,
            "normals",
            &normals_layout,
            &scene_layout,
            &[],
        );
        let shadows = compute_pipeline(
            device,
            "shadows",
            shaders::SHADE,
            "shadows",
            &shadows_layout,
            &scene_layout,
            &[],
        );
        let gi = compute_pipeline(
            device,
            "gi",
            shaders::SHADE,
            "gi_trace",
            &gi_layout,
            &scene_layout,
            &[],
        );
        let gi_resolve = compute_pipeline(
            device,
            "gi resolve",
            shaders::SHADE,
            "gi_resolve",
            &gi_resolve_layout,
            &scene_layout,
            &[],
        );
        let gi_cache = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("radiance cache"),
            size: GI_CACHE_SLOTS * GI_SLOT_WORDS * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let shade = compute_pipeline(
            device,
            "shade",
            shaders::SHADE,
            "shade_pass",
            &shade_layout,
            &scene_layout,
            &[],
        );
        let emissive = emissive_materials(table);
        let emitter_index = EmitterIndex::scan(scene, &emissive);
        let emitters = emitter_index.emitters();
        let emitter_buffer = emitter_buffer(device, &emitters);
        Ok(Self {
            scene: scene_buffers,
            scene_layout,
            camera_buffer,
            frame_buffer,
            materials,
            emissive,
            emitters,
            emitter_buffer,
            camera_layout,
            primary,
            reference,
            normals_layout,
            normals,
            shadows_layout,
            shadows,
            gi_layout,
            gi,
            gi_resolve_layout,
            gi_resolve,
            gi_cache,
            gi_frame: 0,
            gi_boost: 0,
            edit_settling: 0,
            gi_boost_box: EVERYWHERE,
            emitter_index,
            flat_ambient: false,
            shade_layout,
            shade,
            post: Post::new(device),
            sun_epoch: 0,
            history_sun_epoch: 0,
            sun_and_sky_only: false,
            shadow_rays: DEFAULT_SHADOW_RAYS,
            atmosphere: AtmosphereTables::new(device),
            ground_level: 0.0,
            look: Look::default(),
            stages: Stages::default(),
            last_frame_at: None,
            smooth_end: smooth_end(scene),
            upscale: UpscalePass::new(device),
            still: Still::new(),
            scene_epoch: 0,
            cuts: Cuts::default(),
            last_key: None,
            history: History::default(),
            accumulator: Accumulator::new(),
            seed: RNG_SEED,
            debug_view: DebugView::default(),
            uploaded: scene_bytes,
            last_writes: 0,
            last_grown: 0,
            last_grow_time: std::time::Duration::ZERO,
            peak_growth_bytes: 0,
            peak_rebuild_bytes: 0,
            belt,
        })
    }

    /// Finds the emitters of `scene` again, and makes a new buffer of them if they are not what the
    /// last buffer held. The scan reads the packed level 0 (a few thousand words in most scenes) and
    /// is made on every update of the scene, so a dig that opens an emitter to the room is seen by
    /// the next frame.
    fn update_emitters(&mut self, device: &wgpu::Device, scene: &Scene, update: &SceneUpdate) {
        let Applied {
            emitters_changed, ..
        } = self.emitter_index.apply(scene, &self.emissive, update);
        if emitters_changed {
            let emitters = self.emitter_index.emitters();
            if emitters != self.emitters {
                self.emitter_buffer = emitter_buffer(device, &emitters);
                self.emitters = emitters;
            }
        }
        // What an update takes in is what a scan of all of level 0 would find (a scan is the cost
        // that it avoids; this is checked where it is cheap, which is everywhere but a scene
        // of thousands of boxes).
        debug_assert!(
            self.emitters.boxes > 2048 || self.emitters == Emitters::of(scene, &self.emissive),
            "the emitters of an update are not those of the scene"
        );
    }

    /// The emitters of the scene as the shaders have them, for tests.
    pub fn emitters(&self) -> &Emitters {
        &self.emitters
    }

    /// Records the primary visibility pass: one ray per pixel of the lit image of `target`, through
    /// the centre of the pixel, from `camera`, lit by `sun`. Nothing carries over from one call
    /// to the next. The passes are timed by `profiler`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_primary(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
    ) {
        self.gi_frame += 1;
        self.write_uniforms(queue, camera, sun, (0, CENTRE, 0), 0);
        self.encode_atmosphere(
            device,
            queue,
            profiler,
            encoder,
            camera,
            sun,
            target.resolution.internal,
        );
        // Nothing carries over from one call to the next, the exposure included: it settles at
        // once, on this image. (The radiance cache does: it is the world's, not the image's.)
        self.dispatch_primary(device, queue, profiler, encoder, target, 1.0);
        self.gi_boost = self.gi_boost.saturating_sub(1);
    }

    /// Records a frame of primary visibility from `camera`, lit by `sun`: the primary pass, one ray
    /// per pixel of the lit image, through a different point of the pixel every frame; then the
    /// upscale pass, which builds the output image from it and from the frames before. Both
    /// passes are timed by `profiler`.
    ///
    /// The uniforms are written when this is called, so each frame must be submitted before the
    /// next is encoded.
    ///
    /// The history starts over for a new `Target`, a new size or scale, or a debug view; not for
    /// what changes in the scene. Ring re-centres, Edits and teleports are left to the clamp to
    /// the new samples and to history that reprojects off the screen: resetting on a ring move
    /// would wipe the history every frame while flying.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_upscaled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
    ) {
        let rate = post::adaptation_rate(post::NOMINAL_FRAME);
        self.encode_upscaled_adapting(device, queue, profiler, encoder, camera, sun, target, rate);
    }

    /// [`Tracer::encode_upscaled`], with the exposure moving `rate` of the way to its settled value.
    #[allow(clippy::too_many_arguments)]
    fn encode_upscaled_adapting(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
        rate: f32,
    ) {
        // A new sun starts the history over, whatever else is the same: the lit image's shadows
        // are the sun's, and nothing of the old sun's is to be carried into the new one.
        if self.history_sun_epoch != self.sun_epoch {
            self.history.reset();
            self.history_sun_epoch = self.sun_epoch;
        }
        let mut frame = self
            .history
            .begin(target.resolution(), camera, self.debug_view);
        frame.light_settling(self.edit_settling > 0);
        self.gi_frame += 1;
        self.write_uniforms(
            queue,
            camera,
            sun,
            (0, frame.jitter(), 0),
            frame.sun_sample(),
        );
        self.encode_atmosphere(
            device,
            queue,
            profiler,
            encoder,
            camera,
            sun,
            target.resolution.internal,
        );
        self.dispatch_primary(device, queue, profiler, encoder, target, rate);
        self.gi_boost = self.gi_boost.saturating_sub(1);
        self.edit_settling = self.edit_settling.saturating_sub(1);
        self.upscale
            .encode(device, queue, profiler, encoder, &frame, target);
    }

    /// Records one more sample of the reference path tracer for every pixel of the reference image
    /// of `target`, added to the running mean there. If the camera, the sun or the size of
    /// `target` differs from the last call, the mean starts over.
    ///
    /// The uniforms are written when this is called, so each sample must be submitted before the
    /// next is encoded. The pass is timed by `profiler`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_reference(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
    ) {
        self.encode_reference_batch(device, queue, profiler, encoder, camera, sun, target, 1);
    }

    /// Records `samples` more samples of the reference path tracer in **one dispatch**, for every
    /// pixel of the reference image of `target`: each pixel takes them one after the other and
    /// adds them to the running mean, which is written once. The accumulation is what the same
    /// number of single samples ([`Tracer::encode_reference`]) would have made, bit for bit.
    /// Returns the samples taken: `samples`, but at least 1 and at most
    /// [`max_samples`] of the image, the hard cap that keeps one dispatch far under the driver's
    /// timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_reference_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
        samples: u32,
    ) -> u32 {
        let samples = batch_of(samples, target.reference_size);
        let view = View::new(camera, sun, target);
        let first = self.accumulator.begin_batch(&view, samples);
        self.write_uniforms(queue, camera, sun, (first, CENTRE, samples), 0);
        self.encode_atmosphere(
            device,
            queue,
            profiler,
            encoder,
            camera,
            sun,
            target.reference_size,
        );
        self.dispatch(
            device,
            profiler,
            encoder,
            "reference",
            &self.reference,
            target.reference_size,
            &target.reference_hdr.view,
            target,
        );
        self.encode_reference_post(device, queue, profiler, encoder, target, first);
        samples
    }

    /// The Reference view's mean through the display transform, with an exposure of its own. A still
    /// view has nothing to adapt to, so the exposure is set at once to what the mean's boxes
    /// meter, until the view has enough samples for that to be steady (`first` is the index of the
    /// batch's first sample); from then on it is held, and the image converges at one brightness
    /// (the issue: Reference frames use the settled exposure). The accumulation is read, never
    /// written, so this can be run again on the same mean, under other [`Stages`].
    pub fn encode_reference_post(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        first: u32,
    ) {
        let metering = if first < post::REFERENCE_METERED_UP_TO {
            Metering::Adapt(1.0)
        } else {
            Metering::Hold
        };
        self.post.encode(
            device,
            queue,
            profiler,
            encoder,
            &target.reference_hdr,
            &target.reference_scratch,
            &target.reference,
            self.stages,
            // The Reference has no cache: it shows its lit image for that view.
            !matches!(self.debug_view, DebugView::Lit | DebugView::CacheCells),
            Exposed::Reference,
            metering,
        );
    }

    /// Records a frame of a window: the still-frame policy of ADR 0005 (see [`Still`]).
    ///
    /// With `reference` false (Primary mode) it is primary visibility, upscaled. With it true, the
    /// frame is primary visibility too until the view has been still for
    /// [`STILL_FRAMES`](crate::still::STILL_FRAMES) frames, and then a batch of the Reference view,
    /// as many samples in one dispatch as fit the budget ([`crate::batch`]), with the Reference
    /// view starting from sample 0. Any change of the camera, the sun, the size of the image or
    /// the Planet (an Edit, bricks arriving) cuts to primary visibility the same frame.
    ///
    /// `now` is the time on any clock that does not run backwards, for the fade. Returns what was
    /// drawn, for the pass that copies the images to the window to mix them by `fade`.
    ///
    /// The uniforms are written when this is called, so each frame must be submitted before the
    /// next is encoded. The Reference pass is timed by `profiler`, and what the timers say of
    /// earlier frames' batches is taken in here.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
        reference: bool,
        now: std::time::Duration,
    ) -> Plan {
        for (tag, ms) in profiler.take_tagged("reference") {
            self.still.timed(tag, ms);
        }
        // How far the exposure adapts this frame: by the time since the last one.
        let dt = self.last_frame_at.map_or(post::NOMINAL_FRAME, |last| {
            now.saturating_sub(last).as_secs_f32().min(1.0)
        });
        self.last_frame_at = Some(now);
        let key = (View::new(camera, sun, target), self.scene_epoch);
        let was_showing = matches!(self.still.last_plan(), Plan::Reference { .. });
        let plan = self
            .still
            .plan(&key, reference, now, pixels(target.reference_size));
        if was_showing && reference && matches!(plan, Plan::Primary) {
            // A cut, by what changed since the frame before: the view first, so that a camera
            // that moved in the frame of a patch is not counted against the patch.
            match &self.last_key {
                Some((view, _)) if *view != key.0 => self.cuts.view += 1,
                _ => self.cuts.scene += 1,
            }
        }
        self.last_key = Some(key);
        match plan {
            Plan::Primary => {
                self.encode_upscaled_adapting(
                    device,
                    queue,
                    profiler,
                    encoder,
                    camera,
                    sun,
                    target,
                    post::adaptation_rate(dt),
                );
                // Whatever the Reference view had is of a view that has gone: it starts again
                // from sample 0 when the view is next still, not from where it was.
                self.accumulator.reset();
            }
            Plan::Reference { samples, .. } => {
                profiler.tag_frame(self.still.tag());
                self.encode_reference_batch(
                    device, queue, profiler, encoder, camera, sun, target, samples,
                );
            }
        }
        plan
    }

    /// Brings the GPU copy of the Planet up to date with `scene`, which `update` describes the
    /// change to, and starts the reference image over. Only what changed is written, unless it
    /// no longer fits in the buffers, when all of `scene` is uploaded again.
    pub fn update_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        update: &SceneUpdate,
    ) -> Result<()> {
        self.restart_reference();
        self.smooth_end = smooth_end(scene);
        self.update_emitters(device, scene, update);
        // Counted where each write is made, not summed from the patch's cost: [`upload_cost`] is
        // the other count, and the two are compared. Both go over `RingPatch::copies()`, so the
        // comparison does not check that list (the stepped tests' `GpuCopy::assert_is` does).
        let mut written = 0;
        let mut writes = 0;
        self.last_writes = 0;
        self.last_grown = 0;
        self.last_grow_time = std::time::Duration::ZERO;
        match update {
            SceneUpdate::Rebuilt => {
                let before = self.scene.bytes;
                self.scene = SceneBuffers::new(device, &self.scene_layout, scene)?;
                self.peak_rebuild_bytes = self.peak_rebuild_bytes.max(before + self.scene.bytes);
                self.uploaded += self.scene.bytes;
                return Ok(());
            }
            SceneUpdate::Rings(patch) => {
                // What is written goes through the belt into one command buffer, which is sent at
                // once: after the buffers that grew are made, before the frame that draws.
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("scene upload"),
                });
                let grown_at = std::time::Instant::now();
                let mut extra = 0;
                let before = self.scene.bytes;
                self.last_grown =
                    self.scene
                        .grow(device, &self.scene_layout, &mut encoder, scene, &mut extra)?;
                self.last_grow_time = grown_at.elapsed();
                // Only a growth has a peak: with none, `before` is just the scene's size.
                if extra > 0 {
                    self.peak_growth_bytes = self.peak_growth_bytes.max(before + extra);
                }
                let buffers = &self.scene;
                if patch.levels {
                    let header = SceneHeader::new(scene);
                    let bytes = bytemuck::bytes_of(&header);
                    let size = wgpu::BufferSize::new(bytes.len() as u64).expect("a header");
                    self.belt
                        .write_buffer(&mut encoder, &buffers.header, 0, size)
                        .slice(..)
                        .copy_from_slice(bytes);
                    written += bytes.len() as u64;
                    writes += 1;
                }
                // Each copy is a barrier on the GPU, and a patch is mostly small writes that are
                // near each other, so they are made as a few long ones (see
                // `RingPatch::copies`).
                for (part, range) in patch.copies() {
                    let (buffer, words) = match part {
                        Part::Table => (&buffers.table, &scene.table),
                        Part::Regions => (&buffers.regions, &scene.regions),
                        Part::BrickRefs => (&buffers.brick_refs, &scene.brick_refs),
                        Part::Bricks => (&buffers.bricks, &scene.bricks),
                    };
                    let bytes: &[u8] = bytemuck::cast_slice(&words[range.clone()]);
                    let size = wgpu::BufferSize::new(bytes.len() as u64).expect("a range");
                    self.belt
                        .write_buffer(&mut encoder, buffer, range.start as u64 * 4, size)
                        .slice(..)
                        .copy_from_slice(bytes);
                    written += bytes.len() as u64;
                    writes += 1;
                }
                self.belt.finish();
                queue.submit(std::iter::once(encoder.finish()));
                self.belt.recall();
            }
            SceneUpdate::Patched {
                regions,
                brick_refs_from,
                bricks_from,
            } => {
                let buffers = &self.scene;
                if scene.brick_refs.len() > buffers.capacity[2]
                    || scene.bricks.len() > buffers.capacity[3]
                {
                    let before = self.scene.bytes;
                    self.scene = SceneBuffers::new(device, &self.scene_layout, scene)?;
                    self.peak_rebuild_bytes =
                        self.peak_rebuild_bytes.max(before + self.scene.bytes);
                    self.uploaded += self.scene.bytes;
                    return Ok(());
                }
                for &region in regions {
                    let at = region * REGION_WORDS;
                    writes += 1;
                    written += write_words(
                        queue,
                        &buffers.regions,
                        &scene.regions[at..][..REGION_WORDS],
                        at,
                    );
                }
                for (buffer, words, from) in [
                    (&buffers.brick_refs, &scene.brick_refs, *brick_refs_from),
                    (&buffers.bricks, &scene.bricks, *bricks_from),
                ] {
                    if from < words.len() {
                        writes += 1;
                        written += write_words(queue, buffer, &words[from..], from);
                    }
                }
            }
        }
        self.uploaded += written;
        self.last_writes = writes;
        Ok(())
    }

    /// Copies that the last [`Tracer::update_scene`] wrote to the staging belt (the header, and each
    /// range of words), counted as they were written: each is a copy on the GPU with a barrier of
    /// its own. The copies of a growth, old buffer to new, are not in it. 0 after a rebuild, which
    /// makes new buffers.
    pub fn last_writes(&self) -> u32 {
        self.last_writes
    }

    /// Bytes that the last [`Tracer::update_scene`] copied on the GPU from buffers into bigger
    /// ones that grew: not written by the host, and not in [`Tracer::uploaded_bytes`].
    pub fn last_grown_bytes(&self) -> u64 {
        self.last_grown
    }

    /// How long the last [`Tracer::update_scene`] took to make the buffers that grew and to record
    /// the copies into them, on the host (the GPU's copy is not in it). Zero if nothing grew.
    pub fn last_grow_time(&self) -> std::time::Duration {
        self.last_grow_time
    }

    /// How many times each of the scene's table, regions, brick refs and bricks buffers has been
    /// replaced by a bigger one. A buffer that did not grow is the same buffer, and its number
    /// does not change.
    pub fn buffer_generations(&self) -> [u32; 4] {
        self.scene.generation
    }

    /// The most bytes of GPU memory the scene's buffers have taken at once **while a buffer grew**
    /// since the tracer was made: what they held before, and the new buffers, the old buffer and
    /// the new one both there while the copy is made. 0 if none has grown. A rebuild is not in it
    /// ([`Tracer::peak_rebuild_bytes`]).
    pub fn peak_growth_bytes(&self) -> u64 {
        self.peak_growth_bytes
    }

    /// The most bytes of GPU memory the scene's buffers have taken at once **while a rebuilt scene
    /// was made** (the old scene and the new): 0 if there has been no rebuild.
    pub fn peak_rebuild_bytes(&self) -> u64 {
        self.peak_rebuild_bytes
    }

    /// Bytes of GPU memory that the Planet's bricks take: the five buffers of the scene.
    pub fn scene_bytes(&self) -> u64 {
        self.scene.bytes
    }

    /// Bytes of the Planet written to the GPU so far: what [`Tracer::new`] made its buffers
    /// from, and what each update wrote.
    pub fn uploaded_bytes(&self) -> u64 {
        self.uploaded
    }

    /// Starts the reference image and the upscaler's history over, for what the `View` cannot see:
    /// a new `Target`, whose buffer and history are empty whatever the count says.
    pub fn reset_accumulation(&mut self) {
        self.restart_reference();
        self.history.reset();
    }

    pub fn debug_view(&self) -> DebugView {
        self.debug_view
    }

    /// Chooses what the kernels draw (see [`DebugView`]). The reference image keeps
    /// accumulating whichever is chosen.
    pub fn set_debug_view(&mut self, view: DebugView) {
        self.debug_view = view;
    }

    /// The buffer that holds the exposure of `view`: see [`Post::state`].
    pub(crate) fn exposure_state(&self, view: Exposed) -> &wgpu::Buffer {
        self.post.state(view)
    }

    pub fn look(&self) -> Look {
        self.look
    }

    /// The atmosphere's tables, for tests that read them back.
    pub(crate) fn atmosphere_tables(&self) -> &AtmosphereTables {
        &self.atmosphere
    }

    /// Sets the height of the Planet's ground along the up, in Voxels, which the atmosphere
    /// measures the camera's height from: the Planet's radius over the Planet. 0 (the default) for
    /// a world that is not a Planet, where the camera's height is its height above y = 0. The
    /// views start over, as the sky they see changes.
    pub fn set_ground_level(&mut self, voxels: f64) {
        if voxels != self.ground_level {
            self.ground_level = voxels;
            self.restart_reference();
            self.history.reset();
        }
    }

    /// The atmosphere's view of `camera` and `sun` on an image of `size` pixels.
    fn atmosphere_view(
        &self,
        camera: &Camera,
        sun: &Sun,
        (width, height): (u32, u32),
    ) -> AtmosphereView {
        let up = camera.horizon_up;
        let above: f64 = (0..3).map(|a| camera.position[a] * up[a]).sum();
        AtmosphereView {
            camera_height: (above - self.ground_level) / brick_store::VOXELS_PER_METRE,
            up: up.map(|c| c as f32),
            sun_direction: sun.direction(),
            aspect: width as f32 / height.max(1) as f32,
        }
    }

    /// Records the atmosphere's pass for the view of `camera` and `sun` on an image of `size`
    /// pixels, if the look has an atmosphere; the camera's uniform must have been written.
    #[allow(clippy::too_many_arguments)]
    fn encode_atmosphere(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        size: (u32, u32),
    ) {
        if let Some(air) = self.look.atmosphere {
            let view = self.atmosphere_view(camera, sun, size);
            self.atmosphere.encode(
                device,
                queue,
                profiler,
                encoder,
                &self.camera_buffer,
                &air,
                &view,
            );
        }
    }

    /// Says that the sun was changed (turned, or another one set), not that it moves with the
    /// camera: the next frame of Primary starts the upscaler's history over (#52).
    pub fn sun_changed(&mut self) {
        self.sun_epoch += 1;
        self.gi_boost = GI_BOOST_FRAMES;
        self.gi_boost_box = EVERYWHERE;
    }

    /// Says that an Edit changed `bricks` (and not the scene thread's streaming, which changes the
    /// world far from where anyone looks and must not make the cache forget): for
    /// [`GI_BOOST_FRAMES`] frames the cells within [`GI_BOOST_MARGIN`] Voxels of the bricks follow
    /// the new light faster, and the upscaler keeps a short history for as long (it takes the new
    /// light at a fourth of a pixel a frame and not a sixteenth). Called when the Edit is made, which is not when its patch reaches
    /// the GPU (the scene thread packs it a frame or more later): the cache forgets from the Edit.
    pub fn edit_made(&mut self, bricks: &[BrickCoord]) {
        let Some(first) = bricks.first() else {
            return;
        };
        let (mut lo, mut hi) = (first.0, first.0);
        for b in bricks {
            lo = std::array::from_fn(|a| lo[a].min(b.0[a]));
            hi = std::array::from_fn(|a| hi[a].max(b.0[a]));
        }
        let reach = (
            lo.map(|k| k * 8 - GI_BOOST_MARGIN),
            hi.map(|k| (k + 1) * 8 + GI_BOOST_MARGIN),
        );
        self.gi_boost_box = if self.gi_boost > 0 {
            // An Edit while another is settling: the cells of both.
            (
                std::array::from_fn(|a| self.gi_boost_box.0[a].min(reach.0[a])),
                std::array::from_fn(|a| self.gi_boost_box.1[a].max(reach.1[a])),
            )
        } else {
            reach
        };
        self.gi_boost = GI_BOOST_FRAMES;
        self.edit_settling = GI_BOOST_FRAMES;
    }

    /// Sets the light's palette (see [`Look`]). The sun's and the sky's light are part of what the
    /// views estimate, so the reference image starts over, and the upscaler's history with it.
    pub fn set_look(&mut self, look: Look) {
        if look != self.look {
            self.look = look;
            self.restart_reference();
            self.history.reset();
        }
    }

    pub fn stages(&self) -> Stages {
        self.stages
    }

    /// Switches the stages of the art direction (see [`Stages`]). The smooth normals are part of
    /// what is lit, so the reference image starts over when they change; the exposure, bloom and
    /// the tone curve only change how it is shown, which the accumulation never holds.
    pub fn set_stages(&mut self, stages: Stages) {
        if stages != self.stages {
            if stages.smooth_normals != self.stages.smooth_normals
                || stages.normal_bias != self.stages.normal_bias
            {
                self.restart_reference();
            }
            self.stages = stages;
            self.history.reset();
        }
    }

    /// Whether both views are of the sun and the sky alone, with no GI and no emitters: Primary has
    /// no ambient term (the stand-in for GI, ADR 0005's table) and no emitted light, and the
    /// Reference follows no path past its first surface and adds no emitted light. This is the
    /// bench's shadows view (#52), where what is left to compare is the sun's light and the sky. The
    /// Reference's image starts over, and the upscaler's history with it.
    pub fn set_sun_and_sky_only(&mut self, only: bool) {
        if only != self.sun_and_sky_only {
            self.sun_and_sky_only = only;
            self.restart_reference();
            self.history.reset();
        }
    }

    /// Puts the flat ambient of before #54 (0.25 of the surface's colour, on every lit surface) in
    /// place of Primary's GI. It is the bench's "before": a view's Primary-against-truth score with
    /// it is what the radiance cache must not be further from the Reference than. The Reference has
    /// its bounce light either way. The upscaler's history starts over.
    pub fn set_flat_ambient(&mut self, flat: bool) {
        if flat != self.flat_ambient {
            self.flat_ambient = flat;
            self.history.reset();
        }
    }

    /// Sets how many shadow rays a pixel casts towards the sun's disc (the default is
    /// [`DEFAULT_SHADOW_RAYS`]), from 1 to [`MAX_SHADOW_RAYS`]; more are cut to that. The rays of a
    /// pixel are consecutive samples of one sequence, so more rays are a finer penumbra and a
    /// longer `shadows` pass, and the upscaler's history starts over, as its samples change.
    pub fn set_shadow_rays(&mut self, rays: u32) {
        let rays = rays.clamp(1, MAX_SHADOW_RAYS);
        if rays != self.shadow_rays {
            self.shadow_rays = rays;
            self.history.reset();
        }
    }

    /// Sets the seed of the reference's random numbers (the default is [`RNG_SEED`]) and starts
    /// its image over: the same view, sample count and seed give the same image, and another seed
    /// gives an independent one of the same expected value.
    pub fn set_seed(&mut self, seed: u32) {
        self.seed = seed;
        self.restart_reference();
    }

    /// Builds the reference pipeline with `options` (see [`ReferenceOptions`]) and starts its
    /// image over. For tests.
    pub fn set_reference_options(&mut self, device: &wgpu::Device, options: ReferenceOptions) {
        self.reference = compute_pipeline(
            device,
            "reference",
            shaders::PATHTRACE,
            "reference",
            &self.camera_layout,
            &self.scene_layout,
            &options.constants(),
        );
        self.restart_reference();
    }

    /// Starts the reference image over for something that changed what it is of that the `View`
    /// cannot see (the Planet, the seed, the pipeline), and counts it for the still-frame policy:
    /// the view is not Still until it has been unchanged again.
    fn restart_reference(&mut self) {
        self.accumulator.reset();
        self.scene_epoch += 1;
    }

    /// The cuts of Still views since the tracer was made, by cause (see [`Cuts`]). A frame that
    /// leaves Reference mode is not counted: the player did that.
    pub fn cuts(&self) -> Cuts {
        self.cuts
    }

    /// Samples in the reference image so far.
    pub fn samples(&self) -> u32 {
        self.accumulator.samples()
    }

    /// Writes the camera and the frame's uniforms. `(sample_index, jitter, batch)` are the
    /// Reference's sample (a batch's first), where Primary traces in its pixel, and the samples in
    /// the batch; `sun_sample` is the frame that Primary's sun samples are of.
    fn write_uniforms(
        &self,
        queue: &wgpu::Queue,
        camera: &Camera,
        sun: &Sun,
        (sample_index, jitter, batch): (u32, [f32; 2], u32),
        sun_sample: u32,
    ) {
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&camera.uniform()),
        );
        let frame = FrameUniform {
            sun: sun.uniform(),
            params: [sample_index, self.seed, self.debug_view.index(), batch],
            jitter: [jitter[0], jitter[1], 0.0, 0.0],
            look_up: {
                let [x, y, z] = camera.horizon_up;
                [x as f32, y as f32, z as f32, 0.0]
            },
            look: {
                let mut look = self.look.uniform();
                // The sun's light is what the air lets through to the ground.
                if let Some(air) = self.look.atmosphere {
                    let view = self.atmosphere_view(camera, sun, (1, 1));
                    let [r, g, b] = air.sun_light_on_ground(view.mu_s());
                    look.sun = [r, g, b, 0.0];
                }
                look
            },
            flags: [
                u32::from(self.stages.smooth_normals),
                self.smooth_end.to_bits(),
                self.stages.normal_bias.weight().to_bits(),
                u32::from(self.look.atmosphere.is_some()),
            ],
            light: [
                sun_sample,
                // 0 without GI, 1 with the bounce and the cache, 2 with the flat ambient.
                if self.sun_and_sky_only {
                    0
                } else if self.flat_ambient {
                    2
                } else {
                    1
                },
                u32::from(!self.sun_and_sky_only),
                self.shadow_rays,
            ],
            gi: [self.gi_frame, u32::from(self.gi_boost > 0), 0, 0],
            boost_min: [
                self.gi_boost_box.0[0],
                self.gi_boost_box.0[1],
                self.gi_boost_box.0[2],
                0,
            ],
            boost_max: [
                self.gi_boost_box.1[0],
                self.gi_boost_box.1[1],
                self.gi_boost_box.1[2],
                0,
            ],
        };
        queue.write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&frame));
    }

    /// Primary's frame over every pixel of the lit image of `target`, at the internal resolution:
    /// primary visibility, then the smooth normals and the lit image drawn from what it found, then
    /// the display transform, which makes the encoded lit image. A debug view is drawn by primary
    /// visibility itself (the upscaler draws the history length one), and passes through the
    /// display transform as it is.
    fn dispatch_primary(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        rate: f32,
    ) {
        self.dispatch(
            device,
            profiler,
            encoder,
            "primary",
            &self.primary,
            target.resolution.internal,
            &target.lit_hdr.view,
            target,
        );
        // A debug view that primary visibility drew skips the lighting: it is the colours as they
        // are. The history length is the upscaler's, over the lit image; the cache's cells are
        // drawn by the lit image's pass, from the same GI as the lit image has.
        let drawn_early = !matches!(
            self.debug_view,
            DebugView::Lit | DebugView::HistoryLength | DebugView::CacheCells
        );
        let debug = !matches!(self.debug_view, DebugView::Lit | DebugView::HistoryLength);
        if !drawn_early {
            if self.stages.smooth_normals {
                self.dispatch_normals(device, profiler, encoder, target);
            }
            self.dispatch_shadows(device, profiler, encoder, target);
            self.dispatch_gi(device, profiler, encoder, target);
            self.dispatch_shade(device, profiler, encoder, target);
        }
        self.post.encode(
            device,
            queue,
            profiler,
            encoder,
            &target.lit_hdr,
            &target.lit_scratch,
            &target.lit,
            self.stages,
            debug,
            Exposed::Primary,
            Metering::Adapt(rate),
        );
    }

    /// The smooth normal of every hit of the G-buffer (`normals` in `shade.wgsl`).
    fn dispatch_normals(
        &self,
        device: &wgpu::Device,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
    ) {
        let view = plane_entry;
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("smooth normals"),
            layout: &self.normals_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.materials.as_entire_binding(),
                },
                view(5, &target.gbuffer),
                view(6, &target.surface),
                view(7, &target.normals),
            ],
        });
        let (width, height) = target.resolution.internal;
        let mut pass = profiler.compute_pass(encoder, "normals");
        pass.set_pipeline(&self.normals);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }

    /// The sun's light at every hit of the G-buffer: the shadow rays of each pixel
    /// (`shadows` in `shade.wgsl`), from the geometric face, towards points of the sun's disc.
    fn dispatch_shadows(
        &self,
        device: &wgpu::Device,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
    ) {
        let view = plane_entry;
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sun shadows"),
            layout: &self.shadows_layout,
            entries: &[
                buffer_entry(0, &self.camera_buffer),
                buffer_entry(2, &self.frame_buffer),
                view(5, &target.gbuffer),
                view(6, &target.surface),
                view(8, &target.normals),
                view(9, &target.sun_weights),
            ],
        });
        let (width, height) = target.resolution.internal;
        let mut pass = profiler.compute_pass(encoder, "shadows");
        pass.set_pipeline(&self.shadows);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }

    /// The light from the bounce, the sky and the emitters at every hit of the G-buffer (`gi_trace`
    /// in `gi.wgsl`), and the radiance cache's fold of the frame's samples (`gi_resolve` in
    /// `cache.wgsl`), in one timed pass called `gi`.
    fn dispatch_gi(
        &self,
        device: &wgpu::Device,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
    ) {
        let view = plane_entry;
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gi"),
            layout: &self.gi_layout,
            entries: &[
                buffer_entry(0, &self.camera_buffer),
                buffer_entry(2, &self.frame_buffer),
                buffer_entry(4, &self.materials),
                view(5, &target.gbuffer),
                view(6, &target.surface),
                view(8, &target.normals),
                view(10, &target.sun_weights),
                buffer_entry(EMITTERS_BINDING, &self.emitter_buffer),
                buffer_entry(GI_CACHE_BINDING, &self.gi_cache),
                view(20, &target.gi_light),
            ]
            .into_iter()
            .chain(self.atmosphere.entries())
            .collect::<Vec<_>>(),
        });
        let resolve = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gi resolve"),
            layout: &self.gi_resolve_layout,
            entries: &[
                buffer_entry(2, &self.frame_buffer),
                buffer_entry(GI_CACHE_BINDING, &self.gi_cache),
            ],
        });
        let (width, height) = target.resolution.internal;
        let mut pass = profiler.compute_pass(encoder, "gi");
        pass.set_pipeline(&self.gi);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        pass.set_pipeline(&self.gi_resolve);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &resolve, &[]);
        pass.dispatch_workgroups(GI_CACHE_SLOTS.div_ceil(64) as u32, 1, 1);
    }

    /// The lit image as linear light, from the G-buffer and the sun's light at each hit
    /// (`shade_pass` in `shade.wgsl`).
    fn dispatch_shade(
        &self,
        device: &wgpu::Device,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
    ) {
        let view = plane_entry;
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shade"),
            layout: &self.shade_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_buffer.as_entire_binding(),
                },
                view(1, &target.lit_hdr),
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.materials.as_entire_binding(),
                },
                view(5, &target.gbuffer),
                view(6, &target.surface),
                view(10, &target.sun_weights),
                buffer_entry(GI_CACHE_BINDING, &self.gi_cache),
                view(21, &target.gi_light),
            ]
            .into_iter()
            .chain(self.atmosphere.entries())
            .collect::<Vec<_>>(),
        });
        let (width, height) = target.resolution.internal;
        let mut pass = profiler.compute_pass(encoder, "shade");
        pass.set_pipeline(&self.shade);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }

    /// Runs `pipeline` over every pixel of an image `size` big, drawing to `image`.
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &self,
        device: &wgpu::Device,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        name: &str,
        pipeline: &wgpu::ComputePipeline,
        (width, height): (u32, u32),
        image: &wgpu::TextureView,
        target: &Target,
    ) {
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera, image, accumulation, materials and G-buffer"),
            layout: &self.camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(image),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: target.accum.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.materials.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&target.gbuffer.view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&target.surface.view),
                },
                buffer_entry(EMITTERS_BINDING, &self.emitter_buffer),
            ]
            .into_iter()
            .chain(self.atmosphere.entries())
            .collect::<Vec<_>>(),
        });
        let mut pass = profiler.compute_pass(encoder, name);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }

    /// Casts `rays` through `trace()` on the GPU and returns the hits, in order.
    pub fn trace_rays(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &[Ray],
    ) -> Vec<Option<Hit>> {
        if rays.is_empty() {
            return Vec::new();
        }
        // Three vec4s per ray: cell, origin, direction.
        let input: Vec<[u32; 4]> = rays
            .iter()
            .flat_map(|r| {
                let bits = |v: [f32; 3]| [v[0].to_bits(), v[1].to_bits(), v[2].to_bits(), 0];
                [
                    [r.cell[0] as u32, r.cell[1] as u32, r.cell[2] as u32, 0],
                    bits(r.origin),
                    bits(r.dir),
                ]
            })
            .collect();
        let rays_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe rays"),
            contents: bytemuck::cast_slice(&input),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let hits_size = (rays.len() * 3 * 16) as u64;
        let hits_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe hits"),
            size: hits_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let probe_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe"),
            entries: &[
                layout_entry(0, buffer_binding(STORAGE_READ)),
                layout_entry(
                    1,
                    buffer_binding(wgpu::BufferBindingType::Storage { read_only: false }),
                ),
            ],
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe"),
            layout: &probe_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rays_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: hits_buffer.as_entire_binding(),
                },
            ],
        });
        let pipeline = compute_pipeline(
            device,
            "probe",
            shaders::PROBE,
            "probe",
            &probe_layout,
            &self.scene_layout,
            &[],
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            // A one-shot probe with its own submit, so not timed.
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default()); // untimed
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &self.scene.group, &[]);
            pass.set_bind_group(1, &group, &[]);
            pass.dispatch_workgroups((rays.len() as u32).div_ceil(64), 1, 1);
        }
        let bytes = crate::read_buffer(device, queue, encoder, &hits_buffer, hits_size);
        let words: &[[u32; 4]] = bytemuck::cast_slice(&bytes);
        words
            .chunks(3)
            .map(|c| {
                let float = |w: u32| f32::from_bits(w);
                (c[1][3] == 1).then(|| Hit {
                    t: float(c[0][3]),
                    position: [float(c[0][0]), float(c[0][1]), float(c[0][2])],
                    normal: [float(c[1][0]), float(c[1][1]), float(c[1][2])].map(|n| n as i32),
                    voxel: [c[2][0] as i32, c[2][1] as i32, c[2][2] as i32],
                    material: c[2][3] as u16,
                })
            })
            .collect()
    }
}

/// The pixels in an image of `size`.
fn pixels(size: (u32, u32)) -> u64 {
    u64::from(size.0) * u64::from(size.1)
}

/// How far from the camera the smooth normals reach, in Voxels: 30 m, the prototype's, and no
/// further than level 0 does.
pub const SMOOTH_UP_TO: f32 = 600.0;

/// A smoothed hit looks at the Voxels of a cube of 5 round the one in front of its face: its far
/// corner is at most `|(4, 3, 3)|`, under 6 Voxels, from the point that was hit.
const SMOOTH_REACH: f32 = 6.0;

/// Where the smooth normals end for `scene`, in Voxels from the camera: [`SMOOTH_UP_TO`], or
/// where that is more than level 0 holds, the distance that a brick is certain to be level 0's up
/// to (a level 1 brick is only split while it is nearer than its boundary less the hysteresis,
/// [`crate::lod::HYSTERESIS`]) less the reach of the window. A scene that is not made of rings has
/// one level, and no end but 30 m.
fn smooth_end(scene: &Scene) -> f32 {
    scene.bands().map_or(SMOOTH_UP_TO, |bands| {
        let level_0 = bands.floor(1) * (1.0 - crate::lod::HYSTERESIS);
        SMOOTH_UP_TO.min(level_0 as f32 - SMOOTH_REACH).max(1.0)
    })
}

/// The samples one dispatch takes when `samples` are asked for on an image of `size`: at least 1,
/// and at most [`max_samples`] of the size, the cap that keeps a dispatch's work bounded.
fn batch_of(samples: u32, size: (u32, u32)) -> u32 {
    samples.clamp(1, max_samples(pixels(size)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_batch_is_clamped_by_the_size_of_the_image_it_is_of() {
        use crate::batch::MAX_BATCH_SAMPLES;
        assert_eq!(batch_of(0, (2560, 1440)), 1);
        assert_eq!(batch_of(100, (2560, 1440)), MAX_BATCH_SAMPLES);
        assert_eq!(batch_of(100, (1280, 720)), MAX_BATCH_SAMPLES);
        assert_eq!(batch_of(100, (3840, 2160)), 7, "4K");
        assert_eq!(batch_of(5, (3840, 2160)), 5);
        assert_eq!(batch_of(100, (7680, 4320)), 1, "8K");
        assert_eq!(pixels((3840, 2160)), 8_294_400);
    }

    use super::*;
    use crate::{request_gpu, shared_instance};

    /// The size of the window is part of the view that the Reference view is of (#189): a window
    /// that is resized is a change, and cuts to Primary. Needs a Vulkan device.
    #[test]
    fn the_size_of_the_window_is_part_of_the_view() {
        let (_, device, _) = request_gpu(shared_instance(), None, false).unwrap();
        let (camera, sun) = (
            Camera::looking_at([0.0, 5.0, 0.0], [0.0; 3]),
            Sun::default(),
        );
        let at = |size: (u32, u32), scale: f32| {
            let target = Target::with_full_reference(&device, Resolution::scaled(size, scale));
            View::new(&camera, &sun, &target)
        };
        assert_eq!(at((64, 36), 0.5), at((64, 36), 0.5));
        assert_ne!(at((64, 36), 0.5), at((96, 54), 0.5), "the window's size");
        // The Reference view is traced at the window's size, so Primary's scale is not in it.
        assert_eq!(at((64, 36), 0.5), at((64, 36), 1.0));
        let moved = Camera::looking_at([1.0, 5.0, 0.0], [0.0; 3]);
        let target = Target::with_full_reference(&device, Resolution::native(64, 36));
        assert_ne!(
            View::new(&camera, &sun, &target),
            View::new(&moved, &sun, &target)
        );
    }
}
