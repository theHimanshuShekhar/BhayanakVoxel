//! The day (#203) on a Vulkan device (lavapipe in CI): the sky at a time of day, the key light that
//! is the sun by day and the moon by night, the sun that drifts and the one that jumps, and the
//! Still view of the Reference with a sky that does not move.

mod common;

use std::time::Duration;

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};
use common::{PRIMARY, check_golden};
use planet_gen::generate_region;
use render::daylight::{DaySky, Daylight, LocalFrame, hero_time_of_day};
use render::{Camera, Offscreen, Scene, Stages};

const STONE: Material = Material(1);

const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;

/// The Planet's sea level, 320 km, in Voxels.
const GROUND: f64 = 320_000.0 * 20.0;

/// A patch of the test Planet near the +y pole (the same as `atmosphere.rs`'s).
fn planet_patch() -> Scene {
    let store = generate_region(
        0xB4A7_A9AC,
        BrickCoord([-55, 170, -45]),
        BrickCoord([55, 196, 32]),
    );
    Scene::new(&store, None)
}

/// A camera low over the patch, looking at the horizon.
fn ground_camera() -> Camera {
    Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1538.0, -300.0])
}

/// A camera 1.8 Planet radii from the centre, looking down at the limb.
fn orbit_camera() -> Camera {
    let mut camera = Camera::looking_at([0.0, 0.0, 0.0], [-1.0, 0.0, 0.0]);
    camera.position = [1.8 * GROUND, 0.0, 0.0];
    camera.carry_up([1.0, 0.0, 0.0]);
    camera.pitch = -(90.0f64 - 33.7).to_radians();
    camera.fov_y = 38f64.to_radians();
    camera
}

fn ground_gpu() -> Offscreen {
    let mut gpu = Offscreen::new(WIDTH, HEIGHT, &planet_patch()).unwrap();
    gpu.set_ground_level(1535.0);
    gpu
}

fn orbit_gpu() -> Offscreen {
    let mut gpu = Offscreen::new(WIDTH, HEIGHT, &Scene::default()).unwrap();
    gpu.set_ground_level(GROUND);
    gpu
}

/// The view of `camera` as the game draws a still view, with the sky of `time_of_day`.
fn at(gpu: &mut Offscreen, camera: &Camera, time_of_day: f64) -> render::Image {
    gpu.set_day_sky(DaySky::at(time_of_day), true);
    gpu.render(camera)
}

/// A camera on the patch that looks at `time_of_day`'s moon.
fn camera_at_the_moon(time_of_day: f64) -> Camera {
    let base = ground_camera();
    let d = LocalFrame::of(&base).to_world(DaySky::at(time_of_day).moon);
    let p = base.position;
    Camera::looking_at(p, [0, 1, 2].map(|a| p[a] + f64::from(d[a]) * 1000.0))
}

fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// The linear light of the traced image, Stages off: `(width, height, rows from the top)`.
fn hdr(gpu: &mut Offscreen, camera: &Camera) -> (usize, usize, Vec<[f32; 3]>) {
    gpu.set_stages(Stages::NONE);
    gpu.render(camera);
    let ((w, h, light), _) = gpu.read_hdr_and_bloom();
    (w as usize, h as usize, light)
}

// ---- A time of day is one look.

#[test]
fn a_time_of_day_is_one_look_whatever_came_before_it() {
    // Pausing and setting the clock must give the frame of that time of day and nothing else: the
    // sky is a function of the time of day alone, so a frame at 21:36 is the same frame whether the
    // clock ran there, was set there, or was there all along.
    let camera = camera_at_the_moon(0.9);
    let mut fresh = ground_gpu();
    let want = at(&mut fresh, &camera, 0.9);

    let mut travelled = ground_gpu();
    for t in [0.5, 0.72, 0.1, 0.9, 0.3] {
        at(&mut travelled, &camera, t);
    }
    let got = at(&mut travelled, &camera, 0.9);
    assert_eq!(got.rgba, want.rgba, "after other times of day");

    // The clock's own routes to 0.9: a set (a jump) and a drift to it (not one) are the same sky.
    let mut daylight = Daylight::new();
    let mut routed = ground_gpu();
    let mut t = 0.9 - 0.02;
    let drive = daylight.step(t, 0);
    routed.set_day_sky(drive.sky, drive.jumped);
    while t < 0.9 - 1e-9 {
        t += 0.001;
        let drive = daylight.step(t.min(0.9), 0);
        routed.set_day_sky(drive.sky, drive.jumped);
    }
    let drift = routed.render(&camera);
    assert_eq!(drift.rgba, want.rgba, "after a drift");
}

#[test]
fn night_from_the_ground_shows_the_moon_among_the_stars() {
    check_golden(
        "daylight_moon_256x144.png",
        &at(&mut ground_gpu(), &camera_at_the_moon(0.9), 0.9),
        &PRIMARY,
    );
}

#[test]
fn the_golden_hour_from_the_ground_is_a_hazy_pastel_sky_over_the_hills() {
    check_golden(
        "daylight_golden_256x144.png",
        &at(
            &mut ground_gpu(),
            &ground_camera(),
            1.0 - hero_time_of_day(),
        ),
        &PRIMARY,
    );
}

// ---- The moon, the stars and the night.

#[test]
fn the_moon_and_the_stars_are_in_the_night_sky_and_not_in_the_days() {
    let mut gpu = ground_gpu();
    let moon_at = |gpu: &mut Offscreen, t: f64| {
        gpu.set_day_sky(DaySky::at(t), true);
        let (w, h, light) = hdr(gpu, &camera_at_the_moon(0.9));
        (light[h / 2 * w + w / 2], light)
    };
    // The camera is aimed at where the moon is at 0.9; at that time its disc is in the middle of the
    // image, brighter than the sky round it by far.
    let (centre, light) = moon_at(&mut gpu, 0.9);
    let sky = light[4 * 256 + 4];
    assert!(
        luminance(centre) > 8.0 * luminance(sky),
        "the disc {centre:?} against the sky {sky:?}"
    );
    // The stars: points that stand out of the sky round them, in the upper half of the image.
    let stars = |light: &[[f32; 3]]| {
        let (w, h) = (256i32, 144i32);
        (3..h / 2)
            .flat_map(|y| (3..w - 3).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let l = |dx: i32, dy: i32| {
                    luminance(light[(y + dy) as usize * w as usize + (x + dx) as usize])
                };
                let here = l(0, 0);
                // Brighter than the four pixels two away, by half, and not the moon's disc.
                here > 1.5 * [l(-2, 0), l(2, 0), l(0, -2), l(0, 2)].iter().sum::<f32>() / 4.0
                    && (x - 128).abs() + (y - 72).abs() > 24
            })
            .count()
    };
    let at_night = stars(&light);
    assert!(at_night >= 10, "{at_night} stars in the night sky");
    // At noon, looking the same way: no disc and no stars.
    let (centre, light) = moon_at(&mut gpu, 0.5);
    let sky = light[4 * 256 + 4];
    assert!(
        luminance(centre) < 1.5 * luminance(sky),
        "a disc at noon: {centre:?} against {sky:?}"
    );
    assert_eq!(stars(&light), 0, "stars at noon");
}

#[test]
fn the_reference_view_shows_the_same_moon_as_primary() {
    // Both views read the moon through `sky()`: the disc's pixel agrees (the sky is seen
    // directly, with no estimator in between).
    let camera = camera_at_the_moon(0.9);
    let mut gpu = ground_gpu();
    let primary = at(&mut gpu, &camera, 0.9);
    gpu.set_day_sky(DaySky::at(0.9), true);
    let reference = gpu.render_reference(&camera, 4);
    let at_centre = |image: &render::Image| {
        let i = ((image.height / 2 * image.width + image.width / 2) * 4) as usize;
        [image.rgba[i], image.rgba[i + 1], image.rgba[i + 2]].map(i32::from)
    };
    let (p, r) = (at_centre(&primary), at_centre(&reference));
    for c in 0..3 {
        assert!(
            (p[c] - r[c]).abs() <= 12,
            "primary {p:?} against reference {r:?}"
        );
    }
    // And the disc is there: well above the sky's own 8-bit level.
    assert!(p[0] > 150, "{p:?}");
}

/// A flat stone floor with a tall pillar at its middle, in a world whose up is +y.
fn pillar_scene() -> Scene {
    let mut store = BrickStore::new();
    for x in -40..40 {
        for z in -40..40 {
            store.set(VoxelCoord([x, -1, z]), Some(STONE));
        }
    }
    for y in 0..30 {
        for x in 0..8 {
            for z in 0..8 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    Scene::new(&store, None)
}

/// Where the shadow of the pillar falls, as a direction in the floor's plane `(x, z)`, from the
/// dark pixels of the floor, for the sky of `time_of_day`. The camera is over the pillar, a little off
/// the vertical, so that the picture's right is +x and its down is +z.
fn shadow_direction(time_of_day: f64) -> [f64; 2] {
    let mut gpu = Offscreen::new(200, 200, &pillar_scene()).unwrap();
    gpu.set_flat_ambient(false);
    gpu.set_day_sky(DaySky::at(time_of_day), true);
    let camera = Camera::looking_at([4.0, 100.0, 4.4], [4.0, 0.0, 4.0]);
    let (w, h, light) = hdr(&mut gpu, &camera);
    // The floor is the middle of the picture (the sky is outside it): its lit level, and the pixels
    // well under that, which are the shadow.
    let near = |x: usize, y: usize| x.abs_diff(w / 2) < 55 && y.abs_diff(h / 2) < 55;
    let mut lit: Vec<f32> = (0..w * h)
        .filter(|&i| near(i % w, i / w))
        .map(|i| luminance(light[i]))
        .collect();
    lit.sort_by(f32::total_cmp);
    let level = lit[lit.len() * 3 / 4];
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            if near(x, y) && luminance(light[y * w + x]) < 0.4 * level {
                sx += x as f64;
                sy += y as f64;
                n += 1.0;
            }
        }
    }
    assert!(
        n > 40.0,
        "no shadow at {time_of_day}: {n} dark pixels, lit {level}"
    );
    // The pillar is at about the middle of the picture; the shadow's centre against it.
    [sx / n - w as f64 / 2.0, sy / n - h as f64 / 2.0]
}

#[test]
fn the_key_light_is_the_sun_by_day_and_the_moon_by_night() {
    // A pillar's shadow falls away from the key light. By day that is the sun's, by night the
    // moon's (the sun is under the horizon, and the same shadow ray is cast towards the moon).
    let frame = LocalFrame::of(&Camera::looking_at([4.0, 100.0, 4.4], [4.0, 0.0, 4.0]));
    for (time, name) in [
        (0.4, "day"),
        (0.9, "night"),
        (0.1, "night, the moon on the other side"),
    ] {
        let sky = DaySky::at(time);
        let body = if time == 0.4 { sky.sun } else { sky.moon };
        assert!(
            body[1] > 0.2,
            "{name}: the light is above the horizon: {body:?}"
        );
        let d = frame.to_world(body);
        let away = [-f64::from(d[0]), -f64::from(d[2])];
        let seen = shadow_direction(time);
        let cos = (away[0] * seen[0] + away[1] * seen[1])
            / (away[0].hypot(away[1]) * seen[0].hypot(seen[1]));
        assert!(
            cos > 0.85,
            "{name}: the shadow falls {seen:?}, away from the light is {away:?}"
        );
    }
}

/// The view as the game draws it, with its radiance cache and the upscaler's history settled: the
/// sky of `time_of_day`, `frames` frames of the same view.
fn settled(gpu: &mut Offscreen, camera: &Camera, time_of_day: f64, frames: u32) -> render::Image {
    gpu.set_flat_ambient(false);
    gpu.set_day_sky(DaySky::at(time_of_day), true);
    for _ in 1..frames {
        gpu.draw_upscaled(camera);
        gpu.set_day_sky(DaySky::at(time_of_day), false);
    }
    gpu.render_upscaled(camera)
}

/// `images` in rows of `columns`, one picture.
fn sheet(images: &[render::Image], columns: usize) -> render::Image {
    let (w, h) = (images[0].width as usize, images[0].height as usize);
    let rows = images.len().div_ceil(columns);
    let mut rgba = vec![0u8; w * columns * h * rows * 4];
    for (i, image) in images.iter().enumerate() {
        let (cx, cy) = (i % columns, i / columns);
        for y in 0..h {
            let from = y * w * 4;
            let to = ((cy * h + y) * w * columns + cx * w) * 4;
            rgba[to..to + w * 4].copy_from_slice(&image.rgba[from..from + w * 4]);
        }
    }
    render::Image {
        width: (w * columns) as u32,
        height: (h * rows) as u32,
        rgba,
    }
}

/// A look-dev strip for the eye: set `DAY_DUMP=dir` and run with `--ignored`.
#[test]
#[ignore = "writes pictures for the eye"]
fn dump_the_day() {
    let dir = std::path::PathBuf::from(std::env::var("DAY_DUMP").unwrap_or("/tmp/day".into()));
    let times = [
        0.24,
        1.0 - hero_time_of_day(),
        0.5,
        hero_time_of_day(),
        0.775,
        0.9,
        0.0,
        0.15,
    ];
    let g: Vec<_> = times
        .iter()
        .map(|&t| settled(&mut ground_gpu(), &ground_camera(), t, 24))
        .collect();
    let o: Vec<_> = times
        .iter()
        .map(|&t| settled(&mut orbit_gpu(), &orbit_camera(), t, 4))
        .collect();
    // The moon itself: the camera aimed at it, at three times of the night.
    let moon: Vec<_> = [0.85, 0.95, 0.05, 0.1]
        .iter()
        .map(|&t| {
            let base = ground_camera();
            let frame = LocalFrame::of(&base);
            let d = frame.to_world(DaySky::at(t).moon);
            let p = base.position;
            let aim = Camera::looking_at(p, [0, 1, 2].map(|a| p[a] + f64::from(d[a]) * 1000.0));
            settled(&mut ground_gpu(), &aim, t, 24)
        })
        .collect();
    common::write_png(&dir.join("sheet-moon.png"), &sheet(&moon, 4));
    common::write_png(&dir.join("sheet-ground.png"), &sheet(&g, 4));
    common::write_png(&dir.join("sheet-orbit.png"), &sheet(&o, 4));
    let _ = (LocalFrame::of(&ground_camera()), check_golden, PRIMARY);
    let _: Option<(&BrickStore, Material, VoxelCoord, Daylight)> = None;
}

// ---- A sun that drifts and one that jumps.

/// A tiny view of the pillar scene: the frames of these tests are not looked at.
fn small_gpu() -> Offscreen {
    let mut gpu = Offscreen::new(16, 9, &pillar_scene()).unwrap();
    gpu.set_flat_ambient(false);
    gpu
}

fn small_camera() -> Camera {
    Camera::looking_at([4.0, 60.0, 40.0], [4.0, 0.0, 4.0])
}

#[test]
fn a_drifting_sun_does_not_jump_and_a_set_one_does() {
    let camera = small_camera();
    let mut gpu = small_gpu();
    let mut daylight = Daylight::new();
    let mut frame = |gpu: &mut Offscreen, time: f64, epoch: u32| {
        let drive = daylight.step(time, epoch);
        gpu.set_day_sky(drive.sky, drive.jumped);
        gpu.draw_upscaled(&camera);
        gpu.sun_epoch()
    };
    // The first sky is a jump: there is nothing to follow.
    let first = frame(&mut gpu, 0.30, 0);
    // A running clock, a hundred frames at about a degree each, in broad day: the epoch holds.
    let mut time = 0.30;
    for step in 0..100 {
        time += 1.0 / 400.0;
        assert_eq!(frame(&mut gpu, time, 0), first, "drift step {step}");
    }
    // The clock is set (its epoch moves) to the time it is at: a jump, though the sun did not move.
    let set = frame(&mut gpu, time, 1);
    assert_eq!(set, first + 1);
    // Set to another time of day: another jump.
    assert_eq!(frame(&mut gpu, 0.7, 2), set + 1);
    // And a big step of a clock that was not set (a stall; a day of one second): a jump too (two, in
    // this one: the key light changed hands as well, which is a jump of its own).
    assert!(frame(&mut gpu, 0.9, 2) > set + 1);
}

#[test]
fn a_day_of_drifting_jumps_only_where_the_key_light_changes_hands() {
    // 400 frames of a day, at 0.9 degrees of the sun a frame: every step is a drift. The sun's
    // epoch moves only at the first frame and where the one shadowed light is handed from the
    // sun to the moon (at dusk) and from the moon to the sun (at dawn), while both are dim.
    let camera = small_camera();
    let mut gpu = small_gpu();
    let mut daylight = Daylight::new();
    let mut epochs = Vec::new();
    for step in 0..=400 {
        let time = f64::from(step) / 400.0;
        let drive = daylight.step(time, 0);
        gpu.set_day_sky(drive.sky, drive.jumped);
        gpu.draw_upscaled(&camera);
        epochs.push((time, gpu.sun_epoch()));
    }
    let swaps: Vec<f64> = epochs
        .windows(2)
        .filter(|w| w[1].1 != w[0].1)
        .map(|w| w[1].0)
        .collect();
    assert_eq!(swaps.len(), 2, "the key changed hands at {swaps:?}");
    // At dawn, just before the sunrise (0.25); at dusk, just after the sunset (0.75).
    assert!((0.17..0.27).contains(&swaps[0]), "{swaps:?}");
    assert!((0.74..0.85).contains(&swaps[1]), "{swaps:?}");
}

// ---- A Still view needs a sky that holds still.

#[test]
fn the_reference_view_starts_when_the_sky_holds_still_and_never_while_it_moves() {
    let camera = ground_camera();
    let samples = |moving: bool| {
        let mut gpu = Offscreen::new(48, 27, &planet_patch()).unwrap();
        gpu.set_ground_level(1535.0);
        let mut daylight = Daylight::new();
        let mut time = 0.6;
        (0..8)
            .map(|frame| {
                // A running clock moves the sun every frame; a frozen one (what the game does for a
                // view that is still) does not.
                if moving {
                    time += 1.0 / 20_000.0;
                }
                let drive = daylight.step(time, 0);
                gpu.set_day_sky(drive.sky, drive.jumped);
                gpu.draw_frame(&camera, true, Duration::from_millis(17 * frame))
                    .samples
            })
            .collect::<Vec<_>>()
    };
    let held = samples(false);
    assert_eq!(
        held[..2],
        [0, 0],
        "Primary while the view settles: {held:?}"
    );
    assert!(
        held[2..].windows(2).all(|w| w[1] > w[0]),
        "the Reference view accumulates: {held:?}"
    );
    // A sun that drifts a thousandth of a degree a frame still restarts it, every frame.
    let moved = samples(true);
    assert!(
        moved.iter().all(|&n| n == 0),
        "no Reference view: {moved:?}"
    );
}

// ---- The radiance cache keeps up with a drifting sun.

/// The mean absolute difference per channel of two images, in 8-bit levels.
fn levels(a: &render::Image, b: &render::Image) -> f64 {
    let sum: u64 = a
        .rgba
        .chunks(4)
        .zip(b.rgba.chunks(4))
        .map(|(p, q)| (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>())
        .sum();
    sum as f64 / (a.rgba.len() / 4 * 3) as f64
}

/// How far the image of a drifting sun is from the same time's settled one: the clock runs from
/// `start` at `day_seconds` a day, 60 frames a second, for 2 seconds, with the caches left to follow.
/// Returns `(drifted, stale, floor)`: the drifted image's difference from the settled one; the
/// difference of an image drawn at the same time by a cache that has not followed (it was warmed
/// at the start and is asked once at the end); and the difference between two settled frames.
fn follow(day_seconds: f64, start: f64) -> (f64, f64, f64) {
    let camera = ground_camera();
    let per_frame = 1.0 / (60.0 * day_seconds);
    let end = start + 120.0 * per_frame;
    let warm = |gpu: &mut Offscreen, t: f64| {
        gpu.set_flat_ambient(false);
        // The exposure follows the light with a lag of its own (2.5 s), which is not the cache's:
        // it is held at 1 so that what differs is the light.
        gpu.set_stages(Stages {
            exposure: false,
            ..Stages::default()
        });
        gpu.set_day_sky(DaySky::at(t), true);
        for _ in 0..60 {
            gpu.draw_upscaled(&camera);
            gpu.set_day_sky(DaySky::at(t), false);
        }
    };
    let new_gpu = || {
        let mut gpu = Offscreen::new(96, 54, &planet_patch()).unwrap();
        gpu.set_ground_level(1535.0);
        gpu
    };
    // Drifting: each frame a step of the clock, never a jump.
    let mut gpu = new_gpu();
    warm(&mut gpu, start);
    let mut daylight = Daylight::new();
    daylight.step(start, 0);
    for frame in 1..=120 {
        let drive = daylight.step(start + f64::from(frame) * per_frame, 0);
        assert!(!drive.jumped);
        gpu.set_day_sky(drive.sky, false);
        gpu.draw_upscaled(&camera);
    }
    let drifted = gpu.primary_image();
    // Settled: the end time put there (a jump) and left for 60 frames, and a few more for the floor.
    let mut settled_gpu = new_gpu();
    warm(&mut settled_gpu, end);
    let settled = settled_gpu.primary_image();
    for _ in 0..20 {
        settled_gpu.draw_upscaled(&camera);
    }
    let again = settled_gpu.primary_image();
    // Stale: warmed at the start, then one frame at the end, with nothing to say the sun moved.
    let mut stale_gpu = new_gpu();
    warm(&mut stale_gpu, start);
    stale_gpu.set_day_sky(DaySky::at(end), false);
    stale_gpu.draw_upscaled(&camera);
    let stale = stale_gpu.primary_image();
    (
        levels(&drifted, &settled),
        levels(&stale, &settled),
        levels(&settled, &again),
    )
}

#[test]
fn the_cache_keeps_up_with_a_drifting_sun_at_the_games_pace_and_at_a_sped_up_one() {
    // Measured on lavapipe (96 x 54, levels of 255; the floor is two settled frames' difference):
    //   the game's half hour, from 0.55:   drifted 0.24, stale 0.05, floor 0.14
    //   a minute, from 0.55 (noon on):     drifted 0.51, stale 2.13, floor 0.16
    //   a minute, from 0.15 (before dawn): drifted 1.86, stale 4.58, floor 0.35
    // A cache that did not follow would be as far as the stale image (the sun moved 12 degrees in
    // the sped-up runs, 0.4 in the game's): the drifting sun is followed by the cache's own update,
    // with no boost.
    let (drifted, _, floor) = follow(1800.0, 0.55);
    assert!(
        drifted < floor + 0.6,
        "the game's pace: {drifted} (floor {floor})"
    );
    for (start, most) in [(0.55, 1.5), (0.15, 3.0)] {
        let (drifted, stale, floor) = follow(60.0, start);
        assert!(
            drifted < 0.6 * stale && drifted < most,
            "a minute's day from {start}: drifted {drifted}, stale {stale}, floor {floor}"
        );
    }
}

// ---- The exposure aims lower at night.

#[test]
fn the_exposure_aims_lower_when_the_sky_is_dark_so_a_night_stays_dark() {
    // The same lit floor under the legacy look (no atmosphere, so nothing but the night level
    // differs): the metered exposure settles lower by the log2 of the grey's share (0.65 at half night: 0.62 stops; 1.74 at full
    // night, which this scene's bright floor would take under the range's -3 stops).
    let camera = Camera::looking_at([4.0, 60.0, 40.0], [4.0, 0.0, 4.0]);
    let stops = |night: f32| {
        let mut gpu = small_gpu();
        gpu.set_look(render::Look::LEGACY);
        gpu.set_stages(Stages {
            exposure: true,
            ..Stages::NONE
        });
        let mut sky = DaySky::at(0.3);
        sky.night = night;
        gpu.set_day_sky(sky, true);
        gpu.render_upscaled(&camera);
        f64::from(gpu.exposure_stops(false).expect("metered"))
    };
    let (day, night) = (stops(0.0), stops(0.5));
    let want = 0.65f64.log2();
    assert!(
        (night - day - want).abs() < 0.02,
        "day {day}, night {night}: {} stops, wanted {want}",
        night - day
    );
}
