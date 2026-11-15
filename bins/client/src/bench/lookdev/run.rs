//! `client lookdev --out DIR`: renders the eight look-dev shots (#148) with the real pipeline, as
//! the game draws them: the default Look (hero light B) and every stage of the art direction on
//! (#202). For each shot, a Reference image and a Primary image, and for the dark descent a
//! Primary frame sequence on a 60 Hz clock, so that the exposure's adaptation (tau 2.5 s) shows.
//! Written as PNGs, a folder for each shot, and an `index.html` contact sheet.
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
use render::{Camera, Offscreen, Resolution, Sun};

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

const USAGE: &str = "usage: client lookdev --out DIR [--size WxH] [--spp N]\n  \
the 4090: client.exe lookdev --out DIR           (1280x720, 128 spp)\n  \
a smoke test on any device: client lookdev --out DIR --short";

/// What to render, and where to put it.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub out: PathBuf,
    pub size: (u32, u32),
    /// Samples a pixel of a Reference image (four times that in the caves).
    pub spp: u32,
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
        spp: 128,
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
    stills: Vec<Figure>,
    descent: Vec<Figure>,
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn write_sheet(
    out: &Path,
    sections: &[Section],
    options: &Options,
    took: Duration,
) -> Result<(), Box<dyn Error>> {
    let mut html = String::new();
    html.push_str(
        "<!doctype html><meta charset=utf-8><title>look-dev (#202)</title>\
         <style>body{font:14px system-ui;margin:1.5em;background:#16171a;color:#ddd}\
         h1,h2,h3{font-weight:600}h2{margin-top:2.2em;border-top:1px solid #444;padding-top:.6em}\
         figure{margin:0 8px 14px 0;display:inline-block;vertical-align:top}\
         figcaption{max-width:480px;font-size:12px;color:#aaa}img{display:block;max-width:480px;height:auto}\
         .small img{max-width:230px}.small figcaption{max-width:230px}p.d{max-width:80ch;color:#aaa}\
         code{color:#9cf}</style>",
    );
    let (w, h) = options.size;
    let _ = write!(
        html,
        "<h1>Look-dev contact sheet (#148), by the renderer's own pipeline (#202)</h1>\
         <p class=d>Rendered at {w}&times;{h} by the game's pipeline: hero light B, the sun {SUN_FROM_FORWARD}&deg; from each shot's forward \
         ({SUN_ELEVATION}&deg; up, radius 0.12), every stage of the art direction on (tone curve, auto-exposure, \
         bloom, smooth normals). The Reference image takes {} samples a pixel ({}&times; that in the caves); the Primary image is one \
         frame of primary visibility with its exposure settled at once. The dark descent's frames are a Primary sequence on \
         a 60 Hz clock, so the auto-exposure adapts as it does in the game (tau 2.5 s). Took {:.0} s.{}</p>",
        options.spp,
        CAVE_SPP_FACTOR,
        took.as_secs_f64(),
        if options.short {
            " <b>A smoke test: the test Planet, which the places are not on.</b>"
        } else {
            ""
        }
    );
    for section in sections {
        let _ = write!(
            html,
            "<h2>{}</h2><p class=d>{}</p>",
            section.name, section.subject
        );
        for figure in &section.stills {
            let _ = write!(
                html,
                "<figure><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a><figcaption>{1}</figcaption></figure>",
                figure.path, figure.label
            );
        }
        if !section.descent.is_empty() {
            html.push_str("<h3 style=\"clear:both\">The descent, left to right</h3>");
        }
        for figure in &section.descent {
            let _ = write!(
                html,
                "<figure class=small><a href=\"{0}\"><img src=\"{0}\" loading=lazy></a><figcaption>{1}</figcaption></figure>",
                figure.path, figure.label
            );
        }
    }
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
            descent: Vec::new(),
        };
        stills(&mut gpu, shot, &dir, options, &mut section)?;
        if shot.name == "dark descent" {
            descent(&mut gpu, shot, &dir, options, &mut section)?;
        }
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

/// The shot's Reference and Primary images from its still camera: the camera at its second
/// keyframe, or its first if it has one frame.
fn stills(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    options: &Options,
    section: &mut Section,
) -> Result<(), Box<dyn Error>> {
    let path = shot.path();
    let frames = path.keyframe_frames(shot.frames);
    let still = *frames.get(1).unwrap_or(&frames[0]);
    let camera = path.camera_at(still, shot.frames);
    gpu.set_sun(sun_for(&camera));
    let spp = if shot.cave.is_some() {
        options.spp * CAVE_SPP_FACTOR
    } else {
        options.spp
    };

    let t = Instant::now();
    let primary = gpu.render(&camera);
    let file = format!("{dir}/primary.png");
    write_png(&options.out.join(&file), &primary)?;
    section.stills.push(Figure {
        path: file,
        label: format!(
            "{} &middot; Primary, one frame, exposure settled at once",
            shot.name
        ),
    });
    let reference = gpu.render_reference(&camera, spp);
    let file = format!("{dir}/reference.png");
    write_png(&options.out.join(&file), &reference)?;
    section.stills.push(Figure {
        path: file,
        label: format!("{} &middot; Reference, {spp} samples a pixel", shot.name),
    });
    eprintln!("    stills: {:.0} s", t.elapsed().as_secs_f64());
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
        assert_eq!((o.size, o.spp, o.short), ((1280, 720), 128, false));
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
    /// asked for, in a folder for each shot, with the descent's frames.
    #[test]
    fn a_short_run_writes_a_folder_for_each_shot_and_a_sheet_that_names_every_image() {
        let out = std::env::temp_dir().join(format!("lookdev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let options = parse(args(&["--out", out.to_str().unwrap(), "--short"])).unwrap();
        let sheet = run(&options).unwrap();
        let html = std::fs::read_to_string(&sheet).unwrap();
        let images: Vec<&str> = html
            .split("src=\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap())
            .collect();
        // Two shots, a Primary and a Reference each, and the descent's three frames.
        assert_eq!(images.len(), 2 * 2 + 3, "{images:?}");
        for image in images {
            let bytes = std::fs::read(out.join(image)).unwrap();
            let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
            let reader = decoder.read_info().unwrap();
            let info = reader.info();
            assert_eq!((info.width, info.height), (64, 36), "{image}");
        }
        assert!(out.join("00-forest-floor/reference.png").exists());
        assert!(out.join("02-dark-descent/descent-02.png").exists());
        let _ = std::fs::remove_dir_all(&out);
    }
}
