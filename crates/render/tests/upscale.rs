//! The temporal upscaler on a hand-made scene, where what it must do can be worked out: the sizes
//! of the images are independent, the G-buffer holds the first hit, a still camera converges to a
//! sharp image, a moving one is not smeared, and the history length view tells the two apart.
//! Needs a Vulkan device (lavapipe in CI). The goldens of the same two behaviours on the test
//! Planet are in `golden.rs`.

use brick_store::{BrickStore, Material, VoxelCoord};
use render::{Camera, DebugView, Image, Offscreen, Resolution, Scene, SceneUpdate};

const STONE: Material = Material(1);
/// Rock in `materials.toml`.
const STONE_ALBEDO: [f64; 3] = [0.45, 0.43, 0.40];

const INTERNAL: (u32, u32) = (48, 27);
const OUTPUT: (u32, u32) = (96, 54);

/// A floor of rock whose top face is at y = 0, x and z -32..=31, with a block on it, x 0..=7,
/// y 0..=7, z -8..=-1: edges at every angle to the pixel grid for the upscaler to get wrong.
fn floor_and_block() -> Scene {
    let mut store = BrickStore::new();
    for x in -32..32 {
        for z in -32..32 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    for x in 0..8 {
        for z in -8..0 {
            for y in 0..8 {
                store.set(VoxelCoord([x, y, z]), Some(Material(2)));
            }
        }
    }
    Scene::new(&store, None)
}

fn resolution() -> Resolution {
    Resolution {
        internal: INTERNAL,
        output: OUTPUT,
    }
}

fn gpu() -> Offscreen {
    Offscreen::with_resolution(resolution(), &floor_and_block()).unwrap()
}

/// Looks at the block and the floor around it from above and to the side.
fn view() -> Camera {
    Camera::looking_at([14.5, 12.0, 10.5], [4.0, 2.0, -4.0])
}

/// The view, turned `frames` frames' worth of a slow pan to the left: about one output pixel a
/// frame.
fn panned(frames: u32) -> Camera {
    let mut camera = view();
    camera.turn(0.008 * f64::from(frames), 0.0);
    camera
}

/// What the camera sees, as well as it can be known at the output's resolution: traced at four
/// times as many pixels and averaged down, so that edges are anti-aliased.
fn truth(camera: &Camera) -> Image {
    let big = Offscreen::new(OUTPUT.0 * 2, OUTPUT.1 * 2, &floor_and_block())
        .unwrap()
        .render(camera);
    let mut rgba = Vec::new();
    for y in 0..OUTPUT.1 {
        for x in 0..OUTPUT.0 {
            for c in 0..4 {
                let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                    .iter()
                    .map(|(dx, dy)| {
                        let at = (((2 * y + dy) * big.width + 2 * x + dx) * 4) as usize + c;
                        u32::from(big.rgba[at])
                    })
                    .sum();
                rgba.push(((sum + 2) / 4) as u8);
            }
        }
    }
    Image {
        width: OUTPUT.0,
        height: OUTPUT.1,
        rgba,
    }
}

/// Mean squared error per channel, 0..=255 scale.
fn mse(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: u64 = a
        .rgba
        .chunks(4)
        .zip(b.rgba.chunks(4))
        .flat_map(|(p, q)| (0..3).map(move |c| u64::from(p[c].abs_diff(q[c])).pow(2)))
        .sum();
    sum as f64 / (3 * a.width * a.height) as f64
}

fn pixel(image: &Image, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * image.width + x) * 4) as usize;
    [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]]
}

#[test]
fn the_traced_image_and_the_presented_one_have_sizes_of_their_own() {
    let mut gpu = gpu();
    let traced = gpu.render(&view());
    assert_eq!((traced.width, traced.height), INTERNAL);
    let presented = gpu.render_upscaled(&view());
    assert_eq!((presented.width, presented.height), OUTPUT);
    assert_eq!(presented.rgba.len(), (OUTPUT.0 * OUTPUT.1 * 4) as usize);
    // And any other pair works, up as well as down.
    let mut other = Offscreen::with_resolution(
        Resolution {
            internal: (40, 30),
            output: (30, 20),
        },
        &floor_and_block(),
    )
    .unwrap();
    assert_eq!(other.render(&view()).width, 40);
    assert_eq!(other.render_upscaled(&view()).width, 30);
}

#[test]
fn the_first_frame_is_primary_visibility_stretched_to_the_output() {
    let mut gpu = gpu();
    let traced = gpu.render(&view());
    let first = gpu.render_upscaled(&view());
    // With no history, an output pixel is the traced pixel it lies in.
    for (x, y) in [(0, 0), (31, 17), (95, 53), (50, 20)] {
        assert_eq!(pixel(&first, x, y), pixel(&traced, x / 2, y / 2), "{x},{y}");
    }
}

#[test]
fn primary_visibility_writes_the_distance_the_normal_and_the_albedo_of_the_first_hit() {
    let mut gpu = gpu();
    // 20 Voxels above the floor, looking straight down at it.
    gpu.render(&Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5]));
    let g = gpu.gbuffer();
    assert_eq!((g.width, g.height), INTERNAL);
    let pixels = (INTERNAL.0 * INTERNAL.1) as usize;
    assert_eq!(
        (g.depth.len(), g.normal.len(), g.albedo.len()),
        (pixels, pixels, pixels * 4)
    );
    // The pixel next to the middle looks almost straight down: the floor is 20 Voxels away,
    // a little more along the ray, and faces up.
    let middle = (INTERNAL.1 / 2 * INTERNAL.0 + INTERNAL.0 / 2) as usize;
    assert!(
        (20.0..21.0).contains(&g.depth[middle]),
        "{}",
        g.depth[middle]
    );
    assert_eq!(g.normal[middle], [0.0, 1.0, 0.0]);
    let encoded = STONE_ALBEDO.map(|c| c.powf(1.0 / 2.2) * 255.0);
    for c in 0..3 {
        // Stone's colour varies a little from Voxel to Voxel.
        let got = f64::from(g.albedo[middle * 4 + c]);
        assert!((got - encoded[c]).abs() < 25.0, "{got} vs {encoded:?}");
    }
    // Every ray hits the floor or the block, whose top is 12 Voxels below the camera; the
    // further from the middle, the further along the ray.
    assert!(g.depth.iter().all(|&d| d >= 11.99));
    assert!(g.depth[0] > g.depth[middle]);

    // Looking at the sky: no hit, no normal, black.
    gpu.render(&Camera::looking_at([0.5, 20.0, 0.5], [0.5, 40.0, 0.5]));
    let sky = gpu.gbuffer();
    assert!(sky.depth.iter().all(|&d| d < 0.0));
    assert!(sky.normal.iter().all(|&n| n == [0.0; 3]));
    assert!(sky.albedo.chunks(4).all(|p| p == [0, 0, 0, 255]));
}

#[test]
fn a_still_camera_converges_to_the_full_resolution_image() {
    let mut gpu = gpu();
    let truth = truth(&view());
    let first = mse(&gpu.render_upscaled(&view()), &truth);
    let mut last = first;
    let mut errors = vec![first];
    for _ in 0..31 {
        last = mse(&gpu.render_upscaled(&view()), &truth);
        errors.push(last);
    }
    eprintln!("mse against the supersampled image by frame: {errors:.1?}");
    assert!(last < first / 2.0, "{first} then {last}");
    // The same scene traced at every output pixel, to within the anti-aliasing the jitter adds
    // at edges: nearly all of the image is the same flat colours.
    assert!(last < 20.0, "{last}");
    // And it is steady: another frame changes nothing much.
    let next = gpu.render_upscaled(&view());
    let again = gpu.render_upscaled(&view());
    assert!(mse(&next, &again) < 2.0, "{}", mse(&next, &again));
}

#[test]
fn a_still_camera_keeps_the_edges_as_steep_as_the_supersampled_image() {
    // Sharpness as the size of the steps between neighbouring pixels along the block's top edge:
    // a blurred edge has small steps in many pixels, a sharp one a big step in a few.
    let steepest = |image: &Image| {
        let mut steps = Vec::new();
        for y in 0..image.height {
            for x in 1..image.width {
                let (a, b) = (pixel(image, x - 1, y), pixel(image, x, y));
                steps.push((0..3).map(|c| i32::from(a[c].abs_diff(b[c]))).sum::<i32>());
            }
        }
        steps.into_iter().max().unwrap()
    };
    let mut gpu = gpu();
    gpu.render_upscaled(&view());
    for _ in 0..30 {
        gpu.render_upscaled(&view());
    }
    let converged = gpu.render_upscaled(&view());
    let truth = truth(&view());
    eprintln!(
        "steepest step: {} against {}",
        steepest(&converged),
        steepest(&truth)
    );
    // The edge of the block against the floor is as steep as in the supersampled image.
    assert!(steepest(&converged) * 10 >= steepest(&truth) * 8);
}

/// The view, moved `frames` frames' worth sideways, a couple of output pixels a frame at the
/// distance of the block, with the camera still looking the same way. Unlike a turn, this moves
/// the camera, so the reprojection needs the shift between the cameras.
fn strafed(frames: u32) -> Camera {
    let mut camera = view();
    let right = camera.right();
    camera.position =
        std::array::from_fn(|a| camera.position[a] + right[a] * 0.25 * f64::from(frames));
    camera
}

/// The view, moved `frames` frames' worth towards what it looks at.
fn dollied(frames: u32) -> Camera {
    let mut camera = view();
    let forward = camera.forward();
    camera.position =
        std::array::from_fn(|a| camera.position[a] + forward[a] * 0.3 * f64::from(frames));
    camera
}

/// Flies `path` for 24 frames and checks that the last is not smeared: it is `gain` times closer
/// to what the camera sees now than to what it saw 8 frames ago, and no worse than one frame upscaled
/// with no history.
fn check_motion(path: fn(u32) -> Camera, gain: f64) {
    let mut gpu = gpu();
    let frames = 24;
    let mut image = gpu.render_upscaled(&path(0));
    for frame in 1..=frames {
        image = gpu.render_upscaled(&path(frame));
    }
    let now = mse(&image, &truth(&path(frames)));
    // A frame of history that was not moved with the camera would be this far from it.
    let unmoved = mse(&image, &truth(&path(frames - 8)));
    // A single upscaled frame is the least that the motion can be asked to match.
    let mut fresh = Offscreen::with_resolution(resolution(), &floor_and_block()).unwrap();
    let single = mse(&fresh.render_upscaled(&path(frames)), &truth(&path(frames)));
    eprintln!(
        "motion: {now:.1} against the camera now, {unmoved:.1} against 8 frames back, a single frame {single:.1}"
    );
    assert!(now * gain < unmoved, "{now} against {unmoved}");
    assert!(now < single, "{now} against {single}");
}

#[test]
fn a_panning_camera_is_not_smeared_beyond_what_clamping_allows() {
    check_motion(panned, 3.0);
}

#[test]
fn a_camera_moving_sideways_is_not_smeared_beyond_what_clamping_allows() {
    // Sideways, the block uncovers what is behind it, which no history can show.
    check_motion(strafed, 1.5);
}

#[test]
fn a_camera_moving_forwards_is_not_smeared_beyond_what_clamping_allows() {
    check_motion(dollied, 3.0);
}

#[test]
fn history_that_no_longer_matches_is_thrown_away_and_the_new_colour_wins_at_once() {
    // A floor of one Material that turns into another between two frames of the same view: what
    // the history holds is wrong everywhere, and the clamp cannot make it right.
    let scene = |material| {
        let mut store = BrickStore::new();
        for x in -32..32 {
            for z in -32..32 {
                store.set(VoxelCoord([x, -1, z]), Some(material));
            }
        }
        Scene::new(&store, None)
    };
    let camera = Camera::looking_at([0.5, 10.0, 0.5], [0.5, 0.0, 0.5]);
    // Twenty frames of stone, then one frame of soil, shown as `view`.
    let frame_after_the_change = |view| {
        let mut gpu = Offscreen::with_resolution(resolution(), &scene(STONE)).unwrap();
        for _ in 0..20 {
            gpu.render_upscaled(&camera);
        }
        gpu.update_scene(&scene(Material(2)), &SceneUpdate::Rebuilt)
            .unwrap();
        gpu.set_debug_view(view);
        pixel(&gpu.render_upscaled(&camera), 40, 20)
    };
    // The history has started again: one frame, the dark end of the scale (0.10, 0, 0.30).
    near(
        frame_after_the_change(DebugView::HistoryLength),
        [25.5, 0.0, 76.5],
    );
    // And the first frame already shows the new colour, to within the variation of a Voxel's
    // colour: nothing of the old shows through.
    let soil = frame_after_the_change(DebugView::Lit);
    let encoded = |c: [f64; 3]| c.map(|c| c.powf(1.0 / 2.2) * 255.0);
    let (new, old) = (encoded([0.3, 0.5, 0.2]), encoded(STONE_ALBEDO));
    let off =
        |p: [u8; 3], to: [f64; 3]| (0..3).map(|c| (f64::from(p[c]) - to[c]).abs()).sum::<f64>();
    assert!(off(soil, new) < off(soil, old) / 3.0, "{soil:?}");
}

#[test]
fn history_length_shows_a_young_history_dark_and_an_old_one_bright() {
    let mut gpu = gpu();
    gpu.set_debug_view(DebugView::HistoryLength);
    // On the floor, away from the block and its edges.
    let at = (10, 48);
    // One frame of history: the dark end of the scale, (0.10, 0, 0.30).
    let one = gpu.render_upscaled(&view());
    near(pixel(&one, at.0, at.1), [25.5, 0.0, 76.5]);
    // Sixteen frames, which is as long as it goes: the bright end, (1, 0.9, 0.2).
    for _ in 0..15 {
        gpu.render_upscaled(&view());
    }
    let old = gpu.render_upscaled(&view());
    near(pixel(&old, at.0, at.1), [255.0, 229.5, 51.0]);
    // A camera that turns away has nothing in its history: every pixel starts again.
    let mut away = view();
    away.turn(std::f64::consts::PI, 0.0);
    let young = gpu.render_upscaled(&away);
    near(pixel(&young, at.0, at.1), [25.5, 0.0, 76.5]);
}

#[test]
fn history_length_grows_by_a_frame_at_a_time() {
    let mut gpu = gpu();
    gpu.set_debug_view(DebugView::HistoryLength);
    let mut reds = Vec::new();
    for _ in 0..5 {
        reds.push(pixel(&gpu.render_upscaled(&view()), 10, 48)[0]);
    }
    // Frame n is (n - 1) / 15 of the way from 25.5 to 255 in red.
    let expected: Vec<f64> = (0..5).map(|n| 25.5 + 229.5 * f64::from(n) / 15.0).collect();
    for (got, want) in reds.iter().zip(&expected) {
        assert!(
            (f64::from(*got) - want).abs() <= 2.0,
            "{reds:?} vs {expected:?}"
        );
    }
}

#[test]
fn a_debug_view_shows_what_was_traced_and_leaves_no_history_behind() {
    let mut gpu = gpu();
    for _ in 0..5 {
        gpu.render_upscaled(&view());
    }
    gpu.set_debug_view(DebugView::Normals);
    let normals = gpu.render_upscaled(&view());
    // The floor faces up: (0.5, 1, 0.5) once scaled to 0..1.
    near(pixel(&normals, 10, 48), [127.5, 255.0, 127.5]);
    gpu.set_debug_view(DebugView::Lit);
    let back = gpu.render_upscaled(&view());
    // The lit image is not coloured by the debug view's: it is a first frame again.
    let mut fresh = Offscreen::with_resolution(resolution(), &floor_and_block()).unwrap();
    assert_eq!(back, fresh.render_upscaled(&view()));
}

#[test]
fn a_frame_runs_the_primary_pass_then_the_upscale_pass_and_times_both() {
    let mut gpu = gpu();
    gpu.render_upscaled(&view());
    let names: Vec<_> = gpu.timings().passes().iter().map(|p| &*p.name).collect();
    assert_eq!(names, ["primary", "upscale"]);
    if gpu.timers_supported() {
        for pass in gpu.timings().passes() {
            assert!(
                pass.times.average().is_some_and(f64::is_finite),
                "{}",
                pass.name
            );
        }
    }
}

#[test]
fn the_reference_image_is_traced_at_the_internal_resolution() {
    let mut gpu = gpu();
    let image = gpu.render_reference(&view(), 2);
    assert_eq!((image.width, image.height), INTERNAL);
}

fn near(actual: [u8; 3], expected: [f64; 3]) {
    for c in 0..3 {
        assert!(
            (f64::from(actual[c]) - expected[c]).abs() <= 2.0,
            "{actual:?} is not {expected:?}"
        );
    }
}

#[test]
fn drawing_without_reading_back_leaves_the_same_history_and_the_same_times() {
    let mut drawn = gpu();
    let mut read = gpu();
    for frame in 0..4 {
        drawn.draw_upscaled(&panned(frame));
        read.render_upscaled(&panned(frame));
    }
    assert_eq!(
        drawn.render_upscaled(&panned(4)),
        read.render_upscaled(&panned(4))
    );
    if drawn.timers_supported() {
        // Every frame is timed, and the times are in when the call returns.
        let times = |gpu: &Offscreen, pass| gpu.timings().pass(pass).unwrap().times.count();
        assert_eq!(times(&drawn, "primary"), times(&read, "primary"));
        assert_eq!(times(&drawn, "upscale"), times(&read, "upscale"));
    }
}
