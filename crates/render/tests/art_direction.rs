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
/// pushed away from; kept to the face's side with `bias` of its normal.
fn smooth_normal(store: &BrickStore, voxel: [i32; 3], face: [i32; 3], bias: f64) -> [f64; 3] {
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
    let tilted = [0, 1, 2].map(|a| -sum[a] / l + bias * face[a]);
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
            let want = smooth_normal(&store, hit.voxel, hit.normal, 0.25);
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

/// The linear luminance of the middle pixel of an image.
fn middle_luminance(image: &Image) -> f64 {
    let c = linear(image, image.width / 2, image.height / 2);
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// The floor with the exposure alone on, under the legacy light with the sun at `direction`.
fn exposure_only(sun: [f32; 3]) -> Offscreen {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material("clear glass"))).unwrap();
    gpu.set_stages(Stages {
        exposure: true,
        ..OFF
    });
    gpu.set_look(Look::LEGACY);
    gpu.set_sun(Sun::new(sun, 0.02).unwrap());
    gpu
}

// Low enough that the floor is under 1 (the 8-bit image clips at 1).
const BRIGHT_SUN: [f32; 3] = [0.0, 1.0, 3.0];
const NO_SUN: [f32; 3] = [0.0, -1.0, 0.0];

#[test]
fn the_exposure_moves_one_minus_e_to_the_minus_dt_over_tau_of_the_way_in_stops() {
    use std::time::Duration;
    // What the floor is under each sun with nothing adapting: the exposure's range holds both, and
    // the exposure that settles a scene is 0.18 over its luminance.
    let settled = |sun| {
        let mut gpu = exposure_only(sun);
        gpu.set_stages(OFF);
        let image = gpu.render(&overhead());
        let mut logs = 0.0;
        for y in 0..image.height {
            for x in 0..image.width {
                let c = linear(&image, x, y);
                logs += (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
                    .max(1e-4)
                    .ln();
            }
        }
        (0.18 / (logs / f64::from(image.width * image.height)).exp()).log2() as f32
    };
    let (from, to) = (settled(BRIGHT_SUN), settled(NO_SUN));
    assert!(
        (-3.0..=5.0).contains(&from) && (-3.0..=5.0).contains(&to),
        "{from} {to}"
    );

    let mut gpu = exposure_only(BRIGHT_SUN);
    gpu.draw_frame(&overhead(), false, Duration::ZERO);
    let first = gpu.exposure_stops(false).unwrap();
    assert!(
        (first - from).abs() < 0.02,
        "the first frame sets it at once: {first} for {from}"
    );
    gpu.set_sun(Sun::new(NO_SUN, 0.02).unwrap());
    // One second on (dt = 1 s, tau = 2.5 s): the exposure has moved 1 - e^(-0.4) = 33 % of the way
    // from where it was to where the dark scene settles it, measured in stops.
    gpu.draw_frame(&overhead(), false, Duration::from_secs(1));
    let stops = gpu.exposure_stops(false).unwrap();
    let moved = f64::from((stops - first) / (to - first));
    let expected = 1.0 - (-1.0f64 / 2.5).exp();
    assert!(
        (moved - expected).abs() < 0.02,
        "moved {moved} of the way, expected {expected}"
    );
}

#[test]
fn primary_keeps_its_own_exposure_through_a_still_view_of_the_reference() {
    use std::time::Duration;
    let mut gpu = exposure_only(BRIGHT_SUN);
    gpu.draw_frame(&overhead(), false, Duration::ZERO);
    let before = middle_luminance(&gpu.shown());
    assert!((before - 0.18).abs() < 0.01, "{before}");
    // The Reference view is looking at a dark scene (the sun gone), and meters it: far brighter
    // exposure than the bright floor's.
    gpu.set_sun(Sun::new(NO_SUN, 0.02).unwrap());
    gpu.reference_batch(&overhead(), 4);
    let reference = middle_luminance(&gpu.render_reference(&overhead(), 0));
    assert!(
        reference > 0.1,
        "the Reference view meters its own scene: {reference}"
    );
    // The camera moves on and the light is back: Primary's first frame starts from its own last
    // exposure, so the bright floor is what it was, not the dark scene's exposure on a bright floor.
    gpu.set_sun(Sun::new(BRIGHT_SUN, 0.02).unwrap());
    gpu.draw_frame(&overhead(), false, Duration::from_millis(17));
    let after = middle_luminance(&gpu.shown());
    assert!((after - before).abs() < 0.01, "{after} after {before}");
}

/// A courtyard, 10 Voxels square and 14 deep, in a floor: light reaches its floor by the sky and
/// the walls' bounces, so a few samples of it are mostly dark noise.
fn courtyard() -> Scene {
    let mut store = BrickStore::new();
    for x in -40..40 {
        for z in -40..40 {
            let inside = (-5..5).contains(&x) && (-5..5).contains(&z);
            let top = if inside { -14 } else { 0 };
            for y in -30..top {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    Scene::new(&store, None)
}

#[test]
fn the_reference_view_looks_as_bright_at_four_samples_as_at_sixty_four() {
    // Looking into the courtyard: four samples' mean is mostly dark pixels, and a meter that
    // read each of them would push the exposure up. The Reference view meters the mean's 4 x 4 boxes
    // and holds the exposure from 16 samples, so the brightness the view converges at is the
    // one it starts at, to within the noise.
    let scene = courtyard();
    let camera = Camera::looking_at([0.5, 12.0, 0.5], [0.5, -14.0, 0.5]);
    let mut gpu = Offscreen::new(SIZE * 2, SIZE * 2, &scene).unwrap();
    gpu.set_stages(Stages {
        exposure: true,
        ..OFF
    });
    gpu.set_sun(Sun::from_angles(0.3, 0.9, 0.3));
    let stops = |gpu: &mut Offscreen, taken| {
        gpu.reference_batch(&camera, taken);
        gpu.exposure_stops(true).unwrap()
    };
    let one = stops(&mut gpu, 4);
    let sixty_four = stops(&mut gpu, 60);
    let later = stops(&mut gpu, 192);
    eprintln!("exposure at 4: {one}, 64: {sixty_four}, 256: {later} stops");
    // Within 0.04 stops (3 % in brightness). Metered on pixels, not boxes, the same views differ by
    // 0.065 on lavapipe: the bound is between the two.
    assert!(
        (one - sixty_four).abs() < 0.04,
        "{one} stops at 4 samples, {sixty_four} at 64"
    );
    // Held from 16 samples: more of them make the noise fall and do not move the brightness.
    assert_eq!(later, sixty_four, "the exposure is held");
}

/// A flat ground, Voxels y -4..=-1 (its top face is at y = 0), x and z -120..120, as rings: the
/// bands' `d0` of 40 puts the end of level 0 at 80 Voxels.
fn ringed_ground() -> Scene {
    let mut store = BrickStore::new();
    for x in -120..120 {
        for z in -120..120 {
            for y in -4..0 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    store.build_lod(4);
    let bound = render::scene::Bound {
        centre: [0, 0, 0],
        radius: 400,
    };
    Scene::rings(
        &store,
        [0.5, 20.0, 0.5],
        render::lod::Bands::with_d0(40.0),
        bound,
    )
    .unwrap()
}

#[test]
fn the_smooth_normals_end_before_level_zero_does() {
    // Level 0 ends at 80 Voxels (a brick is certain to be its own up to 0.95 of that). Smoothed at
    // the seam, a Voxel's window would hold the next level's bricks, which level 0 does not list,
    // as air, and the flat ground's normal would tilt along the ring where they begin. On flat
    // ground every normal is straight up, at every distance.
    let scene = ringed_ground();
    let camera = Camera::looking_at([0.5, 20.0, 0.5], [60.5, 8.0, 0.5]);
    let image = shading_normals(&scene, &camera, Stages::default());
    let mut ground = 0;
    for y in 0..image.height {
        for x in 0..image.width {
            let n = normal_of(&image, (x, y));
            if n[1] > 0.0 {
                ground += 1;
                assert!(n[1] > 0.98, "({x}, {y}): the normal {n:?} is tilted");
            }
        }
    }
    assert!(
        ground > SIZE * SIZE / 4,
        "the ground fills the view: {ground}"
    );
}

#[test]
fn the_smooth_normal_fades_into_the_face_normal_over_the_last_fifth_of_its_range() {
    // Straight down onto the stairs from `t` Voxels over them: the middle of the image is `t`
    // away, on the tread of the step at x = 32, 33. The tilt (the normal's x) is the smooth normal's
    // up to 24 m (480 Voxels), is none from 30 m (600), and goes between in a line.
    let (_, scene) = staircase(STONE);
    let tilt_at = |t: f64| {
        let camera = over_the_stairs(17.0 + t);
        let image = shading_normals(&scene, &camera, Stages::default());
        normal_of(&image, (SIZE / 2, SIZE / 2))[0]
    };
    let full = tilt_at(300.0);
    assert!(full.abs() > 0.1, "the stairs tilt the normal: {full}");
    assert!(
        (tilt_at(470.0) - full).abs() < 0.02,
        "full smoothing before 24 m"
    );
    let half = tilt_at(540.0);
    assert!(
        (half - full / 2.0).abs() < 0.03,
        "half way: {half} for {full}"
    );
    let near_end = tilt_at(588.0);
    assert!(
        (near_end - full / 10.0).abs() < 0.03,
        "a tenth left at 29.4 m: {near_end}"
    );
    assert!(tilt_at(610.0).abs() < 0.01, "none past 30 m");
}

/// `image` (RGB, `width` wide) blurred along `axis` by a Gaussian of `sigma` pixels, its edge
/// repeated beyond it: what the prototype's CPU blur did, at full resolution.
fn gaussian(
    image: &[[f32; 3]],
    (width, height): (usize, usize),
    vertical: bool,
    sigma: f32,
) -> Vec<[f32; 3]> {
    let radius = (sigma * 3.0).ceil() as i32;
    let weights: Vec<f32> = (-radius..=radius)
        .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    let mut out = vec![[0.0; 3]; image.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0.0f32; 3];
            for (k, w) in weights.iter().enumerate() {
                let i = k as i32 - radius;
                let (sx, sy) = if vertical {
                    (x, (y as i32 + i).clamp(0, height as i32 - 1) as usize)
                } else {
                    ((x as i32 + i).clamp(0, width as i32 - 1) as usize, y)
                };
                for c in 0..3 {
                    sum[c] += image[sy * width + sx][c] * w;
                }
            }
            out[y * width + x] = sum.map(|v| v / total);
        }
    }
    out
}

/// The bloom of the prototype, at full resolution: the mean of three Gaussians of
/// `[2.5, 7, 18] * scale` pixels.
fn full_resolution_bloom(image: &[[f32; 3]], size: (usize, usize), scale: f32) -> Vec<[f32; 3]> {
    let blurs: Vec<_> = [2.5, 7.0, 18.0]
        .map(|s| {
            gaussian(
                &gaussian(image, size, false, s * scale),
                size,
                true,
                s * scale,
            )
        })
        .into();
    (0..image.len())
        .map(|i| std::array::from_fn(|c| (blurs[0][i][c] + blurs[1][i][c] + blurs[2][i][c]) / 3.0))
        .collect()
}

/// The GPU's quarter-size bloom, read at full-resolution pixel `(x, y)` as the `tone` pass does:
/// bilinear, at the pixel's centre.
fn bloom_at(
    bloom: &(u32, u32, Vec<[f32; 3]>),
    full: (usize, usize),
    x: usize,
    y: usize,
) -> [f32; 3] {
    let (w, h, pixels) = (bloom.0 as usize, bloom.1 as usize, &bloom.2);
    let u = (x as f32 + 0.5) / full.0 as f32 * w as f32 - 0.5;
    let v = (y as f32 + 0.5) / full.1 as f32 * h as f32 - 0.5;
    let (x0, y0) = (u.floor(), v.floor());
    let (fx, fy) = (u - x0, v - y0);
    let at = |i: f32, j: f32| {
        pixels[(j.clamp(0.0, h as f32 - 1.0) as usize) * w + i.clamp(0.0, w as f32 - 1.0) as usize]
    };
    std::array::from_fn(|c| {
        let top = at(x0, y0)[c] * (1.0 - fx) + at(x0 + 1.0, y0)[c] * fx;
        let bottom = at(x0, y0 + 1.0)[c] * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0)[c] * fx;
        top * (1.0 - fy) + bottom * fy
    })
}

#[test]
fn the_bloom_of_a_bright_pixel_is_the_prototypes_blur_at_its_sigmas_within_the_quarter_size_error()
{
    // One lamp in a black sky, seen from 91 Voxels away, so that it is about a pixel: the image is
    // black everywhere but there, and what the GPU's bloom is, is the blur of that pixel.
    let (width, height) = (640usize, 360usize);
    let mut store = BrickStore::new();
    store.set(VoxelCoord([0, 0, 0]), Some(material("white light")));
    let scene = Scene::new(&store, None);
    let mut gpu = Offscreen::new(width as u32, height as u32, &scene).unwrap();
    gpu.set_stages(Stages { bloom: true, ..OFF });
    gpu.set_look(Look {
        sun: [0.0; 3],
        zenith: [0.0; 3],
        horizon: [0.0; 3],
        ..Look::LEGACY
    });
    gpu.render(&Camera::looking_at([0.5, 91.0, 0.5], [0.5, 0.0, 0.5]));
    let ((_, _, hdr), bloom) = gpu.read_hdr_and_bloom();
    let size = (width, height);
    let truth = full_resolution_bloom(&hdr, size, width as f32 / 640.0);
    // Over the lamp and out to 80 pixels from it, along the row it is in.
    let lamp = (0..hdr.len())
        .max_by(|&a, &b| hdr[a][1].total_cmp(&hdr[b][1]))
        .unwrap();
    let (lx, ly) = (lamp % width, lamp / width);
    let peak = truth[lamp][1];
    assert!(
        hdr[lamp][1] > 1.0 && hdr[lamp + 40][1] == 0.0,
        "one lamp in the dark"
    );
    // How far the GPU's bloom along the lamp's row, from 10 pixels out (the tail, which the
    // widest blurs make) to 60, is from a full-resolution blur at `scale` times the decided
    // sigmas, as a share of the light in that tail. The narrowest blur (2.5 pixels here) is under
    // a texel at a quarter of the size and is read bilinearly, so over the lamp itself the bloom is
    // about half its true value, and at 6 to 8 pixels out a quarter high; that is the accepted
    // departure of the quarter-size bloom, and the tail is where the sigmas are told.
    let tail = |scale: f32| {
        let blur = full_resolution_bloom(&hdr, size, width as f32 / 640.0 * scale);
        let (mut difference, mut light) = (0.0f32, 0.0f32);
        for dx in 10..=60usize {
            let at = ly * width + lx + dx;
            difference += (bloom_at(&bloom, size, lx + dx, ly)[1] - blur[at][1]).abs();
            light += truth[at][1];
        }
        difference / light
    };
    let (decided, narrower, wider) = (tail(1.0), tail(0.8), tail(1.25));
    eprintln!(
        "tail error: decided {decided:.3}, 20 % narrower {narrower:.3}, 25 % wider {wider:.3}"
    );
    assert!(decided < 0.06, "{decided}");
    assert!(
        narrower > 2.0 * decided && wider > 2.0 * decided,
        "the sigmas are told: {narrower} and {wider} against {decided}"
    );
    // Over the lamp the quarter-size bloom is about half the true one (0.5 of the peak on
    // lavapipe); it is bounded, and it is 0.05 of that which is added.
    let core = (bloom_at(&bloom, size, lx, ly)[1] - peak).abs() / peak;
    assert!(core < 0.6, "{core}");
    // The light that it carries: the bloom's total (a texel is 16 pixels) is the truth's, to 2 %.
    let gpu_total: f32 = bloom.2.iter().map(|p| p[1]).sum::<f32>() * 16.0;
    let true_total: f32 = truth.iter().map(|p| p[1]).sum();
    assert!(
        (gpu_total / true_total - 1.0).abs() < 0.02,
        "{gpu_total} against {true_total}"
    );
}

#[test]
fn the_reference_view_meters_afresh_after_a_debug_view_instead_of_holding_a_stale_exposure() {
    // A bright view is metered; a debug view is shown (the exposure passes are skipped); the view
    // changes, and 20 samples pass (more than the 16 after which the exposure is held); back on the
    // lit image the Reference view must have metered the new view, not hold the old one.
    let scene = floor_of(material("clear glass"));
    let mut gpu = Offscreen::new(SIZE, SIZE, &scene).unwrap();
    gpu.set_stages(Stages {
        exposure: true,
        ..OFF
    });
    gpu.set_look(Look::LEGACY);
    gpu.set_sun(Sun::new(BRIGHT_SUN, 0.02).unwrap());
    gpu.reference_batch(&overhead(), 4);
    let bright = gpu.exposure_stops(true).unwrap();
    gpu.set_debug_view(DebugView::Normals);
    gpu.reference_batch(&overhead(), 1);
    // The view changes (the sun goes down) and takes 20 samples under the debug view.
    gpu.set_sun(Sun::new(NO_SUN, 0.02).unwrap());
    gpu.reference_batch(&overhead(), 20);
    gpu.set_debug_view(DebugView::Lit);
    gpu.reference_batch(&overhead(), 1);
    let metered = gpu.exposure_stops(true).unwrap();
    // What a fresh Reference view meters of the dark scene, with the stage on from the start.
    let mut fresh = Offscreen::new(SIZE, SIZE, &scene).unwrap();
    fresh.set_stages(Stages {
        exposure: true,
        ..OFF
    });
    fresh.set_look(Look::LEGACY);
    fresh.set_sun(Sun::new(NO_SUN, 0.02).unwrap());
    fresh.reference_batch(&overhead(), 21);
    let truth = fresh.exposure_stops(true).unwrap();
    assert!(
        (metered - truth).abs() < 0.05,
        "held {metered}, the view meters {truth} (was {bright})"
    );
    assert!(
        (metered - bright).abs() > 0.3,
        "the scene changed enough to tell"
    );
}

/// The floor under hero light B with its sun's radiance as `sun`, shown under each tone curve with
/// the exposure off: for each curve, the pixel at the middle of the image, and the linear light
/// that the pixel was shown from (read from the lit image before the transform).
fn floor_under_each_curve(sun: [f32; 3]) -> Vec<(render::ToneCurve, [f32; 3], [f32; 3])> {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material("grassy soil"))).unwrap();
    gpu.set_look(Look {
        sun,
        ..Look::HERO_B
    });
    gpu.set_sun(Sun::new([0.3, 0.8, 0.5], 0.12).unwrap());
    render::ToneCurve::ALL
        .into_iter()
        .map(|curve| {
            gpu.set_stages(Stages {
                tone: true,
                curve,
                ..OFF
            });
            let image = gpu.render(&overhead());
            let at = ((16 * image.width + 16) * 4) as usize;
            let shown = [0, 1, 2].map(|c| f32::from(image.rgba[at + c]) / 255.0);
            let ((width, _, light), _) = gpu.read_hdr_and_bloom();
            (curve, shown, light[(16 * width + 16) as usize])
        })
        .collect()
}

#[test]
fn each_tone_curve_shows_the_light_the_way_its_cpu_twin_says_from_the_toe_to_the_shoulder() {
    // A dim sun, and one so bright that the floor's light is far past 1, where the curves roll off.
    let mut brightest = 0.0f32;
    for sun in [[0.5, 0.3, 0.1], [60.0, 20.0, 8.0]] {
        let shown = floor_under_each_curve(sun);
        for &(curve, got, light) in &shown {
            brightest = brightest.max(light[0]);
            let want = curve.apply(light);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() < 0.012,
                    "{curve:?} channel {c}: {} is not {} (light {light:?})",
                    got[c],
                    want[c]
                );
            }
        }
        // The three are three different pictures: AgX is not Neutral, and the desaturated one is
        // less colourful than Neutral.
        let spread = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
        let by = |curve| shown.iter().find(|s| s.0 == curve).unwrap().1;
        let (neutral, agx, desat) = (
            by(render::ToneCurve::Neutral),
            by(render::ToneCurve::Agx),
            by(render::ToneCurve::NeutralDesat),
        );
        let apart = (0..3)
            .map(|c| (neutral[c] - agx[c]).abs())
            .fold(0.0, f32::max);
        assert!(apart > 0.02, "{shown:?}");
        assert!(spread(desat) < spread(neutral), "{shown:?}");
    }
    assert!(
        brightest > 8.0,
        "the bright case reaches the shoulder: {brightest}"
    );
}

#[test]
fn without_the_face_bias_the_smooth_normal_is_the_smooth_one_alone_and_the_image_starts_over() {
    let (store, scene) = staircase(STONE);
    let camera = over_the_stairs(60.0);
    let stages = Stages {
        normal_bias: render::NormalBias::None,
        ..Stages::default()
    };
    let unbiased = shading_normals(&scene, &camera, stages);
    let biased = shading_normals(&scene, &camera, Stages::default());
    let mut compared = 0;
    let mut agree = 0;
    for y in (2..SIZE - 2).step_by(3) {
        for x in (2..SIZE - 2).step_by(3) {
            let ray = pixel_ray(&camera, (x, y), SIZE);
            let Some(hit) = trace(&scene, &ray) else {
                continue;
            };
            let want = smooth_normal(&store, hit.voxel, hit.normal, 0.0);
            let got = normal_of(&unbiased, (x, y));
            compared += 1;
            if (0..3).all(|c| (got[c] - want[c]).abs() < 0.03) {
                agree += 1;
            }
        }
    }
    assert!(compared >= 40, "{compared}");
    assert!(
        agree * 10 >= compared * 9,
        "{agree} of {compared} agree with the CPU twin"
    );
    // Without the bias the normal leans further over the slope than with it.
    let (a, b) = (normal_of(&unbiased, (16, 16)), normal_of(&biased, (16, 16)));
    assert!(a[0] < b[0] - 0.05, "{a:?} against {b:?}");

    // The bias is part of what is lit: the Reference image starts over when it changes (and does
    // not when only the tone curve does).
    let mut gpu = Offscreen::new(SIZE, SIZE, &scene).unwrap();
    let camera = over_the_stairs(60.0);
    gpu.reference_batch(&camera, 4);
    assert_eq!(gpu.reference_samples(), 4);
    gpu.set_stages(Stages {
        curve: render::ToneCurve::Neutral,
        ..Stages::default()
    });
    assert_eq!(gpu.reference_samples(), 4);
    gpu.set_stages(stages);
    assert_eq!(gpu.reference_samples(), 0);
}

#[test]
fn the_reference_mean_can_be_shown_again_under_another_curve_without_taking_a_sample() {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor_of(material("grassy soil"))).unwrap();
    let camera = overhead();
    let first = gpu.render_reference(&camera, 4);
    gpu.set_stages(Stages {
        curve: render::ToneCurve::Neutral,
        ..Stages::default()
    });
    let neutral = gpu.reshow_reference().unwrap();
    assert_eq!(gpu.reference_samples(), 4);
    assert_ne!(first.rgba, neutral.rgba);
    gpu.set_stages(Stages::default());
    let again = gpu.reshow_reference().unwrap();
    assert_eq!(
        first.rgba, again.rgba,
        "the same mean, curve and exposure give the same image"
    );
    // After a restart there is no mean to show: not the old one.
    gpu.set_stages(Stages {
        normal_bias: render::NormalBias::None,
        ..Stages::default()
    });
    assert_eq!(gpu.reference_samples(), 0);
    assert!(gpu.reshow_reference().is_none());
}
