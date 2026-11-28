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
pub use hw::{HwPhases, HwUpdate};
pub use look::Look;
pub use offscreen::{
    BACKEND_VARIABLE, DrawTimes, FrameDrawn, GpuInfo, GpuPasses, Image, Offscreen, UpdateTimes,
};
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

/// How rays find the Planet's bricks (#50): hardware ray queries over the bricks' boxes ([`hw`]),
/// which need an adapter with `EXPERIMENTAL_RAY_QUERY` and are **the default** wherever the
/// adapter has them (the owner, 2026-11-21: "RT default but customizable to fallback to
/// compute"); or the compute-shader DDA, which is the fallback, the player's setting for a GPU
/// that ray queries do not suit, and the CPU-checkable reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RtBackend {
    /// The compute-shader DDA.
    Compute,
    /// Ray queries. A shadow ray (the sun's, in `shade.wgsl` and the GI's) ends at the first box
    /// it hits that has a Voxel in its way (`RAY_FLAG_TERMINATE_ON_FIRST_HIT`): all it needs to know
    /// is that something is there, as the compute walk, which goes front to back, does.
    #[default]
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
#[cfg(test)]
fn request_gpu(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    timers: bool,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let (adapter, device, queue, ..) =
        request_gpu_for(instance, surface, timers, RtBackend::Compute, false)?;
    Ok((adapter, device, queue))
}

/// What a device's ray-query features are, and why: the adapter's, the setting's and a failed
/// request's part in it (#260). The renderer reads it to say what the setting does now and at the
/// next launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtDevice {
    /// Why this adapter cannot run ray queries, if it cannot (or the run is held to none, as
    /// `--profile low-end` does): the compute backend is then the only one.
    pub unavailable: Option<String>,
    /// Whether the device was made with the ray-query features (and wgpu's experimental opt-in).
    pub on_device: bool,
    /// Why the request for a device with them failed, if it did and the device was made again
    /// without them.
    pub failed: Option<String>,
}

impl RtDevice {
    /// The backend that runs for `requested` on this device, `features` and `limits` being the
    /// device's. A hardware request on a device made without ray queries runs compute and says why:
    /// the adapter has none, the request for them failed, or the device was made for the compute
    /// setting and needs a restart (the features of a device are fixed when it is made).
    pub fn choose(
        &self,
        requested: RtBackend,
        features: wgpu::Features,
        limits: &wgpu::Limits,
    ) -> RtChoice {
        let mut choice = choose_backend(requested, features, limits);
        if requested.is_hardware() && !self.on_device {
            choice.fallback = Some(self.why_not());
        }
        choice
    }

    fn why_not(&self) -> String {
        if let Some(why) = &self.unavailable {
            why.clone()
        } else if let Some(why) = &self.failed {
            format!("the device could not be made with ray queries ({why})")
        } else {
            "this device was made without ray queries, for the compute setting: restart to use them"
                .to_string()
        }
    }

    /// What the player should know about the device's features, if it is not what the setting asks
    /// for (the overlay says it): they are fixed when the device is made, so a change of the
    /// setting is a change of the next launch's device.
    pub fn note(&self, requested: RtBackend) -> Option<String> {
        if requested.is_hardware() && !self.on_device && self.unavailable.is_none() {
            Some(if let Some(why) = &self.failed {
                format!(
                    "the device could not be made with ray queries, so compute runs ({why}); \
                     the next launch tries again, and the compute setting skips it"
                )
            } else {
                "restart needed: this device was made without ray queries, for the compute \
                 setting; the next launch has them"
                    .to_string()
            })
        } else if !requested.is_hardware() && self.on_device {
            Some(
                "the device keeps its ray-query features until the next launch, which makes it \
                 without them"
                    .to_string(),
            )
        } else {
            None
        }
    }
}

/// What a device is made with (#260): the features it asks the adapter for, and whether it takes
/// wgpu's experimental opt-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeviceRequest {
    features: wgpu::Features,
    experimental: bool,
}

impl DeviceRequest {
    /// The request for an adapter with `adapter` features: timestamps where it has them and `timers`
    /// says so, and **the experimental ray-query features only with `ray_queries`**, which is only
    /// ever true for a hardware backend that the adapter can run: the compute setting, an adapter
    /// without ray queries and the low-end profile ask for none of them.
    fn new(adapter: wgpu::Features, timers: bool, ray_queries: bool, wait: bool) -> Self {
        let mut features = if timers {
            adapter & profiler::FEATURES
        } else {
            wgpu::Features::empty()
        };
        if ray_queries {
            features |= wgpu::Features::EXPERIMENTAL_RAY_QUERY;
            if wait {
                // The diagnosis of the builds' GPU time (`--hw-wait`) puts timestamps between them.
                features |= adapter & wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
            }
        }
        Self {
            features,
            experimental: ray_queries,
        }
    }

    fn descriptor(self, adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
        wgpu::DeviceDescriptor {
            label: Some("bhayanak device"),
            required_features: self.features,
            // The Planet's bricks are one big storage buffer, well over the portable 128 MiB limit.
            required_limits: adapter.limits(),
            experimental_features: if self.experimental {
                // SAFETY: ray queries are experimental in wgpu 30 (ADR 0001): they may hit bugs of
                // wgpu's that are undefined behaviour from safe code. Opted into only when the
                // hardware backend is asked for (the setting, #260) and the adapter has ray
                // queries; the compute setting and the low-end profile make a device without any
                // of it, and a device that fails with it is made again without (see
                // [`make_device`]).
                unsafe { wgpu::ExperimentalFeatures::enabled() }
            } else {
                wgpu::ExperimentalFeatures::disabled()
            },
            ..Default::default()
        }
    }
}

/// Makes a device with `request(true)` if `with_ray_queries`, and if that fails, once more with
/// `request(false)`: a driver, or wgpu's experimental path, that chokes on the ray-query features
/// must not stop the game from starting (#260). Returns what was made, whether it has them, and
/// why the first try failed if it did. If the second try fails too, that is the error. `request`
/// is the seam that the tests of the fallback use, since a CI adapter does not fail.
fn make_device<T>(
    with_ray_queries: bool,
    mut request: impl FnMut(bool) -> Result<T>,
) -> Result<(T, bool, Option<String>)> {
    if !with_ray_queries {
        return Ok((request(false)?, false, None));
    }
    match request(true) {
        Ok(device) => Ok((device, true, None)),
        Err(why) => {
            let why = why.to_string();
            eprintln!("warning: no GPU device with ray queries ({why}); trying without them");
            Ok((request(false)?, false, Some(why)))
        }
    }
}

/// [`request_gpu`] for a backend: with a hardware one, a device with ray queries if the adapter has
/// them (and if that fails, one without), and the choice made (see [`choose_backend`]). With
/// `hide_ray_queries` the adapter is taken to have none, for testing the fallback through the real
/// device request.
fn request_gpu_for(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    timers: bool,
    backend: RtBackend,
    hide_ray_queries: bool,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue, RtChoice, RtDevice)> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface,
        ..Default::default()
    }))?;
    let mut features = adapter.features();
    // The constrained profile (`--profile low-end`, `--no-ray-queries`, #264) is a machine without
    // ray queries, so the compute tracer: the low-end run on any adapter.
    let held_off = !machine::profile().ray_queries;
    if hide_ray_queries || held_off {
        features.remove(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
    }
    let choice = choose_backend(backend, features, &adapter.limits());
    let mut unavailable = choose_backend(RtBackend::Hardware, features, &adapter.limits()).fallback;
    if held_off {
        unavailable = Some(
            "ray queries are off for this run (--profile low-end, --no-ray-queries or \
             BHAYANAK_RAY_QUERIES=0)"
                .to_string(),
        );
    }
    let with_ray_queries = choice.active.is_hardware();
    let (device_queue, on_device, failed) = make_device(with_ray_queries, |ray_queries| {
        let request = DeviceRequest::new(
            adapter.features(),
            timers,
            ray_queries,
            ray_queries && hw::process_wait(),
        );
        Ok(pollster::block_on(
            adapter.request_device(&request.descriptor(&adapter)),
        )?)
    })?;
    let (device, queue) = device_queue;
    let rt_device = RtDevice {
        unavailable,
        on_device,
        failed,
    };
    let choice = rt_device.choose(backend, device.features(), &device.limits());
    Ok((adapter, device, queue, choice, rt_device))
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

    fn limits() -> wgpu::Limits {
        wgpu::Limits {
            max_blas_primitive_count: 1,
            max_tlas_instance_count: 1,
            ..wgpu::Limits::default()
        }
    }

    /// What the device request asks for, for `backend` on an adapter with `adapter` features and
    /// `allowed` ray queries (the profile): what [`request_gpu_for`] does before it asks.
    fn requested(backend: RtBackend, adapter: wgpu::Features, allowed: bool) -> DeviceRequest {
        let mut features = adapter;
        if !allowed {
            features.remove(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
        }
        let ray_queries = choose_backend(backend, features, &limits())
            .active
            .is_hardware();
        DeviceRequest::new(adapter, true, ray_queries, true)
    }

    #[test]
    fn the_requested_features_follow_the_setting() {
        let adapter = wgpu::Features::EXPERIMENTAL_RAY_QUERY
            | wgpu::Features::TIMESTAMP_QUERY
            | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        let ray_query = wgpu::Features::EXPERIMENTAL_RAY_QUERY;

        // Hardware, on an adapter that has them: the features and the experimental opt-in.
        for hardware in [RtBackend::Hardware, RtBackend::HardwareNearest] {
            let r = requested(hardware, adapter, true);
            assert!(r.features.contains(ray_query) && r.experimental);
        }
        // The compute setting: neither, but the timers stay.
        let r = requested(RtBackend::Compute, adapter, true);
        assert!(!r.features.contains(ray_query) && !r.experimental);
        assert!(r.features.contains(wgpu::Features::TIMESTAMP_QUERY));
        assert!(
            !r.features
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
        );
        // The low-end profile (`--profile low-end`, `--no-ray-queries`, `BHAYANAK_RAY_QUERIES=0`),
        // whatever the setting says: the same request as the compute setting.
        let low_end = requested(RtBackend::Hardware, adapter, false);
        assert_eq!(low_end, r);
        // An adapter without them: the same again.
        let none = requested(
            RtBackend::Hardware,
            adapter & !wgpu::Features::EXPERIMENTAL_RAY_QUERY,
            true,
        );
        assert_eq!(none, r);
        // Without timers there is nothing but what the backend needs.
        assert_eq!(
            DeviceRequest::new(adapter, false, false, false).features,
            wgpu::Features::empty()
        );
    }

    #[test]
    fn a_device_that_fails_with_ray_queries_is_made_again_without() {
        use std::cell::RefCell;
        let asked = RefCell::new(Vec::new());
        let (made, on_device, failed) = make_device(true, |ray_queries| {
            asked.borrow_mut().push(ray_queries);
            if ray_queries {
                Err("the driver choked".into())
            } else {
                Ok("device")
            }
        })
        .unwrap();
        assert_eq!(made, "device");
        assert!(!on_device);
        assert_eq!(failed.as_deref(), Some("the driver choked"));
        assert_eq!(*asked.borrow(), [true, false], "one retry, without them");

        // It works the first time: no retry, and no failure to report.
        let asked = RefCell::new(Vec::new());
        let (_, on_device, failed) = make_device(true, |ray_queries| {
            asked.borrow_mut().push(ray_queries);
            Ok(())
        })
        .unwrap();
        assert!(on_device && failed.is_none());
        assert_eq!(*asked.borrow(), [true]);

        // Not asked for (compute, low-end): one request, without, and no warning.
        let asked = RefCell::new(Vec::new());
        let (_, on_device, failed) = make_device(false, |ray_queries| {
            asked.borrow_mut().push(ray_queries);
            Ok(())
        })
        .unwrap();
        assert!(!on_device && failed.is_none());
        assert_eq!(*asked.borrow(), [false]);

        // If the device cannot be made without them either, that is the error: it is tried once.
        let asked = RefCell::new(0);
        let err = make_device(true, |_| -> Result<()> {
            *asked.borrow_mut() += 1;
            Err("no device at all".into())
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "no device at all");
        assert_eq!(*asked.borrow(), 2);
    }

    #[test]
    fn the_device_says_why_compute_runs_and_what_the_next_launch_does() {
        let with = wgpu::Features::EXPERIMENTAL_RAY_QUERY;
        let none = wgpu::Features::empty();
        let device = |unavailable: Option<&str>, on_device, failed: Option<&str>| RtDevice {
            unavailable: unavailable.map(str::to_string),
            on_device,
            failed: failed.map(str::to_string),
        };

        // A device with them, the setting hardware: all as asked, nothing to say.
        let on = device(None, true, None);
        let choice = on.choose(RtBackend::Hardware, with, &limits());
        assert_eq!(
            (choice.active, choice.fallback),
            (RtBackend::Hardware, None)
        );
        assert_eq!(on.note(RtBackend::Hardware), None);
        // Switched to compute at run time: it runs at once, the device's features change at the
        // next launch.
        assert_eq!(
            on.choose(RtBackend::Compute, with, &limits()).active,
            RtBackend::Compute
        );
        assert!(on.note(RtBackend::Compute).unwrap().contains("next launch"));

        // A device made for the compute setting, switched to hardware: compute stays, a restart.
        let off = device(None, false, None);
        let choice = off.choose(RtBackend::Hardware, none, &limits());
        assert_eq!(choice.active, RtBackend::Compute);
        assert!(choice.fallback.unwrap().contains("restart"));
        assert!(off.note(RtBackend::Hardware).unwrap().contains("restart"));
        assert_eq!(off.note(RtBackend::Compute), None);

        // The request failed: compute, the failure said, in the log's reason and the overlay's note.
        let failed = device(None, false, Some("the driver choked"));
        let choice = failed.choose(RtBackend::Hardware, none, &limits());
        assert!(choice.fallback.unwrap().contains("the driver choked"));
        assert!(
            failed
                .note(RtBackend::Hardware)
                .unwrap()
                .contains("the driver choked")
        );
        assert_eq!(failed.note(RtBackend::Compute), None);

        // An adapter (or a profile) without them is not a restart: the reason, and no note.
        let held = device(Some("held off"), false, None);
        let choice = held.choose(RtBackend::Hardware, none, &limits());
        assert_eq!(choice.fallback.as_deref(), Some("held off"));
        assert_eq!(held.note(RtBackend::Hardware), None);
    }

    /// The compute setting makes a device without the ray-query features, and no fallback is
    /// reported: on any adapter, so on CI's.
    #[test]
    fn the_compute_setting_makes_a_device_without_ray_queries() {
        let (_, device, _, choice, rt_device) =
            request_gpu_for(shared_instance(), None, false, RtBackend::Compute, false).unwrap();
        assert!(
            !device
                .features()
                .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY)
        );
        assert!(!rt_device.on_device && rt_device.failed.is_none());
        assert_eq!((choice.active, choice.fallback), (RtBackend::Compute, None));
    }

    /// A run held off ray queries (`--profile low-end`, `--no-ray-queries`, or an adapter without
    /// them, which `hide_ray_queries` stands in for) makes a device without the features and the
    /// experimental opt-in, though the setting is hardware: through the real request, so a change
    /// to how `request_gpu_for` decides whether to ask for them fails here.
    #[test]
    fn a_hardware_setting_held_off_makes_a_device_without_ray_queries() {
        let (_, device, _, choice, rt_device) =
            request_gpu_for(shared_instance(), None, false, RtBackend::Hardware, true).unwrap();
        assert!(
            !device
                .features()
                .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY)
        );
        assert!(!rt_device.on_device && rt_device.failed.is_none());
        assert!(rt_device.unavailable.is_some());
        assert_eq!(choice.active, RtBackend::Compute);
        assert!(choice.fallback.is_some());
    }

    /// The hardware setting on an adapter that has ray queries (lavapipe's Mesa, where it does)
    /// makes the device with them; where it has none the test says so and passes, as `hw_agreement`
    /// does.
    #[test]
    fn the_hardware_setting_makes_a_device_with_ray_queries_where_the_adapter_has_them() {
        let (_, device, _, choice, rt_device) =
            request_gpu_for(shared_instance(), None, false, RtBackend::Hardware, false).unwrap();
        let has = device
            .features()
            .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
        assert_eq!(rt_device.on_device, has);
        assert_eq!(choice.active.is_hardware(), has);
        if !has {
            eprintln!("no ray queries on this adapter: {:?}", choice.fallback);
        }
    }
}
