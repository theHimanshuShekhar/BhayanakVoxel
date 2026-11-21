//! The day cycle of `client bench --day-cycle` (#203): a whole day of the world clock, run fast, over
//! the flight's path, on the GPU of the flight.
//!
//! It answers three questions:
//!
//! 1. **Does a day cost a frame?** The clock runs on a fixed 60 Hz schedule (a frame is a sixtieth of
//!    a second of the clock, whatever the wall clock says), so a day of `--day-seconds` seconds is
//!    `60 * seconds` frames, and the sun moves a fraction of a degree a frame, which is a drift and
//!    never a jump. The frames' times are held to the real-time mode's limit of 16.7 ms: **none
//!    may be over it**, and the 99th percentile too is a gate. Through dusk the key light changes
//!    hands, which is the one place the cost could show; `key_swaps` says how many there were (two
//!    in a day).
//! 2. **Does the exposure pump?** Auto-exposure follows the day (τ 2.5 s, [`render::post`]), which
//!    must be a smooth rise at dusk and fall at dawn, not a wobble. The exposure is read back
//!    every few frames (outside the timed part) and [`exposure_stability`] says how far and how fast
//!    it went and how often it turned round.
//! 3. **Does the radiance cache keep up with a sun that drifts?** A drifting sun does not boost the
//!    cache (ADR 0005, #54); it follows by its normal updates. [`cache_follow`] drifts the clock
//!    from four times of day for 2 seconds at the default pace and at the sped-up one, and compares
//!    the image it ends with against the same time of day after a jump (the cache boosted, then
//!    settled). The difference is in 8-bit levels, next to the floor of two settled frames.
//!
//! The clock here is stepped by the bench itself, and not by [`crate::day::DayClock`], whose
//! freeze rule is about a camera that stops: here the camera moves every frame, so it would run
//! anyway.

use std::error::Error;
use std::time::Instant;

use render::{DaySky, Daylight, Image, Mode, Offscreen};
use serde::Serialize;
use sim::WorldClock;

use super::path::CameraPath;
use super::stats::Stats;
use super::{Flight, draw};

/// The real-time mode's frame-time gate, in ms (`[streaming] frame_time_p99_ms`): a 60 Hz frame.
pub const FRAME_LIMIT_MS: f64 = 16.7;

/// The clock's frames a second.
const RATE: f64 = 60.0;

/// The length of the sped-up day when `--day-seconds` does not say: a minute, 30 times faster than
/// the game's half hour.
pub const DEFAULT_DAY_SECONDS: f64 = 60.0;

/// The shortest day: a sun that moves 2 degrees a frame is a jump, and this bench's frames are
/// meant to be drifts. 4 seconds is 240 frames, 1.5 degrees a frame.
pub const SHORTEST_DAY_SECONDS: f64 = 4.0;

/// Frames between two readings of the exposure (6 is 10 a second).
const EXPOSURE_EVERY: usize = 6;

/// An exposure that has moved this many stops since its last turning point has turned (below it
/// is the metering's own noise), in [`exposure_stability`].
const EXPOSURE_DEADBAND: f64 = 0.05;

/// Times of day (the fraction of the day) that [`cache_follow`] looks at: before dawn, the morning,
/// the afternoon and the night after dusk. None is within 2 seconds of a sped-up day of where the key
/// light changes hands (0.21 and 0.75), which is a jump and would show the boost and not the drift.
const FOLLOW_TIMES: [f64; 4] = [0.15, 0.30, 0.55, 0.82];

/// Frames to warm the caches at a time of day, to run the drift, and to settle the reference.
const WARM_FRAMES: u32 = 90;
const DRIFT_FRAMES: u32 = 120;
const SETTLE_FRAMES: u32 = 90;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DayCycleReport {
    /// The length of the day that was run, in seconds of the clock, and the frames in it.
    pub day_seconds: f64,
    pub frames: usize,
    /// The frames' wall times, in ms, and how many of them were over [`FRAME_LIMIT_MS`], with the
    /// 99th percentile (nearest rank).
    pub frame_ms: Stats,
    pub frame_p99_ms: f64,
    pub frames_over_limit: usize,
    /// The most the sun moved in a frame, in degrees (a jump is 2), and the count of times the
    /// sun's epoch moved: once for the first frame and each key swap of the day (two).
    pub sun_step_max_degrees: f32,
    pub sun_jumps: u64,
    /// The exposure over the day.
    pub exposure: ExposureStability,
    /// The radiance cache against a jump, at the default pace and at the sped-up one.
    pub cache_follow: Vec<CacheFollow>,
    pub gates: DayCycleGates,
}

/// What [`exposure_stability`] says of the exposure's course.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExposureStability {
    pub samples: usize,
    pub min_stops: f64,
    pub max_stops: f64,
    /// The fastest the exposure moved over any second of the clock, in stops a second.
    pub max_stops_per_s: f64,
    /// The sum of the changes, in stops: the path the exposure walked.
    pub total_variation_stops: f64,
    /// `total_variation / (max - min)`: 1 for one way and no way back, about 2 for a day that goes up
    /// at dusk and down at dawn, and more for each wobble.
    pub travel_over_range: f64,
    /// How many times the exposure turned round by more than the deadband. A day has a few natural
    /// ones (the bright of noon, the dark of night, the bright again); pumping adds many.
    pub reversals: usize,
}

/// The cache's lag behind a drifting sun, from one time of day.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CacheFollow {
    pub day_seconds: f64,
    pub time_of_day: f64,
    /// The sun's elevation at the time the drift ended, in degrees.
    pub sun_elevation: f32,
    /// How far the sun drifted in the run, in degrees.
    pub drifted_degrees: f32,
    /// The mean absolute difference per channel, in 8-bit levels, between the drifted image and the
    /// settled image of the same time, and between two settled frames (the floor).
    pub drifted_levels: f64,
    pub floor_levels: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DayCycleGates {
    pub over: Vec<String>,
}

/// The 99th percentile of `times` by nearest rank.
fn p99(times: &[f64]) -> f64 {
    if times.is_empty() {
        return 0.0;
    }
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[(sorted.len() * 99).div_ceil(100) - 1]
}

/// How the exposure went, from its readings `stops` taken every `seconds_apart` of the clock.
pub fn exposure_stability(stops: &[f64], seconds_apart: f64) -> ExposureStability {
    let min = stops.iter().copied().fold(f64::INFINITY, f64::min);
    let max = stops.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let total: f64 = stops.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
    // The change over any one second: a window of that many readings.
    let window = ((1.0 / seconds_apart).round() as usize).max(1);
    let max_rate = stops
        .iter()
        .zip(stops.iter().skip(window))
        .map(|(a, b)| (b - a).abs() / (window as f64 * seconds_apart))
        .fold(0.0, f64::max);
    // A turn is a change of direction after a run of more than the deadband.
    let mut reversals = 0;
    let (mut extreme, mut direction) = (stops.first().copied().unwrap_or(0.0), 0i8);
    for &s in stops {
        match direction {
            0 => {
                if (s - extreme).abs() > EXPOSURE_DEADBAND {
                    direction = if s > extreme { 1 } else { -1 };
                    extreme = s;
                }
            }
            1 => {
                if s > extreme {
                    extreme = s;
                } else if extreme - s > EXPOSURE_DEADBAND {
                    reversals += 1;
                    direction = -1;
                    extreme = s;
                }
            }
            _ => {
                if s < extreme {
                    extreme = s;
                } else if s - extreme > EXPOSURE_DEADBAND {
                    reversals += 1;
                    direction = 1;
                    extreme = s;
                }
            }
        }
    }
    let range = max - min;
    ExposureStability {
        samples: stops.len(),
        min_stops: if stops.is_empty() { 0.0 } else { min },
        max_stops: if stops.is_empty() { 0.0 } else { max },
        max_stops_per_s: max_rate,
        total_variation_stops: total,
        travel_over_range: if range > 1e-9 { total / range } else { 0.0 },
        reversals,
    }
}

/// The mean absolute difference per channel of two images of a size, in 8-bit levels (alpha left
/// out).
pub fn mean_levels(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut n) = (0u64, 0u64);
    for (p, q) in a.rgba.chunks(4).zip(b.rgba.chunks(4)) {
        for c in 0..3 {
            sum += u64::from(p[c].abs_diff(q[c]));
            n += 1;
        }
    }
    sum as f64 / n.max(1) as f64
}

/// The angle between the sun of two skies, in degrees.
fn sun_step_degrees(a: &DaySky, b: &DaySky) -> f32 {
    render::daylight::angle_between(a.sun, b.sun).to_degrees()
}

/// Runs the day: `day_seconds` of the clock over `path`, from `start` (a fraction of the day).
pub fn run(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    path: &CameraPath,
    day_seconds: f64,
    start: f64,
    follow_camera: &render::Camera,
) -> Result<DayCycleReport, Box<dyn Error>> {
    let frames = (day_seconds * RATE).round() as usize;
    eprintln!("day cycle: a day of {day_seconds} s, {frames} frames...");
    let mut clock = WorldClock::new(start, day_seconds);
    let mut daylight = Daylight::new();
    // Warm up on the first frames' view, so that the frame 0 of the measured ones is not a first.
    let first = path.camera_at(0, frames as u32);
    flight.follow(gpu, &first)?;
    let drive = daylight.step(clock.time_of_day(), clock.epoch());
    gpu.set_day_sky(drive.sky, drive.jumped);
    for _ in 0..8 {
        draw(gpu, Mode::Primary, &first, 1, false);
    }
    let epoch_before = gpu.sun_epoch();
    let mut last_sky = drive.sky;
    let (mut wall, mut exposure) = (Vec::with_capacity(frames), Vec::new());
    let mut sun_step_max = 0.0f32;
    for frame in 0..frames {
        let camera = path.camera_at(frame as u32, frames as u32);
        flight.follow(gpu, &camera)?;
        clock.advance(1.0 / RATE);
        let drive = daylight.step(clock.time_of_day(), clock.epoch());
        sun_step_max = sun_step_max.max(sun_step_degrees(&last_sky, &drive.sky));
        last_sky = drive.sky;
        gpu.set_day_sky(drive.sky, drive.jumped);
        let began = Instant::now();
        draw(gpu, Mode::Primary, &camera, 1, false);
        wall.push(began.elapsed().as_secs_f64() * 1e3);
        if frame % EXPOSURE_EVERY == 0
            && let Some(stops) = gpu.exposure_stops(false)
        {
            exposure.push(f64::from(stops));
        }
    }
    let sun_jumps = gpu.sun_epoch() - epoch_before;
    let frame_p99_ms = p99(&wall);
    let frames_over_limit = wall.iter().filter(|&&ms| ms > FRAME_LIMIT_MS).count();
    let follow = {
        let mut all = Vec::new();
        // The game's pace and the sped-up one.
        for seconds in [sim::DEFAULT_DAY_LENGTH_SECONDS, day_seconds] {
            for time in FOLLOW_TIMES {
                all.push(cache_follow(gpu, follow_camera, seconds, time));
            }
        }
        all
    };
    let mut over = Vec::new();
    if frame_p99_ms > FRAME_LIMIT_MS {
        over.push(format!("frame_p99_ms {frame_p99_ms:.2} > {FRAME_LIMIT_MS}"));
    }
    if frames_over_limit > 0 {
        over.push(format!(
            "{frames_over_limit} frames over {FRAME_LIMIT_MS} ms"
        ));
    }
    Ok(DayCycleReport {
        day_seconds,
        frames,
        frame_ms: Stats::of(&wall),
        frame_p99_ms,
        frames_over_limit,
        sun_step_max_degrees: sun_step_max,
        sun_jumps,
        exposure: exposure_stability(&exposure, EXPOSURE_EVERY as f64 / RATE),
        cache_follow: follow,
        gates: DayCycleGates { over },
    })
}

/// Draws `frames` frames of the same view with the sky of `sky`, `jumped` the first time.
fn hold(gpu: &mut Offscreen, camera: &render::Camera, sky: DaySky, jumped: bool, frames: u32) {
    gpu.set_day_sky(sky, jumped);
    for _ in 0..frames {
        gpu.draw_upscaled(camera);
        gpu.set_day_sky(sky, false);
    }
}

/// The cache's lag behind a drifting sun, from `time` with a day of `day_seconds`: the clock is set
/// there and the view settled; the clock then runs for [`DRIFT_FRAMES`] frames with the caches left
/// to follow, and the image is compared with the same time's after a jump and a settle.
pub fn cache_follow(
    gpu: &mut Offscreen,
    camera: &render::Camera,
    day_seconds: f64,
    time: f64,
) -> CacheFollow {
    // The exposure follows the light with a lag of its own (2.5 s), which is not the cache's: it is
    // held at 1 so that what differs between the images is the light.
    gpu.set_stages(render::Stages {
        exposure: false,
        ..render::Stages::default()
    });
    let mut clock = WorldClock::new(time, day_seconds);
    let mut daylight = Daylight::new();
    let start = daylight.step(clock.time_of_day(), clock.epoch());
    hold(gpu, camera, start.sky, true, WARM_FRAMES);
    let mut last = start.sky;
    for _ in 0..DRIFT_FRAMES {
        clock.advance(1.0 / RATE);
        let drive = daylight.step(clock.time_of_day(), clock.epoch());
        gpu.set_day_sky(drive.sky, drive.jumped);
        gpu.draw_upscaled(camera);
        last = drive.sky;
    }
    let drifted = gpu.primary_image();
    // The same time again, put there (a jump) and settled, and then a little longer for a floor.
    hold(gpu, camera, last, true, SETTLE_FRAMES);
    let settled = gpu.primary_image();
    hold(gpu, camera, last, false, SETTLE_FRAMES / 3);
    let again = gpu.primary_image();
    gpu.set_stages(render::Stages::default());
    CacheFollow {
        day_seconds,
        time_of_day: time,
        sun_elevation: last.sun_elevation_degrees(),
        drifted_degrees: sun_step_degrees(&start.sky, &last),
        drifted_levels: mean_levels(&drifted, &settled),
        floor_levels: mean_levels(&settled, &again),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reading every tenth of a second.
    fn stability(stops: &[f64]) -> ExposureStability {
        exposure_stability(stops, 0.1)
    }

    #[test]
    fn a_steady_exposure_has_no_travel_and_no_turns() {
        let s = stability(&[1.0; 50]);
        assert_eq!(s.total_variation_stops, 0.0);
        assert_eq!(s.max_stops_per_s, 0.0);
        assert_eq!(s.reversals, 0);
        assert_eq!(s.travel_over_range, 0.0);
    }

    #[test]
    fn a_rise_and_a_fall_is_one_reversal_and_travels_twice_its_range() {
        let up: Vec<f64> = (0..=40).map(|i| f64::from(i) * 0.1).collect();
        let down: Vec<f64> = up.iter().rev().skip(1).copied().collect();
        let s = stability(&[up, down].concat());
        assert_eq!(s.reversals, 1);
        assert!((s.travel_over_range - 2.0).abs() < 1e-9, "{s:?}");
        assert_eq!((s.min_stops, s.max_stops), (0.0, 4.0));
        // 0.1 stop per reading, readings of a tenth of a second: a stop a second.
        assert!((s.max_stops_per_s - 1.0).abs() < 1e-9, "{s:?}");
    }

    #[test]
    fn a_wobble_is_counted_as_every_turn_and_a_flicker_under_the_deadband_is_not() {
        // 0, 0.2, 0, 0.2 ...: a turn at each reading but the first two.
        let wobble: Vec<f64> = (0..20)
            .map(|i| if i % 2 == 0 { 0.0 } else { 0.2 })
            .collect();
        let s = stability(&wobble);
        assert!(s.reversals >= 17, "{s:?}");
        let flicker: Vec<f64> = (0..20)
            .map(|i| if i % 2 == 0 { 0.0 } else { 0.03 })
            .collect();
        assert_eq!(stability(&flicker).reversals, 0);
    }

    #[test]
    fn no_readings_say_nothing_and_do_not_panic() {
        let s = stability(&[]);
        assert_eq!(
            (s.samples, s.reversals, s.total_variation_stops),
            (0, 0, 0.0)
        );
    }

    #[test]
    fn the_99th_percentile_is_the_nearest_rank() {
        let times: Vec<f64> = (1..=200).map(f64::from).collect();
        assert_eq!(p99(&times), 198.0);
        assert_eq!(p99(&[]), 0.0);
        assert_eq!(p99(&[5.0]), 5.0);
    }

    #[test]
    fn the_mean_of_levels_leaves_alpha_out() {
        let a = Image {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 10, 10, 10, 0],
        };
        let b = Image {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 0, 0, 0, 0, 255],
        };
        assert_eq!(mean_levels(&a, &b), 5.0);
    }

    #[test]
    fn a_sped_up_day_of_the_shortest_length_drifts_below_the_jump_angle() {
        const { assert!(DEFAULT_DAY_SECONDS >= SHORTEST_DAY_SECONDS) };
        let step = 360.0 / (SHORTEST_DAY_SECONDS * RATE);
        assert!(step < f64::from(render::daylight::JUMP_ANGLE.to_degrees()));
    }
}
