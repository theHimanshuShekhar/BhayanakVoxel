//! `client lookdev --out DIR`: renders the eight look-dev shots (#148) with the real pipeline, as
//! the game draws them: the default Look (hero light B). For each shot, one Primary image at the
//! game's settings, a **combination sheet** of Reference images (the tone curves against the
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
use render::{Camera, NormalBias, Offscreen, Resolution, Stages, Sun, ToneCurve};

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

/// The caves are lit by their crystals alone, which a path finds only by chance (the Reference
/// view has no light sampling for emissives): four times the samples.
const CAVE_SPP_FACTOR: u32 = 4;

const USAGE: &str = "usage: client lookdev --out DIR [--size WxH] [--spp N] [--compare DIR]\n  \
the 4090: client.exe lookdev --out DIR [--compare OLD_SHEET_DIR]   (1280x720, 64 spp)\n  \
--compare: the folder of the #148 sheet; each shot's OLD/NN-shot/hero-hero-b.png is shown first\n  \
a smoke test on any device: client lookdev --out DIR --short";

/// The smooth-normal settings of the combination sheet, in the order of its columns' groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Normals {
    /// Smoothing off: every hit has its face's normal, as the prototype's hero frames did.
    Faces,
    /// Smooth normals with a quarter of the face's normal added: what the game has.
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
    let up = camera.horizon_up;
    let forward = camera.forward();
    let tilt = dot(forward, up);
    let flat = unit(std::array::from_fn(|a| forward[a] - up[a] * tilt));
    let side = cross(up, flat);
    let (az, el) = (SUN_FROM_FORWARD.to_radians(), SUN_ELEVATION.to_radians());
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
         game's settings (Neutral, smooth normals with the bias), one frame, exposure settled at once; Primary has no sun \
         shadows yet (#52). Took {:.0} s.{}</p>",
        options.spp,
        CAVE_SPP_FACTOR,
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
    html.push_str("<th>Primary, the game's settings</th></tr>");
    let width = 1 + usize::from(compare) + NORMALS.len() * CURVES.len() + 1;
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
        };
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
    write_sheet(&options.out, &sections, options, began.elapsed())?;
    let sheet = options.out.join("index.html");
    eprintln!(
        "wrote {} ({:.0} s)",
        sheet.display(),
        began.elapsed().as_secs_f64()
    );
    Ok(sheet)
}

/// The camera of a shot's stills: its second keyframe, or its first if it has one frame.
fn still_camera(shot: &Shot) -> Camera {
    let path = shot.path();
    let frames = path.keyframe_frames(shot.frames);
    let still = *frames.get(1).unwrap_or(&frames[0]);
    path.camera_at(still, shot.frames)
}

/// The shot's Primary image from its still camera, at the game's settings.
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
    let primary = gpu.render(&camera);
    let file = format!("{dir}/primary.png");
    write_png(&options.out.join(&file), &primary)?;
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
                image = gpu.reshow_reference();
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
        // Two shots, nine combinations and a Primary each, the descent's three frames, and the
        // one old frame.
        assert_eq!(images.len(), 2 * (9 + 1) + 3 + 1, "{images:?}");
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
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn the_combinations_are_three_curves_against_three_normal_settings() {
        assert_eq!(columns().len(), 9);
        let game = Normals::Biased.stages(ToneCurve::Neutral);
        assert_eq!(game, Stages::default());
        let faces = Normals::Faces.stages(ToneCurve::Agx);
        assert!(!faces.smooth_normals && faces.curve == ToneCurve::Agx);
        let smooth = Normals::Smooth.stages(ToneCurve::Agx);
        assert!(smooth.smooth_normals && smooth.normal_bias == NormalBias::None);
    }
}
