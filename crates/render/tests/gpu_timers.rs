//! GPU pass timers on a real (software) device: every pass the renderer runs reports a time
//! where the device has timestamp queries, and where it has none, the renderer still works and
//! the passes are listed without times. Needs a Vulkan device (lavapipe in CI), which may or may
//! not have timestamp queries; each test checks whichever it got.

use brick_store::{BrickStore, Material, VoxelCoord};
use render::{Camera, Offscreen, Scene};

fn scene() -> Scene {
    let mut store = BrickStore::new();
    for x in -16..16 {
        for z in -16..16 {
            store.set(VoxelCoord([x, -1, z]), Some(Material(1)));
        }
    }
    Scene::new(&store, None)
}

fn overhead() -> Camera {
    Camera::looking_at([0.5, 10.0, 0.5], [0.5, 0.0, 0.5])
}

fn offscreen() -> Offscreen {
    let offscreen = Offscreen::new(32, 32, &scene()).unwrap();
    eprintln!(
        "timestamp queries {}",
        if offscreen.timers_supported() {
            "supported: checking the times"
        } else {
            "not supported: checking the n/a path"
        }
    );
    offscreen
}

#[test]
fn the_primary_pass_is_timed_or_listed_as_unavailable() {
    let mut gpu = offscreen();
    let image = gpu.render(&overhead());
    // Timers never change what is drawn.
    assert!(image.rgba.iter().any(|&b| b != 0));

    let timings = gpu.timings();
    let primary = timings.pass("primary").expect("the pass registered itself");
    if gpu.timers_supported() {
        let average = primary.times.average().expect("a time arrived");
        assert!(average >= 0.0 && average.is_finite(), "{average}");
        assert!(primary.active);
        assert_eq!(primary.times.count(), 1);
        // The frame is primary visibility and the passes after it (#202: the smooth normals, the
        // light, the display transform), each timed on its own, so the frame is at least it.
        assert!(timings.total().last() >= primary.times.last());
        for pass in [
            "normals",
            "shadows",
            "shade",
            "exposure",
            "exposure_adapt",
            "bloom_down",
            "bloom_h",
            "bloom_v",
            "tone",
        ] {
            assert!(
                timings.pass(pass).is_some(),
                "{pass} has no line of its own"
            );
        }
    } else {
        assert_eq!(primary.times.average(), None);
        assert_eq!(primary.times.max(), None);
        assert_eq!(timings.total().average(), None);
    }
}

#[test]
fn passes_register_in_the_order_they_first_run_and_only_the_latest_is_active() {
    let mut gpu = offscreen();
    gpu.render(&overhead());
    gpu.render_reference(&overhead(), 3);

    let names: Vec<_> = gpu.timings().passes().iter().map(|p| &*p.name).collect();
    // The passes of the display transform are shared by both views, so the Reference view adds
    // only its own.
    assert_eq!(
        names,
        [
            "primary",
            "normals",
            "shadows",
            "shade",
            "exposure",
            "exposure_adapt",
            "bloom_down",
            "bloom_h",
            "bloom_v",
            "tone",
            "reference"
        ]
    );
    let active: Vec<_> = gpu.timings().passes().iter().map(|p| p.active).collect();
    if gpu.timers_supported() {
        // The latest frame ran the Reference view and the display transform after it.
        let ran = [
            "exposure",
            "exposure_adapt",
            "bloom_down",
            "bloom_h",
            "bloom_v",
            "tone",
            "reference",
        ];
        for (name, active) in names.iter().zip(&active) {
            assert_eq!(*active, ran.contains(name), "{name}");
        }
        let reference = gpu.timings().pass("reference").unwrap();
        assert_eq!(reference.times.count(), 3);
        assert_eq!(gpu.timings().pass("primary").unwrap().times.count(), 1);
    } else {
        assert!(active.iter().all(|a| !a));
    }
}

#[test]
fn every_frame_is_timed_not_only_the_first() {
    let mut gpu = offscreen();
    gpu.set_keep_history(true);
    for _ in 0..10 {
        gpu.render(&overhead());
    }
    let primary = gpu.timings().pass("primary").unwrap();
    // More frames than the ring of readback buffers is long.
    let expected = if gpu.timers_supported() { 10 } else { 0 };
    assert_eq!(primary.times.count(), expected);
    assert_eq!(primary.times.history().len(), expected as usize);
    assert!(
        primary
            .times
            .history()
            .iter()
            .all(|t| t.is_finite() && *t >= 0.0)
    );
    // The CPU's clock needs no GPU support: every frame but the first has a time.
    assert_eq!(gpu.timings().wall().count(), 9);
}

#[test]
fn a_device_without_timestamp_queries_renders_the_same_with_no_times() {
    let mut untimed = Offscreen::without_timers(32, 32, &scene()).unwrap();
    assert!(!untimed.timers_supported());
    let image = untimed.render(&overhead());
    assert_eq!(untimed.render_reference(&overhead(), 2).width, 32);
    // The pass is listed, with "n/a" for a time, and nothing broke.
    let primary = untimed.timings().pass("primary").unwrap();
    assert_eq!((primary.times.average(), primary.times.max()), (None, None));
    assert_eq!(untimed.timings().total().average(), None);
    assert_eq!(untimed.timings().wall().count(), 2);
    // Timers never change what is drawn.
    assert_eq!(
        image,
        Offscreen::new(32, 32, &scene())
            .unwrap()
            .render(&overhead())
    );
}

#[test]
fn a_drawn_frames_gpu_times_are_its_own_or_absent() {
    let mut gpu = offscreen();
    for frame in 1..=4u64 {
        gpu.draw_upscaled_timed(&overhead());
        let draw = gpu.last_draw_times();
        if gpu.timers_supported() {
            let times = draw
                .gpu
                .expect("this frame's own times are in once it has waited");
            let (primary, upscale, total) = (
                times.primary_ms.expect("the primary pass has a time"),
                times.upscale_ms.expect("the upscale pass has a time"),
                times.total_ms.expect("the frame has a span"),
            );
            // The span holds both passes, and they are the latest the timings have: one more time a
            // frame, none skipped and none from another frame.
            assert!(total >= primary && total >= upscale, "{times:?}");
            let timings = gpu.timings();
            assert_eq!(timings.pass("primary").unwrap().times.count(), frame);
            assert_eq!(timings.pass("primary").unwrap().times.last(), Some(primary));
            assert_eq!(timings.pass("upscale").unwrap().times.last(), Some(upscale));
        } else {
            assert_eq!(draw.gpu, None, "no timers, no times");
        }
    }
}
