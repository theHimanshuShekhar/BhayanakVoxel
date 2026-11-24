//! wgpu rendering on the Vulkan backend: compute-shader DDA through the Planet's bricks.
//!
//! `trace(ray) -> Hit` in `shaders/trace.wgsl` is the single entry point the renderer calls to
//! find what a ray hits. [`reference`] is its CPU twin, used by tests. A window ([`Renderer`])
//! and an offscreen target ([`Offscreen`]) share one [`tracer::Tracer`], so tests render with the
//! code that the window runs.
//!
//! Two ways of drawing share that `trace()`: primary visibility, and the reference path tracer
//! (`shaders/pathtrace.wgsl`), which accumulates samples while nothing changes and is the ground
//! truth for later lighting techniques. [`accumulation`] decides when it starts over.
//!
//! Primary visibility traces at an internal resolution, half of the output's per axis by default,
//! and writes a G-buffer beside the image; [`upscale`] builds the presented image from it and
//! from the frames before (TAAU). The reference path tracer needs no upscaling.
//!
//! Every pass is timed on the GPU where the device allows ([`Profiler`], [`timing`]), and
//! [`overlay`] draws those times, with the render settings, as an egui panel. [`DebugView`]s show
//! one input of the shading at a time.

pub mod accumulation;
pub mod atmosphere;
pub mod batch;
pub mod camera;
mod composite;
pub mod cpu;
pub mod daylight;
mod debug_view;
pub mod denoise;
pub mod emitters;
mod gpu_materials;
pub mod hw;
pub mod lod;
pub mod look;
mod offscreen;
pub mod overlay;
pub mod post;
mod profiler;
pub mod reference;
#[cfg(test)]
mod sampling_tests;
pub mod scene;
mod shaders;
pub mod still;
pub mod sun;
pub mod timing;
pub mod tracer;
pub mod upscale;
mod window;

use std::error::Error;

pub use camera::Camera;
pub use daylight::{DaySky, Daylight, Drive, LocalFrame, Moon, Night};
pub use debug_view::DebugView;
pub use denoise::Denoise;
pub use hw::HwUpdate;
pub use look::Look;
pub use offscreen::{DrawTimes, FrameDrawn, GpuInfo, GpuPasses, Image, Offscreen, UpdateTimes};
pub use post::{NormalBias, Stages, ToneCurve};
pub use profiler::Profiler;
pub use scene::{
    Lengths, PatchCopy, Rebuild, RingPatch, RingsFull, Room, Scene, SceneUpdate, UploadCost,
};
pub use sun::Sun;
pub use timing::Timings;
pub use tracer::{
    DEFAULT_SHADOW_RAYS, EmitterEstimator, Fault, MAX_SHADOW_RAYS, ReferenceOptions, ShadowWeight,
    SkyEstimator,
};
pub use upscale::Resolution;
pub use window::{Mode, Renderer};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Vulkan is the only supported backend (ADR 0001).
fn create_instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    wgpu::Instance::new(desc)
}

/// The one Vulkan instance of a process's headless renderers (`Offscreen`), made on first use.
///
/// An instance per renderer is a `vkCreateInstance` per renderer, and with VK_EXT_debug_utils on
/// (wgpu's debug builds name every object) the Vulkan loader, up to SDK 1.4.341 (1.4.309 here; fixed
/// upstream in 1.4.350 by Vulkan-Loader 5ee27b30c0, which drops `loader_global_instance_list_lock`),
/// crashes when one thread creates an instance while another names an object:
/// `vkSetDebugUtilsObjectNameEXT` walks the loader's global list of instances and reads the
/// `icd_terms` of one that is on the list but still being built. Parallel tests make renderers in
/// parallel, so they share one instance (#194).
///
/// This removes the instance-creation race and no more. A narrower race at teardown may remain:
/// `vkDestroyDevice` holds only `loader_lock` while it frees a device, and the same walk, under the
/// instance-list lock, reads every device. It is not new and did not show in 30 stress runs. The
/// workaround can go once the loaders in use are 1.4.350 or later.
fn shared_instance() -> &'static wgpu::Instance {
    static INSTANCE: std::sync::OnceLock<wgpu::Instance> = std::sync::OnceLock::new();
    INSTANCE.get_or_init(create_instance)
}

/// How rays find the Planet's bricks (#50): the compute-shader DDA, which is the default, the
/// fallback and the CPU-checkable reference; or hardware ray queries over the bricks' boxes
/// ([`hw`]), which need an adapter with `EXPERIMENTAL_RAY_QUERY`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RtBackend {
    #[default]
    Compute,
    /// Ray queries. A shadow ray (the sun's, in `shade.wgsl` and the GI's) ends at the first box
    /// it hits that has a Voxel in its way (`RAY_FLAG_TERMINATE_ON_FIRST_HIT`): all it needs to know
    /// is that something is there, as the compute walk, which goes front to back, does.
    Hardware,
    /// Ray queries, with shadow rays that look for the nearest hit like any other ray: what
    /// `Hardware` was before it had the early-out, kept to tell what the early-out is worth.
    HardwareNearest,
}

impl RtBackend {
    pub fn name(self) -> &'static str {
        match self {
            RtBackend::Compute => "compute",
            RtBackend::Hardware => "hw",
            RtBackend::HardwareNearest => "hw-nearest",
        }
    }

    /// Whether this is one of the ray-query backends.
    pub fn is_hardware(self) -> bool {
        self != RtBackend::Compute
    }
}

/// What was asked for and what runs: the hardware backend is chosen only if the adapter can do ray
/// queries, and anything else runs the compute path with no error, `fallback` saying why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtChoice {
    pub requested: RtBackend,
    pub active: RtBackend,
    pub fallback: Option<String>,
}

/// The backend to run for a request, on an adapter with `features` and `limits`.
pub fn choose_backend(
    requested: RtBackend,
    features: wgpu::Features,
    limits: &wgpu::Limits,
) -> RtChoice {
    let fallback = if !requested.is_hardware() {
        None
    } else if !features.contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY) {
        Some("the adapter has no ray queries (EXPERIMENTAL_RAY_QUERY)".to_string())
    } else if limits.max_blas_primitive_count == 0 || limits.max_tlas_instance_count == 0 {
        Some("the adapter's acceleration structure limits are 0".to_string())
    } else {
        None
    };
    RtChoice {
        requested,
        active: if fallback.is_none() {
            requested
        } else {
            RtBackend::Compute
        },
        fallback,
    }
}

/// Requests a device on a Vulkan adapter, presentable to `surface` if one is given. With
/// `timers`, it has timestamp queries if the adapter does.
fn request_gpu(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    timers: bool,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let (adapter, device, queue, _) =
        request_gpu_for(instance, surface, timers, RtBackend::Compute, false)?;
    Ok((adapter, device, queue))
}

/// [`request_gpu`] for a backend: with a hardware one, a device with ray queries if the adapter has
/// them, and the choice made (see [`choose_backend`]). With `hide_ray_queries` the adapter is
/// taken to have none, for testing the fallback through the real device request.
fn request_gpu_for(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    timers: bool,
    backend: RtBackend,
    hide_ray_queries: bool,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue, RtChoice)> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface,
        ..Default::default()
    }))?;
    let mut features = adapter.features();
    if hide_ray_queries {
        features.remove(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
    }
    let choice = choose_backend(backend, features, &adapter.limits());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("bhayanak device"),
        // Pass timers, where the adapter has them; without them the timers read "n/a".
        required_features: if timers {
            adapter.features() & profiler::FEATURES
        } else {
            wgpu::Features::empty()
        } | if choice.active.is_hardware() {
            wgpu::Features::EXPERIMENTAL_RAY_QUERY
        } else {
            wgpu::Features::empty()
        },
        // The Planet's bricks are one big storage buffer, well over the portable 128 MiB limit.
        required_limits: adapter.limits(),
        experimental_features: if choice.active.is_hardware() {
            // SAFETY: ray queries are experimental in wgpu 30 (ADR 0001): they may hit bugs of
            // wgpu's that are undefined behaviour from safe code. Opted into only for the
            // hardware backend, which is a spike (#50) and never the default.
            unsafe { wgpu::ExperimentalFeatures::enabled() }
        } else {
            wgpu::ExperimentalFeatures::disabled()
        },
        ..Default::default()
    }))?;
    Ok((adapter, device, queue, choice))
}

/// Submits `encoder` and returns the first `size` bytes of `staging`, a `MAP_READ` buffer that
/// the encoder copies into.
fn finish_and_read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
    staging: &wgpu::Buffer,
    size: u64,
) -> Vec<u8> {
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    staging
        .slice(..size)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("receiver outlives the map");
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll failed");
    receiver
        .recv()
        .expect("map callback ran")
        .expect("staging buffer maps");
    let bytes = staging
        .slice(..size)
        .get_mapped_range()
        .expect("mapped range")
        .to_vec();
    staging.unmap();
    bytes
}

fn staging_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Copies `size` bytes of `buffer` (which needs `COPY_SRC`) to the CPU.
fn read_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut encoder: wgpu::CommandEncoder,
    buffer: &wgpu::Buffer,
    size: u64,
) -> Vec<u8> {
    let staging = staging_buffer(device, size);
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    finish_and_read(device, queue, encoder, &staging, size)
}

/// A linear colour component (0 is black, 1 is white) as the 8-bit image holds it, with the true
/// sRGB curve (IEC 61966-2-1), as 0 to 1: the function `encode` in `common.wgsl` computes, for
/// both views. Not clamped above 1, which the 8-bit image does.
pub fn srgb_encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// The linear component of an sRGB-encoded one (0 to 1): the inverse of [`srgb_encode`].
pub fn srgb_decode(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a Vulkan driver; CI uses lavapipe (mesa-vulkan-drivers).
    #[test]
    fn vulkan_device_can_be_created_without_a_window() {
        let (adapter, _device, _queue) = request_gpu(shared_instance(), None, true).unwrap();
        assert_eq!(adapter.get_info().backend, wgpu::Backend::Vulkan);
    }
}
