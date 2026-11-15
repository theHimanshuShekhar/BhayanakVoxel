//! The art direction of the renderer (#202): the display transform (auto-exposure, bloom, the tone
//! curve) and the smooth computed normals, each switched on alone against what the view draws with
//! every stage off. Needs a Vulkan device (lavapipe in CI).
//!
//! The tone curve, the exposure and the normals have a CPU twin here and in `render::post`, which the
//! GPU is held to. The goldens are the check that the whole look is what was approved.

use brick_store::{BrickStore, Material, VoxelCoord};
use materials::MaterialTable;
use render::reference::{Ray, trace};
use render::{Camera, DebugView, Image, Look, Offscreen, Resolution, Scene, Stages, Sun, camera};

const STONE: Material = Material(1);
const SIZE: u32 = 32;

/// A flat floor, Voxels y -8..=-1 (its top face is at y = 0), x and z -64..=63.
fn floor_of(material: Material) -> Scene {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(material));
            }
        }
    }
    Scene::new(&store, None)
}

fn overhead() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5])
}

/// A floor with no colour variation, lit by the legacy light, drawn with `stages`.
fn lit_floor(stages: Stages) -> Image {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material("clear glass"))).unwrap();
    gpu.set_look(Look::LEGACY);
    gpu.set_stages(stages);
    gpu.render(&overhead())
}

fn material(name: &str) -> Material {
    Material(MaterialTable::built_in().by_name(name).unwrap().id)
}

const OFF: Stages = Stages::NONE;

fn linear(image: &Image, x: u32, y: u32) -> [f64; 3] {
    let at = ((y * image.width + x) * 4) as usize;
    [0, 1, 2].map(|c| f64::from(render::srgb_decode(f32::from(image.rgba[at + c]) / 255.0)))
}

#[test]
fn auto_exposure_puts_a_uniform_scene_at_middle_grey() {
    let exposed = lit_floor(Stages {
        exposure: true,
        ..OFF
    });
    let plain = lit_floor(OFF);
    // The floor is the same light everywhere, so its mean log luminance is its luminance, and the
    // exposure puts that at 0.18 (the stage's range, -3 to +5 stops, holds this floor).
    let luminance = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let before = luminance(linear(&plain, 16, 16));
    let after = luminance(linear(&exposed, 16, 16));
    assert!(
        (0.125..=32.0).contains(&(0.18 / before)),
        "the test floor is outside the exposure's range: {before}"
    );
    assert!(
        (after - 0.18).abs() < 0.18 * 0.03,
        "{after} is not 0.18 (it was {before})"
    );
}

#[test]
fn the_tone_curve_is_khronos_pbr_neutral_then_the_srgb_curve() {
    let curved = lit_floor(Stages { tone: true, ..OFF });
    let plain = lit_floor(OFF);
    let want = render::post::neutral(linear(&plain, 16, 16).map(|c| c as f32));
    let got = linear(&curved, 16, 16);
    for c in 0..3 {
        assert!(
            (got[c] - f64::from(want[c])).abs() < 0.004,
            "channel {c}: {} is not {}",
            got[c],
            want[c]
        );
    }
}

#[test]
fn bloom_adds_five_per_cent_of_the_blur_and_spreads_light_past_an_edge() {
    // A uniform scene blurs to itself: bloom adds 0.05 of it everywhere.
    let bloomed = lit_floor(Stages { bloom: true, ..OFF });
    let plain = lit_floor(OFF);
    for (x, y) in [(2, 2), (16, 16), (29, 5)] {
        let (b, p) = (linear(&bloomed, x, y), linear(&plain, x, y));
        for c in 0..3 {
            assert!(
                (b[c] / p[c] - 1.05).abs() < 0.02,
                "{x},{y}: {} against {}",
                b[c],
                p[c]
            );
        }
    }

    // The horizon: bright sky above, a dark floor under it. Light spreads onto the floor near the
    // edge, and not far from it.
    let edge = |stages: Stages| {
        let mut gpu = Offscreen::new(128, 72, &floor_of(STONE)).unwrap();
        gpu.set_look(Look::LEGACY);
        gpu.set_stages(stages);
        gpu.render(&Camera::looking_at([0.5, 3.0, 0.5], [0.5, 3.0, -60.0]))
    };
    let (bloomed, plain) = (edge(Stages { bloom: true, ..OFF }), edge(OFF));
    let gain = |y: u32| linear(&bloomed, 64, y)[1] / linear(&plain, 64, y)[1];
    // The horizon is at the middle row. Just under it, and near the bottom.
    let (near, far) = (gain(40), gain(70));
    assert!(
        near > far + 0.03,
        "near the edge x{near:.3}, far from it x{far:.3}"
    );
    assert!((far - 1.05).abs() < 0.02, "far from the edge x{far:.3}");
}

#[test]
fn a_debug_view_is_not_exposed_bloomed_or_toned() {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(STONE)).unwrap();
    gpu.set_debug_view(DebugView::Normals);
    let all = gpu.render(&overhead());
    gpu.set_stages(OFF);
    assert_eq!(gpu.render(&overhead()), all);
    // Straight up: (0, 1, 0) is (0.5, 1, 0.5).
    let at = ((16 * SIZE + 16) * 4) as usize;
    assert_eq!(&all.rgba[at..at + 3], [128, 255, 128]);
}

// -------------------------------------------------------------------------------------------
// Smooth normals

/// A slope of stair-steps: the ground is `x / 2` Voxels high at column x, over z 0..64. Seen from
/// above, the treads are 2 wide and every face normal is straight up.
fn staircase(material: Material) -> (BrickStore, Scene) {
    let mut store = BrickStore::new();
    for x in 0..64 {
        for z in 0..64 {
            for y in -4..=x / 2 {
                store.set(VoxelCoord([x, y, z]), Some(material));
            }
        }
    }
    let scene = Scene::new(&store, None);
    (store, scene)
}

/// What the shaders do (`smooth_normal` in `common.wgsl`): the solid Voxels of a cube of 5 round
/// the empty Voxel in front of the face, each weighted by a Gaussian of 1.4 Voxels in its distance,
/// pushed away from; kept to the face's side with a quarter of its normal.
fn smooth_normal(store: &BrickStore, voxel: [i32; 3], face: [i32; 3]) -> [f64; 3] {
    let front = [0, 1, 2].map(|a| voxel[a] + face[a]);
    let mut sum = [0.0f64; 3];
    for dz in -2..=2 {
        for dy in -2..=2 {
            for dx in -2..=2 {
                let at = [front[0] + dx, front[1] + dy, front[2] + dz];
                if store.get(VoxelCoord(at)).is_some() {
                    let d2 = f64::from(dx * dx + dy * dy + dz * dz);
                    let w = (-d2 / (2.0 * 1.4 * 1.4)).exp();
                    sum[0] += f64::from(dx) * w;
                    sum[1] += f64::from(dy) * w;
                    sum[2] += f64::from(dz) * w;
                }
            }
        }
    }
    let face = face.map(f64::from);
    let length = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let l = length(sum);
    if l * l < 1e-6 {
        return face;
    }
    let tilted = [0, 1, 2].map(|a| -sum[a] / l + 0.25 * face[a]);
    let l = length(tilted);
    tilted.map(|c| c / l)
}

/// The ray that the shaders cast through the middle of `pixel` of an image `size` across.
fn pixel_ray(camera: &Camera, pixel: (u32, u32), size: u32) -> Ray {
    let ndc = |p: u32| (f64::from(p) + 0.5) / f64::from(size) * 2.0 - 1.0;
    let tan_half = (camera.fov_y / 2.0).tan();
    let (forward, right, up) = (camera.forward(), camera.right(), camera.up());
    let d = [0, 1, 2]
        .map(|a| forward[a] + right[a] * ndc(pixel.0) * tan_half - up[a] * ndc(pixel.1) * tan_half);
    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let (cell, origin) = camera::split(camera.position);
    Ray {
        cell,
        origin,
        dir: d.map(|c| (c / l) as f32),
    }
}

/// The view's normal at `pixel`, from the debug image: `(n + 1) / 2`.
fn normal_of(image: &Image, (x, y): (u32, u32)) -> [f64; 3] {
    let at = ((y * image.width + x) * 4) as usize;
    [0, 1, 2].map(|c| f64::from(image.rgba[at + c]) / 255.0 * 2.0 - 1.0)
}

fn shading_normals(scene: &Scene, camera: &Camera, stages: Stages) -> Image {
    let mut gpu = Offscreen::new(SIZE, SIZE, scene).unwrap();
    gpu.set_stages(stages);
    gpu.set_debug_view(DebugView::ShadingNormals);
    gpu.render(camera)
}

/// Above the stairs, looking down: every face normal is straight up.
fn over_the_stairs(height: f64) -> Camera {
    Camera::looking_at([32.5, height, 32.5], [32.5, 16.0, 32.5])
}

#[test]
fn smooth_normals_follow_the_slope_the_stairs_make_and_match_the_cpu_twin() {
    let (store, scene) = staircase(STONE);
    let camera = over_the_stairs(60.0);
    let smooth = shading_normals(&scene, &camera, Stages::default());
    let mut compared = 0;
    let mut tilted = 0;
    for y in (2..SIZE - 2).step_by(3) {
        for x in (2..SIZE - 2).step_by(3) {
            let ray = pixel_ray(&camera, (x, y), SIZE);
            let Some(hit) = trace(&scene, &ray) else {
                continue;
            };
            let want = smooth_normal(&store, hit.voxel, hit.normal);
            let got = normal_of(&smooth, (x, y));
            compared += 1;
            // 8 bits of a component, and the half floats it went through.
            if (0..3).all(|c| (got[c] - want[c]).abs() < 0.03) {
                tilted += 1;
            }
            // The slope rises with x, so the normal leans back towards -x, and never down.
            assert!(got[0] < 0.01 && got[1] > 0.0, "{got:?} at {x},{y}");
        }
    }
    assert!(compared >= 40, "{compared}");
    // A pixel near the edge of a tread can see a neighbouring Voxel on the GPU: nearly all agree.
    assert!(
        tilted * 10 >= compared * 9,
        "{tilted} of {compared} agree with the CPU twin"
    );
    // And it is a slope's normal, not the faces': the middle of a tread leans well over.
    let middle = normal_of(&smooth, (16, 16));
    assert!(middle[0] < -0.15, "{middle:?}");
}

#[test]
fn with_the_stage_off_every_hit_has_its_face_normal() {
    let (_, scene) = staircase(STONE);
    let camera = over_the_stairs(60.0);
    let off = shading_normals(
        &scene,
        &camera,
        Stages {
            smooth_normals: false,
            ..Stages::default()
        },
    );
    let mut gpu = Offscreen::new(SIZE, SIZE, &scene).unwrap();
    gpu.set_debug_view(DebugView::Normals);
    assert_eq!(off, gpu.render(&camera));
}

#[test]
fn built_materials_keep_their_hard_faces() {
    let (_, scene) = staircase(material("planks"));
    let camera = over_the_stairs(60.0);
    let smooth = shading_normals(&scene, &camera, Stages::default());
    let mut gpu = Offscreen::new(SIZE, SIZE, &scene).unwrap();
    gpu.set_debug_view(DebugView::Normals);
    assert_eq!(smooth, gpu.render(&camera));
}

#[test]
fn hits_past_thirty_metres_keep_their_face_normals() {
    let (_, scene) = staircase(STONE);
    let size = SIZE;
    // 30 m is 600 Voxels: from 700 Voxels up the stairs are all further than that.
    let far = over_the_stairs(700.0);
    let mut gpu = Offscreen::new(size, size, &scene).unwrap();
    gpu.set_debug_view(DebugView::Normals);
    let faces = gpu.render(&far);
    assert_eq!(shading_normals(&scene, &far, Stages::default()), faces);
    // From 100 up they are smoothed.
    let near = over_the_stairs(100.0);
    assert_ne!(shading_normals(&scene, &near, Stages::default()), {
        gpu.render(&near)
    });
}

#[test]
fn the_reference_view_is_lit_with_the_smooth_normal_too() {
    // The same ground, the sun low over the slope's lee: with smooth normals the lit side is
    // brighter and the far side darker than the flat treads' faces are, so the two images differ,
    // in the accumulation as well as in what is shown.
    let (_, scene) = staircase(STONE);
    let camera = over_the_stairs(60.0);
    let mean = |stages: Stages| {
        let mut gpu = Offscreen::new(SIZE, SIZE, &scene).unwrap();
        gpu.set_stages(stages);
        gpu.set_sun(Sun::from_angles(-1.2, 0.5, 0.05));
        for _ in 0..8 {
            gpu.reference_sample(&camera);
        }
        gpu.read_accumulation()
    };
    let (smooth, faces) = (mean(Stages::default()), mean(Stages::NONE));
    assert_ne!(smooth, faces);
    // The display transform never touches the accumulation: switching only it off changes none.
    let only_display = mean(Stages {
        smooth_normals: true,
        ..Stages::NONE
    });
    assert_eq!(only_display, smooth);
}

#[test]
fn every_stage_has_a_pass_with_a_gpu_line_of_its_own() {
    let (_, scene) = staircase(STONE);
    let mut gpu = Offscreen::with_resolution(Resolution::native(SIZE, SIZE), &scene).unwrap();
    gpu.draw_upscaled(&over_the_stairs(60.0));
    for pass in [
        "normals",
        "shade",
        "exposure",
        "exposure_adapt",
        "bloom_down",
        "bloom_h",
        "bloom_v",
        "tone",
    ] {
        assert!(gpu.timings().pass(pass).is_some(), "no pass {pass}");
    }
}

#[test]
fn the_exposure_follows_a_change_of_light_over_seconds_not_at_once() {
    use std::time::Duration;
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material("clear glass"))).unwrap();
    gpu.set_stages(Stages {
        exposure: true,
        ..OFF
    });
    gpu.set_look(Look::LEGACY);
    // The sun at 45 degrees, so that the floor's light is inside the exposure's range.
    gpu.set_sun(Sun::new([0.0, 1.0, 1.0], 0.02).unwrap());
    let luminance = |image: &Image| {
        let c = linear(image, 16, 16);
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    };
    // The first frame sets the exposure at once: middle grey.
    gpu.draw_frame(&overhead(), false, Duration::ZERO);
    let first = luminance(&gpu.shown());
    assert!((first - 0.18).abs() < 0.01, "{first}");
    // The sun goes down: the light drops to a fifth or so (the ambient 0.25 against 1.35). A sixtieth of a second on, the eye has barely moved:
    // the picture is dark, as it would be.
    gpu.set_sun(Sun::new([0.0, -1.0, 0.0], 0.02).unwrap());
    gpu.draw_frame(&overhead(), false, Duration::from_millis(17));
    let dark = luminance(&gpu.shown());
    assert!(dark < 0.18 * 0.5, "{dark}: the exposure moved at once");
    // It settles over some seconds (a frame is a second here: the longest step counted), towards
    // middle grey again, and goes no further than the exposure's range allows.
    let mut last = dark;
    for second in 1..=30 {
        gpu.draw_frame(&overhead(), false, Duration::from_secs(second));
        let now = luminance(&gpu.shown());
        assert!(now >= last - 1e-4, "second {second}: {now} after {last}");
        last = now;
    }
    assert!((last - 0.18).abs() < 0.01, "{last}");
}
