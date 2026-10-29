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
        // One pass, so the whole frame is that pass.
        assert_eq!(timings.total().last(), primary.times.last());
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
    assert_eq!(names, ["primary", "reference"]);
    let active: Vec<_> = gpu.timings().passes().iter().map(|p| p.active).collect();
    if gpu.timers_supported() {
        assert_eq!(active, [false, true]);
        let reference = gpu.timings().pass("reference").unwrap();
        assert_eq!(reference.times.count(), 3);
        assert_eq!(gpu.timings().pass("primary").unwrap().times.count(), 1);
    } else {
        assert_eq!(active, [false, false]);
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
