//! The Reference view's still-frame policy on a real (software) device (#189, ADR 0005): batches
//! of samples that add up to the same accumulation as single samples, the Reference view cut to
//! Primary and started over from sample 0 by any change, and the fade that is never stored.
//! Needs a Vulkan device (lavapipe in CI).

use std::time::Duration;

use brick_store::{Brick, BrickCoord, BrickStore, Material, VoxelCoord};
use render::batch::MAX_BATCH_SAMPLES;
use render::scene::{Core, SceneUpdate};
use render::still::{FADE, Plan};
use render::{Camera, Image, Offscreen, Resolution, Scene, Sun};

const STONE: Material = Material(1);
const SOIL: Material = Material(2);
const CORE: Core = Core {
    radius: 96,
    material: STONE,
};
const SIZE: u32 = 24;
/// One frame at 60 Hz.
const FRAME: Duration = Duration::from_micros(16_667);

/// Looking down at the top of a ball of stone with a soil crust, from outside: the scene of the
/// Edit tests, which has bounce light (the crust is lit by the sky and by the sun).
fn camera() -> Camera {
    Camera::looking_at([0.0, 150.0, 0.0], [0.0, 80.0, 0.0])
}

fn other_camera() -> Camera {
    Camera::looking_at([20.0, 150.0, 0.0], [0.0, 80.0, 0.0])
}

fn store() -> BrickStore {
    let mut store = BrickStore::new();
    for x in -4..4 {
        for z in -4..4 {
            store.insert(BrickCoord([x, 12, z]), Brick::solid(SOIL));
        }
    }
    store
}

fn scene() -> Scene {
    Scene::new(&store(), Some(CORE))
}

fn new_gpu() -> Offscreen {
    Offscreen::new(SIZE, SIZE, &scene()).unwrap()
}

/// The bits of every float, so that a comparison is of the bits and not of the values (NaN equals
/// NaN, 0.0 differs from -0.0).
fn bits(accumulation: &[f32]) -> Vec<u32> {
    accumulation.iter().map(|f| f.to_bits()).collect()
}

/// The accumulation of `samples` single samples from sample 0, on a view of its own.
fn singles(camera: &Camera, samples: u32) -> Vec<u32> {
    let mut gpu = new_gpu();
    for _ in 0..samples {
        gpu.reference_sample(camera);
    }
    bits(&gpu.read_accumulation())
}

#[test]
fn two_batches_of_three_are_six_single_samples_bit_for_bit() {
    let six = singles(&camera(), 6);

    let mut gpu = new_gpu();
    assert_eq!(gpu.reference_batch(&camera(), 3), 3);
    assert_eq!(gpu.reference_batch(&camera(), 3), 3);
    assert_eq!(gpu.reference_samples(), 6);
    assert_eq!(bits(&gpu.read_accumulation()), six);

    // However the six are split, a batch of any size is the same samples.
    for split in [[6, 0], [1, 5], [5, 1], [2, 4]] {
        let mut gpu = new_gpu();
        for count in split.into_iter().filter(|&c| c > 0) {
            gpu.reference_batch(&camera(), count);
        }
        assert_eq!(gpu.reference_samples(), 6, "{split:?}");
        assert_eq!(bits(&gpu.read_accumulation()), six, "{split:?}");
    }

    // The image that is shown is the same too, not just the float mean.
    let mut batched = new_gpu();
    batched.reference_batch(&camera(), 3);
    batched.reference_batch(&camera(), 3);
    let mut single = new_gpu();
    for _ in 0..6 {
        single.reference_sample(&camera());
    }
    assert_eq!(
        batched.render_reference(&camera(), 0),
        single.render_reference(&camera(), 0)
    );
}

#[test]
fn a_batch_of_one_is_a_single_sample_and_a_batch_is_capped() {
    let mut gpu = new_gpu();
    assert_eq!(gpu.reference_batch(&camera(), 1), 1);
    assert_eq!(bits(&gpu.read_accumulation()), singles(&camera(), 1));
    // Asking for more than the cap takes the cap; asking for none takes one.
    assert_eq!(gpu.reference_batch(&camera(), 100_000), MAX_BATCH_SAMPLES);
    assert_eq!(gpu.reference_batch(&camera(), 0), 1);
    assert_eq!(gpu.reference_samples(), 1 + MAX_BATCH_SAMPLES + 1);
    assert_eq!(
        bits(&gpu.read_accumulation()),
        singles(&camera(), 1 + MAX_BATCH_SAMPLES + 1)
    );
}

#[test]
fn the_reference_view_traces_at_the_output_resolution_whatever_primarys_scale_is() {
    let resolution = Resolution::scaled((64, 36), 0.5);
    assert_eq!(resolution.internal, (32, 18));
    let mut like_a_window = Offscreen::with_full_reference(resolution, &scene()).unwrap();
    let image = like_a_window.render_reference(&camera(), 1);
    assert_eq!((image.width, image.height), (64, 36));
    assert_eq!(like_a_window.read_accumulation().len(), 64 * 36 * 3);
    // The bench's flight keeps timing a sample at the traced size: that is what its budgets are in.
    let mut traced = Offscreen::with_resolution(resolution, &scene()).unwrap();
    let image = traced.render_reference(&camera(), 1);
    assert_eq!((image.width, image.height), (32, 18));
}

/// Draws frames of `camera` at 60 Hz from frame `from` for `frames` frames, as a window in
/// Reference mode does.
fn frames(gpu: &mut Offscreen, camera: &Camera, from: u32, frames: u32) -> Vec<render::FrameDrawn> {
    (from..from + frames)
        .map(|n| gpu.draw_frame(camera, true, FRAME * n))
        .collect()
}

/// Whether two pictures of the same size differ.
fn differ(a: &Image, b: &Image) -> bool {
    a.rgba != b.rgba
}

/// A pixel's channel is between the two images' values, to the rounding of 8 bits.
fn between(shown: &Image, from: &Image, to: &Image) -> bool {
    shown
        .rgba
        .iter()
        .zip(from.rgba.iter().zip(&to.rgba))
        .all(|(&s, (&f, &t))| s >= f.min(t).saturating_sub(1) && s <= f.max(t).saturating_add(1))
}

#[test]
fn the_reference_view_fades_in_over_primary_and_the_fade_is_never_stored() {
    let mut gpu = new_gpu();
    // Two frames of a view that has just stopped: Primary, and nothing of the Reference view.
    let first = frames(&mut gpu, &camera(), 0, 2);
    assert!(
        first
            .iter()
            .all(|f| f.plan == Plan::Primary && f.samples == 0)
    );
    let primary = gpu.primary_image();
    assert_eq!(gpu.shown(), primary, "Primary is shown as it is");

    // The third frame is the first batch: the fade starts from nothing, so it still shows Primary.
    let began = frames(&mut gpu, &camera(), 2, 1).pop().unwrap();
    let Plan::Reference { samples, fade } = began.plan else {
        panic!(
            "the third frame of a still view is Reference: {:?}",
            began.plan
        );
    };
    assert_eq!((samples, fade), (1, 0.0), "1 on the first frame of a view");
    assert_eq!(gpu.shown(), primary);
    let reference = gpu.render_reference(&camera(), 0);
    assert!(
        differ(&primary, &reference),
        "the two views are not the same picture"
    );

    // Halfway through the fade the picture is between the two, and is neither. (Frame 2 began it at
    // 2 frames; 75 ms later is the middle.)
    let middle = gpu.draw_frame(&camera(), true, FRAME * 2 + FADE / 2);
    assert!((middle.plan.fade() - 0.5).abs() < 1e-6, "{:?}", middle.plan);
    let reference = gpu.render_reference(&camera(), 0);
    let shown = gpu.shown();
    assert!(between(&shown, &primary, &reference));
    assert!(differ(&shown, &primary) && differ(&shown, &reference));
    // What is stored is the samples and nothing else: the accumulation is, bit for bit, what the
    // same number of plain samples make, with no Primary and no fade in it.
    assert_eq!(
        bits(&gpu.read_accumulation()),
        singles(&camera(), middle.samples)
    );

    // After 150 ms the Reference view is all that is shown, and still nothing was stored of the
    // fade.
    let done = gpu.draw_frame(&camera(), true, FRAME * 2 + FADE);
    assert_eq!(done.plan.fade(), 1.0);
    assert_eq!(gpu.shown(), gpu.render_reference(&camera(), 0));
    assert_eq!(
        bits(&gpu.read_accumulation()),
        singles(&camera(), done.samples)
    );
}

/// Digs a ball like an Edit does and returns the bricks it changed.
fn dig(store: &mut BrickStore, centre: [i32; 3], radius: i32) -> Vec<BrickCoord> {
    let mut changed = std::collections::BTreeSet::new();
    for x in -radius..=radius {
        for y in -radius..=radius {
            for z in -radius..=radius {
                if x * x + y * y + z * z <= radius * radius {
                    let voxel = VoxelCoord([centre[0] + x, centre[1] + y, centre[2] + z]);
                    let brick = voxel.brick();
                    if store.brick(brick).is_none() && brick.within_ball(CORE.radius) {
                        store.insert(brick, Brick::solid(STONE));
                    }
                    if store.get(voxel).is_some() {
                        changed.insert(brick);
                        store.set(voxel, None);
                    }
                }
            }
        }
    }
    changed.into_iter().collect()
}

/// What a change is, for the table of the next test.
enum Change {
    Camera,
    Sun,
    Edit,
}

#[test]
fn a_change_cuts_to_primary_and_restarts_from_sample_zero() {
    for (what, change) in [
        ("the camera", Change::Camera),
        ("the sun", Change::Sun),
        ("an Edit", Change::Edit),
    ] {
        let mut gpu = new_gpu();
        let mut store = store();
        let mut scene = Scene::new(&store, Some(CORE));
        // A long, still view: the Reference view has taken samples, and is all that is shown.
        let still = frames(&mut gpu, &camera(), 0, 14);
        let last = still.last().unwrap();
        assert!(last.samples >= 12, "{what}: {}", last.samples);
        assert_eq!(last.plan.fade(), 1.0, "{what}");
        let reference = gpu.render_reference(&camera(), 0);
        assert_eq!(gpu.shown(), reference, "{what}");

        // The change, and the next frame is Primary, entirely: no blend with the Reference view
        // that was on screen, and none of its samples left.
        let (camera_now, mut expected) = match change {
            Change::Camera => (other_camera(), Offscreen::new(SIZE, SIZE, &scene).unwrap()),
            Change::Sun => {
                let sun = Sun::from_angles(2.0, 0.6, 0.04);
                gpu.set_sun(sun);
                let mut expected = Offscreen::new(SIZE, SIZE, &scene).unwrap();
                expected.set_sun(sun);
                (camera(), expected)
            }
            Change::Edit => {
                let changed = dig(&mut store, [0, 98, 0], 14);
                let update = scene.update(&store, &changed).unwrap();
                assert!(matches!(update, SceneUpdate::Patched { .. }), "{what}");
                gpu.update_scene(&scene, &update).unwrap();
                (camera(), Offscreen::new(SIZE, SIZE, &scene).unwrap())
            }
        };
        let cut = frames(&mut gpu, &camera_now, 14, 1).pop().unwrap();
        assert_eq!(cut.plan, Plan::Primary, "{what}: cut to Primary at once");
        assert_eq!(cut.samples, 0, "{what}");
        assert_eq!(gpu.reference_samples(), 0, "{what}: started over");
        assert_eq!(gpu.shown(), gpu.primary_image(), "{what}: no blend");
        let second = frames(&mut gpu, &camera_now, 15, 1).pop().unwrap();
        assert_eq!(second.plan, Plan::Primary, "{what}: still unsettled");

        // Still again for a few frames: the Reference view comes back from sample 0, a batch of 1
        // and the fade from nothing, and what is stored is the samples of the new view only.
        let back = frames(&mut gpu, &camera_now, 16, 1).pop().unwrap();
        assert_eq!(
            back.plan,
            Plan::Reference {
                samples: 1,
                fade: 0.0
            },
            "{what}"
        );
        assert_eq!(back.samples, 1, "{what}");
        let more = frames(&mut gpu, &camera_now, 17, 3);
        let taken = more.last().unwrap().samples;
        assert!(taken >= 4, "{what}: {taken}");
        for _ in 0..taken {
            expected.reference_sample(&camera_now);
        }
        assert_eq!(
            bits(&gpu.read_accumulation()),
            bits(&expected.read_accumulation()),
            "{what}: the accumulation is the new view's samples from sample 0, with nothing of \
             the old view's, of Primary or of a fade"
        );
    }
}

/// Leaving Reference mode and coming back with nothing else changed (the same camera, the same
/// sun, the same Planet): the key of the view is the same all along, so only the Primary frames'
/// reset starts the Reference view over. Without it the old samples would be added to.
#[test]
fn leaving_reference_mode_and_coming_back_starts_from_sample_zero_with_nothing_else_changed() {
    let mut gpu = new_gpu();
    let drawn = frames(&mut gpu, &camera(), 0, 12);
    assert!(drawn.last().unwrap().samples > 1, "samples were taken");
    // Primary mode, on the same view: a few Primary frames.
    for n in 12..15 {
        let drawn = gpu.draw_frame(&camera(), false, FRAME * n);
        assert_eq!((drawn.plan, drawn.samples), (Plan::Primary, 0));
    }
    assert_eq!(gpu.reference_samples(), 0, "nothing of the view is kept");
    // Back to Reference mode: it is shown at once (the view is still), from sample 0, in a batch
    // of 1.
    let back = gpu.draw_frame(&camera(), true, FRAME * 15);
    assert!(
        matches!(back.plan, Plan::Reference { samples: 1, .. }),
        "{:?}",
        back.plan
    );
    assert_eq!(back.samples, 1);
    assert_eq!(bits(&gpu.read_accumulation()), singles(&camera(), 1));
}

#[test]
fn primary_mode_never_draws_the_reference_view() {
    let mut gpu = new_gpu();
    for n in 0..8 {
        let drawn = gpu.draw_frame(&camera(), false, FRAME * n);
        assert_eq!((drawn.plan, drawn.samples), (Plan::Primary, 0));
    }
    // Switching to Reference mode on a view that is already still shows it at once.
    let drawn = gpu.draw_frame(&camera(), true, FRAME * 8);
    assert!(
        matches!(drawn.plan, Plan::Reference { samples: 1, .. }),
        "{:?}",
        drawn.plan
    );
}

#[test]
fn a_batch_that_was_timed_sets_the_size_of_the_next_ones() {
    // On a device with timers the sizes follow what the GPU reports; on one without, they stay at
    // 1. Either way the size is between 1 and the cap, starts at 1, and a view that has moved
    // starts at 1 again.
    let mut gpu = new_gpu();
    let drawn = frames(&mut gpu, &camera(), 0, 60);
    let sizes: Vec<u32> = drawn.iter().map(|f| f.plan.samples()).collect();
    assert_eq!(sizes[..2], [0, 0]);
    assert_eq!(sizes[2], 1);
    assert!(
        sizes[2..]
            .iter()
            .all(|&s| (1..=MAX_BATCH_SAMPLES).contains(&s))
    );
    assert!(
        sizes[2..].windows(2).all(|w| w[1] <= w[0] + 1),
        "never up by more than one: {sizes:?}"
    );
    // Where the device has timers and a sample is cheap enough for two to fit in the budget (a
    // software driver on a busy machine may not be), the size must have risen: the timer's times
    // are reaching the budget.
    let slowest_sample = drawn
        .iter()
        .filter(|f| f.plan.samples() > 0)
        .filter_map(|f| f.reference_ms.map(|ms| ms / f64::from(f.plan.samples())))
        .fold(0.0, f64::max);
    if gpu.timers_supported() && slowest_sample > 0.0 && slowest_sample < 4.0 {
        assert!(
            sizes.last() > Some(&1),
            "a view this cheap has room for more: {sizes:?}, {slowest_sample} ms a sample"
        );
    }
    let moved = frames(&mut gpu, &other_camera(), 60, 3);
    assert_eq!(moved[2].plan.samples(), 1);
}
