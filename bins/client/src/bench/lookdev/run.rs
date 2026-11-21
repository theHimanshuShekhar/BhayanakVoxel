//! `client lookdev --out DIR`: renders the eight look-dev shots (#148) with the real pipeline, as
//! the game draws them: the default Look (hero light B). For each shot, a Primary image at the
//! game's settings (with the sun's shadows, #52: one frame, and the same view settled over frames of
//! a still camera), a **combination sheet** of Reference images (the tone curves against the
//! smooth-normal settings, for the owner to pick from, #202), and for the dark descent a Primary
//! frame sequence on a 60 Hz clock, so that the exposure's adaptation (tau 2.5 s) shows. Written
//! as PNGs, a folder for each shot, and an `index.html` contact sheet.
//!
//! The Reference image is accumulated once for each normal setting (the normals change what is
//! lit), and shown through each tone curve, which only changes how the mean is shown.
//!
//! The scene of a shot is set up as the prototype's was (`proto-148-lookdev`): the level 0 ring
//! round the place, the Guard, the hand-placed stamps ([`stamps`]), and every brick the bands ask
//! for. It is not a bench mode: nothing here is timed and the bench's paths do not call it.

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use brick_store::{BrickStore, VOXELS_PER_METRE};
use planet_gen::planet::Planet;
use planet_gen::world_map::{TEST_MAP_SIZE, TEST_RADIUS, WORLD_MAP_SIZE, WorldMap};
use render::lod::Bands;
use render::sun::DEFAULT_ANGULAR_RADIUS;
use render::{Camera, Look, NormalBias, Offscreen, Resolution, Stages, Sun, ToneCurve};

use super::stamps;
use super::{Hue, Place, SHOTS, Shot, Stamp};
use crate::bench::write_png;

/// Voxels round a shot's place that the level 0 ring holds: 30 m.
const RING: i32 = (30.0 * VOXELS_PER_METRE) as i32;

/// The sun's elevation in a shot's own frame, in degrees (hero light B's).
const SUN_ELEVATION: f64 = 8.0;
/// The sun's angle from the shot's forward, in degrees, to the same side as the prototype's: the
/// side that is the camera's left (`cross(up, forward)`).
const SUN_FROM_FORWARD: f64 = 70.0;

/// The atmosphere sheet's time-of-day strip (#53): the sun's elevations, in degrees, night, dusk,
/// the horizon, the hero sun, the afternoon and midday; and the shots it is made of (outdoors, the
/// forest floor and the canyon with its haze).
const STRIP_ELEVATIONS: [f64; 6] = [-12.0, -4.0, 0.0, SUN_ELEVATION, 25.0, 60.0];
const STRIP_SHOTS: [&str; 2] = ["forest floor", "canyon with haze"];

/// The caves are lit by their crystals alone: four times the samples (the Reference samples the
/// crystals directly since #54, and the light that reaches the far side of a cave takes more).
const CAVE_SPP_FACTOR: u32 = 4;

const USAGE: &str = "usage: client lookdev --out DIR [--size WxH] [--spp N] [--compare DIR] [--atmosphere] [--untinted] [--shot NAME]...\n  \
the 4090: client.exe lookdev --out DIR [--compare OLD_SHEET_DIR]   (1280x720, 64 spp)\n  \
--gi (#54): instead of the combinations, each shot (and the emissive room) at the game's settings as the Reference\n  \
has it, as Primary was before the radiance cache (the flat ambient of 0.25), and as it is with the cache's GI\n  \
(on lavapipe: client lookdev --out DIR --gi --size 480x270 --spp 16)\n  \
--compare: the folder of the #148 sheet; each shot's OLD/NN-shot/hero-hero-b.png is shown first\n  \
--atmosphere (#53): instead of the combinations, each shot three times at the game's settings, hero light B\n  \
with today's gradient sky and haze, then with the untinted atmosphere, then with the atmosphere and the\n  \
hero tint (the game's), in both views; then a time-of-day strip of the forest floor and the canyon, the\n  \
Reference view at sun elevations of -12, -4, 0, 8, 25 and 60 degrees, untinted and tinted\n  \
(on lavapipe: client lookdev --out DIR --atmosphere --size 480x270 --spp 16)\n  \
--shot: only the shots whose name has NAME in it (e.g. --shot canyon --shot forest), for a quick look\n  \
--untinted: the other sheets (and the stills) with the physical atmosphere and no hero tint\n  \
a smoke test on any device: client lookdev --out DIR --short";

/// The smooth-normal settings of the combination sheet, in the order of its columns' groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Normals {
    /// Smoothing off: every hit has its face's normal, as the prototype's hero frames did.
    Faces,
    /// Smooth normals with a quarter of the face's normal added: the owner's pick, what the game has.
    Biased,
    /// Smooth normals with no face bias.
    Smooth,
}

const NORMALS: [Normals; 3] = [Normals::Faces, Normals::Biased, Normals::Smooth];
/// The tone curves of the sheet, AgX-like first: the curve of the frames the owner liked.
const CURVES: [ToneCurve; 3] = [ToneCurve::Agx, ToneCurve::Neutral, ToneCurve::NeutralDesat];

impl Normals {
    fn slug(self) -> &'static str {
        match self {
            Normals::Faces => "face-normals",
            Normals::Biased => "smooth-face-bias",
            Normals::Smooth => "smooth-no-bias",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Normals::Faces => "face normals",
            Normals::Biased => "smooth normals, 0.25 face bias (the game's)",
            Normals::Smooth => "smooth normals, no bias",
        }
    }

    /// Every stage on, with this normal setting and `curve`.
    fn stages(self, curve: ToneCurve) -> Stages {
        Stages {
            curve,
            smooth_normals: self != Normals::Faces,
            normal_bias: if self == Normals::Smooth {
                NormalBias::None
            } else {
                NormalBias::Face
            },
            ..Stages::default()
        }
    }
}

fn curve_label(curve: ToneCurve) -> String {
    match curve {
        ToneCurve::Agx => "AgX-like".to_string(),
        ToneCurve::Neutral => "Neutral".to_string(),
        ToneCurve::NeutralDesat => format!(
            "Neutral, desaturated (keeps {:.0}% of the colour)",
            render::post::NEUTRAL_DESAT_KEPT * 100.0
        ),
    }
}

/// What to render, and where to put it.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub out: PathBuf,
    pub size: (u32, u32),
    /// Samples a pixel of a Reference image (four times that in the caves).
    pub spp: u32,
    /// The folder of the #148 sheet, whose hero B frame of each shot is shown beside the new ones.
    pub compare: Option<PathBuf>,
    /// Frames of the dark descent's sequence that are written, evenly over its length.
    pub descent_frames: u32,
    /// The atmosphere's sheet (#53): each shot in both views under hero light B's gradient (what
    /// the renderer had) and under the atmosphere, side by side, in place of the combinations.
    pub atmosphere: bool,
    /// The GI sheet (#54): each shot, and the emissive room, as the Reference has it, as Primary was
    /// with the flat ambient, and as Primary is with the radiance cache, in place of the
    /// combinations.
    pub gi: bool,
    /// The other sheets with the physical atmosphere and no hero tint ([`Look::HERO_B_UNTINTED`]); the
    /// atmosphere's sheet shows both whatever this is.
    pub untinted: bool,
    /// Only the shots whose names contain one of these (all of them if there are none).
    pub only: Vec<String>,
    /// A smoke test: the test Planet (the shots' places are not there: the images are of nothing
    /// in particular), two shots, tiny.
    pub short: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        out: PathBuf::new(),
        size: (1280, 720),
        spp: 64,
        compare: None,
        descent_frames: 12,
        atmosphere: false,
        gi: false,
        untinted: false,
        only: Vec::new(),
        short: false,
    };
    let (mut have_out, mut size, mut spp) = (false, None, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}\n{USAGE}"));
        match arg.as_str() {
            "--out" => {
                options.out = value("a directory")?.into();
                have_out = true;
            }
            "--size" => {
                let v = value("WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size is WxH")?;
                size = Some((
                    w.parse().map_err(|_| "--size is WxH")?,
                    h.parse().map_err(|_| "--size is WxH")?,
                ));
            }
            "--spp" => spp = Some(value("a count")?.parse().map_err(|_| "--spp is a count")?),
            "--compare" => options.compare = Some(value("a directory")?.into()),
            "--short" => options.short = true,
            "--atmosphere" => options.atmosphere = true,
            "--gi" => options.gi = true,
            "--untinted" => options.untinted = true,
            "--shot" => options.only.push(value("a name")?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if !have_out {
        return Err(format!("--out is required\n{USAGE}"));
    }
    if options.short {
        options.size = (64, 36);
        options.spp = 1;
        options.descent_frames = 3;
    }
    options.size = size.unwrap_or(options.size);
    options.spp = spp.unwrap_or(options.spp);
    if options.size.0 == 0 || options.size.1 == 0 || options.spp == 0 {
        return Err(format!("the size and the samples are not zero\n{USAGE}"));
    }
    Ok(options)
}

/// Runs `client lookdev` with the arguments after `lookdev`.
pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    match parse(args) {
        Ok(options) => match run(&options) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("lookdev: {e}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The sun of a shot, in the shot's own frame: [`SUN_FROM_FORWARD`] degrees from `camera`'s
/// forward (flattened to the horizon), on the same side as the prototype's (`cross(up, forward)`,
/// the camera's left), [`SUN_ELEVATION`] degrees above the horizon, with the default radius (0.12).
/// "Up" is the camera's `horizon_up`, which is the Planet's radial up where the shot is, so the
/// sun is as high above the ground in every shot. (`Sun::local` measures from the frame's zero-yaw
/// heading, which is not where a shot looks.)
fn sun_for(camera: &Camera) -> Sun {
    sun_at(camera, SUN_ELEVATION)
}

/// [`sun_for`] at another elevation (degrees), under the same rules.
fn sun_at(camera: &Camera, elevation: f64) -> Sun {
    let up = camera.horizon_up;
    let forward = camera.forward();
    let tilt = dot(forward, up);
    let flat = unit(std::array::from_fn(|a| forward[a] - up[a] * tilt));
    let side = cross(up, flat);
    let (az, el) = (SUN_FROM_FORWARD.to_radians(), elevation.to_radians());
    let direction: [f32; 3] = std::array::from_fn(|a| {
        (up[a] * el.sin() + (flat[a] * az.cos() + side[a] * az.sin()) * el.cos()) as f32
    });
    Sun::new(direction, DEFAULT_ANGULAR_RADIUS).expect("a camera has a direction")
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    let length = dot(a, a).sqrt();
    a.map(|c| c / length)
}

/// Writes the shot's stamps into the ring; how many Voxels were set.
fn place_stamps(store: &mut BrickStore, shot: &Shot) -> usize {
    let bed = shot.cave.map(|c| c.bed);
    let mut written = 0;
    // The daylight crystals: the meadow gets two clusters, so that the dev sees a crystal in the
    // sun as well as in a cave.
    if shot.name == "meadow" {
        let flowers = shot.places.iter().filter(|p| p.stamp == Stamp::Flowers);
        for (place, hue) in flowers.step_by(2).zip([Hue::Cyan, Hue::Magenta]) {
            let crystal = Place {
                stamp: Stamp::Crystal(hue),
                ..*place
            };
            written += stamps::stamp(store, &crystal, None);
        }
    }
    for place in shot.places {
        written += stamps::stamp(store, place, bed);
    }
    written
}

/// An image of the contact sheet.
struct Figure {
    path: String,
    label: String,
}

/// What the sheet says of a shot.
struct Section {
    name: &'static str,
    subject: &'static str,
    /// The Primary still, at the game's settings.
    stills: Vec<Figure>,
    /// The old pick's hero B frame (`--compare`), if the folder has one for the shot.
    old: Option<Figure>,
    /// The Reference combinations, the first of which is the configuration of the frames the owner
    /// liked.
    combinations: Vec<Figure>,
    descent: Vec<Figure>,
    /// The atmosphere's sheet: the Reference and the settled Primary under the gradient, then the
    /// same under the atmosphere.
    versus: Vec<Figure>,
    /// The time-of-day strip (outdoor shots only): for each of [`VERSUS`]'s untinted and tinted
    /// looks, the Reference view at each of [`STRIP_ELEVATIONS`], in that order.
    strip: Vec<Figure>,
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The columns of the combination sheet, left to right (after the shot's name): the old frame if
/// there is one, the nine Reference combinations, the Primary still.
fn columns() -> Vec<String> {
    let mut columns = Vec::new();
    for normals in NORMALS {
        for curve in CURVES {
            let mut label = format!("{} + {}", curve_label(curve), normals.label());
            if normals == Normals::Faces && curve == CURVES[0] {
                label.push_str("<br><b>as the #148 hero B frames</b>");
            }
            columns.push(label);
        }
    }
    columns
}

fn write_sheet(
    out: &Path,
    sections: &[Section],
    options: &Options,
    took: Duration,
) -> Result<(), Box<dyn Error>> {
    let mut html = String::new();
    html.push_str(
        "<!doctype html><meta charset=utf-8><title>look-dev combinations (#202)</title>\
         <style>body{font:14px system-ui;margin:1.5em;background:#16171a;color:#ddd}\
         h1,h2,h3{font-weight:600}p.d{max-width:90ch;color:#aaa}code{color:#9cf}\
         table{border-collapse:collapse}th,td{vertical-align:top;padding:4px 6px;border-top:1px solid #444}\
         th{font-size:12px;font-weight:500;color:#aaa;text-align:left;max-width:320px}\
         td.name{max-width:190px;font-size:12px;color:#aaa;position:sticky;left:0;background:#16171a}\
         td.name b{color:#ddd;font-size:15px}img{display:block;width:320px;height:auto}\
         .small img{width:230px}.descent{display:flex;gap:8px}\
         figure{margin:0}figcaption{font-size:11px;color:#aaa;max-width:320px}</style>",
    );
    let (w, h) = options.size;
    let _ = write!(
        html,
        "<h1>Look-dev combinations (#202), by the renderer's own pipeline</h1>\
         <p class=d>Rendered at {w}&times;{h}: hero light B, the sun {SUN_FROM_FORWARD}&deg; from each shot's forward \
         ({SUN_ELEVATION}&deg; up, radius 0.12), auto-exposure and bloom on. Each row is a shot; the nine Reference images \
         are three tone curves (the prototype's AgX-like, Khronos Neutral, and Neutral with its colour reduced in linear \
         light) against three normal settings (face normals, as the prototype's hero frames; smooth normals with the game's \
         0.25 face bias; smooth normals with none). A Reference image takes {} samples a pixel ({}&times; that in the \
         caves), accumulated once for each normal setting and shown through each curve. The last column is Primary at the \
         game's settings (AgX-like, smooth normals with the bias), one frame, exposure settled at once, with the sun's \
         shadows (two shadow rays a pixel, each towards a point of the sun's disc, #52), so its penumbra is noisy; the one after \
         it is the same view after {} frames of a still camera, as the window shows a view that has stopped, \
         with the upscaler's history having smoothed that noise. Took {:.0} s.{}</p>",
        options.spp,
        CAVE_SPP_FACTOR,
        settle_frames(options),
        took.as_secs_f64(),
        if options.short {
            " <b>A smoke test: the test Planet, which the places are not on.</b>"
        } else {
            ""
        }
    );
    let compare = options.compare.is_some();
    html.push_str("<table><tr><th>shot</th>");
    if compare {
        html.push_str("<th>#148's hero B frame (the prototype, AgX-like, face normals)</th>");
    }
    for column in columns() {
        let _ = write!(html, "<th>{column}</th>");
    }
    html.push_str("<th>Primary, the game's settings, one frame</th><th>Primary, settled</th></tr>");
    let width = 1 + usize::from(compare) + NORMALS.len() * CURVES.len() + 2;
    for section in sections {
        let _ = write!(
            html,
            "<tr><td class=name><b>{}</b><br>{}</td>",
            section.name, section.subject
        );
        let cell = |html: &mut String, figure: &Figure| {
            let _ = write!(
                html,
                "<td><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a></td>",
                figure.path
            );
        };
        // The old frame has a cell where the shot has one, and an empty one where it has not (the
        // prototype's caves were lit by hero A and night, not B).
        if compare {
            match &section.old {
                Some(old) => cell(&mut html, old),
                None => html.push_str("<td>no frame of the old sheet</td>"),
            }
        }
        for figure in &section.combinations {
            cell(&mut html, figure);
        }
        for figure in &section.stills {
            cell(&mut html, figure);
        }
        html.push_str("</tr>");
        if !section.descent.is_empty() {
            let _ = write!(
                html,
                "<tr><td class=name>the descent, at the game's settings, left to right</td>\
                 <td colspan={}><div class=descent>",
                width - 1
            );
            for figure in &section.descent {
                let _ = write!(
                    html,
                    "<figure class=small><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a><figcaption>{1}</figcaption></figure>",
                    figure.path, figure.label
                );
            }
            html.push_str("</div></td></tr>");
        }
    }
    html.push_str("</table>");
    std::fs::write(out.join("index.html"), html)?;
    Ok(())
}

/// Renders the shots, writes their images and the sheet, and returns the sheet's path.
pub fn run(options: &Options) -> Result<PathBuf, Box<dyn Error>> {
    let began = Instant::now();
    std::fs::create_dir_all(&options.out)?;
    let ground_level = if options.short {
        f64::from(TEST_RADIUS)
    } else {
        f64::from(planet_gen::world_map::WORLD_RADIUS)
    };
    let (planet, guard_first, shots): (Planet, usize, Vec<usize>) = if options.short {
        // The shots' places are those of the 320 km Planet. On the test one they are in empty
        // space, which is enough to run every step of the render.
        let descent = SHOTS.iter().position(|s| s.name == "dark descent").unwrap();
        (
            Planet::new(WorldMap::with_radius(
                crate::SEED,
                TEST_MAP_SIZE,
                TEST_RADIUS,
            )),
            crate::loader::GUARD_FIRST + 2,
            vec![0, descent],
        )
    } else {
        eprintln!("making the 320 km world map (about 25 s)...");
        let planet = Planet::new(WorldMap::generate(crate::SEED, WORLD_MAP_SIZE));
        let first = crate::loader::GUARD_FIRST;
        (planet, first, (0..SHOTS.len()).collect())
    };
    // `--shot`: only the shots with those names in them, in a short run too.
    let shots: Vec<usize> = shots
        .into_iter()
        .filter(|&n| {
            options.only.is_empty()
                || options
                    .only
                    .iter()
                    .any(|o| SHOTS[n].name.contains(o.as_str()))
        })
        .collect();
    if shots.is_empty() {
        return Err(format!("no shot has {:?} in its name", options.only).into());
    }
    let (w, h) = options.size;
    let d0 = f64::from(h) / (2.0 * 35f64.to_radians().tan());
    let mut sections = Vec::new();
    for n in shots {
        let shot = &SHOTS[n];
        let t = Instant::now();
        eprintln!("[{n}] {}: building the place", shot.name);
        let site = shot.keys[0].position;
        let mut store = planet.crust_around(site, RING, crate::RING_LEVELS - 1);
        crate::loader::hold_guard(&mut store, &planet, guard_first);
        let written = place_stamps(&mut store, shot);
        let mut scene = crate::planet_scene(&store, site, Bands::with_d0(d0), planet.max_radius())?;
        crate::bench::caves::fill(&mut scene, &mut store, site)?;
        // The shipped Materials and the prototype's eight crystals.
        let mut gpu = Offscreen::with_materials(Resolution::native(w, h), &scene, stamps::table())?;
        gpu.set_flat_ambient(false);
        gpu.set_denoise(render::Denoise::Full);
        eprintln!(
            "[{n}] {}: {written} stamp Voxels, ready in {:.0} s; {}",
            shot.name,
            t.elapsed().as_secs_f64(),
            gpu.gpu_info().name
        );
        let dir = format!("{n:02}-{}", slug(shot.name));
        std::fs::create_dir_all(options.out.join(&dir))?;
        let mut section = Section {
            name: shot.name,
            subject: shot.subject,
            stills: Vec::new(),
            old: None,
            combinations: Vec::new(),
            descent: Vec::new(),
            versus: Vec::new(),
            strip: Vec::new(),
        };
        gpu.set_ground_level(ground_level);
        if options.untinted {
            gpu.set_look(Look::HERO_B_UNTINTED);
        }
        if options.gi {
            let camera = still_camera(shot);
            gpu.set_sun(sun_for(&camera));
            let spp = if shot.cave.is_some() {
                options.spp * CAVE_SPP_FACTOR
            } else {
                options.spp
            };
            gi_versus(&mut gpu, &camera, &dir, spp, options, &mut section)?;
            eprintln!(
                "[{n}] {}: done in {:.0} s",
                shot.name,
                t.elapsed().as_secs_f64()
            );
            sections.push(section);
            continue;
        }
        if options.atmosphere {
            versus(&mut gpu, shot, &dir, options, &mut section)?;
            if STRIP_SHOTS.contains(&shot.name) {
                time_of_day(&mut gpu, shot, &dir, options, &mut section)?;
            }
            eprintln!(
                "[{n}] {}: done in {:.0} s",
                shot.name,
                t.elapsed().as_secs_f64()
            );
            sections.push(section);
            continue;
        }
        // The game's settings first, for the Primary still and the descent; then the combinations.
        primary_still(&mut gpu, shot, &dir, options, &mut section)?;
        if shot.name == "dark descent" {
            descent(&mut gpu, shot, &dir, options, &mut section)?;
        }
        combinations(&mut gpu, shot, &dir, options, &mut section)?;
        eprintln!(
            "[{n}] {}: done in {:.0} s",
            shot.name,
            t.elapsed().as_secs_f64()
        );
        sections.push(section);
    }
    if options.gi {
        sections.push(emissive_room(options)?);
        write_gi_sheet(&options.out, &sections, options, began.elapsed())?;
    } else if options.atmosphere {
        write_atmosphere_sheet(&options.out, &sections, options, began.elapsed())?;
    } else {
        write_sheet(&options.out, &sections, options, began.elapsed())?;
    }
    let sheet = options.out.join("index.html");
    eprintln!(
        "wrote {} ({:.0} s)",
        sheet.display(),
        began.elapsed().as_secs_f64()
    );
    Ok(sheet)
}

/// The GI sheet's three images of a view, in the order of its columns: the Reference, Primary with
/// the flat ambient it had before #54, and Primary with the radiance cache.
const GI_COLUMNS: [&str; 3] = [
    "the Reference (the truth)",
    "Primary before #54: the flat ambient of 0.25",
    "Primary with the radiance cache's GI",
];

/// The view at the game's settings as the Reference has it, then as Primary draws it, settled
/// (the window's view of a camera that has stopped), with the flat ambient and with GI.
fn gi_versus(
    gpu: &mut Offscreen,
    camera: &Camera,
    dir: &str,
    spp: u32,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    gpu.set_stages(Stages::default());
    let reference = gpu.render_reference(camera, spp);
    let mut images = vec![("reference", reference)];
    for (slug, flat) in [("primary-flat", true), ("primary-gi", false)] {
        gpu.set_flat_ambient(flat);
        // The flat baseline is main's Primary of before the denoiser; the GI image is the game's.
        gpu.set_denoise(if flat {
            render::Denoise::Off
        } else {
            render::Denoise::Full
        });
        // A new history, and a cache with nothing in it.
        gpu.reset_accumulation();
        let mut settled = gpu.render_upscaled(camera);
        for _ in 1..settle_frames(options) {
            settled = gpu.render_upscaled(camera);
        }
        images.push((slug, settled));
    }
    gpu.set_flat_ambient(false);
    gpu.set_denoise(render::Denoise::Full);
    for (slug, image) in images {
        let file = format!("{dir}/{slug}.png");
        write_png(&options.out.join(&file), &image)?;
        section.versus.push(Figure {
            path: file,
            label: String::new(),
        });
    }
    Ok(())
}

/// The emissive room of the bench's convergence phase (#54): a closed box of rock lit by one block
/// of warm light, in the three images of the GI sheet.
fn emissive_room(options: &Options) -> Result<Section, Box<dyn Error>> {
    let view = crate::bench::convergence::emissive_room();
    let (w, h) = options.size;
    let mut gpu = Offscreen::with_resolution(Resolution::native(w, h), &view.scene)?;
    gpu.set_flat_ambient(false);
    gpu.set_denoise(render::Denoise::Full);
    let dir = "room";
    std::fs::create_dir_all(options.out.join(dir))?;
    let mut section = Section {
        name: "emissive room",
        subject: "a closed box of rock, lit by one block of warm light on the floor (the bench's room)",
        stills: Vec::new(),
        old: None,
        combinations: Vec::new(),
        descent: Vec::new(),
        versus: Vec::new(),
        strip: Vec::new(),
    };
    gi_versus(
        &mut gpu,
        &view.camera,
        dir,
        options.spp,
        options,
        &mut section,
    )?;
    Ok(section)
}

fn write_gi_sheet(
    out: &Path,
    sections: &[Section],
    options: &Options,
    took: Duration,
) -> Result<(), Box<dyn Error>> {
    let mut html = String::new();
    html.push_str(
        "<!doctype html><meta charset=utf-8><title>look-dev GI (#54)</title>\
         <style>body{font:14px system-ui;margin:1.5em;background:#16171a;color:#ddd}\
         h1{font-weight:600}p.d{max-width:90ch;color:#aaa}\
         table{border-collapse:collapse}th,td{vertical-align:top;padding:4px 6px;border-top:1px solid #444}\
         th{font-size:12px;font-weight:500;color:#aaa;text-align:left;max-width:420px}\
         td.name{max-width:190px;font-size:12px;color:#aaa;position:sticky;left:0;background:#16171a}\
         td.name b{color:#ddd;font-size:15px}img{display:block;width:420px;height:auto}</style>",
    );
    let (w, h) = options.size;
    let _ = write!(
        html,
        "<h1>Look-dev: GI and emissive Voxels (#54)</h1>\
         <p class=d>Rendered at {w}&times;{h} at the game's settings (AgX-like, smooth normals with the face bias, \
         auto-exposure and bloom, hero light B, the tinted atmosphere). Left to right: the Reference ({} samples a \
         pixel, {}&times; that in the caves), Primary as it was before the radiance cache (a flat ambient of 0.25 of \
         the surface's colour), and Primary with the cache's GI, both after {} frames of a still camera as the \
         window shows a view that has stopped. The Reference is the truth: the cache's image is the one to hold to it, \
         with the art direction (hazy, soft, pastel, a time-of-day tint, a warm hero light) where it was. Took {:.0} s.{}</p>",
        options.spp,
        CAVE_SPP_FACTOR,
        settle_frames(options),
        took.as_secs_f64(),
        if options.short {
            " <b>A smoke test: the test Planet, which the places are not on.</b>"
        } else {
            ""
        }
    );
    html.push_str("<table><tr><th>shot</th>");
    for column in GI_COLUMNS {
        let _ = write!(html, "<th>{column}</th>");
    }
    html.push_str("</tr>");
    for section in sections {
        let _ = write!(
            html,
            "<tr><td class=name><b>{}</b><br>{}</td>",
            section.name, section.subject
        );
        for figure in &section.versus {
            let _ = write!(
                html,
                "<td><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a></td>",
                figure.path
            );
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    std::fs::write(out.join("index.html"), html)?;
    Ok(())
}

/// The three looks of the atmosphere's sheet, in the order of its columns.
const VERSUS: [(&str, &str, Look); 3] = [
    (
        "gradient",
        "today's gradient sky and haze (1/300 per m)",
        Look::HERO_B_GRADIENT,
    ),
    (
        "untinted",
        "the atmosphere, untinted (physical)",
        Look::HERO_B_UNTINTED,
    ),
    (
        "tinted",
        "the atmosphere with the hero tint (the game's)",
        Look::HERO_B,
    ),
];

/// The shot at the game's settings (AgX-like, smooth normals with the face bias, auto-exposure and
/// bloom) under each of [`VERSUS`]: the Reference image, then Primary settled (the window's view
/// of a camera that has stopped). The sun, the camera and everything but the look are the same.
fn versus(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    let camera = still_camera(shot);
    gpu.set_sun(sun_for(&camera));
    gpu.set_stages(Stages::default());
    let spp = if shot.cave.is_some() {
        options.spp * CAVE_SPP_FACTOR
    } else {
        options.spp
    };
    for (view, reference) in [("reference", true), ("primary", false)] {
        for (slug, _, look) in VERSUS {
            let t = Instant::now();
            gpu.set_look(look);
            let image = if reference {
                gpu.render_reference(&camera, spp)
            } else {
                gpu.reset_accumulation();
                let mut settled = gpu.render_upscaled(&camera);
                for _ in 1..settle_frames(options) {
                    settled = gpu.render_upscaled(&camera);
                }
                settled
            };
            let file = format!("{dir}/{view}-{slug}.png");
            write_png(&options.out.join(&file), &image)?;
            section.versus.push(Figure {
                path: file,
                label: String::new(),
            });
            eprintln!("    {view}, {slug}: {:.0} s", t.elapsed().as_secs_f64());
        }
    }
    Ok(())
}

/// The shot's Reference view at each of [`STRIP_ELEVATIONS`] under the untinted atmosphere and the
/// tinted one, at the game's settings: only the sun's height and the look change.
fn time_of_day(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    let camera = still_camera(shot);
    gpu.set_stages(Stages::default());
    for (slug, _, look) in &VERSUS[1..] {
        gpu.set_look(*look);
        for elevation in STRIP_ELEVATIONS {
            let t = Instant::now();
            gpu.set_sun(sun_at(&camera, elevation));
            let image = gpu.render_reference(&camera, options.spp);
            let file = format!("{dir}/strip-{elevation:+03}-{slug}.png");
            write_png(&options.out.join(&file), &image)?;
            section.strip.push(Figure {
                path: file,
                label: format!("{elevation:+.0}&deg;"),
            });
            eprintln!(
                "    time of day, {slug}, sun {elevation:+.0} deg: {:.0} s",
                t.elapsed().as_secs_f64()
            );
        }
    }
    Ok(())
}

fn write_atmosphere_sheet(
    out: &Path,
    sections: &[Section],
    options: &Options,
    took: Duration,
) -> Result<(), Box<dyn Error>> {
    let mut html = String::new();
    html.push_str(
        "<!doctype html><meta charset=utf-8><title>look-dev: the atmosphere (#53)</title>\
         <style>body{font:14px system-ui;margin:1.5em;background:#16171a;color:#ddd}\
         h1{font-weight:600}p.d{max-width:90ch;color:#aaa}\
         table{border-collapse:collapse}th,td{vertical-align:top;padding:4px 6px;border-top:1px solid #444}\
         th{font-size:12px;font-weight:500;color:#aaa;text-align:left;max-width:420px}\
         td.name{max-width:190px;font-size:12px;color:#aaa;position:sticky;left:0;background:#16171a}\
         td.name b{color:#ddd;font-size:15px}img{display:block;width:420px;height:auto}</style>",
    );
    let (w, h) = options.size;
    let _ = write!(
        html,
        "<h1>The atmosphere against today's gradient (#53)</h1>\
         <p class=d>Rendered at {w}&times;{h} by the renderer's own pipeline: hero light B, the sun {SUN_FROM_FORWARD}&deg; \
         from each shot's forward ({SUN_ELEVATION}&deg; up), the game's settings (AgX-like, smooth normals with the face \
         bias, auto-exposure and bloom). Each row is a shot. The first three columns are the Reference view ({} samples a \
         pixel, {}&times; in the caves) under today's gradient sky and 1/300 per metre haze, then under the untinted (physical) atmosphere, then \
         under the atmosphere with the hero tint, which is the game's; the \
         last three are Primary, settled after {} frames of a still camera, under each. Only the look changes between a \
         pair: the camera, the sun's direction, the Planet and the settings are the same. Below them, the time of day. \
         Took {:.0} s.{}</p>",
        options.spp,
        CAVE_SPP_FACTOR,
        settle_frames(options),
        took.as_secs_f64(),
        if options.short {
            " <b>A smoke test: the test Planet, which the places are not on.</b>"
        } else {
            ""
        }
    );
    html.push_str("<table><tr><th>shot</th>");
    for view in ["Reference", "Primary, settled"] {
        for (_, label, _) in VERSUS {
            let _ = write!(html, "<th>{view}: {label}</th>");
        }
    }
    html.push_str("</tr>");
    for section in sections {
        let _ = write!(
            html,
            "<tr><td class=name><b>{}</b><br>{}</td>",
            section.name, section.subject
        );
        for figure in &section.versus {
            let _ = write!(
                html,
                "<td><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a></td>",
                figure.path
            );
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    let strips: Vec<&Section> = sections.iter().filter(|s| !s.strip.is_empty()).collect();
    if !strips.is_empty() {
        let _ = write!(
            html,
            "<h1>The tint by the time of day (#53)</h1>\
             <p class=d>The Reference view at the game's settings, the sun {SUN_FROM_FORWARD}&deg; from each \
             shot's forward as above, at six elevations: night, dusk, the horizon, the hero sun ({SUN_ELEVATION}&deg;), the \
             afternoon and midday. For each shot, the untinted (physical) atmosphere over the tinted one, \
             which is the game's: the tint leans violet, mauve and purple when the sun is low or under the \
             horizon, and hazier and more orange when it is up, blending between by the sun's elevation. The \
             display's auto-exposure settles on each image, as it does in the game after a moment, so a dark \
             sky is not as dark here as it is in the first frames of a night.</p>"
        );
        html.push_str("<table><tr><th>shot</th><th>look</th>");
        for elevation in STRIP_ELEVATIONS {
            let _ = write!(html, "<th>sun {elevation:+.0}&deg;</th>");
        }
        html.push_str("</tr>");
        for section in strips {
            for (row, (_, label, _)) in VERSUS[1..].iter().enumerate() {
                let _ = write!(
                    html,
                    "<tr><td class=name><b>{}</b></td><td class=name>{label}</td>",
                    section.name
                );
                for figure in
                    &section.strip[row * STRIP_ELEVATIONS.len()..][..STRIP_ELEVATIONS.len()]
                {
                    let _ = write!(
                        html,
                        "<td><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a></td>",
                        figure.path
                    );
                }
                html.push_str("</tr>");
            }
        }
        html.push_str("</table>");
    }
    std::fs::write(out.join("index.html"), html)?;
    Ok(())
}

/// The camera of a shot's stills: its second keyframe, or its first if it has one frame.
fn still_camera(shot: &Shot) -> Camera {
    let path = shot.path();
    let frames = path.keyframe_frames(shot.frames);
    let still = *frames.get(1).unwrap_or(&frames[0]);
    path.camera_at(still, shot.frames)
}

/// Frames of a still camera that the settled Primary image has: twice the upscaler's 16-frame
/// history, so that the history is full of the sun's samples.
const SETTLE_FRAMES: u32 = 32;

fn settle_frames(options: &Options) -> u32 {
    if options.short { 2 } else { SETTLE_FRAMES }
}

/// The shot's Primary image from its still camera, at the game's settings: one frame, which has
/// the sun's shadows from two samples of the disc a pixel (#52) and goes through the denoiser's
/// filter with no history (#55: `render` has no denoiser, so the frame is drawn as a window draws
/// its first), and the same view after
/// [`SETTLE_FRAMES`] frames of a camera that has not moved, which is what a window shows a view that
/// has stopped.
fn primary_still(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    let camera = still_camera(shot);
    gpu.set_stages(Stages::default());
    gpu.set_sun(sun_for(&camera));
    gpu.reset_accumulation();
    let primary = gpu.render_upscaled(&camera);
    let file = format!("{dir}/primary.png");
    write_png(&options.out.join(&file), &primary)?;
    section.stills.push(Figure {
        path: file,
        label: String::new(),
    });
    // The upscaler's history starts with the first of these frames, whatever the last shot or the
    // single frame left in it, and the camera does not move.
    gpu.reset_accumulation();
    let mut settled = gpu.render_upscaled(&camera);
    for _ in 1..settle_frames(options) {
        settled = gpu.render_upscaled(&camera);
    }
    let file = format!("{dir}/primary-settled.png");
    write_png(&options.out.join(&file), &settled)?;
    section.stills.push(Figure {
        path: file,
        label: String::new(),
    });
    Ok(())
}

/// The shot's Reference image under every combination of normals and tone curve: one
/// accumulation for each normal setting, shown through each curve. The old frame of the shot
/// (`--compare`) goes in first, if the folder has it.
fn combinations(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    if let Some(old) = &options.compare {
        let from = old.join(dir).join("hero-hero-b.png");
        if from.exists() {
            let file = format!("{dir}/old-hero-b.png");
            std::fs::copy(&from, options.out.join(&file))?;
            section.old = Some(Figure {
                path: file,
                label: String::new(),
            });
        } else {
            eprintln!("    no {} in the old sheet", from.display());
        }
    }
    let camera = still_camera(shot);
    gpu.set_sun(sun_for(&camera));
    let spp = if shot.cave.is_some() {
        options.spp * CAVE_SPP_FACTOR
    } else {
        options.spp
    };
    for normals in NORMALS {
        let t = Instant::now();
        gpu.set_stages(normals.stages(CURVES[0]));
        // The accumulation starts over here: the normals are part of what is lit.
        let mut image = gpu.render_reference(&camera, spp);
        for (n, curve) in CURVES.into_iter().enumerate() {
            if n > 0 {
                gpu.set_stages(normals.stages(curve));
                image = gpu
                    .reshow_reference()
                    .expect("the accumulation has its samples");
            }
            let file = format!("{dir}/reference-{}-{}.png", curve.slug(), normals.slug());
            write_png(&options.out.join(&file), &image)?;
            section.combinations.push(Figure {
                path: file,
                label: String::new(),
            });
        }
        eprintln!("    {}: {:.0} s", normals.slug(), t.elapsed().as_secs_f64());
    }
    Ok(())
}

/// The dark descent as a Primary sequence on a 60 Hz clock: every frame of the shot is drawn the
/// way a window draws one (`Offscreen::draw_frame`, which moves the exposure by the time since the
/// last frame), and `options.descent_frames` of them, evenly over the shot, are written.
fn descent(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    let path = shot.path();
    let written = options.descent_frames.max(2);
    let last = shot.frames - 1;
    let at: Vec<u32> = (0..written)
        .map(|k| (f64::from(k) * f64::from(last) / f64::from(written - 1)).round() as u32)
        .collect();
    // The sun of the shot's first frame all the way: the light does not move, the eye adapts.
    gpu.set_sun(sun_for(&path.camera_at(0, shot.frames)));
    // The still before it left Primary's exposure at the still's metering. The strip starts from
    // frame 0's own, as the prototype's curve did: one frame of it, with nothing carried over,
    // sets the exposure at once.
    gpu.render(&path.camera_at(0, shot.frames));
    let t = Instant::now();
    for frame in 0..shot.frames {
        let camera = path.camera_at(frame, shot.frames);
        let now = Duration::from_secs_f64(f64::from(frame) / 60.0);
        gpu.draw_frame(&camera, false, now);
        if let Some(k) = at.iter().position(|&f| f == frame) {
            let file = format!("{dir}/descent-{k:02}.png");
            write_png(&options.out.join(&file), &gpu.shown())?;
            section.descent.push(Figure {
                path: file,
                label: format!("frame {frame} &middot; {:.2} s", f64::from(frame) / 60.0),
            });
        }
    }
    eprintln!(
        "    descent ({} frames): {:.0} s",
        shot.frames,
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_arguments_are_out_size_and_spp_with_the_documented_defaults() {
        let o = parse(args(&["--out", "d"])).unwrap();
        assert_eq!((o.size, o.spp, o.short), ((1280, 720), 64, false));
        assert_eq!(o.compare, None);
        let o = parse(args(&["--out", "d", "--compare", "old"])).unwrap();
        assert_eq!(o.compare, Some(PathBuf::from("old")));
        let o = parse(args(&["--out", "d", "--size", "640x360", "--spp", "16"])).unwrap();
        assert_eq!((o.size, o.spp), ((640, 360), 16));
        let o = parse(args(&["--out", "d", "--short"])).unwrap();
        assert!(o.short && o.size.0 < 100 && o.spp == 1);
        let o = parse(args(&[
            "--out", "d", "--shot", "canyon", "--shot", "forest",
        ]))
        .unwrap();
        assert_eq!(o.only, ["canyon", "forest"]);
        assert!(parse(args(&["--out", "d"])).unwrap().only.is_empty());
        assert!(parse(args(&["--out", "d", "--shot"])).is_err());
        assert!(parse(args(&[])).is_err(), "--out is required");
        assert!(parse(args(&["--out", "d", "--size", "big"])).is_err());
        assert!(parse(args(&["--out", "d", "--spp", "0"])).is_err());
        assert!(parse(args(&["--out", "d", "--nope"])).is_err());
    }

    #[test]
    fn the_sun_is_70_degrees_from_each_shots_forward_on_its_left_and_8_degrees_up() {
        for shot in SHOTS {
            let camera = shot.path().camera_at(0, shot.frames);
            let (up, forward) = (camera.horizon_up, camera.forward());
            let sun = sun_for(&camera);
            let d = sun.direction().map(f64::from);
            // 8 degrees above the horizon of the shot's own frame.
            let elevation = dot(d, up).asin().to_degrees();
            assert!((elevation - 8.0).abs() < 1e-3, "{}: {elevation}", shot.name);
            // 70 degrees from the forward, the horizon's part of it.
            let flat = unit(std::array::from_fn(|a| {
                forward[a] - up[a] * dot(forward, up)
            }));
            let along = dot(
                unit(std::array::from_fn(|a| d[a] - up[a] * dot(d, up))),
                flat,
            );
            assert!(
                (along.acos().to_degrees() - 70.0).abs() < 1e-3,
                "{}",
                shot.name
            );
            // On the side that `cross(up, forward)` points to: the prototype's.
            assert!(dot(d, cross(up, flat)) > 0.0, "{}", shot.name);
            assert!((sun.angular_radius() - 0.12).abs() < 1e-6);
        }
    }

    /// The shot list's places are those of the 320 km Planet, which a test does not build (25 s):
    /// the smoke test draws them on the test Planet, where they are empty space, which is enough
    /// to run every step. What it holds is that every file the sheet names is a PNG of the size
    /// asked for, in a folder for each shot, nine Reference combinations and a Primary for each,
    /// the descent's frames, and the old sheet's frame (`--compare`) where the folder has one.
    #[test]
    fn a_short_run_writes_a_folder_for_each_shot_and_a_sheet_that_names_every_image() {
        let out = std::env::temp_dir().join(format!("lookdev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        // An old sheet that has the forest floor's hero B frame and not the descent's.
        let old = out.join("old");
        std::fs::create_dir_all(old.join("00-forest-floor")).unwrap();
        let frame = render::Image {
            width: 64,
            height: 36,
            rgba: vec![200; 64 * 36 * 4],
        };
        write_png(&old.join("00-forest-floor/hero-hero-b.png"), &frame).unwrap();
        let new = out.join("new");
        let options = parse(args(&[
            "--out",
            new.to_str().unwrap(),
            "--compare",
            old.to_str().unwrap(),
            "--short",
        ]))
        .unwrap();
        let sheet = run(&options).unwrap();
        let html = std::fs::read_to_string(&sheet).unwrap();
        let images: Vec<&str> = html
            .split("src=\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap())
            .collect();
        // Two shots, nine combinations and two Primary stills each, the descent's three frames, and the
        // one old frame.
        assert_eq!(images.len(), 2 * (9 + 2) + 3 + 1, "{images:?}");
        for image in &images {
            let bytes = std::fs::read(new.join(image)).unwrap();
            let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
            let reader = decoder.read_info().unwrap();
            let info = reader.info();
            assert_eq!((info.width, info.height), (64, 36), "{image}");
        }
        // The first combination is AgX-like with face normals, the frames the owner liked.
        let first = format!(
            "00-forest-floor/reference-agx-{}.png",
            Normals::Faces.slug()
        );
        assert!(images.contains(&first.as_str()), "{images:?}");
        assert!(images.contains(&"00-forest-floor/old-hero-b.png"));
        assert!(html.contains("as the #148 hero B frames"));
        assert!(html.contains("no frame of the old sheet"));
        assert!(new.join("02-dark-descent/descent-02.png").exists());
        assert!(new.join("00-forest-floor/primary.png").exists());
        assert!(new.join("00-forest-floor/primary-settled.png").exists());
        // The atmosphere's sheet: each shot in two views under three looks, and the strip.
        let versus = out.join("versus");
        let options = parse(args(&[
            "--out",
            versus.to_str().unwrap(),
            "--short",
            "--atmosphere",
        ]))
        .unwrap();
        assert!(options.atmosphere);
        let html = std::fs::read_to_string(run(&options).unwrap()).unwrap();
        let images: Vec<&str> = html
            .split("src=\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap())
            .collect();
        // Each shot in two views under three looks, and the forest floor's strip: six sun
        // elevations under two looks.
        assert_eq!(images.len(), 3 * 4 + 2 * 6, "{images:?}");
        assert!(images.contains(&"00-forest-floor/strip-+08-tinted.png"));
        assert!(images.contains(&"00-forest-floor/strip--12-untinted.png"));
        assert!(images.contains(&"00-forest-floor/reference-gradient.png"));
        assert!(images.contains(&"00-forest-floor/primary-tinted.png"));
        assert!(images.contains(&"00-forest-floor/reference-untinted.png"));
        for image in &images {
            assert!(versus.join(image).exists(), "{image}");
        }
        // The GI sheet (#54): each shot and the emissive room in three images.
        let gi = out.join("gi");
        let options = parse(args(&["--out", gi.to_str().unwrap(), "--short", "--gi"])).unwrap();
        assert!(options.gi);
        let html = std::fs::read_to_string(run(&options).unwrap()).unwrap();
        let images: Vec<&str> = html
            .split("src=\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap())
            .collect();
        // Two shots and the room, each the Reference, Primary with the flat ambient and with GI.
        assert_eq!(images.len(), 3 * 3, "{images:?}");
        assert!(images.contains(&"00-forest-floor/primary-flat.png"));
        assert!(images.contains(&"00-forest-floor/primary-gi.png"));
        assert!(images.contains(&"room/reference.png"));
        for image in &images {
            assert!(gi.join(image).exists(), "{image}");
        }
        assert!(html.contains("emissive room"));
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn the_combinations_are_three_curves_against_three_normal_settings() {
        assert_eq!(columns().len(), 9);
        let game = Normals::Biased.stages(ToneCurve::Agx);
        assert_eq!(game, Stages::default());
        let faces = Normals::Faces.stages(ToneCurve::Agx);
        assert!(!faces.smooth_normals && faces.curve == ToneCurve::Agx);
        let smooth = Normals::Smooth.stages(ToneCurve::Agx);
        assert!(smooth.smooth_normals && smooth.normal_bias == NormalBias::None);
    }
}
