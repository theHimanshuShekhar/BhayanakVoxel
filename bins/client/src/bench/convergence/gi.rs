//! Primary's GI against the Reference (#54, #55, ADR 0005): how close Primary's image is to the
//! Reference's truth in each convergence view, with the radiance cache and with the flat ambient
//! that Primary had before it, denoised and not, and how fast the lit result follows a dig.
//!
//! **What is scored.** In each of the convergence views (the golden's, the pit and the emissive
//! room) Primary is drawn as the window draws it, traced at half the size and upscaled, and scored
//! by FLIP (LDR, [`score::FLIP_PPD`], mean and 99th percentile, as the other views are) against the
//! view's truth A, which `run_view` has made: the same image, the same seed and the same size, so
//! that the scores of one run say how the views stand and nothing else. Primary is drawn twice, on
//! a GPU context of its own each:
//! - **`flat`**: the flat ambient of 0.25 that main's Primary had before #54
//!   ([`Offscreen::set_flat_ambient`]), the baseline, with no denoiser. It is not a mode of the game;
//!   it is here so that "no view moves further from the Reference than main's Primary" has a number
//!   on the same build, and the report has each view's score before and after.
//! - **`raw`**: the cache's GI with the denoiser off ([`render::Denoise::Off`]): Primary as #54 left
//!   it, which shows what the denoiser did and what it did not (a denoiser removes noise, not bias).
//! - **`gi`**: the cache's GI as the window draws it, with the denoiser on
//!   ([`render::Denoise::Full`], #55) and the sun's shadow rays weighed as `--shadow-weight` says.
//!   **This is what is gated.**
//! - **`other_weight`**: `gi` with the other weight of the sun's shadow rays
//!   ([`render::ShadowWeight`]), so that one run says which measures better.
//!
//! The Reference's own score at 64 samples (`reference_64`, the convergence curve's point) is
//! reported beside them, not gated: it is what a path tracer gets for a frame's budget of samples.
//!
//! Each is scored twice, as the shadows view is: **`first_frame`**, the frame after the view was
//! made, with a cache that has nothing in it, and **`settled`**, after [`Settings::settle_frames`]
//! frames of a camera that has not moved, the cache and the upscaler's history having gathered what
//! they gather. **The gates are on the settled frame**, since the first one of a cold cache is
//! the noisiest the GI is, and the player has it once, in the frame the scene is made; the first
//! frame is reported beside it.
//!
//! **The gates** (the ticket's acceptance, the run's exit status 2 if any is over):
//! - **every** view's settled image of `gi` is at FLIP [`PRIMARY_TARGET_FLIP`] or under (#55: the
//!   room's was 0.10 until the denoiser);
//! - no view's settled GI image is further from truth than its flat-ambient one;
//! - the lit result settles within [`SETTLE_WITHIN_MS`] of a dig ([`EditSettle`]);
//! - the Reference converges in the room: FLIP at most [`still::FLIP_TARGET`] within
//!   [`still::ROOM_WITHIN_S`] (0.3 s, the owner's exception for the closed room) **of GPU time**
//!   (the line's `gpu_s`, the owner's decision of 2026-11-20); the still run's wall clock for it is
//!   reported and not gated.
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

use brick_store::VoxelCoord;
use render::{Camera, Denoise, Image, Offscreen, Resolution, Scene, ShadowWeight};
use serde::Serialize;

use super::shadows::{CONVERGED_FRAMES, Converged, Scores};
use super::views::{self, View};
use super::{score, still, write_png};

/// The FLIP Primary's settled image is held to against the Reference's truth in every view, at
/// 1440p on the 4090 (#55's; ADR 0005's "acceptable" line). It was 0.10 in the room until the
/// denoiser. A denoiser removes noise, not bias, so this is Primary's bias as much as its noise.
pub const PRIMARY_TARGET_FLIP: f64 = 0.05;
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
    /// Whether the dig goes the game's way: a ring scene of the test Planet with the room in it, the
    /// Edit made through `World::edit` with the scene thread, the frames paced at 60 Hz by the clock
    /// (the scene thread's time is real). Off for `--short`, which has no Planet to make.
    pub game_path: bool,
    /// How the sun's shadow rays are weighed in `gi` (`--shadow-weight`): the gates are on it.
    pub shadow_weight: ShadowWeight,
    /// Frames of a still camera that Primary with no denoiser is averaged over for `converged` (0:
    /// not measured).
    pub converged_frames: u32,
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
            game_path: true,
            shadow_weight: ShadowWeight::default(),
            converged_frames: CONVERGED_FRAMES,
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
            game_path: false,
            shadow_weight: ShadowWeight::default(),
            converged_frames: 0,
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
    /// Primary with the cache's GI and no denoiser: as #54 left it.
    pub raw: Variant,
    /// Primary as the window draws it: the cache's GI, the denoiser on (#55), and the shadow rays
    /// weighed as `shadow_weight` says. What the gates are held on.
    pub gi: Variant,
    /// `gi` with the other weight of the sun's shadow rays.
    pub other_weight: Variant,
    /// The names of the two weights.
    pub shadow_weight: &'static str,
    pub other_shadow_weight: &'static str,
    /// FLIP's mean of the Reference at 64 samples against truth A: the curve's point, reported
    /// beside Primary's score and not gated. Null without FLIP or where the curve has no point
    /// at 64.
    pub reference_64: Option<f64>,
    /// Primary converged (no denoiser, averaged over many frames of the still view), and the view's
    /// error split into its bias (FLIP of that against truth A: what no denoiser removes) and the
    /// denoiser's residual (FLIP of `gi.settled` against it); null where `--converged-frames` is 0.
    pub converged: Option<Converged>,
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
    /// Whether the dig went the game's way (a ring scene, `World::edit` and the scene thread), not
    /// on a flat scene.
    pub game_path: bool,
}

/// The gates, and what is over.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gates {
    pub primary_target_flip: f64,
    pub settle_within_ms: f64,
    pub reference_flip: f64,
    /// ... within this many seconds in the outdoor views (which have no gate here), and in the room
    /// `reference_room_within_s`.
    pub reference_within_s: f64,
    pub reference_room_within_s: f64,
    /// Whether the gates were held: with FLIP only.
    pub held: bool,
    /// The names of the gates that are over, empty if none: `primary-flip:VIEW` (a view's settled
    /// FLIP over [`PRIMARY_TARGET_FLIP`], the sun view's too), `no-further:VIEW`, `settle`,
    /// `reference-room`.
    pub over: Vec<String>,
    /// The gates that were not held because the view cannot tell their line from its own noise
    /// (ADR 0005: a line counts where it is at least `MIN_LINE_OVER_FLOOR` times the floor):
    /// `reference-room` where the room's FLIP 0.05 is under four times its floor.
    pub not_counted: Vec<String>,
}

/// The report's `convergence.gi` block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GiReport {
    pub edit: EditSettle,
    pub gates: Gates,
}

/// The mean luminance of an 8-bit RGBA image in linear light (Rec. 709 weights), which is the
/// light itself where the display is the sRGB curve alone.
pub(super) fn mean_luminance(image: &Image) -> f64 {
    let l = |c: u8| f64::from(render::srgb_decode(f32::from(c) / 255.0));
    let pixels = image.rgba.as_chunks::<4>().0;
    let n = pixels.len() as f64;
    pixels
        .iter()
        .map(|p| 0.2126 * l(p[0]) + 0.7152 * l(p[1]) + 0.0722 * l(p[2]))
        .sum::<f64>()
        / n
}

/// How Primary is lit in one of the variants.
#[derive(Clone, Copy)]
struct Lighting {
    /// The flat ambient of before #54 instead of the cache's GI.
    flat: bool,
    denoise: Denoise,
    shadow_weight: ShadowWeight,
}

/// Primary of `view`'s scene, as the window draws it, in the legacy light of the convergence views:
/// the first frame and the frame after `frames`.
fn draw(
    view: &View,
    settings: &Settings,
    lighting: Lighting,
) -> Result<(Image, Image), Box<dyn Error>> {
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    let mut gpu = Offscreen::with_resolution(resolution, &view.scene)?;
    gpu.use_legacy_light();
    gpu.set_flat_ambient(lighting.flat);
    gpu.set_denoise(lighting.denoise);
    gpu.set_shadow_weight(lighting.shadow_weight);
    let first = gpu.render_upscaled(&view.camera);
    let mut settled = first.clone();
    for _ in 1..settings.settle_frames.max(2) {
        settled = gpu.render_upscaled(&view.camera);
    }
    Ok((first, settled))
}

/// Primary of `view`'s scene with the cache's GI, the denoiser off and the shadow rays weighed as
/// `weight` says, averaged over `settings.converged_frames` frames of the still camera after
/// `settle_frames` of them: its own mean (#55's `Primary converged`).
fn converged_image(
    view: &View,
    settings: &Settings,
    weight: ShadowWeight,
    denoise: Denoise,
) -> Result<Option<Image>, Box<dyn Error>> {
    if settings.converged_frames == 0 {
        return Ok(None);
    }
    eprintln!(
        "convergence: {}: Primary, denoiser {denoise:?}, averaged over {} frames...",
        view.name, settings.converged_frames
    );
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    let mut gpu = Offscreen::with_resolution(resolution, &view.scene)?;
    gpu.use_legacy_light();
    gpu.set_flat_ambient(false);
    gpu.set_denoise(denoise);
    gpu.set_shadow_weight(weight);
    for _ in 0..settings.settle_frames.max(2) {
        gpu.render_upscaled(&view.camera);
    }
    Ok(score::mean_of_frames(settings.converged_frames, || {
        gpu.render_upscaled(&view.camera)
    }))
}

/// Splits a view's error (#55): `converged` is Primary averaged over many frames with no denoiser,
/// so FLIP of it against truth A is the bias and FLIP of Primary's settled frames against it is the
/// noise that is left, which the denoiser is for. Also the sun view's.
#[allow(clippy::too_many_arguments)]
pub(super) fn converged_split(
    converged: &Image,
    denoised_converged: &Image,
    truth: &Image,
    truth_rgb: &[u8],
    denoised_settled: &Image,
    raw_settled: &Image,
    size: (usize, usize),
    frames: u32,
    flip: bool,
    name: &str,
) -> Result<Converged, String> {
    let rgb = score::rgb_of_rgba(&converged.rgba);
    let against = |what: &str, reference: &[u8], test: &Image| -> Result<Option<Scores>, String> {
        if !flip {
            return Ok(None);
        }
        eprintln!("convergence: {name}: FLIP of {what}...");
        score::flip(reference, &score::rgb_of_rgba(&test.rgba), size).map(|f| Some(f.into()))
    };
    Ok(Converged {
        frames,
        luminance_ratio: mean_luminance(converged) / mean_luminance(truth),
        bias: against(
            "Primary converged against truth (the bias)",
            truth_rgb,
            converged,
        )?,
        residual: against(
            "Primary denoised, settled, against Primary converged (the residual)",
            &rgb,
            denoised_settled,
        )?,
        raw_residual: against(
            "Primary with no denoiser, settled, against Primary converged",
            &rgb,
            raw_settled,
        )?,
        denoised_limit: against(
            "Primary denoised and averaged against truth (the denoiser's limit)",
            truth_rgb,
            denoised_converged,
        )?,
        denoiser_shift: against(
            "Primary denoised and averaged against Primary converged (the denoiser's shift)",
            &rgb,
            denoised_converged,
        )?,
    })
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
    let mut variant =
        |label: &str, lighting: Lighting| -> Result<(Variant, Image), Box<dyn Error>> {
            let (first, settled) = draw(view, settings, lighting)?;
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
            let variant = Variant {
                first_frame: scored("first frame", &first)?,
                settled: scored("settled", &settled)?,
            };
            Ok((variant, settled))
        };
    let weight = settings.shadow_weight;
    let other = match weight {
        ShadowWeight::CentreCosine => ShadowWeight::PerSample,
        ShadowWeight::PerSample => ShadowWeight::CentreCosine,
    };
    let lit = |denoise, shadow_weight| Lighting {
        flat: false,
        denoise,
        shadow_weight,
    };
    let (flat, _) = variant(
        "flat",
        Lighting {
            flat: true,
            denoise: Denoise::Off,
            shadow_weight: weight,
        },
    )?;
    let (raw, raw_settled) = variant("raw", lit(Denoise::Off, weight))?;
    let (gi, gi_settled) = variant("gi", lit(Denoise::Full, weight))?;
    let (other_weight, _) = variant(&format!("gi-{}", other.name()), lit(Denoise::Full, other))?;
    let converged = match converged_image(view, settings, weight, Denoise::Off)? {
        Some(image) => {
            let denoised =
                converged_image(view, settings, weight, Denoise::Full)?.expect("the same frames");
            write_png(&folder.join("primary-converged-denoised.png"), &denoised)?;
            files.push("primary-converged-denoised.png".to_string());
            write_png(&folder.join("primary-converged.png"), &image)?;
            files.push("primary-converged.png".to_string());
            Some(converged_split(
                &image,
                &denoised,
                truth,
                truth_rgb,
                &gi_settled,
                &raw_settled,
                size,
                settings.converged_frames,
                settings.flip,
                name,
            )?)
        }
        None => None,
    };
    files.sort();
    let gi_not_further = flat
        .settled
        .flip
        .zip(gi.settled.flip)
        .map(|(flat, gi)| gi.flip_mean <= flat.flip_mean);
    Ok(ViewGi {
        flat,
        raw,
        gi,
        other_weight,
        shadow_weight: weight.name(),
        other_shadow_weight: other.name(),
        reference_64: None,
        converged,
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

/// Digs the gap in the room's wall and times how long the lit result takes to follow: through the
/// game's path with [`Settings::game_path`], else on a flat scene (CI's smoke test).
pub fn dig(settings: &Settings, out: &Path) -> Result<EditSettle, Box<dyn Error>> {
    if settings.game_path {
        dig_in_the_game(settings, out)
    } else {
        dig_flat(settings, out)
    }
}

/// The frames of a dig and what they say, from the Edit's start.
struct Frames {
    frame_ms: Vec<f64>,
    luminance: Vec<f64>,
    slow: usize,
}

/// Where a dig ends: the settled time, the image and the report from `frames`.
fn settle_report(
    settings: &Settings,
    out: &Path,
    gpu: &mut Offscreen,
    before: f64,
    update_ms: f64,
    voxels_dug: usize,
    frames: Frames,
) -> Result<EditSettle, Box<dyn Error>> {
    let Frames {
        frame_ms,
        luminance,
        slow,
    } = frames;
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
        voxels_dug,
        update_ms,
        before,
        after,
        frame_ms,
        luminance,
        settle_ms,
        slow_frames: slow,
        within_target: settle_ms.map(|ms| ms <= SETTLE_WITHIN_MS),
        game_path: settings.game_path,
    })
}

/// The mean luminance of the last [`END_FRAMES`] of `recent`.
fn tail_mean(recent: &[f64]) -> f64 {
    let tail = &recent[recent.len().saturating_sub(END_FRAMES)..];
    tail.iter().sum::<f64>() / tail.len() as f64
}

/// The dig on a flat scene: the Edit's update made on this thread and copied at once.
fn dig_flat(settings: &Settings, out: &Path) -> Result<EditSettle, Box<dyn Error>> {
    let (mut store, camera) = views::walled_room();
    let mut scene = Scene::new(&store, None);
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    let mut gpu = Offscreen::with_resolution(resolution, &scene)?;
    gpu.use_legacy_light();
    gpu.set_flat_ambient(false);
    gpu.set_denoise(Denoise::Full);
    gpu.set_shadow_weight(settings.shadow_weight);
    // The warm-up: the camera still, the cache gathering the dark side's light.
    let mut recent = Vec::new();
    for _ in 0..settings.warm_frames {
        recent.push(mean_luminance(&gpu.render_upscaled(&camera)));
    }
    let before = tail_mean(&recent);

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
    gpu.edit_made(&changed);
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
    let frames = Frames {
        frame_ms,
        luminance,
        slow,
    };
    settle_report(
        settings,
        out,
        &mut gpu,
        before,
        update_ms,
        gap.len(),
        frames,
    )
}

/// How many Voxels of the room's wall (x = 18..20) are solid in `store`.
fn wall_voxels(store: &brick_store::BrickStore, at: [i32; 3]) -> usize {
    let mut solid = 0;
    for x in 18..20 {
        for y in 0..16 {
            for z in 0..40 {
                let v = VoxelCoord([x + at[0], y + at[1], z + at[2]]);
                solid += usize::from(store.get(v).is_some());
            }
        }
    }
    solid
}

/// The dig the game's way. The room is made in the test Planet's store a little above the spawn
/// (a ring scene, streamed by the loader and the scene thread), the camera is inside it on the dark
/// side, and the Edit is the player's: [`World::edit`] with a Brush aimed at the wall, which only
/// queues the work for the scene thread and returns. The scene thread's patch of it arrives in a
/// frame's `recentre`, as it does in the game's (`main.rs`), and is copied to the GPU there; the
/// cache is told which bricks the Edit changed when it is made (`World::take_edited`). The frames
/// are paced at 60 Hz by the clock, since the scene thread's time is real, and the time is from the
/// click.
fn dig_in_the_game(settings: &Settings, out: &Path) -> Result<EditSettle, Box<dyn Error>> {
    use net::{EditMode, Shape, loopback};
    use sim::Predictor;

    use crate::bench::stream::{TestPlanet, test_planet};
    use crate::loader::Loader;
    use crate::tool::Tool;
    use crate::world::World;

    let TestPlanet {
        mut store,
        mut scene,
        spawn,
        bands,
        ..
    } = test_planet();
    // The room's corner: its wall's aim point is 3 m above the spawn (the Edit's reach is from
    // the camera, the room is a few metres from the character).
    let up = spawn.horizon_up;
    let at: [i32; 3] = std::array::from_fn(|a| {
        (spawn.position[a] + up[a] * 60.0 - views::WALL_AIM[a]).round() as i32
    });
    let shift = at.map(f64::from);
    let shifted = |p: [f64; 3]| std::array::from_fn(|a| p[a] + shift[a]);
    let placed = views::put_walled_room(&mut store, at);
    store.refresh_lod(placed.iter().copied());
    scene.update(&store, &placed)?;
    let walls_before = wall_voxels(&store, at);

    let camera = Camera::looking_at(shifted(views::DARK_SIDE.0), shifted(views::DARK_SIDE.1));
    // The player aims at the wall from where they stand.
    let aim = Camera::looking_at(shifted(views::DARK_SIDE.0), shifted(views::WALL_AIM));
    let source = store.source().expect("the store has a source");
    let (client, _server) = loopback();
    let mut world = World::new(scene, Predictor::new(store), client).with_loader(Loader::new(
        source,
        camera.position,
        bands,
        2,
    ));
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    let mut gpu = Offscreen::with_resolution(resolution, world.scene())?;
    gpu.use_legacy_light();
    gpu.set_flat_ambient(false);
    gpu.set_denoise(Denoise::Full);
    gpu.set_shadow_weight(settings.shadow_weight);

    let frame = Duration::from_secs_f64(FRAME_MS / 1e3);
    // One frame as the game draws it: the camera is told, what the scene thread made is copied.
    let frame_of = |world: &mut World, gpu: &mut Offscreen| -> Result<(), Box<dyn Error>> {
        if let Some(update) = world.recentre(camera.position)? {
            gpu.update_scene(world.scene(), &update)?;
        }
        gpu.draw_upscaled(&camera);
        Ok(())
    };
    // The warm-up: the scene thread brings the rings round the camera, and the cache gathers the
    // dark side's light. The luminance is read at the end of it only.
    let mut recent = Vec::new();
    for n in 0..settings.warm_frames {
        let begun = Instant::now();
        frame_of(&mut world, &mut gpu)?;
        if let Some(rest) = frame.checked_sub(begun.elapsed()) {
            std::thread::sleep(rest);
        }
        if n + END_FRAMES as u32 >= settings.warm_frames {
            recent.push(mean_luminance(&gpu.primary_image()));
        }
    }
    let before = tail_mean(&recent);

    // The click.
    let tool = Tool {
        mode: EditMode::Dig,
        shape: Shape::Cube,
        radius: 4,
        ..Tool::default()
    };
    let started = Instant::now();
    let edits = world.edits_made();
    let update = world.edit(&aim, &tool)?;
    if world.edits_made() == edits {
        return Err("the dig was not made: nothing was aimed at, or it is out of reach".into());
    }
    if let Some(update) = update {
        gpu.update_scene(world.scene(), &update)?;
    }
    gpu.edit_made(&world.take_edited());
    let update_ms = started.elapsed().as_secs_f64() * 1e3;
    let voxels_dug = walls_before - wall_voxels(&world.store(), at);

    // The frames, from the click, one for every 1/60 s of the clock: a frame that takes longer takes
    // its own time. The luminance is read outside the clock (its readback is not a frame's).
    let (mut frame_ms, mut luminance) = (Vec::new(), Vec::new());
    let mut slow = 0;
    let mut reading = Duration::ZERO;
    for _ in 0..settings.dig_frames {
        let begun = Instant::now();
        frame_of(&mut world, &mut gpu)?;
        let took = begun.elapsed();
        if took > frame {
            slow += 1;
        }
        if let Some(rest) = frame.checked_sub(took) {
            std::thread::sleep(rest);
        }
        frame_ms.push((started.elapsed() - reading).as_secs_f64() * 1e3);
        let read = Instant::now();
        luminance.push(mean_luminance(&gpu.primary_image()));
        reading += read.elapsed();
    }
    let frames = Frames {
        frame_ms,
        luminance,
        slow,
    };
    settle_report(
        settings, out, &mut gpu, before, update_ms, voxels_dug, frames,
    )
}

/// The gates of the phase over its views and its dig.
///
/// Each view comes with whether its Reference was within the target ([`still::FLIP_TARGET`] in
/// [`still::WITHIN_S`] of the wall clock; the room's in [`still::ROOM_WITHIN_S`] of GPU time, which
/// `convergence::reference_within` works out) and whether that line counts in the view.
///
/// `sun` is the settled FLIP mean of the sun-and-sky view's Primary (`convergence.shadows`), which
/// is held to [`PRIMARY_TARGET_FLIP`] too.
pub fn gates(
    settings: &Settings,
    views: &[(&'static str, &ViewGi, Option<bool>, bool)],
    sun: Option<f64>,
    edit: &EditSettle,
) -> Gates {
    let mut over = Vec::new();
    let mut not_counted = Vec::new();
    if settings.flip {
        for &(name, gi, reference_within, counts) in views {
            // A view that FLIP did not score is over: nothing says it is close to the truth.
            let flip = gi.gi.settled.flip.map(|f| f.flip_mean);
            if !flip.is_some_and(|f| f <= PRIMARY_TARGET_FLIP) {
                over.push(format!("primary-flip:{name}"));
            }
            if name == "room" {
                if !counts {
                    not_counted.push("reference-room".to_string());
                } else if reference_within != Some(true) {
                    over.push("reference-room".to_string());
                }
            }
            if gi.gi_not_further != Some(true) {
                over.push(format!("no-further:{name}"));
            }
        }
        if !sun.is_some_and(|f| f <= PRIMARY_TARGET_FLIP) {
            over.push("primary-flip:sun".to_string());
        }
        if edit.within_target != Some(true) {
            over.push("settle".to_string());
        }
    }
    Gates {
        primary_target_flip: PRIMARY_TARGET_FLIP,
        settle_within_ms: SETTLE_WITHIN_MS,
        reference_flip: still::FLIP_TARGET,
        reference_within_s: still::WITHIN_S,
        reference_room_within_s: still::ROOM_WITHIN_S,
        held: settings.flip,
        over,
        not_counted,
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
        for (what, v) in [
            ("flat ambient", &gi.flat),
            ("GI, no denoiser", &gi.raw),
            (gi.shadow_weight, &gi.gi),
            (gi.other_shadow_weight, &gi.other_weight),
        ] {
            let what = if std::ptr::eq(v, &gi.gi) || std::ptr::eq(v, &gi.other_weight) {
                format!("denoised, {what}")
            } else {
                what.to_string()
            };
            eprintln!(
                "  {name:<7} {what:<30} first frame {} ({:.3}), settled after {} frames {} ({:.3})",
                num(v.first_frame.flip.map(|f| f.flip_mean), 4),
                v.first_frame.luminance_ratio,
                gi.settle_frames,
                num(v.settled.flip.map(|f| f.flip_mean), 4),
                v.settled.luminance_ratio,
            );
        }
        eprintln!(
            "  {name:<7} the Reference at 64 samples: FLIP {} (reported, not gated)",
            num(gi.reference_64, 4)
        );
        if let Some(c) = &gi.converged {
            eprintln!(
                "  {name:<7} Primary converged ({} frames, luminance {:.3}): bias {} (FLIP against truth), \
                 residual {} denoised, {} with no denoiser, the denoiser's own shift {}, its limit against truth {}",
                c.frames,
                c.luminance_ratio,
                num(c.bias.map(|f| f.flip_mean), 4),
                num(c.residual.map(|f| f.flip_mean), 4),
                num(c.raw_residual.map(|f| f.flip_mean), 4),
                num(c.denoiser_shift.map(|f| f.flip_mean), 4),
                num(c.denoised_limit.map(|f| f.flip_mean), 4),
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
    } else {
        if !g.not_counted.is_empty() {
            eprintln!(
                "  gates: not held, the line is under {} times the floor: {}",
                super::MIN_LINE_OVER_FLOOR,
                g.not_counted.join(", ")
            );
        }
        if g.over.is_empty() {
            let held = if g.not_counted.is_empty() {
                "all met"
            } else {
                "the others met"
            };
            eprintln!("  gates: {held}");
        } else {
            eprintln!("  gates: OVER: {}", g.over.join(", "));
        }
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
            raw: gi,
            gi,
            other_weight: gi,
            shadow_weight: "per-sample",
            other_shadow_weight: "centre-cosine",
            reference_64: Some(0.2),
            converged: None,
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
            game_path: false,
        }
    }

    #[test]
    fn the_gates_are_the_tickets_and_each_is_over_by_itself() {
        let settings = Settings::full((2560, 1440));
        let ok = [
            ("golden", &view(0.2, 0.05), None, true),
            ("pit", &view(0.2, 0.04), None, true),
            ("room", &view(0.4, 0.03), Some(true), true),
        ];
        let g = gates(&settings, &ok, Some(0.04), &edit(Some(300.0)));
        assert!(g.held && g.over.is_empty(), "{g:?}");
        // Every view over the one FLIP (the room's 0.10 is not what it was), the sun view too, a
        // view that moved away, a dig that took too long, a Reference that did not get there in
        // time: each is named.
        let worse = view(0.04, 0.06);
        let bad = [
            ("golden", &worse, None, true),
            ("pit", &view(0.2, 0.04), None, true),
            ("room", &view(0.4, 0.09), Some(false), true),
        ];
        let g = gates(&settings, &bad, Some(0.051), &edit(Some(600.0)));
        assert_eq!(
            g.over,
            [
                "primary-flip:golden",
                "no-further:golden",
                "primary-flip:room",
                "reference-room",
                "primary-flip:sun",
                "settle"
            ]
        );
        // A sun view that FLIP did not score is over.
        let g = gates(&settings, &ok, None, &edit(Some(300.0)));
        assert_eq!(g.over, ["primary-flip:sun"]);
        // A Reference whose line the room cannot tell from its floor is not held to it, and the
        // report says so.
        let blurred = [("room", &view(0.4, 0.03), Some(false), false)];
        let g = gates(&settings, &blurred, Some(0.04), &edit(Some(300.0)));
        assert!(g.over.is_empty(), "{g:?}");
        assert_eq!(g.not_counted, ["reference-room"]);
        // A dig whose time cannot be told is over too.
        let g = gates(&settings, &ok, Some(0.04), &edit(None));
        assert_eq!(g.over, ["settle"]);
        // Without FLIP no gate is held.
        let g = gates(&Settings::short((80, 45)), &bad, None, &edit(Some(600.0)));
        assert!(!g.held && g.over.is_empty());
    }

    /// The dig the game's way (a ring scene of the test Planet, `World::edit`, the scene thread)
    /// on a software driver, for whoever wants to see it: it makes the test Planet's spawn ring, which
    /// takes seconds, so it is not run with the others (`cargo test --release -p client -- --ignored
    /// --nocapture the_dig_through`).
    #[test]
    #[ignore = "makes the test Planet: seconds, and slow on a software driver"]
    fn the_dig_through_the_games_path_changes_the_room_and_settles() {
        let out = std::env::temp_dir().join(format!("bhayanak-dig-{}", std::process::id()));
        let _ = fs::remove_dir_all(&out);
        fs::create_dir_all(&out).unwrap();
        for game_path in [false, true] {
            if game_path && std::env::var_os("DIG_FLAT_ONLY").is_some() {
                continue;
            }
            let settings = Settings {
                size: (320, 180),
                settle_frames: 4,
                flip: false,
                warm_frames: 60,
                dig_frames: 90,
                game_path,
                shadow_weight: ShadowWeight::default(),
                converged_frames: 0,
            };
            let edit = dig(&settings, &out).unwrap();
            let at = |n: usize| edit.luminance[n] / edit.after;
            eprintln!(
                "game path {game_path}: dug {} Voxels, update {:.2} ms, before {:.4} after {:.4}, \
                 settle {:?} ms (after frame {:?}), {} slow frames; the share of the end at frames 0 2 5 10 20 30 45 \
                 60: {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                edit.voxels_dug,
                edit.update_ms,
                edit.before,
                edit.after,
                edit.settle_ms,
                settled_at(&edit.luminance, edit.before, edit.after),
                edit.slow_frames,
                at(0),
                at(2),
                at(5),
                at(10),
                at(20),
                at(30),
                at(45),
                at(60)
            );
            assert_eq!(edit.game_path, game_path);
            assert!(
                edit.voxels_dug > 100,
                "the wall was dug: {}",
                edit.voxels_dug
            );
            assert!(edit.after > edit.before * 1.2, "the light shows");
            assert!(edit.settle_ms.is_some());
        }
        let _ = fs::remove_dir_all(&out);
    }

    #[test]
    fn the_targets_are_the_tickets() {
        // #55: one FLIP for every view (the room's was 0.10 before the denoiser).
        assert_eq!(PRIMARY_TARGET_FLIP, 0.05);
        assert_eq!(SETTLE_WITHIN_MS, 500.0);
        assert_eq!((still::FLIP_TARGET, still::WITHIN_S), (0.05, 0.25));
        // The room's exception (the owner, 2026-11-20; ADR 0005), a mark the run scores.
        assert_eq!(still::within_s_for("room"), 0.3);
        assert_eq!(still::within_s_for("golden"), 0.25);
        assert!(still::MARKS_S.contains(&still::ROOM_WITHIN_S));
    }

    #[test]
    fn exit_status_is_2_for_an_over_gate_and_0_for_no_fail() {
        let mut report = GiReport {
            edit: edit(Some(1.0)),
            gates: gates(&Settings::short((80, 45)), &[], None, &edit(None)),
        };
        assert_eq!(exit_code(&report, false), 0);
        report.gates.over.push("settle".into());
        assert_eq!(exit_code(&report, false), 2);
        assert_eq!(exit_code(&report, true), 0);
    }
}
