//! The art direction's display transform (#202, the owner's picks of #148): auto-exposure, bloom
//! and the tone curve, run on the linear HDR image of a view. The CPU side of
//! `shaders/post.wgsl`, which says what each pass does.
//!
//! It runs on both views' images (ADR 0005): Primary's lit image before the upscaler, and the
//! Reference view's mean (as a copy of it, never the accumulation itself) before the copy to the
//! window. It lights nothing and changes nothing a view accumulates, and each stage can be
//! switched off ([`Stages`]), so that the dev can see what it does.
//!
//! The numbers are the prototype's, as rendered (#200, `bench/lookdev/post.rs`):
//! - **Tone curve:** Khronos PBR Neutral, then the sRGB curve of the image.
//! - **Auto-exposure:** the scene's log-average luminance to middle grey 0.18, within 0.125 to 32
//!   (-3 to +5 stops), adapting in stops with `k = 1 - e^(-dt / tau)`, tau 2.5 s. Each view has an
//!   exposure of its own. The Reference view's still frames use the settled one (`k = 1`), metered
//!   on the mean's 4 x 4 boxes and held from [`REFERENCE_METERED_UP_TO`] samples.
//! - **Bloom:** 0.05 of the mean of the exposed image blurred at three scales. The Gaussians'
//!   standard deviations are `[2.5, 7, 18] * width / 640` pixels of the image they are of: 5, 14 and
//!   36 px of a 1280-wide image and 10, 28 and 72 px of a 2560-wide one (the prototype scaled them
//!   by `width / 640`, and its sheet was 1280 wide). The blurs are made at a quarter of the size,
//!   so that the widest costs 109 taps and not 433.

use bytemuck::{Pod, Zeroable};

use crate::profiler::Profiler;
use crate::shaders;
use crate::tracer::{
    Plane, buffer_binding, buffer_entry, layout_entry, plane_entry, storage_texture,
};

/// Format of the linear HDR images that the kernels draw and this transform reads.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// How much of the bloom is added to the exposed image.
pub const BLOOM_STRENGTH: f32 = 0.05;
/// The bloom's three Gaussians, as standard deviations in pixels of an image 640 wide.
pub const BLOOM_SIGMAS: [f32; 3] = [2.5, 7.0, 18.0];
/// The width that [`BLOOM_SIGMAS`] are for.
pub const BLOOM_WIDTH: f32 = 640.0;
/// How much smaller than the image the blurs are made, along each axis.
const BLOOM_DOWN: u32 = 4;

/// The grey that auto-exposure puts the scene's mean at.
pub const MIDDLE_GREY: f32 = 0.18;
/// The exposures auto-exposure may reach: -3 and +5 stops.
pub const EXPOSURE_RANGE: (f32, f32) = (0.125, 32.0);
/// The time constant of the exposure's adaptation, in seconds.
pub const ADAPTATION_TAU: f32 = 2.5;

/// The workgroup size of the exposure's first pass, in each direction (`exposure` in `post.wgsl`).
const EXPOSURE_TILE: u32 = 16;

/// The stages of the art direction that can be switched off, for comparison. All on is the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stages {
    /// The tone curve ([`Stages::curve`]). Off, light above 1 is white, as before #202.
    pub tone: bool,
    /// Which tone curve, when the stage is on. Neutral is the game's until the owner picks.
    pub curve: ToneCurve,
    /// How the smooth normals are kept to the face's side. Face (0.25) is the game's.
    pub normal_bias: NormalBias,
    /// Auto-exposure. Off, the exposure is 1.
    pub exposure: bool,
    pub bloom: bool,
    /// Smooth computed normals on natural Materials. Off, every hit has its face's normal.
    pub smooth_normals: bool,
}

impl Stages {
    /// Every stage off: what the renderer drew before #202, bar the light's palette.
    pub const NONE: Stages = Stages {
        tone: false,
        curve: ToneCurve::Neutral,
        normal_bias: NormalBias::Face,
        exposure: false,
        bloom: false,
        smooth_normals: false,
    };
}

impl Default for Stages {
    fn default() -> Self {
        Self {
            tone: true,
            curve: ToneCurve::Neutral,
            normal_bias: NormalBias::Face,
            exposure: true,
            bloom: true,
            smooth_normals: true,
        }
    }
}

/// The tone curves the look-dev sheet compares (the owner's pick of the combination is still to
/// come, #202). The game's is [`ToneCurve::Neutral`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneCurve {
    /// Khronos PBR Neutral, then the sRGB curve.
    Neutral,
    /// The prototype's AgX-like curve, which is the display-encoded image itself.
    Agx,
    /// Neutral, then [`NEUTRAL_DESAT_KEPT`] of its colourfulness in linear light, then sRGB.
    NeutralDesat,
}

impl ToneCurve {
    pub const ALL: [ToneCurve; 3] = [ToneCurve::Neutral, ToneCurve::Agx, ToneCurve::NeutralDesat];

    pub fn slug(self) -> &'static str {
        match self {
            ToneCurve::Neutral => "neutral",
            ToneCurve::Agx => "agx",
            ToneCurve::NeutralDesat => "neutral-desat",
        }
    }

    pub fn from_slug(slug: &str) -> Option<ToneCurve> {
        Self::ALL.into_iter().find(|c| c.slug() == slug)
    }

    /// What the display transform shows of the linear, exposed `colour` (the CPU twin of `tone` in
    /// `post.wgsl`), as 0..1 encoded values.
    pub fn apply(self, colour: [f32; 3]) -> [f32; 3] {
        let colour = colour.map(|x| x.max(0.0));
        match self {
            ToneCurve::Neutral => neutral(colour).map(srgb_encode),
            ToneCurve::Agx => agx(colour),
            ToneCurve::NeutralDesat => {
                desaturate(neutral(colour), NEUTRAL_DESAT_KEPT).map(srgb_encode)
            }
        }
    }
}

/// The share of Neutral's colourfulness that [`ToneCurve::NeutralDesat`] keeps: the rest is taken
/// toward Rec.709 luminance, in linear light. Fitted on the real renderer's Reference images of the
/// six open-air look-dev shots under hero light B, with face normals: the value at which the mean
/// HSV saturation of Neutral matches AgX's (ratio 1.0 at 0.55; 1.77 undesaturated) and the
/// colour (RGB over luma) differs least from AgX's (error 0.095 undesaturated, 0.019 at 0.6,
/// 0.021 at 0.5). The tones themselves stay Neutral's: that is not what this changes.
pub const NEUTRAL_DESAT_KEPT: f32 = 0.55;

/// How the smooth normal is kept to the side of the face the ray came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalBias {
    /// A quarter of the face's normal is added to the smooth one: the prototype's, as rendered.
    Face,
    /// Nothing is added: the normal is the smooth one alone.
    None,
}

impl NormalBias {
    pub const ALL: [NormalBias; 2] = [NormalBias::Face, NormalBias::None];

    pub fn slug(self) -> &'static str {
        match self {
            NormalBias::Face => "face-bias",
            NormalBias::None => "no-bias",
        }
    }

    /// How much of the face's normal is added to the unit smooth normal.
    pub fn weight(self) -> f32 {
        match self {
            NormalBias::Face => 0.25,
            NormalBias::None => 0.0,
        }
    }
}

/// The true sRGB encoding of one linear value, clamped to 0..1.
pub fn srgb_encode(linear: f32) -> f32 {
    let c = linear.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// `colour` with its distance from its Rec.709 luminance scaled by `kept` (1 is unchanged, 0 is
/// grey), in the light it is given in.
pub fn desaturate(colour: [f32; 3], kept: f32) -> [f32; 3] {
    let luminance = 0.2126 * colour[0] + 0.7152 * colour[1] + 0.0722 * colour[2];
    colour.map(|x| luminance + (x - luminance) * kept)
}

/// The prototype's AgX-like curve (Sobotka's, in the polynomial form of Wrensch) on one linear
/// colour: the CPU twin of `agx` in `post.wgsl`, and the prototype's `agx` as it was (#200,
/// `bench/lookdev/post.rs`). Its result is the display-encoded value, not light.
pub fn agx(colour: [f32; 3]) -> [f32; 3] {
    const IN: [[f32; 3]; 3] = [
        [0.842_479, 0.078_433_6, 0.079_223_74],
        [0.042_328_24, 0.878_468_6, 0.079_166_13],
        [0.042_375_65, 0.078_433_6, 0.879_143],
    ];
    const OUT: [[f32; 3]; 3] = [
        [1.196_879, -0.098_020_88, -0.099_029_74],
        [-0.052_896_85, 1.151_903_1, -0.098_961_18],
        [-0.052_971_64, -0.098_043_45, 1.151_073_7],
    ];
    const MIN_EV: f32 = -12.473_93;
    const MAX_EV: f32 = 4.026_069;
    let mul = |m: [[f32; 3]; 3], c: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
    };
    let v = mul(IN, colour.map(|x| x.max(1e-10)));
    let v = v.map(|x| (x.log2().clamp(MIN_EV, MAX_EV) - MIN_EV) / (MAX_EV - MIN_EV));
    let contrast = |x: f32| {
        let (x2, x4) = (x * x, x * x * x * x);
        15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x
            - 0.00232
    };
    mul(OUT, v.map(contrast)).map(|x| x.clamp(0.0, 1.0))
}

/// The standard deviations, in pixels of an image `width` wide, of the bloom's three blurs.
pub fn bloom_sigmas(width: u32) -> [f32; 3] {
    BLOOM_SIGMAS.map(|s| s * width as f32 / BLOOM_WIDTH)
}

/// The exposure that puts a scene whose mean log luminance (natural log) is `mean_log_luminance`
/// at middle grey, within [`EXPOSURE_RANGE`]: the settled exposure.
pub fn settled_exposure(mean_log_luminance: f32) -> f32 {
    (MIDDLE_GREY / mean_log_luminance.exp()).clamp(EXPOSURE_RANGE.0, EXPOSURE_RANGE.1)
}

/// The share of the way to the settled exposure that `dt` seconds covers: `1 - e^(-dt / tau)`.
pub fn adaptation_rate(dt: f32) -> f32 {
    1.0 - (-dt.max(0.0) / ADAPTATION_TAU).exp()
}

/// The Khronos PBR Neutral tone mapper on one linear colour: the CPU twin of `neutral` in
/// `post.wgsl`, for tests.
pub fn neutral(colour: [f32; 3]) -> [f32; 3] {
    const START_COMPRESSION: f32 = 0.8 - 0.04;
    const DESATURATION: f32 = 0.15;
    let lowest = colour[0].min(colour[1]).min(colour[2]);
    let offset = if lowest < 0.08 {
        lowest - 6.25 * lowest * lowest
    } else {
        0.04
    };
    let c = colour.map(|x| x - offset);
    let peak = c[0].max(c[1]).max(c[2]);
    if peak < START_COMPRESSION {
        return c;
    }
    let d = 1.0 - START_COMPRESSION;
    let new_peak = 1.0 - d * d / (peak + d - START_COMPRESSION);
    let c = c.map(|x| x * new_peak / peak);
    let g = 1.0 - 1.0 / (DESATURATION * (peak - new_peak) + 1.0);
    c.map(|x| x + (new_peak - x) * g)
}

/// The time between frames that have no clock of their own (the bench's flight, the tests): a
/// 60 Hz frame, so that what they draw does not depend on how fast the machine is.
pub const NOMINAL_FRAME: f32 = 1.0 / 60.0;

/// What a view's exposure does in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Metering {
    /// Moves `rate` of the way to what the image meters ([`adaptation_rate`]); 1 sets it at once.
    Adapt(f32),
    /// Stays as it is: the Reference view's, once it has [`REFERENCE_METERED_UP_TO`] samples.
    Hold,
}

impl Metering {
    fn rate(self) -> f32 {
        match self {
            Metering::Adapt(rate) => rate,
            Metering::Hold => 0.0,
        }
    }
}

/// `Post` in `post.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostUniform {
    sizes: [u32; 4],
    stages: [u32; 4],
    params: [f32; 4],
    sigmas: [f32; 4],
    counts: [u32; 4],
}

/// The images of the passes at one size, which a view of that size owns: the exposure's partial
/// sums, and the bloom's quarter-size images.
pub(crate) struct Scratch {
    size: (u32, u32),
    quarter: (u32, u32),
    /// One partial sum for each workgroup of the exposure's first pass.
    partials: wgpu::Buffer,
    partial_count: u32,
    down: Plane,
    blurred: [Plane; 3],
    bloom: Plane,
}

impl Scratch {
    /// The bloom image (`Rgba16Float`) and its size.
    pub fn bloom(&self) -> (&Plane, (u32, u32)) {
        (&self.bloom, self.quarter)
    }

    pub fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        let quarter = (
            size.0.div_ceil(BLOOM_DOWN).max(1),
            size.1.div_ceil(BLOOM_DOWN).max(1),
        );
        let partial_count = size.0.div_ceil(EXPOSURE_TILE) * size.1.div_ceil(EXPOSURE_TILE);
        let plane = |label: &str| Plane::new(device, label, quarter, HDR_FORMAT);
        Self {
            size,
            quarter,
            partials: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("exposure partial sums"),
                size: u64::from(partial_count.max(1)) * 4,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
            partial_count,
            down: plane("bloom down"),
            blurred: [
                plane("bloom blur 0"),
                plane("bloom blur 1"),
                plane("bloom blur 2"),
            ],
            bloom: plane("bloom"),
        }
    }
}

/// Which view an image is of. Each keeps an exposure of its own: the Reference view's is metered
/// on the still view it accumulates, and Primary, when the camera moves again, goes on adapting
/// from what it showed before, not from the Reference's metering of a view that has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exposed {
    Primary,
    Reference,
}

/// The samples after which the Reference view holds the exposure it has metered. The mean of fewer
/// is too noisy to meter (its dark pixels read as a dark scene and push the exposure up), and the
/// exposure that settles is the one of a view that has enough for the metering to be steady; from
/// there on the image converges at the exposure it has, as a settled view should.
pub const REFERENCE_METERED_UP_TO: u32 = 16;

/// The passes, and the exposure each view keeps from frame to frame.
pub(crate) struct Post {
    uniform: wgpu::Buffer,
    /// The exposure's state of each view ([`Exposed`]; `exposure_state` in `post.wgsl`): in stops,
    /// as an exposure, and whether it has been set.
    states: [wgpu::Buffer; 2],
    sampler: wgpu::Sampler,
    exposure: Pipeline,
    adapt: Pipeline,
    down: Pipeline,
    blur_h: Pipeline,
    blur_v: Pipeline,
    tone: Pipeline,
}

/// A compute pipeline and the layout of its one bind group.
struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl Post {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post"),
            source: wgpu::ShaderSource::Wgsl(shaders::POST.into()),
        });
        let uniform = layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform));
        let texture = |binding, filterable| {
            layout_entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let storage = |binding, format| layout_entry(binding, storage_texture(format));
        let buffer = |binding| {
            layout_entry(
                binding,
                buffer_binding(wgpu::BufferBindingType::Storage { read_only: false }),
            )
        };
        let pipeline = |name: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(name),
                entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(name),
                compilation_options: Default::default(),
                cache: None,
            });
            Pipeline { pipeline, layout }
        };
        let hdr = HDR_FORMAT;
        Self {
            uniform: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("post"),
                size: size_of::<PostUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            states: ["primary exposure state", "reference exposure state"].map(|label| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: 16,
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            }),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("bloom"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            exposure: pipeline("exposure", &[uniform, texture(1, false), buffer(2)]),
            adapt: pipeline("exposure_adapt", &[uniform, buffer(2), buffer(3)]),
            down: pipeline("bloom_down", &[uniform, texture(1, false), storage(4, hdr)]),
            blur_h: pipeline(
                "bloom_h",
                &[
                    uniform,
                    texture(5, false),
                    storage(6, hdr),
                    storage(7, hdr),
                    storage(8, hdr),
                ],
            ),
            blur_v: pipeline(
                "bloom_v",
                &[
                    uniform,
                    texture(9, false),
                    texture(10, false),
                    texture(11, false),
                    storage(12, hdr),
                ],
            ),
            tone: pipeline(
                "tone",
                &[
                    uniform,
                    texture(1, false),
                    buffer(3),
                    texture(13, true),
                    layout_entry(
                        14,
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    ),
                    storage(15, wgpu::TextureFormat::Rgba8Unorm),
                ],
            ),
        }
    }

    /// The buffer that holds the exposure of `view` (`[stops, exposure, set]`, as `f32`s).
    pub fn state(&self, view: Exposed) -> &wgpu::Buffer {
        &self.states[view as usize]
    }

    /// Records the passes that turn the linear image `hdr` (of the size `scratch` is for) into the
    /// 8-bit image `shown`, each timed by `profiler`.
    ///
    /// `passthrough` says that `hdr` holds a debug view's colours (already encoded): they are
    /// copied as they are. `view` says whose exposure it is. `exposure` is what the exposure does
    /// this frame ([`Metering`]). The uniforms are written when this is called.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        hdr: &Plane,
        scratch: &Scratch,
        shown: &Plane,
        stages: Stages,
        passthrough: bool,
        view: Exposed,
        exposure: Metering,
    ) {
        let (width, height) = scratch.size;
        let sigmas = bloom_sigmas(width).map(|s| s / BLOOM_DOWN as f32);
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&PostUniform {
                sizes: [width, height, scratch.quarter.0, scratch.quarter.1],
                stages: [
                    // 0 is off, else the curve's number from 1.
                    if stages.tone {
                        stages.curve as u32 + 1
                    } else {
                        0
                    },
                    u32::from(stages.exposure),
                    u32::from(stages.bloom),
                    u32::from(passthrough),
                ],
                params: [
                    BLOOM_STRENGTH,
                    exposure.rate().clamp(0.0, 1.0),
                    f32::from(matches!(exposure, Metering::Hold)),
                    NEUTRAL_DESAT_KEPT,
                ],
                sigmas: [sigmas[0], sigmas[1], sigmas[2], 0.0],
                counts: [
                    scratch.partial_count,
                    u32::from(view == Exposed::Reference),
                    0,
                    0,
                ],
            }),
        );
        let group = |pipeline: &Pipeline, entries: &[wgpu::BindGroupEntry<'_>]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: &pipeline.layout,
                entries,
            })
        };
        let state = &self.states[view as usize];
        if !stages.exposure || passthrough {
            // Nothing meters with the stage off or while a debug view is shown, and what the state
            // holds goes stale (the Reference view could hold, from 16 samples of another view, an
            // exposure it never metered): it is metered afresh at the next lit frame.
            queue.write_buffer(state, 0, &[0u8; 16]);
        }
        let buffer = buffer_entry;
        let view = plane_entry;
        let uniform = buffer(0, &self.uniform);
        let mut run = |name: &str,
                       pipeline: &Pipeline,
                       group: wgpu::BindGroup,
                       size: (u32, u32),
                       tile: u32| {
            let mut pass = profiler.compute_pass(encoder, name);
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(tile), size.1.div_ceil(tile), 1);
        };
        // A debug view's colours are what they are: nothing to expose or blur.
        if !passthrough {
            if stages.exposure {
                run(
                    "exposure",
                    &self.exposure,
                    group(
                        &self.exposure,
                        &[uniform.clone(), view(1, hdr), buffer(2, &scratch.partials)],
                    ),
                    scratch.size,
                    EXPOSURE_TILE,
                );
                // One workgroup, which sums the partial sums.
                run(
                    "exposure_adapt",
                    &self.adapt,
                    group(
                        &self.adapt,
                        &[
                            uniform.clone(),
                            buffer(2, &scratch.partials),
                            buffer(3, state),
                        ],
                    ),
                    (256, 1),
                    256,
                );
            }
            if stages.bloom {
                run(
                    "bloom_down",
                    &self.down,
                    group(
                        &self.down,
                        &[uniform.clone(), view(1, hdr), view(4, &scratch.down)],
                    ),
                    scratch.quarter,
                    8,
                );
                run(
                    "bloom_h",
                    &self.blur_h,
                    group(
                        &self.blur_h,
                        &[
                            uniform.clone(),
                            view(5, &scratch.down),
                            view(6, &scratch.blurred[0]),
                            view(7, &scratch.blurred[1]),
                            view(8, &scratch.blurred[2]),
                        ],
                    ),
                    scratch.quarter,
                    8,
                );
                run(
                    "bloom_v",
                    &self.blur_v,
                    group(
                        &self.blur_v,
                        &[
                            uniform.clone(),
                            view(9, &scratch.blurred[0]),
                            view(10, &scratch.blurred[1]),
                            view(11, &scratch.blurred[2]),
                            view(12, &scratch.bloom),
                        ],
                    ),
                    scratch.quarter,
                    8,
                );
            }
        }
        run(
            "tone",
            &self.tone,
            group(
                &self.tone,
                &[
                    uniform,
                    view(1, hdr),
                    buffer(3, state),
                    view(13, &scratch.bloom),
                    wgpu::BindGroupEntry {
                        binding: 14,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    view(15, shown),
                ],
            ),
            scratch.size,
            8,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bloom_sigmas_are_the_prototypes_scaled_by_the_width_over_640() {
        // The source is the prototype's `width / 640`; the owner picked from a 1280-wide sheet.
        assert_eq!(bloom_sigmas(640), [2.5, 7.0, 18.0]);
        assert_eq!(bloom_sigmas(1280), [5.0, 14.0, 36.0]);
        assert_eq!(bloom_sigmas(2560), [10.0, 28.0, 72.0]);
    }

    #[test]
    fn the_exposure_puts_the_mean_at_middle_grey_within_minus_three_and_plus_five_stops() {
        // A scene whose log-average luminance is 0.36 is brought down by a stop.
        assert!((settled_exposure(0.36f32.ln()) - 0.5).abs() < 1e-6);
        assert_eq!(
            settled_exposure(1e-6f32.ln()),
            32.0,
            "a dark scene: +5 stops"
        );
        assert_eq!(
            settled_exposure(1e3f32.ln()),
            0.125,
            "a bright one: -3 stops"
        );
    }

    #[test]
    fn the_exposure_adapts_by_one_minus_e_to_the_minus_dt_over_tau() {
        assert_eq!(adaptation_rate(0.0), 0.0);
        assert!((adaptation_rate(ADAPTATION_TAU) - (1.0 - (-1.0f32).exp())).abs() < 1e-6);
        assert!(adaptation_rate(1.0 / 60.0) < adaptation_rate(1.0 / 30.0));
        assert!(adaptation_rate(1e6) > 0.999_999);
        assert_eq!(adaptation_rate(-1.0), 0.0, "time does not run backwards");
    }

    #[test]
    fn the_tone_curve_is_the_identity_less_its_toe_until_it_rolls_off_towards_one() {
        // Khronos PBR Neutral: below the start of the shoulder, the colour less the toe's offset.
        let c = neutral([0.3, 0.3, 0.3]);
        assert!(c.iter().all(|&x| (x - 0.26).abs() < 1e-6), "{c:?}");
        // Black stays black.
        assert_eq!(neutral([0.0; 3]), [0.0; 3]);
        // Monotone, and never over 1, however bright.
        let mut last = 0.0;
        for i in 0..200 {
            let v = neutral([i as f32 * 0.5; 3])[0];
            assert!(v >= last && v <= 1.0, "{i}: {v}");
            last = v;
        }
        assert!(neutral([1000.0; 3])[0] > 0.99);
        // Very bright colours desaturate towards white.
        let hot = neutral([40.0, 10.0, 2.0]);
        let cool = neutral([4.0, 1.0, 0.2]);
        assert!(hot[2] / hot[0] > cool[2] / cool[0]);
    }

    #[test]
    fn the_agx_like_curve_is_the_prototypes_at_a_few_inputs() {
        // The prototype's `agx` (bench/lookdev/post.rs of proto-148-lookdev), evaluated apart in
        // double precision.
        let cases: [([f32; 3], [f32; 3]); 6] = [
            ([0.18; 3], [0.49668, 0.49674, 0.49675]),
            ([1.0, 0.5, 0.2], [0.80099, 0.68350, 0.53879]),
            ([0.02, 0.05, 0.2], [0.20344, 0.29617, 0.52947]),
            ([8.0, 2.0, 0.5], [0.99131, 0.88655, 0.75717]),
            ([0.0; 3], [0.0; 3]),
            ([100.0; 3], [0.99841, 0.99863, 0.99864]),
        ];
        for (input, want) in cases {
            let got = agx(input);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() < 2e-4,
                    "{input:?}: {got:?} is not {want:?}"
                );
            }
        }
        // It is the encoded image itself: the curve's result is what ToneCurve shows.
        assert_eq!(ToneCurve::Agx.apply([1.0, 0.5, 0.2]), agx([1.0, 0.5, 0.2]));
    }

    #[test]
    fn the_desaturating_curve_takes_colour_toward_luminance_in_linear_light_and_keeps_grey() {
        let c = [0.6, 0.3, 0.1];
        let luminance = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let d = desaturate(c, 0.5);
        for i in 0..3 {
            assert!((d[i] - (luminance + (c[i] - luminance) * 0.5)).abs() < 1e-6);
        }
        // The luminance does not move, nothing moves at 1, everything is grey at 0, and grey stays.
        let l = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!((l(d) - luminance).abs() < 1e-6);
        for (a, b) in desaturate(c, 1.0).into_iter().zip(c) {
            assert!((a - b).abs() < 1e-6);
        }
        let grey = desaturate(c, 0.0);
        assert!(grey.iter().all(|x| (x - luminance).abs() < 1e-6));
        assert_eq!(desaturate([0.4; 3], 0.3), [0.4; 3]);
        // The curve is Neutral's, then that, then sRGB: less colourful than Neutral, same order.
        let shown = ToneCurve::NeutralDesat.apply([0.9, 0.4, 0.1]);
        let plain = ToneCurve::Neutral.apply([0.9, 0.4, 0.1]);
        assert!(shown[0] - shown[2] < plain[0] - plain[2]);
        assert!(shown[0] > shown[1] && shown[1] > shown[2]);
        let want = desaturate(neutral([0.9, 0.4, 0.1]), NEUTRAL_DESAT_KEPT).map(srgb_encode);
        assert_eq!(shown, want);
    }

    #[test]
    fn the_default_stages_are_neutral_with_the_face_bias() {
        let stages = Stages::default();
        assert_eq!(
            (stages.curve, stages.normal_bias),
            (ToneCurve::Neutral, NormalBias::Face)
        );
        assert_eq!(NormalBias::Face.weight(), 0.25);
        assert_eq!(NormalBias::None.weight(), 0.0);
        for curve in ToneCurve::ALL {
            assert_eq!(ToneCurve::from_slug(curve.slug()), Some(curve));
        }
    }
}
