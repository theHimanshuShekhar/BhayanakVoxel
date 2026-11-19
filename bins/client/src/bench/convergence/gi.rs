//! Primary's GI against the Reference (#54, ADR 0005): how close Primary's image is to the
//! Reference's truth in each convergence view, with the radiance cache and with the flat ambient
//! that Primary had before it, and how fast the lit result follows a dig.
//!
//! **What is scored.** In each of the convergence views (the golden's, the pit and the emissive
//! room) Primary is drawn as the window draws it, traced at half the size and upscaled, and scored
//! by FLIP (LDR, [`score::FLIP_PPD`], mean and 99th percentile, as the other views are) against the
//! view's truth A, which `run_view` has made: the same image, the same seed and the same size, so
//! that the scores of one run say how the views stand and nothing else. Primary is drawn twice, on
//! a GPU context of its own each:
//! - **`flat`**: the flat ambient of 0.25 that main's Primary has ([`Offscreen::set_flat_ambient`]),
//!   the baseline. It is not a mode of the game; it is here so that "no view moves further from
//!   the Reference than main's Primary" has a number on the same build, and the report has each
//!   view's score before and after.
//! - **`gi`**: the cache's GI, as the window draws it.
//!
//! Each is scored twice, as the shadows view is: **`first_frame`**, the frame after the view was
//! made, with a cache that has nothing in it, and **`settled`**, after [`Settings::settle_frames`]
//! frames of a camera that has not moved, the cache and the upscaler's history having gathered what
//! they gather. **The gates are on the settled frame**, since the first one of a cold cache is
//! the noisiest the GI is, and the player has it once, in the frame the scene is made; the first
//! frame is reported beside it.
//!
//! **The gates** (the ticket's acceptance, the run's exit status 2 if any is over):
//! - the room's settled GI image is at FLIP [`ROOM_TARGET_FLIP`] or under (#55 tightens it);
//! - no view's settled GI image is further from truth than its flat-ambient one;
//! - the lit result settles within [`SETTLE_WITHIN_MS`] of a dig ([`EditSettle`]);
//! - the Reference converges in the room: FLIP at most [`still::FLIP_TARGET`] within
//!   [`still::WITHIN_S`] of the hold beginning, which the room's still run already scores.
//!
//! **The dig.** The emissive room with a wall across it that the lamp's light does not get
//! through, seen from the dark side, warmed up until the cache has settled. A gap is dug in the
//! wall as the game applies an Edit (`Scene::update`, `Offscreen::update_scene`), and frames are
//! drawn at 60 Hz as the window draws them: a frame that takes longer than a 60 Hz frame takes its
//! own time (the window's vsync would wait for the next), one that is quicker takes 1/60 s. The
//! image's mean luminance, in linear light, is read after each frame (outside its time); the
//! lit result has **settled** at the first frame after which the mean stays within a tenth of the
//! change's size of where the run ends, and the time is that frame's, from the start of the
//! Edit's own update.
//!
//! **CI.** `--short` runs the views and the dig at a few dozen pixels with no FLIP: the images
//! are drawn and the luminances are compared, the report has the right shape and no gate is
//! held.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use render::{Image, Offscreen, Resolution, Scene};
use serde::Serialize;

use super::shadows::Scores;
use super::views::{self, View};
use super::{score, still, write_png};

/// The FLIP the room's settled GI image is held to against the Reference's truth, at 1440p on the
/// 4090 (the ticket's; #55's denoiser tightens it).
pub const ROOM_TARGET_FLIP: f64 = 0.10;
/// The lit result of a dig has settled within this long of it, in milliseconds (the ticket's
/// half second).
pub const SETTLE_WITHIN_MS: f64 = 500.0;
/// A dig's lit result has settled where the image's mean luminance is within this share of the
/// change's size from where it ends up.
const SETTLED_WITHIN: f64 = 0.1;
/// A dig has to change the room's mean luminance by this share of it at least for the time to
/// mean anything.
const MIN_CHANGE: f64 = 0.05;
/// A 60 Hz frame's time, in milliseconds.
const FRAME_MS: f64 = 1000.0 / 60.0;
/// Frames of the camera at the room's other side before the dig, enough for the cache to be warm.
const WARM_FRAMES: u32 = 90;
/// The frames drawn after the dig (two seconds at 60 Hz), the last [`END_FRAMES`] of which say
/// where the lit result ends up.
const DIG_FRAMES: u32 = 120;
const END_FRAMES: usize = 10;

/// What to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Primary's output size, and the truths' (Primary traces at half of it).
    pub size: (u32, u32),
    /// Frames of a still camera that Primary has for `settled`.
    pub settle_frames: u32,
    /// Whether to score with FLIP, and so to hold the gates (off for `--short`).
    pub flip: bool,
    /// The dig's warm-up and frames.
    pub warm_frames: u32,
    pub dig_frames: u32,
}

impl Settings {
    /// 1440p for the default `--size`, 64 frames of a still camera (about a second at 60 Hz): the
    /// cache fills from nothing in a couple of dozen frames, and the upscaler's history of 16
    /// then takes as many more to be of the light that has settled.
    pub fn full(size: (u32, u32)) -> Self {
        Self {
            size,
            settle_frames: 64,
            flip: true,
            warm_frames: WARM_FRAMES,
            dig_frames: DIG_FRAMES,
        }
    }

    /// A smoke test: a few pixels, a few frames, no FLIP.
    pub fn short(size: (u32, u32)) -> Self {
        Self {
            size,
            settle_frames: 4,
            flip: false,
            warm_frames: 6,
            dig_frames: 12,
        }
    }
}

/// An image of Primary's scored against truth A.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Score {
    /// Null where FLIP was not run.
    pub flip: Option<Scores>,
    /// The mean luminance of the image over truth A's image's, in linear light (both 8-bit images,
    /// as the player sees them): 1 is as bright as the truth, and a view that is too dark or too
    /// bright is off it. Works without FLIP.
    pub luminance_ratio: f64,
}

/// Primary's first frame and its settled frame, in one of the ways of lighting it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Variant {
    pub first_frame: Score,
    pub settled: Score,
}

/// A convergence view's Primary against its truth, before and after the cache.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ViewGi {
    /// Primary with the flat ambient of before #54: the baseline.
    pub flat: Variant,
    /// Primary with the cache's GI.
    pub gi: Variant,
    /// Frames of a still camera that `settled` is after.
    pub settle_frames: u32,
    /// Whether the settled GI image is at least as close to truth as the flat one by FLIP mean;
    /// null without FLIP. No view may be further.
    pub gi_not_further: Option<bool>,
    /// The files of this view's Primary images, in its folder.
    pub files: Vec<String>,
}

/// What a dig of the room's wall does to the lit result.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EditSettle {
    pub width: u32,
    pub height: u32,
    /// Voxels dug out of the wall.
    pub voxels_dug: usize,
    /// The CPU time of the Edit's own update (the scene's change and the writes to the GPU), in ms.
    pub update_ms: f64,
    /// The image's mean luminance, in linear light: before the dig (the last frames of the warm-up),
    /// and where it ends up (the last frames of the run).
    pub before: f64,
    pub after: f64,
    /// The time of each frame of the run from the start of the update, in ms, and the image's
    /// mean luminance after it.
    pub frame_ms: Vec<f64>,
    pub luminance: Vec<f64>,
    /// The time of the first frame after which the luminance stays within a tenth of the change
    /// from where it ends up, in ms; null where the dig changed it too little to tell
    /// (`MIN_CHANGE`).
    pub settle_ms: Option<f64>,
    /// The frames that took longer than a 60 Hz frame (the GPU's, not the clock's).
    pub slow_frames: usize,
    /// Whether it settled within [`SETTLE_WITHIN_MS`]; null where `settle_ms` is.
    pub within_target: Option<bool>,
}

/// The gates, and what is over.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gates {
    pub room_target_flip: f64,
    pub settle_within_ms: f64,
    pub reference_flip: f64,
    pub reference_within_s: f64,
    /// Whether the gates were held: with FLIP only.
    pub held: bool,
    /// The names of the gates that are over, empty if none: `room-flip`, `no-further:VIEW`,
    /// `settle`, `reference-room`.
    pub over: Vec<String>,
}

/// The report's `convergence.gi` block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GiReport {
    pub edit: EditSettle,
    pub gates: Gates,
}

/// The mean luminance of an 8-bit RGBA image in linear light (Rec. 709 weights), which is the
/// light itself where the display is the sRGB curve alone.
fn mean_luminance(image: &Image) -> f64 {
    let l = |c: u8| f64::from(render::srgb_decode(f32::from(c) / 255.0));
    let pixels = image.rgba.as_chunks::<4>().0;
    let n = pixels.len() as f64;
    pixels
        .iter()
        .map(|p| 0.2126 * l(p[0]) + 0.7152 * l(p[1]) + 0.0722 * l(p[2]))
        .sum::<f64>()
        / n
}

/// Primary of `view`'s scene, as the window draws it, in the legacy light of the convergence views:
/// the first frame and the frame after `frames`.
fn draw(view: &View, settings: &Settings, flat: bool) -> Result<(Image, Image), Box<dyn Error>> {
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    let mut gpu = Offscreen::with_resolution(resolution, &view.scene)?;
    gpu.use_legacy_light();
    gpu.set_flat_ambient(flat);
    let first = gpu.render_upscaled(&view.camera);
    let mut settled = first.clone();
    for _ in 1..settings.settle_frames.max(2) {
        settled = gpu.render_upscaled(&view.camera);
    }
    Ok((first, settled))
}

/// Scores Primary's two ways of lighting `view` against truth A (`truth`, the image the player would
/// see, and `truth_rgb`, the same as 8-bit RGB) and writes the images to `folder`. Brightness is
/// compared on the 8-bit images, both clipped at white as the player sees them: a lamp that is
/// brighter than white would otherwise make Primary look dark beside the truth's unclipped
/// accumulation.
pub fn score_view(
    view: &View,
    settings: &Settings,
    truth: &Image,
    truth_rgb: &[u8],
    folder: &Path,
) -> Result<ViewGi, Box<dyn Error>> {
    let name = view.name;
    let size = (settings.size.0 as usize, settings.size.1 as usize);
    let truth_mean = mean_luminance(truth);
    let mut files = Vec::new();
    let mut variant = |label: &str, flat: bool| -> Result<Variant, Box<dyn Error>> {
        let (first, settled) = draw(view, settings, flat)?;
        let mut scored = |when: &str, image: &Image| -> Result<Score, Box<dyn Error>> {
            let flip = if settings.flip {
                eprintln!("convergence: {name}: FLIP of Primary, {label}, {when}...");
                Some(score::flip(truth_rgb, &score::rgb_of_rgba(&image.rgba), size)?.into())
            } else {
                None
            };
            let file = format!("primary-{label}-{}.png", when.replace(' ', "-"));
            write_png(&folder.join(&file), image)?;
            files.push(file);
            Ok(Score {
                flip,
                luminance_ratio: mean_luminance(image) / truth_mean,
            })
        };
        Ok(Variant {
            first_frame: scored("first frame", &first)?,
            settled: scored("settled", &settled)?,
        })
    };
    let flat = variant("flat", true)?;
    let gi = variant("gi", false)?;
    files.sort();
    let gi_not_further = flat
        .settled
        .flip
        .zip(gi.settled.flip)
        .map(|(flat, gi)| gi.flip_mean <= flat.flip_mean);
    Ok(ViewGi {
        flat,
        gi,
        settle_frames: settings.settle_frames.max(2),
        gi_not_further,
        files,
    })
}

/// The frame of `series` after which every value stays within `SETTLED_WITHIN` of the change
/// (`before` to `after`) from `after`: the index of the first frame of that tail, `None` where the
/// run does not end settled (it never does, or the dig changed too little).
pub fn settled_at(series: &[f64], before: f64, after: f64) -> Option<usize> {
    let change = (after - before).abs();
    if change < MIN_CHANGE * before.abs().max(f64::MIN_POSITIVE) {
        return None;
    }
    let within = |v: f64| (v - after).abs() <= SETTLED_WITHIN * change;
    let from = series
        .iter()
        .rposition(|&v| !within(v))
        .map_or(0, |i| i + 1);
    (from < series.len()).then_some(from)
}

/// Digs the gap in the room's wall and times how long the lit result takes to follow.
pub fn dig(settings: &Settings, out: &Path) -> Result<EditSettle, Box<dyn Error>> {
    let (mut store, camera) = views::walled_room();
    let mut scene = Scene::new(&store, None);
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    let mut gpu = Offscreen::with_resolution(resolution, &scene)?;
    gpu.use_legacy_light();
    gpu.set_flat_ambient(false);
    // The warm-up: the camera still, the cache gathering the dark side's light.
    let mut recent = Vec::new();
    for _ in 0..settings.warm_frames {
        recent.push(mean_luminance(&gpu.render_upscaled(&camera)));
    }
    let tail = &recent[recent.len().saturating_sub(END_FRAMES)..];
    let before = tail.iter().sum::<f64>() / tail.len() as f64;

    // The Edit: the gap's Voxels become air, the scene changes, the GPU is told, as the game does.
    let gap = views::room_gap();
    let started = Instant::now();
    let mut changed = std::collections::BTreeSet::new();
    for &v in &gap {
        changed.insert(v.brick());
        store.set(v, None);
    }
    let changed: Vec<_> = changed.into_iter().collect();
    let update = scene.update(&store, &changed)?;
    gpu.update_scene(&scene, &update)?;
    let update_ms = started.elapsed().as_secs_f64() * 1e3;

    // The frames, at 60 Hz: each takes its own time or a frame's, whichever is more. The time is
    // counted from the start of the update.
    let mut at = started.elapsed();
    let (mut frame_ms, mut luminance) = (Vec::new(), Vec::new());
    let mut slow = 0;
    for _ in 0..settings.dig_frames {
        let drawn = Instant::now();
        gpu.draw_upscaled(&camera);
        let took = drawn.elapsed();
        if took > Duration::from_secs_f64(FRAME_MS / 1e3) {
            slow += 1;
        }
        at += took.max(Duration::from_secs_f64(FRAME_MS / 1e3));
        frame_ms.push(at.as_secs_f64() * 1e3);
        // Read after the clock has been read: not in any frame's time.
        luminance.push(mean_luminance(&gpu.primary_image()));
    }
    let tail = &luminance[luminance.len().saturating_sub(END_FRAMES)..];
    let after = tail.iter().sum::<f64>() / tail.len() as f64;
    let settle_ms = settled_at(&luminance, before, after).map(|i| frame_ms[i]);
    // The last frame, for whoever looks at the result.
    let image = gpu.primary_image();
    let folder = out.join(super::FOLDER).join("dig");
    fs::create_dir_all(&folder).map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    write_png(&folder.join("after.png"), &image)?;
    Ok(EditSettle {
        width: settings.size.0,
        height: settings.size.1,
        voxels_dug: gap.len(),
        update_ms,
        before,
        after,
        frame_ms,
        luminance,
        settle_ms,
        slow_frames: slow,
        within_target: settle_ms.map(|ms| ms <= SETTLE_WITHIN_MS),
    })
}

/// The gates of the phase over its views and its dig.
pub fn gates(
    settings: &Settings,
    views: &[(&'static str, &ViewGi, Option<bool>)],
    edit: &EditSettle,
) -> Gates {
    let mut over = Vec::new();
    if settings.flip {
        for &(name, gi, reference_within) in views {
            if name == "room" {
                // A room that FLIP did not score is over: nothing says it is lit.
                let flip = gi.gi.settled.flip.map(|f| f.flip_mean);
                if !flip.is_some_and(|f| f <= ROOM_TARGET_FLIP) {
                    over.push("room-flip".to_string());
                }
                if reference_within != Some(true) {
                    over.push("reference-room".to_string());
                }
            }
            if gi.gi_not_further != Some(true) {
                over.push(format!("no-further:{name}"));
            }
        }
        if edit.within_target != Some(true) {
            over.push("settle".to_string());
        }
    }
    Gates {
        room_target_flip: ROOM_TARGET_FLIP,
        settle_within_ms: SETTLE_WITHIN_MS,
        reference_flip: still::FLIP_TARGET,
        reference_within_s: still::WITHIN_S,
        held: settings.flip,
        over,
    }
}

/// The exit status of the gates: 2 if any is over, unless `no_fail`.
pub fn exit_code(report: &GiReport, no_fail: bool) -> u8 {
    if report.gates.over.is_empty() || no_fail {
        0
    } else {
        2
    }
}

/// The views' scores and the dig in brief, for whoever is watching the run.
pub fn print_summary(views: &[(&'static str, &ViewGi)], report: &GiReport) {
    let num =
        |v: Option<f64>, digits: usize| v.map_or("n/a".to_string(), |v| format!("{v:.digits$}"));
    eprintln!("convergence, Primary against truth (FLIP mean; luminance over truth's):");
    for &(name, gi) in views {
        for (what, v) in [("flat ambient", &gi.flat), ("GI", &gi.gi)] {
            eprintln!(
                "  {name:<7} {what:<12} first frame {} ({:.3}), settled after {} frames {} ({:.3})",
                num(v.first_frame.flip.map(|f| f.flip_mean), 4),
                v.first_frame.luminance_ratio,
                gi.settle_frames,
                num(v.settled.flip.map(|f| f.flip_mean), 4),
                v.settled.luminance_ratio,
            );
        }
    }
    let e = &report.edit;
    eprintln!(
        "  dig: update {:.2} ms; the lit result goes from {:.4} to {:.4} and settles after {} ms \
         ({} frames over 60 Hz)",
        e.update_ms,
        e.before,
        e.after,
        num(e.settle_ms, 0),
        e.slow_frames
    );
    let g = &report.gates;
    if !g.held {
        eprintln!("  gates: not held (no FLIP)");
    } else if g.over.is_empty() {
        eprintln!("  gates: all met");
    } else {
        eprintln!("  gates: OVER: {}", g.over.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_has_settled_at_the_first_frame_after_which_it_stays_near_where_it_ends() {
        // From 1.0 to 2.0, within 0.1 of 2.0 from the fourth frame on.
        let series = [1.0, 1.4, 1.7, 1.95, 2.05, 1.98, 2.0];
        assert_eq!(settled_at(&series, 1.0, 2.0), Some(3));
        // A frame that leaves again after being near is not the start of the settled tail.
        let series = [1.0, 1.95, 1.2, 1.95, 2.0, 2.0];
        assert_eq!(settled_at(&series, 1.0, 2.0), Some(3));
        // Settled from the first frame.
        assert_eq!(settled_at(&[2.0, 2.0, 2.0], 1.0, 2.0), Some(0));
    }

    #[test]
    fn a_dig_that_changes_the_room_too_little_has_no_settling_time() {
        assert_eq!(settled_at(&[1.0, 1.01, 1.02], 1.0, 1.02), None);
        // The last frame is not near the end: nothing settled (cannot happen where the end is the
        // mean of the last frames, and is a no-time and not a panic if it does).
        assert_eq!(settled_at(&[1.0, 1.5, 1.0], 1.0, 2.0), None);
    }

    fn view_gi(first: Option<f64>, settled: Option<f64>) -> Variant {
        let score = |flip: Option<f64>| Score {
            flip: flip.map(|m| Scores {
                flip_mean: m,
                flip_p99: m,
            }),
            luminance_ratio: 1.0,
        };
        Variant {
            first_frame: score(first),
            settled: score(settled),
        }
    }

    fn view(flat: f64, gi: f64) -> ViewGi {
        let (flat, gi) = (view_gi(Some(flat), Some(flat)), view_gi(Some(gi), Some(gi)));
        ViewGi {
            gi_not_further: Some(
                gi.settled.flip.unwrap().flip_mean <= flat.settled.flip.unwrap().flip_mean,
            ),
            flat,
            gi,
            settle_frames: 32,
            files: Vec::new(),
        }
    }

    fn edit(settle_ms: Option<f64>) -> EditSettle {
        EditSettle {
            width: 1,
            height: 1,
            voxels_dug: 1,
            update_ms: 0.0,
            before: 1.0,
            after: 2.0,
            frame_ms: Vec::new(),
            luminance: Vec::new(),
            settle_ms,
            slow_frames: 0,
            within_target: settle_ms.map(|ms| ms <= SETTLE_WITHIN_MS),
        }
    }

    #[test]
    fn the_gates_are_the_tickets_and_each_is_over_by_itself() {
        let settings = Settings::full((2560, 1440));
        let ok = [
            ("golden", &view(0.2, 0.15), None),
            ("pit", &view(0.2, 0.19), None),
            ("room", &view(0.4, 0.09), Some(true)),
        ];
        let g = gates(&settings, &ok, &edit(Some(300.0)));
        assert!(g.held && g.over.is_empty(), "{g:?}");
        // The room over its FLIP, a view that moved away, a dig that took too long, a Reference
        // that did not get there in time: each is named.
        let worse = view(0.2, 0.25);
        let bad = [
            ("golden", &worse, None),
            ("pit", &view(0.2, 0.19), None),
            ("room", &view(0.4, 0.11), Some(false)),
        ];
        let g = gates(&settings, &bad, &edit(Some(600.0)));
        assert_eq!(
            g.over,
            ["no-further:golden", "room-flip", "reference-room", "settle"]
        );
        // A dig whose time cannot be told is over too.
        let g = gates(&settings, &ok, &edit(None));
        assert_eq!(g.over, ["settle"]);
        // Without FLIP no gate is held.
        let g = gates(&Settings::short((80, 45)), &bad, &edit(Some(600.0)));
        assert!(!g.held && g.over.is_empty());
    }

    #[test]
    fn the_targets_are_the_tickets() {
        assert_eq!(ROOM_TARGET_FLIP, 0.10);
        assert_eq!(SETTLE_WITHIN_MS, 500.0);
        assert_eq!((still::FLIP_TARGET, still::WITHIN_S), (0.05, 0.25));
    }

    #[test]
    fn exit_status_is_2_for_an_over_gate_and_0_for_no_fail() {
        let mut report = GiReport {
            edit: edit(Some(1.0)),
            gates: gates(&Settings::short((80, 45)), &[], &edit(None)),
        };
        assert_eq!(exit_code(&report, false), 0);
        report.gates.over.push("settle".into());
        assert_eq!(exit_code(&report, false), 2);
        assert_eq!(exit_code(&report, true), 0);
    }
}
