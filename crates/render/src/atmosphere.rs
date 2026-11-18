//! The Planet's atmosphere (#53): Hillaire 2020's lookup tables, which both views read.
//!
//! The model is the air as three layers of particles: Rayleigh (the blue of the sky), Mie (haze) and
//! ozone (which takes the red out of a low sun's light, and so keeps a sunset's sky blue overhead).
//! `shaders/atmosphere_model.wgsl` is the model and how a table is indexed, `atmosphere.wgsl` the
//! kernels that fill the tables, `sky.wgsl` what the views read. This file is the parameters, the
//! tables' textures and the pass that fills them ([`AtmosphereTables`]), and the CPU's twin of one
//! number the shaders need before any table exists: how much of the sun's light a surface on the
//! ground gets ([`Atmosphere::sun_light_on_ground`]).
//!
//! The tables are:
//! - **transmittance**: how much light the air lets through to the top of the atmosphere, by the
//!   radius and the angle from the zenith (once);
//! - **multi-scattering**: the light that has scattered more than once, which the single-scattering
//!   march adds to its sun (once);
//! - **sky-view**: the sky from the camera, by direction (every frame the view changes);
//! - **aerial perspective**: a volume over the image, holding the light that the air adds along
//!   each pixel's ray out to a distance, and what it lets through (every frame the view changes);
//! - **sun-free sky**: the sky from the ground without the sun, by direction, and the cumulative
//!   distribution of its brightness: what the Reference's escaped rays read and sample (when the
//!   sun's height changes).
//!
//! Distances here are metres, the Planet's; the shaders of the views work in Voxels.

use bytemuck::{Pod, Zeroable};

use crate::profiler::Profiler;
use crate::tracer::{STORAGE_READ, buffer_binding, layout_entry};

/// The tables' sizes. Keep in step with `atmosphere_model.wgsl`.
pub const TRANSMITTANCE_SIZE: (u32, u32) = (256, 64);
pub const MULTISCATTER_SIZE: u32 = 32;
pub const SKY_VIEW_SIZE: (u32, u32) = (192, 108);
pub const GROUND_SKY_SIZE: (u32, u32) = (64, 32);
pub const AERIAL_SIZE: u32 = 32;
/// Words of the sky's cumulative distribution: the rows', each row's texels', then the total.
pub const SKY_CDF_WORDS: u64 =
    (GROUND_SKY_SIZE.1 + GROUND_SKY_SIZE.1 * GROUND_SKY_SIZE.0 + 1) as u64;

/// The least height above the ground that the tables are made for, in metres.
const MIN_CAMERA_HEIGHT: f64 = 0.5;

/// The Planet's radius at sea level, ADR 0008: 320 km.
pub const PLANET_RADIUS_M: f32 = 320_000.0;

/// One setting of the art direction's tint on the physical tables (#53, the owner's decision of
/// 2026-11-18): the air is Hillaire's, and this is what the look adds to the light of the sky and
/// of the air between the camera and a surface. It is not physical, and it is in the tables, so both
/// views read the same tinted sky and the same tinted haze from the same functions (`sky.wgsl`) and
/// the Reference still matches Primary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TintKey {
    /// What the sky's radiance is multiplied by at the horizon and at the zenith (linear RGB), and
    /// between them by the sine of the elevation, as the gradient it replaces blended its two
    /// colours. Below the horizon it is the horizon's. It applies to the sky seen by the camera, to
    /// the sun-free sky that surfaces see (so the light that bounces off the sky is tinted too), and
    /// to the light the air adds along a camera ray to a surface (by that ray's elevation).
    pub sky_horizon: [f32; 3],
    pub sky_zenith: [f32; 3],
    /// An extra haze on the camera ray to a surface, as the gradient had: `1 - exp(-haze * d)` of
    /// `haze_colour` fades in over the `d` metres the ray travels in air, whatever the air's
    /// density there. The physical air is the same; this is added to it. 0 for none.
    pub haze_per_metre: f32,
    pub haze_colour: [f32; 3],
}

impl TintKey {
    /// No tint: the physical tables.
    pub const NONE: TintKey = TintKey {
        sky_horizon: [1.0; 3],
        sky_zenith: [1.0; 3],
        haze_per_metre: 0.0,
        haze_colour: [0.0; 3],
    };

    fn lerp(self, other: TintKey, t: f32) -> TintKey {
        let mix3 = |a: [f32; 3], b: [f32; 3]| std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t);
        TintKey {
            sky_horizon: mix3(self.sky_horizon, other.sky_horizon),
            sky_zenith: mix3(self.sky_zenith, other.sky_zenith),
            haze_per_metre: self.haze_per_metre + (other.haze_per_metre - self.haze_per_metre) * t,
            haze_colour: mix3(self.haze_colour, other.haze_colour),
        }
    }
}

/// The tint by the time of day (the owner's direction of 2026-11-18: more violet, mauve and purple
/// when it is dark, hazier and more orange in the sun): four settings at four heights of the sun,
/// blended between by a smoothstep of the sun's elevation, so nothing pops as the sun moves. The
/// sun's elevation is the one number that picks the setting, so it is a function of the frame's
/// sun alone, and the tables (which are the same in both views) hold it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    /// The settings at [`Tint::ELEVATIONS`]: night, dusk, hero light B's sun, and the day's.
    pub keys: [TintKey; 4],
}

impl Tint {
    /// The sun's elevations of the four settings, in degrees: at or under the first it is night
    /// (violet), at the second dusk (mauve), at the third hero light B's sun (the gradient's mauve
    /// and peach), and at or over the fourth it is day (a hazy orange).
    pub const ELEVATIONS: [f32; 4] = [-8.0, 0.0, 8.0, 30.0];

    /// No tint: the physical tables.
    pub const NONE: Tint = Tint::constant(TintKey::NONE);

    /// The same setting at every height of the sun.
    pub const fn constant(key: TintKey) -> Tint {
        Tint { keys: [key; 4] }
    }

    /// The setting for a sun whose cosine with the up is `mu_s`: continuous in it.
    pub fn at(&self, mu_s: f32) -> TintKey {
        let elevation = mu_s.clamp(-1.0, 1.0).asin().to_degrees();
        let e = Self::ELEVATIONS;
        if elevation <= e[0] {
            return self.keys[0];
        }
        for i in 0..3 {
            if elevation <= e[i + 1] {
                let x = (elevation - e[i]) / (e[i + 1] - e[i]);
                return self.keys[i].lerp(self.keys[i + 1], x * x * (3.0 - 2.0 * x));
            }
        }
        self.keys[3]
    }
}

/// The hero tint (see [`Atmosphere::HERO`]).
const HERO_TINT: Tint = Tint {
    keys: [
        // Night: the sky and the haze lean violet. Found by eye on the look-dev sheet's time-of-day
        // strip (the physical sky is a dim blue-grey).
        TintKey {
            sky_horizon: [0.8, 0.28, 1.4],
            sky_zenith: [0.35, 0.13, 0.8],
            haze_per_metre: 1.0 / 400.0,
            haze_colour: [0.14, 0.07, 0.18],
        },
        // Dusk: mauve and purple.
        TintKey {
            sky_horizon: [1.6, 0.7, 1.5],
            sky_zenith: [0.7, 0.35, 1.0],
            haze_per_metre: 1.0 / 360.0,
            haze_colour: [0.30, 0.15, 0.22],
        },
        // Hero light B's sun (8 degrees), fitted by `fit_the_tint`: warmer at the horizon, and
        // bluer overhead, than the air makes it (an 8-degree sun on a thick shell of air gives a
        // pale, cool sky where the gradient was mauve), and an extra haze on top of the air's own
        // 35 m layer of it (1/300 per metre at the ground in all), fading to the gradient's horizon
        // colour as it did.
        TintKey {
            sky_horizon: [1.916, 0.928, 0.687],
            sky_zenith: [0.769, 0.714, 1.206],
            haze_per_metre: 1.0 / 342.0,
            haze_colour: [1.0, 0.55, 0.38],
        },
        // Day: a hazier, warmer sky, not a clear blue one: an orange horizon, a pale warm zenith,
        // and a denser orange haze.
        TintKey {
            sky_horizon: [1.9, 1.0, 0.55],
            sky_zenith: [1.7, 1.0, 0.7],
            haze_per_metre: 1.0 / 180.0,
            haze_colour: [2.0, 1.1, 0.55],
        },
    ],
};

/// The air, as the model reads it. Rates are per metre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Atmosphere {
    /// The ground sphere's radius, and how high the air reaches above it.
    pub ground_radius: f32,
    pub top_height: f32,
    /// Rayleigh scattering at the ground (per colour channel) and its scale height.
    pub rayleigh: [f32; 3],
    pub rayleigh_height: f32,
    /// Mie scattering and absorption at the ground, the scale height of both, and the phase
    /// function's asymmetry (0 scatters evenly, towards 1 forwards).
    pub mie_scattering: [f32; 3],
    pub mie_absorption: [f32; 3],
    pub mie_height: f32,
    pub mie_g: f32,
    /// Ozone absorption at the middle of its layer, the layer's height there and its half width.
    pub ozone: [f32; 3],
    pub ozone_height: f32,
    pub ozone_half_width: f32,
    /// The sun's light above the air, as the radiance of a surface that faces it, as a multiple of
    /// the surface's colour (look.rs): what the air then takes from it is the sun on the ground.
    pub sun_light: [f32; 3],
    /// The ground's albedo, for the light that the multiple scattering bounces off it.
    pub ground_albedo: [f32; 3],
    /// How high above the ground the sun-free sky is seen from: a surface's height, a couple of
    /// metres.
    pub observer_height: f32,
    /// The look's tint on the sky and the haze (see [`Tint`]).
    pub tint: Tint,
}

impl Atmosphere {
    /// The Planet's air, as the model makes it with no tint, tuned towards hero light B's look: the horizon-to-zenith colours, the
    /// warmth of the sun on the ground and the depth of the haze of the gradient it replaces (the
    /// look-dev sheet shows the two side by side). A white sun above the air; Earth's Rayleigh
    /// colours to within 40 % (blue-ish, a little redder than Earth's), a scale height of 20 km
    /// (the Planet is 320 km: its sky is a thick, low shell), and almost no ozone; and, in place
    /// of Mie's thin high haze, a dense layer 35 m thick whose extinction is the gradient's
    /// 1/300 per metre at the ground. Found by `tune` in `tests/atmosphere.rs`.
    pub const PHYSICAL: Atmosphere = Atmosphere {
        ground_radius: PLANET_RADIUS_M,
        top_height: 100_000.0,
        rayleigh: [8.5e-6, 15.0e-6, 26.0e-6],
        rayleigh_height: 19_700.0,
        mie_scattering: [2.8e-3, 2.8e-3, 2.8e-3],
        mie_absorption: [0.3e-3, 0.3e-3, 0.3e-3],
        mie_height: 35.0,
        mie_g: 0.0,
        ozone: [0.057e-6, 0.164e-6, 0.0074e-6],
        ozone_height: 25_000.0,
        ozone_half_width: 15_000.0,
        sun_light: [4.87, 4.87, 4.87],
        ground_albedo: [0.5, 0.5, 0.5],
        observer_height: 2.0,
        tint: Tint::NONE,
    };

    /// The air of hero light B: [`Atmosphere::PHYSICAL`] with the owner's tint (see [`Tint`]), fitted by
    /// `fit_the_tint` in `tests/atmosphere.rs` so that the hero views read like the gradient and
    /// haze of #148 and #202 (the look-dev sheet shows today's gradient, the untinted air and this).
    pub const HERO: Atmosphere = Atmosphere {
        tint: HERO_TINT,
        ..Self::PHYSICAL
    };

    /// The air's extinction (scattering and absorption) at height `h` metres, per colour channel.
    pub fn extinction(&self, h: f64) -> [f64; 3] {
        let dens_r = (-h / f64::from(self.rayleigh_height)).exp();
        let dens_m = (-h / f64::from(self.mie_height)).exp();
        let dens_o = (1.0
            - (h - f64::from(self.ozone_height)).abs() / f64::from(self.ozone_half_width))
        .max(0.0);
        std::array::from_fn(|c| {
            f64::from(self.rayleigh[c]) * dens_r
                + f64::from(self.mie_scattering[c] + self.mie_absorption[c]) * dens_m
                + f64::from(self.ozone[c]) * dens_o
        })
    }

    /// The share of the sun's light that the air lets through to a point `height` metres above the
    /// ground when the sun's direction makes the cosine `mu_s` with the up there: 0 once the
    /// Planet is in the way. The CPU's twin of the transmittance table.
    pub fn sun_transmittance(&self, height: f32, mu_s: f32) -> [f32; 3] {
        let bottom = f64::from(self.ground_radius);
        let top = bottom + f64::from(self.top_height);
        let r = bottom + f64::from(height);
        let mu = f64::from(mu_s).clamp(-1.0, 1.0);
        let s = (1.0 - mu * mu).sqrt();
        // The ground blocks the sun when the ray meets it ahead.
        if mu < 0.0 && (bottom - r * s) * (bottom + r * s) >= 0.0 {
            return [0.0; 3];
        }
        let disc = (top - r * s) * (top + r * s);
        let length = (-r * mu + disc.max(0.0).sqrt()).max(0.0);
        // The same stepping as the shader's table: steps that grow from the ground.
        const STEPS: u32 = 4096;
        let curve = 10.0_f64;
        let denom = curve.exp() - 1.0;
        let mut depth = [0.0_f64; 3];
        for k in 0..STEPS {
            let a = length * ((curve * f64::from(k) / f64::from(STEPS)).exp() - 1.0) / denom;
            let b = length * ((curve * f64::from(k + 1) / f64::from(STEPS)).exp() - 1.0) / denom;
            let tm = 0.5 * (a + b);
            let rt = (r * mu + tm).hypot(r * s);
            let ext = self.extinction((rt - bottom).max(0.0));
            for c in 0..3 {
                depth[c] += ext[c] * (b - a);
            }
        }
        depth.map(|d| (-d).exp() as f32)
    }

    /// The sun's radiance on a surface on the ground that faces it (a multiple of the surface's
    /// colour), for a sun at the cosine `mu_s` from the up: what [`Look::sun`](crate::Look) is
    /// without the atmosphere.
    pub fn sun_light_on_ground(&self, mu_s: f32) -> [f32; 3] {
        let t = self.sun_transmittance(self.observer_height, mu_s);
        std::array::from_fn(|c| self.sun_light[c] * t[c])
    }
}

/// What changes with the view: where the camera is in the air, and where the sun is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmosphereView {
    /// The camera's height above the ground, in metres.
    pub camera_height: f64,
    /// The local up and the direction to the sun, unit vectors.
    pub up: [f32; 3],
    pub sun_direction: [f32; 3],
    /// The image's width over its height, which the aerial perspective volume's rays follow.
    pub aspect: f32,
}

impl AtmosphereView {
    /// The cosine of the sun's angle from the zenith.
    pub fn mu_s(&self) -> f32 {
        let [ux, uy, uz] = self.up;
        let [sx, sy, sz] = self.sun_direction;
        (ux * sx + uy * sy + uz * sz).clamp(-1.0, 1.0)
    }
}

/// `Atmo` in `atmosphere_model.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct AtmosphereUniform {
    radii: [f32; 4],
    rayleigh: [f32; 4],
    mie_scattering: [f32; 4],
    mie_absorption: [f32; 4],
    ozone: [f32; 4],
    sun_direction: [f32; 4],
    up: [f32; 4],
    sun_light: [f32; 4],
    ground: [f32; 4],
    tint_horizon: [f32; 4],
    tint_zenith: [f32; 4],
    tint_haze: [f32; 4],
}

impl Atmosphere {
    pub fn uniform(&self, view: &AtmosphereView) -> AtmosphereUniform {
        let v3 = |c: [f32; 3], w: f32| [c[0], c[1], c[2], w];
        // A camera on or under the ground is held half a metre above it: at exactly the ground's
        // radius the horizon is a degenerate angle and the sky-view table has bands in it.
        let camera_radius =
            (f64::from(self.ground_radius) + view.camera_height.max(MIN_CAMERA_HEIGHT)) as f32;
        // The tint of this sun's height: the same in both views, as the tables hold it.
        let tint = self.tint.at(view.mu_s());
        AtmosphereUniform {
            radii: [
                self.ground_radius,
                self.ground_radius + self.top_height,
                camera_radius,
                view.aspect,
            ],
            rayleigh: v3(self.rayleigh, self.rayleigh_height),
            mie_scattering: v3(self.mie_scattering, self.mie_height),
            mie_absorption: v3(self.mie_absorption, self.mie_g),
            ozone: v3(self.ozone, self.ozone_height),
            sun_direction: v3(view.sun_direction, self.ozone_half_width),
            up: v3(view.up, self.observer_height),
            sun_light: v3(self.sun_light, 0.0),
            ground: v3(self.ground_albedo, 0.0),
            tint_horizon: v3(tint.sky_horizon, tint.haze_per_metre),
            tint_zenith: v3(tint.sky_zenith, 0.0),
            tint_haze: v3(tint.haze_colour, 0.0),
        }
    }
}

/// The tables' textures and the kernels that fill them.
pub struct AtmosphereTables {
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    transmittance: Table,
    multiscatter: Table,
    sky_view: Table,
    ground_sky: Table,
    aerial_scatter: Table,
    aerial_transmittance: Table,
    /// The cumulative distribution of the sun-free sky's brightness (see `sky.wgsl`).
    cdf: wgpu::Buffer,
    kernels: Kernels,
    /// What the once-only and the slow tables were filled for: the air, and the sun's cosine.
    filled_for: Option<Atmosphere>,
    ground_filled_for: Option<(Atmosphere, u32)>,
}

/// A table's texture and a view of all of it.
struct Table {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// A table that tests read back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableId {
    Transmittance,
    MultiScatter,
    SkyView,
    /// The sun-free sky, by the azimuth from the sun and the sine of the elevation.
    SunFreeSky,
}

struct Kernel {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

struct Kernels {
    transmittance: Kernel,
    multiscatter: Kernel,
    sky_view: Kernel,
    ground_sky: Kernel,
    cdf: Kernel,
    aerial: Kernel,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn table(
    device: &wgpu::Device,
    label: &str,
    size: wgpu::Extent3d,
    dimension: wgpu::TextureDimension,
) -> Table {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension,
        format: FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Table { texture, view }
}

fn table_2d(device: &wgpu::Device, label: &str, (w, h): (u32, u32)) -> Table {
    table(
        device,
        label,
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        wgpu::TextureDimension::D2,
    )
}

fn table_3d(device: &wgpu::Device, label: &str, side: u32) -> Table {
    table(
        device,
        label,
        wgpu::Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: side,
        },
        wgpu::TextureDimension::D3,
    )
}

fn sampled(binding: u32, dimension: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    layout_entry(
        binding,
        wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dimension,
            multisampled: false,
        },
    )
}

fn written(binding: u32, dimension: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    layout_entry(
        binding,
        wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: FORMAT,
            view_dimension: dimension,
        },
    )
}

fn kernel(device: &wgpu::Device, name: &str, entries: &[wgpu::BindGroupLayoutEntry]) -> Kernel {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(name),
        entries,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("atmosphere"),
        source: wgpu::ShaderSource::Wgsl(crate::shaders::ATMOSPHERE.into()),
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
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    Kernel { layout, pipeline }
}

impl AtmosphereTables {
    pub fn new(device: &wgpu::Device) -> Self {
        use wgpu::TextureViewDimension::{D2, D3};
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atmosphere"),
            size: size_of::<AtmosphereUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atmosphere tables"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let cdf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sky distribution"),
            size: SKY_CDF_WORDS.div_ceil(4) * 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_entry =
            |binding| layout_entry(binding, buffer_binding(wgpu::BufferBindingType::Uniform));
        let sampler_entry = |binding| {
            layout_entry(
                binding,
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            )
        };
        let kernels = Kernels {
            transmittance: kernel(
                device,
                "transmittance_kernel",
                &[uniform_entry(0), written(5, D2)],
            ),
            multiscatter: kernel(
                device,
                "multiscatter_kernel",
                &[
                    uniform_entry(0),
                    sampler_entry(2),
                    sampled(3, D2),
                    written(6, D2),
                ],
            ),
            sky_view: kernel(
                device,
                "sky_view_kernel",
                &[
                    uniform_entry(0),
                    sampler_entry(2),
                    sampled(3, D2),
                    sampled(4, D2),
                    written(7, D2),
                ],
            ),
            ground_sky: kernel(
                device,
                "ground_sky_kernel",
                &[
                    uniform_entry(0),
                    sampler_entry(2),
                    sampled(3, D2),
                    sampled(4, D2),
                    written(8, D2),
                ],
            ),
            cdf: kernel(
                device,
                "cdf_kernel",
                &[
                    sampled(11, D2),
                    layout_entry(
                        12,
                        buffer_binding(wgpu::BufferBindingType::Storage { read_only: false }),
                    ),
                ],
            ),
            aerial: kernel(
                device,
                "aerial_kernel",
                &[
                    uniform_entry(0),
                    uniform_entry(1),
                    sampler_entry(2),
                    sampled(3, D2),
                    sampled(4, D2),
                    written(9, D3),
                    written(10, D3),
                ],
            ),
        };
        Self {
            uniform,
            sampler,
            transmittance: table_2d(device, "transmittance", TRANSMITTANCE_SIZE),
            multiscatter: table_2d(
                device,
                "multi-scattering",
                (MULTISCATTER_SIZE, MULTISCATTER_SIZE),
            ),
            sky_view: table_2d(device, "sky view", SKY_VIEW_SIZE),
            ground_sky: table_2d(device, "sun-free sky", GROUND_SKY_SIZE),
            aerial_scatter: table_3d(device, "aerial perspective (added)", AERIAL_SIZE),
            aerial_transmittance: table_3d(device, "aerial perspective (let through)", AERIAL_SIZE),
            cdf,
            kernels,
            filled_for: None,
            ground_filled_for: None,
        }
    }

    /// The layout entries of the bindings 11 to 17 of the views' bind groups, which
    /// [`AtmosphereTables::entries`] fills (`sky.wgsl`).
    pub fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 7] {
        use wgpu::TextureViewDimension::{D2, D3};
        [
            layout_entry(11, buffer_binding(wgpu::BufferBindingType::Uniform)),
            sampled(12, D2),
            sampled(13, D3),
            sampled(14, D3),
            sampled(15, D2),
            layout_entry(16, buffer_binding(STORAGE_READ)),
            layout_entry(
                17,
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            ),
        ]
    }

    /// The bind group entries for [`AtmosphereTables::layout_entries`].
    pub fn entries(&self) -> [wgpu::BindGroupEntry<'_>; 7] {
        let view = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        [
            wgpu::BindGroupEntry {
                binding: 11,
                resource: self.uniform.as_entire_binding(),
            },
            view(12, &self.sky_view.view),
            view(13, &self.aerial_scatter.view),
            view(14, &self.aerial_transmittance.view),
            view(15, &self.ground_sky.view),
            wgpu::BindGroupEntry {
                binding: 16,
                resource: self.cdf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 17,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            },
        ]
    }

    /// The tables for `air` seen as `view`, as one timed pass, "atmosphere": the transmittance and
    /// multi-scattering tables only when the air changed, the sun-free sky when the air or the
    /// sun's height did, and the sky-view and aerial perspective tables every time (the camera
    /// moves). `camera` is the buffer of the camera's uniform, whose rays the aerial perspective
    /// volume is of. The uniform is written now, so the pass must be submitted before the next
    /// call.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &wgpu::Buffer,
        air: &Atmosphere,
        view: &AtmosphereView,
    ) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&air.uniform(view)));
        let once = self.filled_for != Some(*air);
        let ground_key = (*air, view.mu_s().to_bits());
        let ground = once || self.ground_filled_for != Some(ground_key);
        self.filled_for = Some(*air);
        self.ground_filled_for = Some(ground_key);

        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let tex = |view| wgpu::BindingResource::TextureView(view);
        let uniform = self.uniform.as_entire_binding();
        let sampler = wgpu::BindingResource::Sampler(&self.sampler);
        let group = |kernel: &Kernel, entries: &[wgpu::BindGroupEntry]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &kernel.layout,
                entries,
            })
        };
        let k = &self.kernels;
        let transmittance = group(
            &k.transmittance,
            &[
                entry(0, uniform.clone()),
                entry(5, tex(&self.transmittance.view)),
            ],
        );
        let multiscatter = group(
            &k.multiscatter,
            &[
                entry(0, uniform.clone()),
                entry(2, sampler.clone()),
                entry(3, tex(&self.transmittance.view)),
                entry(6, tex(&self.multiscatter.view)),
            ],
        );
        let sky_view = group(
            &k.sky_view,
            &[
                entry(0, uniform.clone()),
                entry(2, sampler.clone()),
                entry(3, tex(&self.transmittance.view)),
                entry(4, tex(&self.multiscatter.view)),
                entry(7, tex(&self.sky_view.view)),
            ],
        );
        let ground_sky = group(
            &k.ground_sky,
            &[
                entry(0, uniform.clone()),
                entry(2, sampler.clone()),
                entry(3, tex(&self.transmittance.view)),
                entry(4, tex(&self.multiscatter.view)),
                entry(8, tex(&self.ground_sky.view)),
            ],
        );
        let cdf = group(
            &k.cdf,
            &[
                entry(11, tex(&self.ground_sky.view)),
                entry(12, self.cdf.as_entire_binding()),
            ],
        );
        let aerial = group(
            &k.aerial,
            &[
                entry(0, uniform.clone()),
                entry(1, camera.as_entire_binding()),
                entry(2, sampler.clone()),
                entry(3, tex(&self.transmittance.view)),
                entry(4, tex(&self.multiscatter.view)),
                entry(9, tex(&self.aerial_scatter.view)),
                entry(10, tex(&self.aerial_transmittance.view)),
            ],
        );

        let mut pass = profiler.compute_pass(encoder, "atmosphere");
        let mut run = |kernel: &Kernel, group: &wgpu::BindGroup, (x, y): (u32, u32)| {
            pass.set_pipeline(&kernel.pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        };
        if once {
            run(
                &k.transmittance,
                &transmittance,
                (
                    TRANSMITTANCE_SIZE.0.div_ceil(8),
                    TRANSMITTANCE_SIZE.1.div_ceil(8),
                ),
            );
            run(
                &k.multiscatter,
                &multiscatter,
                (MULTISCATTER_SIZE.div_ceil(8), MULTISCATTER_SIZE.div_ceil(8)),
            );
        }
        if ground {
            run(
                &k.ground_sky,
                &ground_sky,
                (GROUND_SKY_SIZE.0.div_ceil(8), GROUND_SKY_SIZE.1.div_ceil(8)),
            );
            run(&k.cdf, &cdf, (1, 1));
        }
        run(
            &k.sky_view,
            &sky_view,
            (SKY_VIEW_SIZE.0.div_ceil(8), SKY_VIEW_SIZE.1.div_ceil(8)),
        );
        run(
            &k.aerial,
            &aerial,
            (AERIAL_SIZE.div_ceil(8), AERIAL_SIZE.div_ceil(8)),
        );
    }

    /// The texture of a table that tests read back, and its size.
    pub(crate) fn texture(&self, id: TableId) -> (&wgpu::Texture, (u32, u32)) {
        match id {
            TableId::Transmittance => (&self.transmittance.texture, TRANSMITTANCE_SIZE),
            TableId::MultiScatter => (
                &self.multiscatter.texture,
                (MULTISCATTER_SIZE, MULTISCATTER_SIZE),
            ),
            TableId::SkyView => (&self.sky_view.texture, SKY_VIEW_SIZE),
            TableId::SunFreeSky => (&self.ground_sky.texture, GROUND_SKY_SIZE),
        }
    }

    /// The buffer of the sky's cumulative distribution, for tests that read it back.
    pub(crate) fn cdf(&self) -> &wgpu::Buffer {
        &self.cdf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uniform_is_the_size_the_shader_declares() {
        // Twelve vec4s: `Atmo` in atmosphere_model.wgsl.
        assert_eq!(size_of::<AtmosphereUniform>(), 12 * 16);
    }

    #[test]
    fn the_shaders_and_the_brick_store_count_voxels_to_the_metre_alike() {
        // sky.wgsl's `AIR_VOXELS_PER_METRE` is the shaders' copy of the one definition (ADR 0007).
        let shader = include_str!("shaders/sky.wgsl");
        assert!(shader.contains(&format!(
            "const AIR_VOXELS_PER_METRE = {:?};",
            brick_store::VOXELS_PER_METRE
        )));
    }

    #[test]
    fn the_sun_is_whole_at_noon_reddened_at_the_horizon_and_gone_at_night() {
        let air = Atmosphere::HERO;
        let noon = air.sun_transmittance(2.0, 1.0);
        let low = air.sun_transmittance(2.0, 0.139);
        let night = air.sun_transmittance(2.0, -0.2);
        // Overhead the air lets most of it through, and the most of the red.
        assert!(
            noon[0] > 0.7 && noon[0] > noon[1] && noon[1] > noon[2],
            "{noon:?}"
        );
        // Low, less of every colour, and the blue the least: the sun is redder.
        for c in 0..3 {
            assert!(low[c] < noon[c], "{c}: {low:?} {noon:?}");
        }
        assert!(
            low[0] / low[2] > 2.0 * noon[0] / noon[2],
            "{low:?} {noon:?}"
        );
        assert_eq!(night, [0.0; 3]);
    }

    #[test]
    fn the_sun_is_bright_just_above_the_horizon_and_gone_just_below_it() {
        let air = Atmosphere::HERO;
        let above = air.sun_transmittance(2.0, 0.01);
        let below = air.sun_transmittance(2.0, -0.01);
        assert!(above[0] > 0.0, "{above:?}");
        assert_eq!(below, [0.0; 3]);
    }
}
