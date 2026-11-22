//! Primary's denoiser (#55): the CPU side of `shaders/denoise.wgsl`, and the textures it keeps.
//!
//! Temporal accumulation with clamping, grown to A-SVGF (adaptive temporal, variance-guided
//! à-trous), on the two noisy signals of Primary's light: the sun's weight (`shadows`) and the
//! incident light of the bounce, the sky and the emitters (`gi_trace`). It runs at the traced
//! resolution **before the upscaler**, on the signals as they are before the surface's colour
//! multiplies them, so the Voxels' detail is never blurred and the upscaler (which builds the
//! output image from the jittered frames and averages them) is not duplicated: this history is of
//! light, per traced pixel, carried by the hit's geometry; that one is of colours, per output pixel,
//! carried by the same geometry and clamped to what each frame shows.
//!
//! The frame is, in one timed pass called `denoise` (the tracer opens it):
//! 1. `gi_samples` (`gi.wgsl`): the frame's samples at every pixel (the lattice's four points a pixel,
//!    blended, and the sun's weight), into [`Buffers::samples`];
//! 2. `denoise_temporal`: reprojects the last frame's history (the accumulated light, its second
//!    moments, the geometry), judges it against the pixel's surface and neighbourhood and blends the
//!    samples in; the result is the new history;
//! 3. `denoise_atrous` four times, with steps 1, 2, 4, 8: the variance-guided blur.
//!
//! [`Denoise`] chooses how much of that runs. With the denoiser off only the first kernel does, and
//! `shade_pass` lights the image with the frame's samples as they are, which is what it did before
//! the denoiser.
//!
//! **When a history is valid** is the upscaler's to say, and is the same here: its [`History`]
//! (`upscale.rs`) starts over on the first frame, a new size, a new sun (the sun's weights of the old
//! one must not be carried into the new, #52), a change of look, of stage, of the shadow rays, and for
//! a debug view. An Edit does not reset it: the pixels whose surface changed fail the geometry check
//! by themselves, and the cells that an Edit changes the light of keep a short history for as long as
//! the radiance cache forgets fast ([`SETTLING_HISTORY`], in the cache's own box,
//! [`crate::Tracer::edit_made`]). A fast turn leaves the pixels it uncovers with no history, and the
//! filter blurs them harder for being new (their variance is the neighbourhood's).
//!
//! **The one place where the sun resets the history** is `Tracer::sun_changed` (a "the sun jumped":
//! it was turned or another one was set), which bumps the tracer's `sun_epoch`; the upscaler's
//! `History::begin` then starts over, and the `has_history` word of [`DenoiseUniform`] it gives the
//! denoiser is 0 for that frame. Nothing else in the denoiser looks at the sun. A sun that moves a
//! little every frame (a day and night cycle, #203) must not call `sun_changed` for each step, or
//! the history would never be longer than a frame: it calls it for a jump, and the slow drift is
//! left to the temporal kernel's own checks (the neighbourhood clamp and the adaptive reject follow
//! light that changes by a few percent a frame, which the settled history lags by a frame or two).
//!
//! The filter runs on every pixel, a long history's too (there was a gate that left a quiet one
//! alone; the 4090's pit and room said it cost more than it saved): what a long history has left is
//! the radiance cache's own error, which is the same from frame to frame and so is not in the
//! history's variance, and only the filter averages it.
//!
//! [`History`]: crate::upscale

use bytemuck::{Pod, Zeroable};

use crate::camera::CameraUniform;
use crate::shaders;
use crate::tracer::{Plane, buffer_binding, layout_entry, plane_entry, storage_texture};
use crate::upscale;

/// The frame's samples: the incident light in xyz and the sun's weight in w. In `f32`, not in a
/// half: with the denoiser off these are what `shade_pass` lights the image with, and a half's steps
/// (a few parts in 10 000) would turn the last bit of float noise between two renders of the same
/// scene (a patched scene and a packed one) into a step that the 8-bit image can show.
pub const SAMPLES_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// The accumulated light, as the samples have it, in `f32` so that an average of 32 frames is not
/// rounded away.
pub const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// The second moments of the light's luminance and of the sun's weight, and the number of samples in
/// the history of the pixel.
pub const MOMENTS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// The geometry of the frame: the normal the pixel is lit with (octahedral, two 16-bit numbers in
/// x) and the distance to its hit as an `f32`'s bits in y.
pub const GEO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;
/// The variance of the estimate of each signal.
pub const VARIANCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
/// What the filter's iterations make.
pub const SIGNAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The most samples a pixel's history has: an exponential average of 1/33 of the newest frame, at
/// the least, once it is long.
pub const MAX_HISTORY: u32 = 32;
/// The most that the history has in the cells that an Edit or a new sun is changing the light of
/// (the radiance cache's boost box and frames): the new light takes a fifth of the pixel a frame,
/// and a dig's old light is a tenth of it after 10 frames.
pub const SETTLING_HISTORY: u32 = 4;
/// The iterations of the filter: steps 1, 2, 4 and 8.
pub const ATROUS_ITERATIONS: usize = 4;

/// How much of the denoiser runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Denoise {
    /// None of it: Primary's light is the frame's samples as they are (the fallback, and what the
    /// tests with answers worked out from the samples read).
    Off,
    /// Temporal accumulation with clamping only: the history's mean, with no filter.
    Temporal,
    /// Temporal accumulation and the variance-guided filter: A-SVGF. What the game draws.
    #[default]
    Full,
}

impl Denoise {
    pub const ALL: [Denoise; 3] = [Denoise::Off, Denoise::Temporal, Denoise::Full];

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Temporal => "temporal",
            Self::Full => "full",
        }
    }
}

/// The textures of the denoiser, at the traced resolution.
pub struct Buffers {
    /// The frame's samples: the incident light and the sun's weight.
    samples: Plane,
    /// The history, two of each, taking turns: the accumulated light, the moments and the count of
    /// samples, the geometry.
    accum: [Plane; 2],
    moments: [Plane; 2],
    geo: [Plane; 2],
    /// What the filter reads and writes, two of each, taking turns, and the temporal kernel's
    /// variance.
    signal: [Plane; 2],
    variance: [Plane; 2],
}

impl Buffers {
    pub fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        let two = |label: &str, format| {
            [0, 1].map(|i| Plane::new(device, &format!("{label} {i}"), size, format))
        };
        Self {
            samples: Plane::new(device, "denoise samples", size, SAMPLES_FORMAT),
            accum: two("denoise accumulated light", ACCUM_FORMAT),
            moments: two("denoise moments", MOMENTS_FORMAT),
            geo: two("denoise geometry", GEO_FORMAT),
            signal: two("denoise signal", SIGNAL_FORMAT),
            variance: two("denoise variance", VARIANCE_FORMAT),
        }
    }

    /// The frame's samples: what `shade_pass` lights the image with when the denoiser is off.
    pub(crate) fn samples(&self) -> &Plane {
        &self.samples
    }

    /// What `shade_pass` lights the image with in `mode`, after a frame that wrote history image
    /// `written` (`None` for a frame that had no denoiser, `encode_primary`'s): the incident light in
    /// xyz and the sun's weight in w.
    pub(crate) fn result(&self, mode: Denoise, written: Option<usize>) -> &Plane {
        match (mode, written) {
            (Denoise::Off, _) | (_, None) => &self.samples,
            (Denoise::Temporal, Some(written)) => &self.accum[written],
            // The iterations alternate between the two signal textures, from the first.
            (Denoise::Full, Some(_)) => &self.signal[(ATROUS_ITERATIONS + 1) % 2],
        }
    }

    /// The variance of the estimate that the temporal kernel made last (x: the light's luminance, y:
    /// the sun's weight), for tests.
    pub(crate) fn temporal_variance(&self) -> &Plane {
        &self.variance[0]
    }
}

/// `Denoise` in `denoise.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct DenoiseUniform {
    camera: CameraUniform,
    /// The camera of the frame before: the one the history is of.
    previous: CameraUniform,
    /// How far this camera is from that one, in Voxels; the fourth lane is unused.
    shift: [f32; 4],
    /// 1 if there is a history; 1 if the smooth normals pass ran; the most samples a history has
    /// and the most it has in the cells that are settling, both as `f32`'s bits.
    params: [u32; 4],
    /// The Voxels whose light is being changed (an Edit's box, or everywhere for a new sun): the
    /// first, with 1 in w if the box is on, and one past the last.
    boost_min: [i32; 4],
    boost_max: [i32; 4],
}

/// One frame's share of the denoiser's inputs.
pub(crate) struct Frame {
    uniform: DenoiseUniform,
    /// The history image to read and the one to write: 0 or 1.
    read: usize,
    write: usize,
}

impl Frame {
    /// The frame that goes with the upscaler's `frame` (same cameras, same history: it starts over
    /// when the upscaler's does). `smooth_normals` says whether the pass of the smooth normals ran,
    /// and `boost` is the box of Voxels whose light an Edit or a new sun is changing, while it is.
    pub fn new(
        frame: &upscale::Frame,
        smooth_normals: bool,
        boost: Option<([i32; 3], [i32; 3])>,
    ) -> Self {
        let ([lx, ly, lz], [hx, hy, hz]) = boost.unwrap_or(([0; 3], [0; 3]));
        Self {
            uniform: DenoiseUniform {
                camera: frame.camera(),
                previous: frame.previous(),
                shift: frame.shift(),
                params: [
                    u32::from(frame.has_history()),
                    u32::from(smooth_normals),
                    (MAX_HISTORY as f32).to_bits(),
                    (SETTLING_HISTORY as f32).to_bits(),
                ],
                boost_min: [lx, ly, lz, i32::from(boost.is_some())],
                boost_max: [hx, hy, hz, 0],
            },
            read: frame.read(),
            write: frame.write(),
        }
    }

    /// The history image this frame writes.
    pub fn written(&self) -> usize {
        self.write
    }
}

/// The denoiser's kernels: the pipelines of `denoise.wgsl` over the frame's samples and the history.
pub(crate) struct Pass {
    temporal_layout: wgpu::BindGroupLayout,
    atrous_layout: wgpu::BindGroupLayout,
    temporal: wgpu::ComputePipeline,
    atrous: [wgpu::ComputePipeline; ATROUS_ITERATIONS],
    uniform: wgpu::Buffer,
}

impl Pass {
    pub fn new(device: &wgpu::Device) -> Self {
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
        let loaded = wgpu::TextureSampleType::Float { filterable: false };
        let uint = wgpu::TextureSampleType::Uint;
        let uniform_entry = || layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform));
        let temporal_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("denoise temporal"),
            entries: &[
                uniform_entry(),
                texture(1, uint),
                texture(2, uint),
                texture(3, loaded),
                texture(4, loaded),
                texture(5, loaded),
                texture(6, loaded),
                texture(7, uint),
                layout_entry(8, storage_texture(ACCUM_FORMAT)),
                layout_entry(9, storage_texture(MOMENTS_FORMAT)),
                layout_entry(10, storage_texture(GEO_FORMAT)),
                layout_entry(11, storage_texture(VARIANCE_FORMAT)),
            ],
        });
        let atrous_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("denoise atrous"),
            entries: &[
                uniform_entry(),
                texture(12, loaded),
                texture(13, loaded),
                texture(14, uint),
                layout_entry(15, storage_texture(SIGNAL_FORMAT)),
                layout_entry(16, storage_texture(VARIANCE_FORMAT)),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("denoise"),
            source: wgpu::ShaderSource::Wgsl(shaders::DENOISE.into()),
        });
        let pipeline = |layout: &wgpu::BindGroupLayout, entry: &str, step: f64| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("ATROUS_STEP", step)],
                    ..Default::default()
                },
                cache: None,
            })
        };
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("denoise"),
            size: size_of::<DenoiseUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            temporal: pipeline(&temporal_layout, "denoise_temporal", 1.0),
            atrous: std::array::from_fn(|i| {
                pipeline(&atrous_layout, "denoise_atrous", f64::from(1u32 << i))
            }),
            temporal_layout,
            atrous_layout,
            uniform,
        }
    }

    /// Records the temporal accumulation and, for [`Denoise::Full`], the filter, into `pass` (the
    /// tracer's `denoise` pass, which has made the frame's samples already): from the samples, the
    /// G-buffer, the surface and the smooth normals, with the history of `frame`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::ComputePass<'_>,
        frame: &Frame,
        mode: Denoise,
        size: (u32, u32),
        buffers: &Buffers,
        gbuffer: &Plane,
        surface: &Plane,
        normals: &Plane,
    ) {
        if mode == Denoise::Off {
            return;
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&frame.uniform));
        let (read, write) = (frame.read, frame.write);
        let uniform = wgpu::BindGroupEntry {
            binding: 0,
            resource: self.uniform.as_entire_binding(),
        };
        let temporal = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("denoise temporal"),
            layout: &self.temporal_layout,
            entries: &[
                uniform.clone(),
                plane_entry(1, gbuffer),
                plane_entry(2, surface),
                plane_entry(3, normals),
                plane_entry(4, &buffers.samples),
                plane_entry(5, &buffers.accum[read]),
                plane_entry(6, &buffers.moments[read]),
                plane_entry(7, &buffers.geo[read]),
                plane_entry(8, &buffers.accum[write]),
                plane_entry(9, &buffers.moments[write]),
                plane_entry(10, &buffers.geo[write]),
                plane_entry(11, &buffers.variance[0]),
            ],
        });
        let (width, height) = size;
        let groups = (width.div_ceil(8), height.div_ceil(8));
        pass.set_pipeline(&self.temporal);
        pass.set_bind_group(0, &temporal, &[]);
        pass.dispatch_workgroups(groups.0, groups.1, 1);
        if mode != Denoise::Full {
            return;
        }
        // The iterations alternate between the two signal textures and the two variance ones, the
        // first reading the accumulated light and the temporal kernel's variance.
        for (i, pipeline) in self.atrous.iter().enumerate() {
            let (signal_in, variance_in) = match i {
                0 => (&buffers.accum[write], &buffers.variance[0]),
                _ => (&buffers.signal[(i + 1) % 2], &buffers.variance[i % 2]),
            };
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("denoise atrous"),
                layout: &self.atrous_layout,
                entries: &[
                    uniform.clone(),
                    plane_entry(12, signal_in),
                    plane_entry(13, variance_in),
                    plane_entry(14, &buffers.geo[write]),
                    plane_entry(15, &buffers.signal[i % 2]),
                    plane_entry(16, &buffers.variance[(i + 1) % 2]),
                ],
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
    }
}

#[cfg(test)]
#[path = "denoise_tests.rs"]
mod tests;
