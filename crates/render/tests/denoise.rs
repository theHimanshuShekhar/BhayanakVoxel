//! Primary's denoiser (#55) in the renderer's own frames: a scene, a camera, the upscaler, the GI
//! cache and Edits. The kernels on surfaces and signals made for the purpose are in
//! `src/denoise_tests.rs`. Needs a Vulkan device (lavapipe in CI).

mod common;

use brick_store::{BrickStore, Material, VoxelCoord};
use materials::MaterialTable;
use render::{Camera, Denoise, Image, Offscreen, Resolution, Scene, ShadowWeight, Sun};

fn material(name: &str) -> Material {
    Material(MaterialTable::built_in().by_name(name).unwrap().id)
}

/// A closed room of rock, 40 by 16 by 40 inside, with a block of warm light on the floor, and (if
/// `wall` is set) a wall across it at x = 18..20 that the light does not get through, and (if
/// `pillar` is set) a pillar of rock, 4 by 4 Voxels and the room's height, between the camera and
/// the wall behind the lamp.
fn room(wall: bool, pillar: bool) -> BrickStore {
    let rock = material("rock");
    let lamp = material("warm light");
    let mut store = BrickStore::new();
    for x in -2..42 {
        for y in -2..18 {
            for z in -2..42 {
                let inside = (0..40).contains(&x) && (0..16).contains(&y) && (0..40).contains(&z);
                if !inside || (wall && (18..20).contains(&x)) {
                    store.set(VoxelCoord([x, y, z]), Some(rock));
                }
            }
        }
    }
    for x in 28..32 {
        for y in 0..3 {
            for z in 8..12 {
                store.set(VoxelCoord([x, y, z]), Some(lamp));
            }
        }
    }
    if pillar {
        for x in 14..18 {
            for y in 0..16 {
                for z in 22..26 {
                    store.set(VoxelCoord([x, y, z]), Some(rock));
                }
            }
        }
    }
    store
}

fn room_camera() -> Camera {
    Camera::looking_at([4.0, 6.0, 34.0], [26.0, 3.0, 12.0])
}

const OUTPUT: (u32, u32) = (160, 90);
const INTERNAL: (u32, u32) = (80, 45);

/// An `Offscreen` of `scene` as the window draws Primary (GI, the denoiser in `mode`), at half the
/// resolution of its output, with the light of the old constants.
fn gpu_of(scene: &Scene, mode: Denoise) -> Offscreen {
    let resolution = Resolution {
        internal: INTERNAL,
        output: OUTPUT,
    };
    let mut gpu = Offscreen::with_resolution(resolution, scene).unwrap();
    gpu.use_legacy_light();
    gpu.set_flat_ambient(false);
    gpu.set_denoise(mode);
    gpu
}

fn luminance(light: &[[f32; 4]]) -> Vec<f32> {
    light
        .iter()
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .collect()
}

fn mean(values: &[f32]) -> f32 {
    values.iter().sum::<f32>() / values.len() as f32
}

/// `frames` frames as the window draws them, then the light of the last: its luminance.
fn light_after(gpu: &mut Offscreen, camera: &Camera, frames: u32) -> Vec<f32> {
    for _ in 0..frames {
        gpu.render_upscaled(camera);
    }
    luminance(&gpu.read_light_signals())
}

/// How much the light moves from one frame of a still view to the next: the root mean square of the
/// difference of two frames' luminance, where there is any light.
fn flicker(a: &[f32], b: &[f32]) -> f32 {
    let squares: Vec<f32> = a
        .iter()
        .zip(b)
        .filter(|(a, b)| **a > 1e-4 || **b > 1e-4)
        .map(|(a, b)| (a - b) * (a - b))
        .collect();
    mean(&squares).sqrt()
}

/// What the noise of the light is in a view that does not move, in `mode`: the flicker after 40
/// frames, and the mean light over the view.
fn quiet(mode: Denoise) -> (f32, f32) {
    let scene = Scene::new(&room(false, false), None);
    let camera = room_camera();
    let mut gpu = gpu_of(&scene, mode);
    let a = light_after(&mut gpu, &camera, 40);
    let b = light_after(&mut gpu, &camera, 1);
    (flicker(&a, &b), mean(&b))
}

#[test]
fn a_still_view_of_noisy_gi_is_quiet_denoised_and_no_darker() {
    // What this guards: that the denoiser is in the frame and does its job on the renderer's own
    // signals: the room is lit by one lamp through the GI cache, which is noise at one sample a
    // pixel (the flicker of the raw light from one frame to the next), and the history makes it
    // several times quieter, the filter more. The mean light must be what the raw samples have (the
    // accumulation and the filter are mean-preserving, and the clamp takes at most a little).
    let (raw_flicker, raw_mean) = quiet(Denoise::Off);
    let (temporal_flicker, temporal_mean) = quiet(Denoise::Temporal);
    let (full_flicker, full_mean) = quiet(Denoise::Full);
    eprintln!(
        "flicker off {raw_flicker:.4} temporal {temporal_flicker:.4} full {full_flicker:.4}; \
         mean off {raw_mean:.4} temporal {temporal_mean:.4} full {full_mean:.4}"
    );
    assert!(raw_flicker > 0.0, "the raw light is noise");
    assert!(
        temporal_flicker < raw_flicker / 3.0,
        "temporal: {temporal_flicker} against {raw_flicker}"
    );
    assert!(
        full_flicker < temporal_flicker,
        "the filter is quieter still: {full_flicker} against {temporal_flicker}"
    );
    for (name, m) in [("temporal", temporal_mean), ("full", full_mean)] {
        assert!(
            (m / raw_mean - 1.0).abs() < 0.08,
            "{name} has {m} of light, the raw samples {raw_mean}"
        );
    }
}

#[test]
fn off_is_the_frames_samples_and_changing_the_mode_starts_the_history_over() {
    // What this guards: the fallback switches on the renderer. With the denoiser off the light
    // `shade_pass` reads is the frame's samples exactly. A mode that is changed starts the history
    // over (the first frame after it is its own samples again: temporal accumulation has nothing to
    // average yet), and the three modes light a frame differently.
    let scene = Scene::new(&room(false, false), None);
    let camera = room_camera();
    let mut gpu = gpu_of(&scene, Denoise::Off);
    for _ in 0..4 {
        gpu.render_upscaled(&camera);
    }
    let raw = gpu.read_light_signals();
    let samples = gpu.read_frame_samples();
    assert_eq!(raw, samples, "off lights the frame with its samples");

    gpu.set_denoise(Denoise::Temporal);
    for _ in 0..30 {
        gpu.render_upscaled(&camera);
    }
    let temporal = gpu.read_light_signals();
    let samples = gpu.read_frame_samples();
    assert_ne!(temporal, samples, "temporal averages");
    gpu.set_denoise(Denoise::Full);
    gpu.render_upscaled(&camera);
    let full = gpu.read_light_signals();
    let samples = gpu.read_frame_samples();
    let difference: f32 = luminance(&full)
        .iter()
        .zip(luminance(&samples))
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / full.len() as f32;
    assert!(difference > 0.0, "the filter has changed the first frame");

    // Back to temporal: one frame, its history of one: the same as the samples to a half's rounding.
    gpu.set_denoise(Denoise::Temporal);
    gpu.render_upscaled(&camera);
    let first = luminance(&gpu.read_light_signals());
    let samples = luminance(&gpu.read_frame_samples());
    let worst = first
        .iter()
        .zip(&samples)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(
        worst < 1e-3,
        "after a change of mode: {worst} from the samples"
    );
}

#[test]
fn a_new_sun_drops_the_history_of_the_light() {
    // What this guards: a new sun resets the denoiser's history with the upscaler's (the sun's
    // weights of the old one must not be carried into the new). The frame after `set_sun` has a
    // history of one sample: it is the frame's samples. A frame of the same sun, after as many frames
    // (a control), is not.
    let scene = Scene::new(&room(false, false), None);
    let camera = room_camera();
    let off_from_samples = |gpu: &mut Offscreen| {
        let light = luminance(&gpu.read_light_signals());
        let samples = luminance(&gpu.read_frame_samples());
        light
            .iter()
            .zip(&samples)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max)
    };
    let mut gpu = gpu_of(&scene, Denoise::Temporal);
    for _ in 0..30 {
        gpu.render_upscaled(&camera);
    }
    gpu.render_upscaled(&camera);
    assert!(off_from_samples(&mut gpu) > 0.01, "a history averages");
    gpu.set_sun(Sun::from_angles(1.0, 0.8, 0.05));
    gpu.render_upscaled(&camera);
    assert!(
        off_from_samples(&mut gpu) < 1e-3,
        "the first frame of a sun is its samples"
    );
}

/// Frames of the dig as the window draws it, with the denoiser in `mode`: the mean light of the view
/// before and the means of the 60 frames after.
fn dig(mode: Denoise, told: bool) -> (f32, Vec<f32>) {
    let mut store = room(true, false);
    let mut scene = Scene::new(&store, None);
    let mut gpu = gpu_of(&scene, mode);
    let camera = Camera::looking_at([4.0, 6.0, 34.0], [14.0, 3.0, 10.0]);
    let before = mean(&light_after(&mut gpu, &camera, 90));
    let mut changed = std::collections::BTreeSet::new();
    for x in 18..20 {
        for y in 2..10 {
            for z in 14..24 {
                let v = VoxelCoord([x, y, z]);
                changed.insert(v.brick());
                store.set(v, None);
            }
        }
    }
    let changed: Vec<_> = changed.into_iter().collect();
    let update = scene.update(&store, &changed).unwrap();
    gpu.update_scene(&scene, &update).unwrap();
    if told {
        gpu.edit_made(&changed);
    }
    let series = (0..60)
        .map(|_| mean(&light_after(&mut gpu, &camera, 1)))
        .collect();
    (before, series)
}

#[test]
fn light_let_in_by_a_dig_shows_through_the_denoiser_within_half_a_second() {
    // What this guards: no ghost after a dig. The history of the dark side is 32 frames long, and an
    // Edit that is told to the renderer shortens it in the box of the cells it changes the light of
    // (the same box and frames as the radiance cache's boost). Half a second at 60 Hz is 30 frames:
    // by then the light is within a tenth of the way to where it settles. Not told, the denoiser's
    // history is a time constant of 32 frames and lags (a control: the test would not hold).
    let (before, series) = dig(Denoise::Full, true);
    let end = series[59];
    assert!(end > before * 1.3, "the light shows: {before} to {end}");
    let at_30 = series[29];
    eprintln!("dig: before {before:.4}, frame 30 {at_30:.4}, frame 60 {end:.4}");
    assert!(
        (at_30 - end).abs() < 0.1 * (end - before),
        "frame 30 is {at_30} of {before} to {end}"
    );
}

#[test]
fn a_dig_that_is_not_told_to_the_denoiser_lags_which_is_why_it_is() {
    // The control of the one above: the same dig with no `edit_made`. The radiance cache's own
    // boost is off then too, so this is slower for both, and the denoiser's history is the lag the
    // told dig does not have.
    let (told_before, told) = dig(Denoise::Full, true);
    let (before, silent) = dig(Denoise::Full, false);
    assert!((told_before - before).abs() <= 0.2 * before + 1e-4);
    let reach = |series: &[f32], before: f32| {
        let end = series[59];
        series
            .iter()
            .position(|m| (m - end).abs() < 0.1 * (end - before))
            .unwrap_or(60)
    };
    let (told_at, silent_at) = (reach(&told, told_before), reach(&silent, before));
    eprintln!("frames to within a tenth: told {told_at}, not told {silent_at}");
    assert!(told_at < silent_at, "told {told_at}, not told {silent_at}");
}

/// The mean of the absolute difference of two lights, over the pixels with light in either.
fn difference(a: &[f32], b: &[f32]) -> f32 {
    let d: Vec<f32> = a
        .iter()
        .zip(b)
        .filter(|(a, b)| **a > 1e-4 || **b > 1e-4)
        .map(|(a, b)| (a - b).abs())
        .collect();
    mean(&d)
}

/// A camera strafing through `scene` as the window draws it: the light after `frames` frames of it,
/// the light of the same view settled, and the light of the view of 8 frames before, settled, each
/// as luminance.
fn strafe(scene: &Scene, mode: Denoise, frames: u32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let at = |frames: u32| {
        let mut camera = room_camera();
        let right = camera.right();
        camera.position =
            std::array::from_fn(|a| camera.position[a] + right[a] * 0.4 * f64::from(frames));
        assert!((0.0..40.0).contains(&camera.position[0]), "{frames}");
        camera
    };
    let mut moving = gpu_of(scene, mode);
    for _ in 0..60 {
        moving.render_upscaled(&at(0));
    }
    let mut light = luminance(&moving.read_light_signals());
    for frame in 1..=frames {
        light = light_after(&mut moving, &at(frame), 1);
    }
    let settled = |frame: u32| {
        let mut gpu = gpu_of(scene, mode);
        light_after(&mut gpu, &at(frame), 100)
    };
    (light, settled(frames), settled(frames - 8))
}

#[test]
fn a_wall_a_pillar_uncovers_is_not_ghosted_by_the_pillar_through_the_denoiser() {
    // What this guards: no ghost after a fast move, where the history of one surface is in front of
    // another: a pillar between the camera and the wall behind it that the strafe moves across, and
    // the wall that it uncovers has the pillar in its history. The denoised light of the frame is
    // closer to the view it is of (settled) than to the view of 8 frames before. It fails with the
    // history reprojected to the wrong place (the camera's shift taken wrong) and with history
    // that is never used up. The geometry checks that reject the pillar's history are
    // `denoise.rs`'s `a_new_history_is_the_samples_and_so_is_one_a_fast_turn_leaves`: this room's
    // light is too soft for their absence to show in a mean.
    let scene = Scene::new(&room(false, true), None);
    let (now, settled, old) = strafe(&scene, Denoise::Full, 16);
    let (to_now, to_old) = (difference(&now, &settled), difference(&now, &old));
    eprintln!("strafe past a pillar: {to_now:.4} from its view, {to_old:.4} from 8 frames back");
    assert!(
        to_now * 2.0 < to_old,
        "the light is no closer to its view ({to_now}) than to an old one ({to_old})"
    );
}

#[test]
fn a_sideways_move_through_noisy_gi_has_its_history_carried_along() {
    // What this guards: the history is reprojected with the camera's motion. After 16 frames of
    // strafing the light is closer to the view it is of than to the view before.
    let scene = Scene::new(&room(false, false), None);
    let (now, settled, old) = strafe(&scene, Denoise::Full, 16);
    let (to_now, to_old) = (difference(&now, &settled), difference(&now, &old));
    eprintln!("strafe: {to_now:.4} from its view, {to_old:.4} from 8 frames back");
    assert!(
        to_now * 2.0 < to_old,
        "the light is no closer to its view ({to_now}) than to an old one ({to_old})"
    );
}

/// The sun's weight that the shadow kernel wrote for a floor facing up, with the sun `elevation`
/// radians up with a disc of half-angle `radius`, `rays` rays a pixel, in `weight`'s way: its mean
/// over the frame.
fn floor_sun_weight(weight: ShadowWeight, elevation: f32, radius: f32, rays: u32) -> f32 {
    let rock = material("rock");
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(rock));
            }
        }
    }
    let scene = Scene::new(&store, None);
    let mut gpu = gpu_of(&scene, Denoise::Off);
    gpu.set_sun(Sun::from_angles(0.5, elevation, radius));
    gpu.set_shadow_rays(rays);
    gpu.set_shadow_weight(weight);
    let camera = Camera::looking_at([0.0, 25.0, 0.0], [0.0, 0.0, 0.5]);
    for _ in 0..4 {
        gpu.render_upscaled(&camera);
    }
    let samples = gpu.read_frame_samples();
    mean(&samples.iter().map(|p| p[3]).collect::<Vec<_>>())
}

/// What the per-sample weight averages to on a floor facing up, worked out on the CPU: the mean over
/// the sun's disc (uniform in solid angle) of the cosine to the floor's normal where it is
/// positive, over `0.5 * (1 + cos(radius))`, which is what the Reference's sun weight is.
fn expected_per_sample(elevation: f32, radius: f32) -> f32 {
    let (cos_max, steps) = (radius.cos(), 400);
    let mut total = 0.0;
    for i in 0..steps {
        let cos_theta = 1.0 - (1.0 - cos_max) * (i as f32 + 0.5) / steps as f32;
        let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();
        for j in 0..steps {
            let phi = std::f32::consts::TAU * (j as f32 + 0.5) / steps as f32;
            // The direction in the cone about the sun's centre, which is `elevation` above the
            // horizon: its height is the normal's cosine.
            let height = elevation.sin() * cos_theta + elevation.cos() * sin_theta * phi.cos();
            total += height.max(0.0);
        }
    }
    total / (steps * steps) as f32 / (0.5 * (1.0 + cos_max))
}

#[test]
fn the_per_sample_shadow_weight_is_the_references_and_the_centre_cosine_is_not_at_the_horizon() {
    // What this guards: `ShadowWeight::PerSample` (the switch that the bench uses to compare the two
    // under the denoiser). With a sun high in the sky and narrow, both weights are the cosine to
    // the sun (they agree to a few percent). With a wide sun half below the floor's horizon, the
    // centre-cosine weight is the cosine to the centre of the disc (0.17 at 10 degrees up), while
    // the sun's disc gives the floor what it does above its horizon: the per-sample weight is the
    // CPU's worked-out mean of that, and the centre-cosine one is not.
    let high = |weight| floor_sun_weight(weight, 1.2, 0.04, 16);
    let (c, p) = (
        high(ShadowWeight::CentreCosine),
        high(ShadowWeight::PerSample),
    );
    eprintln!("high sun: centre-cosine {c:.4} per-sample {p:.4}");
    assert!((c / p - 1.0).abs() < 0.04, "{c} against {p}");
    assert!(
        (p / expected_per_sample(1.2, 0.04) - 1.0).abs() < 0.04,
        "{p} against the CPU's {}",
        expected_per_sample(1.2, 0.04)
    );

    let low = |weight| floor_sun_weight(weight, 0.17, 0.3, 64);
    let (c, p) = (
        low(ShadowWeight::CentreCosine),
        low(ShadowWeight::PerSample),
    );
    let expected = expected_per_sample(0.17, 0.3);
    eprintln!("low wide sun: centre-cosine {c:.4} per-sample {p:.4} expected {expected:.4}");
    assert!(
        (p / expected - 1.0).abs() < 0.05,
        "per-sample {p}, the CPU's {expected}"
    );
    assert!(
        (c / expected - 1.0).abs() > 0.1,
        "centre-cosine {c}, the per-sample {expected}"
    );
}

#[test]
fn the_denoised_emissive_room_is_in_the_approved_golden_images() {
    // The room of `gi.rs`'s golden, as the window draws it with the denoiser on: the image the
    // owner judges by eye. (The golden of `gi.rs` has it off.)
    let scene = Scene::new(&room(false, false), None);
    let camera = room_camera();
    let mut gpu = gpu_of(&scene, Denoise::Full);
    let mut image: Image = gpu.render_upscaled(&camera);
    for _ in 1..40 {
        image = gpu.render_upscaled(&camera);
    }
    common::check_golden("emissive_room_denoised.png", &image, &common::PRIMARY);
}
