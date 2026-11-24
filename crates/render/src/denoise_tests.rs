//! Checks of the denoiser's kernels (`shaders/denoise.wgsl`, `denoise.rs`, #55) on the GPU, on signals
//! and surfaces made for the purpose, so that the answer of each is known: a plane of known depth with
//! a noise of known size on it, run through the real pipelines with the real bind groups. What each
//! guards is in its comment, with what happens to it without that.
//!
//! The renderer's own frames, with a scene, a camera and the upscaler, are `tests/denoise.rs`.

use super::{
    Buffers, Denoise, DenoiseUniform, Frame, MAX_HISTORY, Pass, SAMPLES_FORMAT, SETTLING_HISTORY,
};
use crate::camera::{Camera, CameraUniform};
use crate::offscreen::{half_to_f32, read_texture};
use crate::tracer::Plane;
use crate::{request_gpu, shared_instance};

const SIZE: (u32, u32) = (64, 32);
/// The +z face's code in the G-buffer (`face_normal` in `denoise.wgsl`) and the +x face's.
const PLUS_Z: u32 = 6;
const PLUS_X: u32 = 2;

/// What a pixel's surface is: its distance along the ray and its face's code.
type Surface = (f32, u32);

/// Noise of a known size: a number a pixel with mean 0 and standard deviation 1 (uniform), the same
/// every run.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let unit = (self.0 >> 40) as f32 / (1u64 << 24) as f32;
        (unit - 0.5) * 12f32.sqrt()
    }
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    buffers: Buffers,
    pass: Pass,
    gbuffer: Plane,
    surface: Plane,
    normals: Plane,
    /// The history image the next frame reads.
    read: usize,
    camera: Camera,
    noise: Noise,
}

/// What a frame's inputs besides the samples are.
#[derive(Clone)]
struct Inputs {
    has_history: bool,
    /// The camera of the frame before; the history is of its view. `None` is the same camera.
    previous: Option<Camera>,
    boost: Option<([i32; 3], [i32; 3])>,
    /// Whether the smooth normals pass ran (`Rig::smooth_normals` has made them).
    smooth: bool,
}

impl Inputs {
    fn still() -> Self {
        Self {
            has_history: true,
            previous: None,
            boost: None,
            smooth: false,
        }
    }
}

impl Rig {
    fn new() -> Self {
        let (_adapter, device, queue) = request_gpu(shared_instance(), None, false).unwrap();
        let rig = Self {
            buffers: Buffers::new(&device, SIZE),
            pass: Pass::new(&device),
            gbuffer: Plane::new(&device, "test gbuffer", SIZE, crate::tracer::GBUFFER_FORMAT),
            surface: Plane::new(&device, "test surface", SIZE, crate::tracer::SURFACE_FORMAT),
            normals: Plane::new(&device, "test normals", SIZE, crate::tracer::NORMALS_FORMAT),
            device,
            queue,
            read: 0,
            camera: Camera::looking_at([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
            noise: Noise(0x9e37_79b9_7f4a_7c15),
        };
        // No smooth normals: the face's normal is the pixel's.
        rig.upload(&rig.normals, &vec![0u8; (SIZE.0 * SIZE.1 * 8) as usize], 8);
        rig.upload(
            &rig.surface,
            &vec![0u8; (SIZE.0 * SIZE.1 * 16) as usize],
            16,
        );
        rig
    }

    /// Smooth normals for the whole frame: `normal(x, y)` as the pass of the smooth normals would
    /// write it (half floats, a one in w where it ran). Run with `Inputs { smooth: true, .. }`.
    fn smooth_normals(&self, normal: impl Fn(u32, u32) -> [f32; 3]) {
        let mut bytes = Vec::new();
        for y in 0..SIZE.1 {
            for x in 0..SIZE.0 {
                let [nx, ny, nz] = normal(x, y);
                for c in [nx, ny, nz, 1.0] {
                    bytes.extend_from_slice(&to_half(c).to_le_bytes());
                }
            }
        }
        self.upload(&self.normals, &bytes, 8);
    }

    fn upload(&self, plane: &Plane, bytes: &[u8], bytes_per: u32) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &plane.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE.0 * bytes_per),
                rows_per_image: Some(SIZE.1),
            },
            wgpu::Extent3d {
                width: SIZE.0,
                height: SIZE.1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// The distance along the ray through the middle of pixel (x, y) of `camera` to the plane
    /// `distance` in front of the camera's eye (perpendicular to where it looks).
    fn along_ray(camera: &Camera, (x, y): (u32, u32), distance: f32) -> f32 {
        let tan_half = (camera.fov_y / 2.0).tan();
        let aspect = f64::from(SIZE.0) / f64::from(SIZE.1);
        let ndc = |p: u32, n: u32| (f64::from(p) + 0.5) / f64::from(n) * 2.0 - 1.0;
        let (f, r, u) = (camera.forward(), camera.right(), camera.up());
        let d = [0, 1, 2].map(|a| {
            f[a] + r[a] * ndc(x, SIZE.0) * aspect * tan_half - u[a] * ndc(y, SIZE.1) * tan_half
        });
        let length = d.iter().map(|c| c * c).sum::<f64>().sqrt();
        let along_forward = (0..3).map(|a| d[a] * f[a]).sum::<f64>() / length;
        (f64::from(distance) / along_forward) as f32
    }

    /// Sets the surfaces of the frame: `surfaces(x, y)` is a pixel's distance from the eye along
    /// its ray (negative: sky) and its face's code.
    fn set_surfaces(&self, surfaces: impl Fn(u32, u32) -> Surface) {
        let mut bytes = Vec::new();
        for y in 0..SIZE.1 {
            for x in 0..SIZE.0 {
                let (t, face) = surfaces(x, y);
                bytes.extend_from_slice(&t.to_bits().to_le_bytes());
                bytes.extend_from_slice(&(face << 24).to_le_bytes());
            }
        }
        self.upload(&self.gbuffer, &bytes, 8);
    }

    /// A flat wall `distance` in front of the camera with its face to it, across the whole frame.
    fn wall(&self, distance: f32) {
        self.wall_facing(distance, PLUS_Z);
    }

    /// `wall`, with the face's code `face`.
    fn wall_facing(&self, distance: f32, face: u32) {
        let camera = self.camera;
        self.set_surfaces(|x, y| (Self::along_ray(&camera, (x, y), distance), face));
    }

    /// The frame's samples: `signal(x, y)` is a pixel's mean, and `sigma` the standard deviation of
    /// the noise on every channel (the same noise on all, so that the luminance is the number).
    /// The sun's weight, in w, is the same as the light.
    fn samples(&mut self, sigma: f32, signal: impl Fn(u32, u32) -> f32) -> Vec<f32> {
        let mut values = Vec::new();
        let mut bytes = Vec::new();
        for y in 0..SIZE.1 {
            for x in 0..SIZE.0 {
                let value = (signal(x, y) + sigma * self.noise.next()).max(0.0);
                values.push(value);
                for _ in 0..4 {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        assert_eq!(SAMPLES_FORMAT, wgpu::TextureFormat::Rgba32Float);
        self.upload(self.buffers.samples(), &bytes, 16);
        values
    }

    /// Runs one frame of `mode` over the samples last uploaded, and returns the history image it
    /// wrote.
    fn run(&mut self, mode: Denoise, inputs: &Inputs) -> usize {
        let previous = inputs.previous.as_ref().unwrap_or(&self.camera);
        let (read, write) = (self.read, 1 - self.read);
        let camera: CameraUniform = self.camera.uniform();
        let shift: [f32; 4] = {
            let delta = [0, 1, 2].map(|a| (self.camera.position[a] - previous.position[a]) as f32);
            [delta[0], delta[1], delta[2], 0.0]
        };
        let ([lx, ly, lz], [hx, hy, hz]) = inputs.boost.unwrap_or(([0; 3], [0; 3]));
        let frame = Frame {
            uniform: DenoiseUniform {
                camera,
                previous: previous.uniform(),
                shift,
                params: [
                    u32::from(inputs.has_history),
                    u32::from(inputs.smooth),
                    (MAX_HISTORY as f32).to_bits(),
                    (SETTLING_HISTORY as f32).to_bits(),
                ],
                boost_min: [lx, ly, lz, i32::from(inputs.boost.is_some())],
                boost_max: [hx, hy, hz, 0],
            },
            read,
            write,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default()); // untimed
            self.pass.encode(
                &self.device,
                &self.queue,
                &mut pass,
                &frame,
                mode,
                SIZE,
                &self.buffers,
                &self.gbuffer,
                &self.surface,
                &self.normals,
            );
        }
        self.queue.submit([encoder.finish()]);
        self.read = write;
        write
    }

    fn read_plane(&self, plane: &Plane, bytes_per: u32) -> Vec<u8> {
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        read_texture(
            &self.device,
            &self.queue,
            encoder,
            &plane.texture,
            SIZE,
            bytes_per,
        )
    }

    /// The light that `shade_pass` would be lit with after `written`, in `mode`: the luminance
    /// number of each pixel (the channels are the same) and the sun's weight.
    fn light(&self, mode: Denoise, written: usize) -> Vec<[f32; 4]> {
        let plane = self.buffers.result(mode, Some(written));
        if mode == Denoise::Temporal || mode == Denoise::Off {
            self.read_plane(plane, 16)
                .chunks(16)
                .map(|t| {
                    std::array::from_fn(|c| {
                        f32::from_le_bytes(t[4 * c..4 * c + 4].try_into().unwrap())
                    })
                })
                .collect()
        } else {
            self.read_plane(plane, 8)
                .chunks(8)
                .map(|t| {
                    std::array::from_fn(|c| {
                        half_to_f32(u16::from_le_bytes([t[2 * c], t[2 * c + 1]]))
                    })
                })
                .collect()
        }
    }

    /// The moments of the history that `written` is: the second moments and the count.
    fn moments(&self, written: usize) -> Vec<[f32; 4]> {
        self.read_plane(&self.buffers.moments[written], 16)
            .chunks(16)
            .map(|t| {
                std::array::from_fn(|c| f32::from_le_bytes(t[4 * c..4 * c + 4].try_into().unwrap()))
            })
            .collect()
    }

    /// The variance the temporal kernel made of the light's luminance (x) and the sun's weight (y).
    fn variance(&self) -> Vec<[f32; 2]> {
        self.read_plane(self.buffers.temporal_variance(), 8)
            .chunks(8)
            .map(|t| {
                [
                    f32::from_le_bytes(t[0..4].try_into().unwrap()),
                    f32::from_le_bytes(t[4..8].try_into().unwrap()),
                ]
            })
            .collect()
    }
}

/// A half float's bits, for the values a test writes (normal numbers, rounded down).
fn to_half(value: f32) -> u16 {
    if value == 0.0 {
        return 0;
    }
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    assert!((1..31).contains(&exponent), "{value} is not a normal half");
    sign | ((exponent as u16) << 10) | ((bits >> 13) & 0x3ff) as u16
}

fn mean(values: impl Iterator<Item = f32>) -> f32 {
    let (sum, n) = values.fold((0.0, 0.0), |(s, n), v| (s + v, n + 1.0));
    sum / n
}

fn std_dev(values: &[f32]) -> f32 {
    let m = mean(values.iter().copied());
    mean(values.iter().map(|v| (v - m) * (v - m))).sqrt()
}

/// The pixels of columns `from..to` (every row) of a frame, as `f(light)`.
fn columns(light: &[[f32; 4]], from: u32, to: u32, f: impl Fn(&[f32; 4]) -> f32) -> Vec<f32> {
    (0..SIZE.1)
        .flat_map(|y| (from..to).map(move |x| (x, y)))
        .map(|(x, y)| f(&light[(y * SIZE.0 + x) as usize]))
        .collect()
}

/// Frames of a still view, `n` of them, each with fresh noise on `signal`; returns the image the
/// last one wrote.
fn still_frames(
    rig: &mut Rig,
    mode: Denoise,
    n: u32,
    sigma: f32,
    signal: impl Fn(u32, u32) -> f32 + Copy,
) -> usize {
    let mut written = 0;
    for frame in 0..n {
        rig.samples(sigma, signal);
        let inputs = Inputs {
            has_history: frame > 0,
            ..Inputs::still()
        };
        written = rig.run(mode, &inputs);
    }
    written
}

#[test]
fn a_still_view_converges_to_the_mean_of_its_noise() {
    // What this guards: the temporal kernel's accumulation. A wall of light 1.0 with noise of 0.3
    // (the luminance's standard deviation) averages, over a history of 32 frames, to a noise of
    // 0.3 / sqrt(65) = 0.037 about the same mean (the clamp must not take light away). With no
    // history (MAX_HISTORY = 1, or the history never read) the noise is the samples': 0.3.
    let mut rig = Rig::new();
    rig.wall(20.0);
    let written = still_frames(&mut rig, Denoise::Temporal, 96, 0.3, |_, _| 1.0);
    let light = rig.light(Denoise::Temporal, written);
    // Away from the frame's border, where the neighbourhood has all its pixels.
    let values = columns(&light, 8, 56, |p| p[0]);
    let m = mean(values.iter().copied());
    let s = std_dev(&values);
    assert!((m - 1.0).abs() < 0.02, "the mean is {m}, the light 1.0");
    assert!(s < 0.3 / 4.0, "the noise is {s}, of 0.3 in a sample");
    assert!(s > 0.3 / 65f32.sqrt() * 0.5, "an average of 32: {s}");
    // And the history has the length it is capped at.
    let counts = rig.moments(written);
    let count = mean(counts.iter().map(|c| c[2]));
    assert!((count - MAX_HISTORY as f32).abs() < 0.5, "{count} samples");
}

#[test]
fn a_history_that_the_light_has_left_is_given_up_not_ghosted() {
    // What this guards: the adaptive part. A wall that has held 1.0 for 40 frames goes to 0.2 (a
    // dig lets the dark in, a light goes out), with a little noise: the history is a number of
    // deviations away from the neighbourhood and is dropped, so one frame later the wall is at the
    // new light, not at 0.97 of the old. Without the rejection the frame after is at 0.97 * 1.0.
    let mut rig = Rig::new();
    rig.wall(20.0);
    still_frames(&mut rig, Denoise::Temporal, 40, 0.05, |_, _| 1.0);
    rig.samples(0.05, |_, _| 0.2);
    let written = rig.run(Denoise::Temporal, &Inputs::still());
    let light = rig.light(Denoise::Temporal, written);
    let m = mean(columns(&light, 8, 56, |p| p[0]).into_iter());
    assert!(
        (m - 0.2).abs() < 0.05,
        "one frame after, the wall is at {m}, the light 0.2"
    );
}

#[test]
fn light_an_edit_is_changing_is_averaged_over_a_short_history() {
    // What this guards: the boost box. Noisy light (0.3, so that the neighbourhood's own spread
    // does not drop the history by itself) moves from 1.0 to 0.5. In the box that an Edit gives the
    // renderer, ten frames take the wall to within 0.08 of the new light (a history of at most
    // SETTLING_HISTORY samples); outside it, the same ten frames leave a history of 32 at 0.8 or
    // more.
    let run = |boost: Option<([i32; 3], [i32; 3])>| {
        let mut rig = Rig::new();
        rig.wall(20.0);
        still_frames(&mut rig, Denoise::Temporal, 40, 0.3, |_, _| 1.0);
        let mut written = 0;
        for _ in 0..10 {
            rig.samples(0.3, |_, _| 0.5);
            written = rig.run(
                Denoise::Temporal,
                &Inputs {
                    boost,
                    ..Inputs::still()
                },
            );
        }
        let light = rig.light(Denoise::Temporal, written);
        mean(columns(&light, 8, 56, |p| p[0]).into_iter())
    };
    // The test's surface texture is zero: every pixel is Voxel (0, 0, 0).
    let inside = run(Some(([-1, -1, -1], [1, 1, 1])));
    let elsewhere = run(Some(([10, 10, 10], [12, 12, 12])));
    let no_box = run(None);
    assert!((inside - 0.5).abs() < 0.08, "in the box: {inside}");
    assert!(elsewhere > 0.8, "outside the box: {elsewhere}");
    assert!(no_box > 0.8, "no box: {no_box}");
}

#[test]
fn a_new_history_is_the_samples_and_so_is_one_a_fast_turn_leaves() {
    // What this guards: `has_history = 0` (the first frame, a new sun, a resize) and the geometry
    // check on a fast turn. Over a history of 1.0, samples of 0.4 with a noise of 0.3:
    // - with no history the result is the samples, exactly;
    // - after a turn of 100 degrees in one frame (nothing the camera saw before is in view) it is the
    //   samples too: the point each pixel reprojects to is not in the last frame;
    // - after a turn of one degree the history is kept (the result is over 0.5).
    let history_of_one = |rig: &mut Rig| {
        still_frames(rig, Denoise::Temporal, 40, 0.0, |_, _| 1.0);
    };
    let frame_after = |previous: Option<Camera>, has_history: bool, turn: f64| {
        let mut rig = Rig::new();
        rig.wall(20.0);
        history_of_one(&mut rig);
        let before = rig.camera;
        rig.camera.turn(turn.to_radians(), 0.0);
        // A wall of another face where the camera looks now; for a turn of one degree, the same.
        rig.wall_facing(20.0, if turn > 45.0 { PLUS_X } else { PLUS_Z });
        let samples = rig.samples(0.3, |_, _| 0.4);
        let written = rig.run(
            Denoise::Temporal,
            &Inputs {
                has_history,
                previous: previous.or(Some(before)),
                boost: None,
                smooth: false,
            },
        );
        let light = rig.light(Denoise::Temporal, written);
        (samples, light)
    };
    let worst = |samples: &[f32], light: &[[f32; 4]]| {
        samples
            .iter()
            .zip(light)
            .map(|(s, l)| (s - l[0]).abs())
            .fold(0.0, f32::max)
    };
    let (samples, light) = frame_after(None, false, 0.0);
    assert!(
        worst(&samples, &light) < 1e-5,
        "a new history is the samples"
    );
    let (samples, light) = frame_after(None, true, 100.0);
    // The pixels at the very edge of a 100 degree turn may still find something.
    let off = samples
        .iter()
        .zip(&light)
        .filter(|(s, l)| (**s - l[0]).abs() > 1e-4)
        .count();
    assert!(
        off < samples.len() / 50,
        "{off} pixels of the turned view have a history"
    );
    let (_, light) = frame_after(None, true, 1.0);
    let kept = mean(columns(&light, 8, 56, |p| p[0]).into_iter());
    assert!(kept > 0.8, "a turn of one degree keeps the history: {kept}");
}

#[test]
fn a_surface_a_voxel_nearer_does_not_inherit_the_history_of_the_one_behind() {
    // What this guards: `PLANE_TOLERANCE`, the distance from its plane at which a history stops being
    // the pixel's. A terrace's edge, a stair or the floor of a dug voxel is one Voxel off the plane
    // of what the pixel showed, and what it shows now is another surface with another light. A wall
    // 2 Voxels away with a history of 1.0, then the same wall 1 Voxel nearer with samples of 0.4
    // (noise 0.3): the history is not used, the result is the samples (0.41 measured; with the
    // tolerance at 2 Voxels it is 0.95, the old light). The same wall where it was keeps the history
    // (0.95), so that the tolerance is not simply shut.
    let frame_with_wall_at = |distance: f32| {
        let mut rig = Rig::new();
        rig.wall(2.0);
        still_frames(&mut rig, Denoise::Temporal, 40, 0.0, |_, _| 1.0);
        let before = rig.camera;
        rig.wall(distance);
        rig.samples(0.3, |_, _| 0.4);
        let written = rig.run(
            Denoise::Temporal,
            &Inputs {
                has_history: true,
                previous: Some(before),
                boost: None,
                smooth: false,
            },
        );
        let light = rig.light(Denoise::Temporal, written);
        mean(columns(&light, 8, 56, |p| p[0]).into_iter())
    };
    let same = frame_with_wall_at(2.0);
    assert!(same > 0.8, "the same wall keeps its history: {same}");
    let nearer = frame_with_wall_at(1.0);
    assert!(
        nearer < 0.5,
        "a wall a Voxel nearer has the old wall's light: {nearer}"
    );
}

#[test]
fn the_filter_keeps_an_edge_in_depth_that_the_light_does_not_show() {
    // What this guards: the plane-distance stop of the à-trous filter. Two walls side by side, one
    // 20 Voxels away and one 40, lit 3.0 and 2.0 with a noise of 1.0 on both (the luminance stop, which reads
    // the variance, is loose over that step: it is the geometry that must hold the edge). One
    // frame (no history, the variance from the neighbourhood) through the whole filter, whose
    // reach is 30 pixels: the six columns each side of the edge keep their step (1.0, to within the
    // noise that is left). Without the
    // depth stop the step is under 0.95 (0.78 and 0.81 measured).
    let mut rig = Rig::new();
    let camera = rig.camera;
    rig.set_surfaces(|x, y| {
        let distance = if x < SIZE.0 / 2 { 20.0 } else { 40.0 };
        (Rig::along_ray(&camera, (x, y), distance), PLUS_Z)
    });
    rig.samples(1.0, |x, _| if x < SIZE.0 / 2 { 3.0 } else { 2.0 });
    let written = rig.run(
        Denoise::Full,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let light = rig.light(Denoise::Full, written);
    let near = mean(columns(&light, 26, 32, |p| p[0]).into_iter());
    let far = mean(columns(&light, 32, 38, |p| p[0]).into_iter());
    assert!(
        near - far > 0.95,
        "the step across the edge (3.0 to 2.0) is {} after the filter",
        near - far
    );
}

#[test]
fn the_filter_keeps_a_step_in_the_light_on_one_plane_at_settled() {
    // What this guards: the filter's luminance and sun stops (`PHI_LIGHT`, `PHI_SUN`, `LIGHT_FLOOR`,
    // `SUN_FLOOR`), which are all that holds a shadow's edge or the edge of an emitter's pool of
    // light on a flat surface (the plane and the normal are the same either side). A wall of one
    // plane, lit 1.0 on the left and 0.9 on the right (in the light and in the sun's weight alike),
    // with noise of 0.1, 40 frames of a still view in: the history's variance is small, so the stops
    // are tight, and the step across the edge stays at 0.098 (of 0.1) over the six columns each
    // side. Measured with the stops loosened: 0.081 at PHI 20, 0.066 at 50 and 0.028 at 1000, and
    // 0.024 with any one of the floors at 1000 (the sun's floor only in the sun's channel), which
    // no other test sees.
    let mut rig = Rig::new();
    rig.wall(20.0);
    let step = |x: u32, _: u32| if x < SIZE.0 / 2 { 1.0 } else { 0.9 };
    let written = still_frames(&mut rig, Denoise::Full, 40, 0.1, step);
    let light = rig.light(Denoise::Full, written);
    for (name, channel) in [("light", 0), ("sun", 3)] {
        let near = mean(columns(&light, 26, 32, |p| p[channel]).into_iter());
        let far = mean(columns(&light, 32, 38, |p| p[channel]).into_iter());
        assert!(
            near - far > 0.09,
            "the step of the {name} across the edge (0.1) is {} after the filter",
            near - far
        );
    }
}

#[test]
fn the_filter_keeps_a_step_of_one_voxel_in_depth_that_the_light_does_not_show() {
    // What this guards: `PHI_PLANE`, the width of the filter's plane stop, at the size of a Voxel's
    // step (a stair, a terrace, the wall of a dug hole). Two walls side by side, 2 Voxels away and 1
    // (the same plane, a Voxel apart), lit 3.0 and 2.0 with a noise of 1.0 so that the luminance stop
    // is loose: one frame with no history through the whole filter, and the six columns each side of
    // the edge keep their step (1.0, to within the noise that is left). With the plane stop as wide
    // as it was at first (2 Voxels) a Voxel's step is crossed (0.90 measured).
    let mut rig = Rig::new();
    let camera = rig.camera;
    rig.set_surfaces(|x, y| {
        let distance = if x < SIZE.0 / 2 { 2.0 } else { 1.0 };
        (Rig::along_ray(&camera, (x, y), distance), PLUS_Z)
    });
    rig.samples(1.0, |x, _| if x < SIZE.0 / 2 { 3.0 } else { 2.0 });
    let written = rig.run(
        Denoise::Full,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let light = rig.light(Denoise::Full, written);
    let near = mean(columns(&light, 26, 32, |p| p[0]).into_iter());
    let far = mean(columns(&light, 32, 38, |p| p[0]).into_iter());
    assert!(
        near - far > 0.95,
        "the step across the edge (3.0 to 2.0) is {} after the filter",
        near - far
    );
}

/// Frames of a still view over a stair of one-Voxel steps with the smooth normals of a slope of
/// 45 degrees (what the smooth-normal pass makes of a stair), 40 frames of noise of `sigma` on a
/// flat light, through `Denoise::Full`: the spread of the light that comes out.
fn stair_spread(sigma: f32) -> f32 {
    let mut rig = Rig::new();
    let camera = rig.camera;
    // A step of 1 Voxel in depth every 8 pixels, from 8 Voxels away, a stair going away to the right.
    rig.set_surfaces(|x, y| {
        let distance = 8.0 + (x / 8) as f32;
        (Rig::along_ray(&camera, (x, y), distance), PLUS_Z)
    });
    let slope = std::f32::consts::FRAC_1_SQRT_2;
    rig.smooth_normals(|_, _| [-slope, 0.0, slope]);
    let mut written = 0;
    for frame in 0..40 {
        rig.samples(sigma, |_, _| 1.0);
        written = rig.run(
            Denoise::Full,
            &Inputs {
                has_history: frame > 0,
                smooth: true,
                ..Inputs::still()
            },
        );
    }
    let light = rig.light(Denoise::Full, written);
    std_dev(&columns(&light, 8, 56, |p| p[0]))
}

#[test]
fn a_stair_of_smooth_normals_is_still_smoothed_at_the_planes_half_voxel() {
    // What this guards: that the tight plane stop (`PLANE_TOLERANCE` and `PHI_PLANE`, half a Voxel)
    // does not leave the noise on a stair, where the smooth normals tilt and the Voxels' steps are
    // one Voxel apart: a flat light of 1.0 with a noise of 0.3 on a stair of steps 8 pixels wide,
    // 40 frames in, comes out with a spread of 0.015 (0.010 with the stops at 2 Voxels as they were,
    // 0.018 with `PHI_PLANE` at 0.25 and 0.020 at 0.1, against the 0.3 it went in with). A guard
    // against a stop that is too tight for a slope, not a sharp one: it fails from `PHI_PLANE` 0.1.
    let spread = stair_spread(0.3);
    assert!(
        spread < 0.019,
        "the stair's light has a spread of {spread} (0.3 went in)"
    );
}

#[test]
fn the_filter_keeps_an_edge_in_the_normal_that_the_depth_does_not_show() {
    // What this guards: the normal stop. Two faces meet on the same plane of distance (a wall and
    // one turned a quarter turn, as far as the filter can tell: the same distance along each ray),
    // lit 3.0 and 2.0 with a noise of 1.0. The six columns each side of the edge keep their step.
    // Without the normal stop the plane check lets the nearest taps across and the edge softens
    // (the step is under 0.95 (0.78 and 0.81 measured)).
    let mut rig = Rig::new();
    let camera = rig.camera;
    rig.set_surfaces(|x, y| {
        let face = if x < SIZE.0 / 2 { PLUS_Z } else { PLUS_X };
        (Rig::along_ray(&camera, (x, y), 20.0), face)
    });
    rig.samples(1.0, |x, _| if x < SIZE.0 / 2 { 3.0 } else { 2.0 });
    let written = rig.run(
        Denoise::Full,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let light = rig.light(Denoise::Full, written);
    let left = mean(columns(&light, 26, 32, |p| p[0]).into_iter());
    let right = mean(columns(&light, 32, 38, |p| p[0]).into_iter());
    assert!(
        left - right > 0.95,
        "the step across the edge (3.0 to 2.0) is {} after the filter",
        left - right
    );
}

#[test]
fn a_history_a_little_off_its_neighbourhood_is_clamped_to_it() {
    // What this guards: the clamp. A noise-free wall held at 1.0 goes to 0.83: the history is 17%
    // off, too little for the adaptive part to give it up, and past the clamp's range (3 deviations
    // of nothing, and a slack of a tenth). One frame later the wall is within 0.05 of what the
    // clamp lets through (0.917 and a thirty-third of the way to 0.83: 0.915); with no clamp it is
    // at 0.995.
    let mut rig = Rig::new();
    rig.wall(20.0);
    still_frames(&mut rig, Denoise::Temporal, 40, 0.0, |_, _| 1.0);
    rig.samples(0.0, |_, _| 0.83);
    let written = rig.run(Denoise::Temporal, &Inputs::still());
    let light = rig.light(Denoise::Temporal, written);
    let m = mean(columns(&light, 8, 56, |p| p[0]).into_iter());
    assert!(m < 0.95, "the wall is at {m}, 0.915 clamped and 0.995 not");
}

#[test]
fn the_filter_blurs_a_flat_wall_of_noise_and_keeps_its_mean() {
    // What this guards: the filter does its job, and conserves light. On one frame of noise (0.5 on
    // 1.0) a flat wall is blurred to a fraction of the noise, and the mean does not move. A filter
    // that passes its input (a broken stop that is never open) leaves the noise.
    let mut rig = Rig::new();
    rig.wall(20.0);
    let samples = rig.samples(0.5, |_, _| 1.0);
    let written = rig.run(
        Denoise::Full,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let light = rig.light(Denoise::Full, written);
    let values = columns(&light, 8, 56, |p| p[0]);
    let before = std_dev(&samples);
    let after = std_dev(&values);
    assert!(after < before / 3.0, "the noise is {after} of {before}");
    let m = mean(values.iter().copied());
    assert!((m - 1.0).abs() < 0.03, "the mean is {m}");
}

#[test]
fn the_filter_smooths_the_small_noise_of_a_long_history_too() {
    // What this guards: that the filter runs on every pixel. What a settled history has left is the
    // radiance cache's error (a few percent, the same frame after frame), which only the filter can
    // average. A wall of noise of 0.02 on 1.0 (the variance the filter is told is small) comes out
    // with less than half the spread it went in with; with a gate that leaves a quiet pixel alone it
    // comes out as it went in (the spread is a share of 1, as before).
    let mut rig = Rig::new();
    rig.wall(20.0);
    let samples = rig.samples(0.02, |_, _| 1.0);
    let written = rig.run(
        Denoise::Full,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let light = rig.light(Denoise::Full, written);
    let values = columns(&light, 8, 56, |p| p[0]);
    let before: Vec<f32> = (0..SIZE.1)
        .flat_map(|y| (8..56).map(move |x| (x, y)))
        .map(|(x, y)| samples[(y * SIZE.0 + x) as usize])
        .collect();
    let spread = |v: &[f32]| {
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
    };
    let (after, was) = (spread(&values), spread(&before));
    assert!(after < 0.5 * was, "the spread went from {was} to {after}");
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    assert!((mean - 1.0).abs() < 0.01, "the mean moved to {mean}");
}

#[test]
fn the_variance_is_zero_for_a_constant_and_the_noises_for_noise() {
    // What this guards: the variance estimate the filter reads.
    // - A constant wall has none, on the first frame and after 40.
    // - Noise of 0.2 on the first frame has its neighbourhood's variance, times the spatial scale: far
    //   more than zero (without the spatial fallback, a history of one sample has no moments to take
    //   it from and it is zero).
    // - After 40 frames it is the samples' variance over the length of the average: 0.04 / 63.
    let mut rig = Rig::new();
    rig.wall(20.0);
    rig.samples(0.0, |_, _| 1.0);
    rig.run(
        Denoise::Temporal,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let flat = rig.variance();
    assert!(
        flat.iter().all(|v| v[0] < 1e-7 && v[1] < 1e-7),
        "a first frame of a constant"
    );
    still_frames(&mut rig, Denoise::Temporal, 40, 0.0, |_, _| 1.0);
    let flat = rig.variance();
    assert!(
        flat.iter().all(|v| v[0] < 1e-7 && v[1] < 1e-7),
        "40 frames of a constant"
    );

    let mut rig = Rig::new();
    rig.wall(20.0);
    rig.samples(0.2, |_, _| 1.0);
    rig.run(
        Denoise::Temporal,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let first = rig.variance();
    let inner = |v: &[[f32; 2]], pick: usize| {
        mean(
            (0..SIZE.1)
                .flat_map(|y| (8..56).map(move |x| (x, y)))
                .map(|(x, y)| v[(y * SIZE.0 + x) as usize][pick]),
        )
    };
    let first_light = inner(&first, 0);
    // 0.04 of a sample's variance, three times for the neighbours' not being independent.
    assert!(
        (0.05..0.2).contains(&first_light),
        "the first frame's variance is {first_light}"
    );
    let first_sun = inner(&first, 1);
    assert!(
        (0.05..0.2).contains(&first_sun),
        "and the sun's {first_sun}"
    );

    still_frames(&mut rig, Denoise::Temporal, 40, 0.2, |_, _| 1.0);
    let settled = rig.variance();
    let want = 0.04 / 63.0;
    let got = inner(&settled, 0);
    assert!(
        (got / want - 1.0).abs() < 0.35,
        "the settled variance is {got}, {want} expected"
    );
}

#[test]
fn off_runs_nothing_and_the_modes_name_where_the_light_is_read_from() {
    // What this guards: the fallback switches. With the denoiser off the kernels do not run (the
    // history stays as it was: zeros) and the light is the frame's samples; with temporal only it is
    // the history's, and with the filter on it is the filter's last image; a frame that had no
    // denoiser at all (`encode_primary`'s) is lit with its samples in every mode.
    let mut rig = Rig::new();
    rig.wall(20.0);
    rig.samples(0.5, |_, _| 1.0);
    let written = rig.run(
        Denoise::Off,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    assert!(
        rig.moments(written).iter().all(|m| m[2] == 0.0),
        "Off wrote a history"
    );
    let planes = |mode, written| std::ptr::from_ref(rig.buffers.result(mode, written));
    let samples = std::ptr::from_ref(rig.buffers.samples());
    assert_eq!(planes(Denoise::Off, Some(written)), samples);
    assert_ne!(planes(Denoise::Temporal, Some(written)), samples);
    assert_ne!(planes(Denoise::Full, Some(written)), samples);
    assert_ne!(
        planes(Denoise::Temporal, Some(written)),
        planes(Denoise::Full, Some(written))
    );
    for mode in Denoise::ALL {
        assert_eq!(
            planes(mode, None),
            samples,
            "{mode:?} with no frame of the denoiser"
        );
    }
    // Temporal runs the temporal kernel and not the filter; Full runs both.
    let written = rig.run(
        Denoise::Temporal,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    assert!(rig.moments(written).iter().all(|m| m[2] == 1.0));
    let temporal = rig.light(Denoise::Temporal, written);
    let raw = rig.light(Denoise::Off, written);
    assert!(
        temporal
            .iter()
            .zip(&raw)
            .all(|(a, b)| (a[0] - b[0]).abs() < 1e-6),
        "the first frame of Temporal is the samples"
    );
    let written = rig.run(
        Denoise::Full,
        &Inputs {
            has_history: false,
            ..Inputs::still()
        },
    );
    let full = rig.light(Denoise::Full, written);
    assert!(
        full.iter()
            .zip(&raw)
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.01),
        "Full filtered"
    );
}
