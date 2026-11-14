//! PROTOTYPE (#148): `client lookdev --out DIR`. Renders every shot of the look-dev shot list under
//! a grid of looks and writes the PNGs and an `index.html` contact sheet that labels each one.
//! Thrown away once the dev has picked the numbers.
//!
//! The renderer has no tone mapping, auto-exposure, bloom or smooth normals, and a fixed sky and
//! sun. So the prototype adds the lighting that the reference path tracer needs (look uniforms:
//! the sun's colour, the sky's two colours, haze, local up) and does the rest on the CPU, on the
//! tracer's linear HDR image (`post`), or on the GPU's rays (`shading`).

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use brick_store::{BrickStore, VOXELS_PER_METRE};
use planet_gen::planet::Planet;
use planet_gen::world_map::{WORLD_MAP_SIZE, WorldMap};
use render::lod::Bands;
use render::tracer::Look;
use render::{Camera, Image, Offscreen, Sun};

use super::post::{Exposure, Hdr, Tone, exposure_curve, settled_exposure};
use super::stamps;
use super::{Hue, Place, SHOTS, Shot, Stamp, shading};
use crate::bench::write_png;

const RING: i32 = (30.0 * VOXELS_PER_METRE) as i32;

struct Options {
    out: PathBuf,
    size: (u32, u32),
    spp: u32,
    descent_frames: u32,
    only: Option<Vec<usize>>,
}

const USAGE: &str = "usage: client lookdev --out DIR [--size WxH] [--spp N] [--descent-frames N] \
[--shots 0,3,...]\n  the 4090: client.exe lookdev --out DIR            (1280x720, 128 spp)\n  \
lavapipe: client lookdev --out DIR --size 640x360 --spp 24";

fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut o = Options {
        out: PathBuf::new(),
        size: (1280, 720),
        spp: 128,
        descent_frames: 12,
        only: None,
    };
    let mut args = args;
    let mut have_out = false;
    while let Some(a) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{a} needs {what}\n{USAGE}"));
        match a.as_str() {
            "--out" => {
                o.out = value("a directory")?.into();
                have_out = true;
            }
            "--size" => {
                let v = value("WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size is WxH")?;
                o.size = (
                    w.parse().map_err(|_| "--size is WxH")?,
                    h.parse().map_err(|_| "--size is WxH")?,
                );
            }
            "--spp" => o.spp = value("a count")?.parse().map_err(|_| "--spp is a count")?,
            "--descent-frames" => {
                o.descent_frames = value("a count")?
                    .parse()
                    .map_err(|_| "--descent-frames is a count")?;
            }
            "--shots" => {
                o.only = Some(
                    value("a list")?
                        .split(',')
                        .map(|s| s.parse().map_err(|_| "--shots is a list of numbers"))
                        .collect::<Result<_, _>>()?,
                );
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if !have_out {
        return Err(format!("--out is required\n{USAGE}"));
    }
    Ok(o)
}

pub fn main(args: impl Iterator<Item = String>) -> ExitCode {
    match parse(args)
        .map_err(Box::<dyn Error>::from)
        .and_then(|o| run(&o))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lookdev: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The looks.

/// One lighting set-up of the path tracer.
#[derive(Clone, Copy)]
struct Lighting {
    slug: &'static str,
    label: &'static str,
    /// Degrees above the horizon.
    elevation: f32,
    sun: [f32; 3],
    zenith: [f32; 3],
    horizon: [f32; 3],
    /// Haze per metre: the horizon colour in front of what is far.
    haze: f32,
    /// The sun's angular radius, radians.
    radius: f32,
}

const TODAY: Lighting = Lighting {
    slug: "today",
    label: "today's light: neutral high sun, today's sky, no haze",
    elevation: 55.0,
    sun: [1.6, 1.5, 1.35],
    zenith: [0.25, 0.45, 0.85],
    horizon: [0.75, 0.82, 0.92],
    haze: 0.0,
    radius: 0.04,
};

/// #135 item 4: a low sun, a deep blue zenith fading to a peach horizon, warm haze.
const HERO_A: Lighting = Lighting {
    slug: "hero-a",
    label: "hero light A, late afternoon: sun 18°, deep blue zenith, peach horizon, haze 1/400 m",
    elevation: 18.0,
    sun: [1.5, 1.05, 0.60],
    zenith: [0.10, 0.20, 0.52],
    horizon: [1.0, 0.68, 0.50],
    haze: 1.0 / 400.0,
    radius: 0.04,
};

const HERO_B: Lighting = Lighting {
    slug: "hero-b",
    label: "hero light B, golden: sun 8°, redder sun, darker zenith, orange horizon, haze 1/300 m",
    elevation: 8.0,
    sun: [1.3, 0.80, 0.40],
    zenith: [0.12, 0.18, 0.45],
    horizon: [1.0, 0.55, 0.38],
    haze: 1.0 / 300.0,
    radius: 0.04,
};

/// The night toggle: no sun, a dark sky; only emissives and the sky's faint light.
const NIGHT: Lighting = Lighting {
    slug: "night",
    label: "night toggle: no sun, a dark sky",
    elevation: 18.0,
    sun: [0.0; 3],
    zenith: [0.004, 0.007, 0.020],
    horizon: [0.012, 0.016, 0.030],
    haze: 0.0,
    radius: 0.04,
};

fn with_radius(base: Lighting, slug: &'static str, label: &'static str, radius: f32) -> Lighting {
    Lighting {
        slug,
        label,
        radius,
        ..base
    }
}

fn sun_for(light: &Lighting, camera: &Camera, up: [f64; 3]) -> Sun {
    // Across the view, a little towards it: long shadows that show relief.
    let f = camera.forward();
    let d = f[0] * up[0] + f[1] * up[1] + f[2] * up[2];
    let flat = norm(std::array::from_fn(|a| f[a] - up[a] * d));
    let side = norm(cross(up, flat));
    let (az, el) = (70f64.to_radians(), f64::from(light.elevation).to_radians());
    let dir: [f32; 3] = std::array::from_fn(|a| {
        (up[a] * el.sin() + (flat[a] * az.cos() + side[a] * az.sin()) * el.cos()) as f32
    });
    Sun::new(dir, light.radius).expect("a direction")
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = a.iter().map(|c| c * c).sum::<f64>().sqrt();
    a.map(|c| c / l)
}

fn look_for(light: &Lighting, up: [f64; 3]) -> Look {
    Look {
        up: up.map(|c| c as f32),
        sun: light.sun,
        zenith: light.zenith,
        horizon: light.horizon,
        // Per Voxel.
        haze: light.haze / VOXELS_PER_METRE as f32,
    }
}

// ---------------------------------------------------------------------------------------------
// The sheet.

#[derive(Default)]
struct Sheet {
    /// (section, shot, label, path relative to the sheet)
    entries: Vec<(&'static str, String, String, String)>,
    notes: Vec<String>,
}

impl Sheet {
    fn add(&mut self, section: &'static str, shot: &str, label: String, path: String) {
        self.entries.push((section, shot.to_string(), label, path));
    }
}

const SECTIONS: [(&str, &str, &str); 7] = [
    (
        "tone",
        "1. Tone curve and exposure: does a crystal read pastel by day and neon in a cave?",
        "Rows: tone curve. Columns: exposure fixed at 1 (today's) and auto (settled: the scene's mean \
         luminance to middle grey, within -3..+5 stops). Under hero light A, and for the caves also \
         at night, where only the crystals give light.",
    ),
    (
        "adapt",
        "1b. Auto-exposure adaptation rate: the dark descent",
        "Frames of the descent, left to right. Rows: today's (no curve, fixed), the filmic curve with \
         fixed exposure, and the filmic curve with the exposure adapting fast and slowly.",
    ),
    (
        "shading",
        "2. Shading A/B: face normals against smooth computed normals",
        "Left, the face's normal (today); right, normals computed from the 5×5×5 occupancy round the \
         hit. Lit identically (sun with shadow, sky term, no tone curve). Below each pair, the lower middle \
         of the frame enlarged 3×, nearest-neighbour: 5 cm Voxels.",
    ),
    (
        "sun",
        "3. The sun's angular radius, under hero light A (path-traced: the primary pass has no shadows)",
        "0.01 rad, today's 0.04 rad, and 0.12 rad. Look at the penumbra of every shadow edge.",
    ),
    (
        "hero",
        "4a. Hero light palettes and the night toggle",
        "Today's light, hero light A, hero light B, and night, each with the AgX-like curve and auto exposure.",
    ),
    (
        "bloom",
        "4b. Bloom on emissive Voxels: off and subtle",
        "AgX-like curve, auto exposure; bloom is the image blurred at three scales, 0.05 of it added.",
    ),
    (
        "audio",
        "5. The listening pass",
        "Not run: no audio exists yet, so the adoption gate for the ray-traced audio layer (#153) is still open.",
    ),
];

fn write_sheet(
    out: &Path,
    sheet: &Sheet,
    options: &Options,
    took: f64,
) -> Result<(), Box<dyn Error>> {
    let mut html = String::new();
    html.push_str(
        "<!doctype html><meta charset=utf-8><title>look-dev (#148)</title>\
         <style>body{font:14px system-ui;margin:1.5em;background:#16171a;color:#ddd}\
         h1,h2,h3{font-weight:600}h2{margin-top:2.2em;border-top:1px solid #444;padding-top:.6em}\
         figure{margin:0;display:inline-block;vertical-align:top;margin:0 8px 14px 0}\
         figcaption{max-width:480px;font-size:12px;color:#aaa}img{display:block;max-width:480px;height:auto}\
         .small img{max-width:230px}.small figcaption{max-width:230px}p.d{max-width:80ch;color:#aaa}\
         code{color:#9cf}</style>",
    );
    let _ = write!(
        html,
        "<h1>Look-dev contact sheet (#148)</h1><p class=d>PROTOTYPE, not to be merged. Rendered at {}×{}, {} samples a pixel \
         by the reference path tracer, in {took:.0} s. Every image is labelled with what varies. Compare within a row.</p>",
        options.size.0, options.size.1, options.spp
    );
    for note in &sheet.notes {
        let _ = write!(html, "<p class=d>{note}</p>");
    }
    for (key, title, text) in SECTIONS {
        let _ = write!(html, "<h2>{title}</h2><p class=d>{text}</p>");
        let mut shot = String::new();
        for (section, name, label, path) in &sheet.entries {
            if *section != key {
                continue;
            }
            if *name != shot {
                shot.clone_from(name);
                let _ = write!(html, "<h3 style=\"clear:both\">{name}</h3>");
            }
            let class = if key == "adapt" { " class=small" } else { "" };
            let _ = write!(
                html,
                "<figure{class}><a href=\"{path}\"><img src=\"{path}\" loading=lazy></a><figcaption>{label}</figcaption></figure>"
            );
        }
    }
    std::fs::write(out.join("index.html"), html)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The run.

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn run(o: &Options) -> Result<(), Box<dyn Error>> {
    let began = Instant::now();
    std::fs::create_dir_all(&o.out)?;
    eprintln!("making the 320 km world map (about 25 s)...");
    let planet = Planet::new(WorldMap::generate(crate::SEED, WORLD_MAP_SIZE));
    let mut sheet = Sheet::default();
    sheet.notes.push(
        "What today's renderer cannot show, and this sheet adds for the look: tone mapping, auto-exposure and \
         bloom (done here on the HDR image), smooth normals (done here on the CPU), a sky and a sun that follow the \
         local up (today's follow the world's +y), the hero light's palette, haze, and the crystals and vegetation \
         (hand-placed stamps). The sun's radius shows only in the path-traced image."
            .into(),
    );
    let (w, h) = o.size;
    let d0 = f64::from(h) / (2.0 * 35f64.to_radians().tan());
    for (n, shot) in SHOTS.iter().enumerate() {
        if o.only.as_ref().is_some_and(|only| !only.contains(&n)) {
            continue;
        }
        let t = Instant::now();
        eprintln!("[{n}] {}: building the place", shot.name);
        let site = shot.keys[0].position;
        let mut store = planet.crust_around(site, RING, crate::RING_LEVELS - 1);
        crate::loader::hold_guard(&mut store, &planet, crate::loader::guard_first(&planet));
        let written = place_stamps(&mut store, shot, n);
        let bands = Bands::with_d0(d0);
        let mut scene = crate::planet_scene(&store, site, bands, planet.max_radius())?;
        crate::bench::caves::fill(&mut scene, &mut store, site)?;
        let mut gpu = Offscreen::new(w, h, &scene)?;
        eprintln!(
            "[{n}] {}: {written} stamp Voxels, scene ready in {:.0} s; {}",
            shot.name,
            t.elapsed().as_secs_f64(),
            gpu.gpu_info().name
        );
        let dir = format!("{n:02}-{}", slug(shot.name));
        std::fs::create_dir_all(o.out.join(&dir))?;
        shot_stills(&mut gpu, &store, shot, &dir, o, &mut sheet)?;
        if shot.name == "dark descent" {
            descent(&mut gpu, shot, &dir, o, &mut sheet)?;
        }
        eprintln!(
            "[{n}] {}: done in {:.0} s",
            shot.name,
            t.elapsed().as_secs_f64()
        );
    }
    write_sheet(&o.out, &sheet, o, began.elapsed().as_secs_f64())?;
    eprintln!(
        "wrote {} ({:.0} s)",
        o.out.join("index.html").display(),
        began.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Writes the shot's stamps into the ring; how many Voxels were set.
fn place_stamps(store: &mut BrickStore, shot: &Shot, n: usize) -> usize {
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
    let _ = n;
    for place in shot.places {
        written += stamps::stamp(store, place, bed);
    }
    written
}

fn hdr(gpu: &mut Offscreen, camera: &Camera, spp: u32) -> Hdr {
    let (width, height, pixels) = gpu.render_reference_hdr(camera, spp);
    Hdr {
        width,
        height,
        rgb: pixels.iter().map(|p| [p[0], p[1], p[2]]).collect(),
    }
}

fn save(
    sheet: &mut Sheet,
    o: &Options,
    dir: &str,
    section: &'static str,
    shot: &Shot,
    file: &str,
    label: String,
    image: &Image,
) -> Result<(), Box<dyn Error>> {
    let rel = format!("{dir}/{file}.png");
    write_png(&o.out.join(&rel), image)?;
    sheet.add(section, shot.name, label, rel);
    Ok(())
}

fn shot_stills(
    gpu: &mut Offscreen,
    store: &BrickStore,
    shot: &Shot,
    dir: &str,
    o: &Options,
    sheet: &mut Sheet,
) -> Result<(), Box<dyn Error>> {
    let path = shot.path();
    // The still of the shot: its first keyframe; the last too for the pull-out, whose second half is
    // the Planet.
    let frames = path.keyframe_frames(shot.frames);
    let still = *frames.get(1).unwrap_or(&frames[0]);
    let camera = path.camera_at(still, shot.frames);
    let site = shot.keys[0].position;
    let up = norm(site);
    let cave = shot.cave.is_some();

    let mut lights: Vec<Lighting> = if cave {
        vec![HERO_A, NIGHT]
    } else {
        vec![
            TODAY,
            HERO_A,
            HERO_B,
            with_radius(HERO_A, "sun-small", "sun 0.01 rad", 0.01),
            with_radius(HERO_A, "sun-large", "sun 0.12 rad", 0.12),
            NIGHT,
        ]
    };
    // Hero A at 0.04 stands for itself in the sun row, too.
    lights.dedup_by_key(|l| l.slug);
    let mut hdrs: Vec<(Lighting, Hdr)> = Vec::new();
    for light in &lights {
        let t = Instant::now();
        gpu.set_look(look_for(light, up));
        gpu.set_sun(sun_for(light, &camera, up));
        hdrs.push((*light, hdr(gpu, &camera, o.spp)));
        eprintln!("    {}: {:.0} s", light.slug, t.elapsed().as_secs_f64());
    }
    let find = |slug: &str| &hdrs.iter().find(|(l, _)| l.slug == slug).unwrap().1;
    let auto = |h: &Hdr| settled_exposure(h.log_luminance());

    // 1. tone x exposure, under hero A, and at night for the caves.
    let mut grids = vec![HERO_A];
    if cave {
        grids.push(NIGHT);
    }
    for light in grids {
        let h = find(light.slug);
        for tone in Tone::ALL {
            for (exposure, name) in [(1.0, "fixed 1"), (auto(h), "auto")] {
                let label = format!(
                    "{} · {} · {} (×{exposure:.2}) · {}",
                    shot.name,
                    light.slug,
                    tone.name(),
                    name
                );
                let image = h.develop(tone, exposure, 0.0);
                save(
                    sheet,
                    o,
                    dir,
                    "tone",
                    shot,
                    &format!(
                        "tone-{}-{}-{}",
                        light.slug,
                        tone.slug(),
                        name.replace(' ', "")
                    ),
                    label,
                    &image,
                )?;
            }
        }
    }

    // 4a. the palettes, 4b. bloom, 3. sun radius.
    for (light, h) in &hdrs {
        let e = auto(h);
        let image = h.develop(Tone::AgX, e, 0.0);
        save(
            sheet,
            o,
            dir,
            "hero",
            shot,
            &format!("hero-{}", light.slug),
            format!("{} · AgX-like · auto (×{e:.2})", light.label),
            &image,
        )?;
    }
    for light in [HERO_A, NIGHT] {
        let h = find(light.slug);
        let e = auto(h);
        for (strength, name) in [(0.0, "bloom off"), (0.05, "bloom subtle (0.05)")] {
            let image = h.develop(Tone::AgX, e, strength);
            save(
                sheet,
                o,
                dir,
                "bloom",
                shot,
                &format!("bloom-{}-{}", light.slug, name.split(' ').nth(1).unwrap()),
                format!(
                    "{} · {} · AgX-like · auto (×{e:.2}) · {name}",
                    shot.name, light.slug
                ),
                &image,
            )?;
        }
    }
    if !cave {
        for (slug, radius) in [
            ("sun-small", "0.01"),
            ("hero-a", "0.04 (today's)"),
            ("sun-large", "0.12"),
        ] {
            let h = find(slug);
            let e = auto(h);
            let image = h.develop(Tone::AgX, e, 0.0);
            save(
                sheet,
                o,
                dir,
                "sun",
                shot,
                &format!("sun-{slug}"),
                format!(
                    "{} · sun radius {radius} rad · hero A · AgX-like · auto",
                    shot.name
                ),
                &image,
            )?;
        }
    }

    // 2. shading A/B, on the natural ground and rock: not in the caves (nothing there is lit by the
    // sun) and not the pull-out, which is not at 5 cm.
    if !cave && shot.name != "pull-out to the Planet" {
        let size = o.size;
        let sun = sun_for(&HERO_A, &camera, up);
        let t = Instant::now();
        let (face, smooth) = shading::ab(gpu, store, &camera, size, &sun, up.map(|c| c as f32));
        eprintln!("    shading A/B: {:.0} s", t.elapsed().as_secs_f64());
        let name = shot.name;
        save(
            sheet,
            o,
            dir,
            "shading",
            shot,
            "shading-face",
            format!("{name} · face normals (today)"),
            &face,
        )?;
        save(
            sheet,
            o,
            dir,
            "shading",
            shot,
            "shading-smooth",
            format!("{name} · smooth computed normals"),
            &smooth,
        )?;
        save(
            sheet,
            o,
            dir,
            "shading",
            shot,
            "shading-face-zoom",
            format!("{name} · face normals · lower middle enlarged 3×"),
            &zoom(&face),
        )?;
        save(
            sheet,
            o,
            dir,
            "shading",
            shot,
            "shading-smooth-zoom",
            format!("{name} · smooth normals · lower middle enlarged 3×"),
            &zoom(&smooth),
        )?;
    }
    Ok(())
}

/// The lower middle third of `image`, enlarged 3× without smoothing.
fn zoom(image: &Image) -> Image {
    let (w, h) = (image.width as usize, image.height as usize);
    let (cw, ch) = (w / 3, h / 3);
    let (x0, y0) = (w / 3, h * 11 / 20);
    let mut rgba = Vec::with_capacity(cw * ch * 9 * 4);
    for y in 0..ch * 3 {
        for x in 0..cw * 3 {
            let at = ((y0 + y / 3) * w + x0 + x / 3) * 4;
            rgba.extend_from_slice(&image.rgba[at..at + 4]);
        }
    }
    Image {
        width: (cw * 3) as u32,
        height: (ch * 3) as u32,
        rgba,
    }
}

/// The dark descent as a short frame sequence: the filmic curve under fixed exposure, and with
/// the exposure adapting at two rates.
fn descent(
    gpu: &mut Offscreen,
    shot: &Shot,
    dir: &str,
    o: &Options,
    sheet: &mut Sheet,
) -> Result<(), Box<dyn Error>> {
    let path = shot.path();
    let n = o.descent_frames.max(2);
    let up = norm(shot.keys[0].position);
    let frames: Vec<u32> = (0..n)
        .map(|k| (f64::from(k) * f64::from(shot.frames - 1) / f64::from(n - 1)).round() as u32)
        .collect();
    gpu.set_look(look_for(&HERO_A, up));
    let mut images = Vec::new();
    for &frame in &frames {
        let camera = path.camera_at(frame, shot.frames);
        gpu.set_sun(sun_for(&HERO_A, &path.camera_at(0, shot.frames), up));
        images.push(hdr(gpu, &camera, (o.spp / 2).max(4)));
        eprintln!("    descent frame {frame}");
    }
    let targets: Vec<f32> = images
        .iter()
        .map(|h| settled_exposure(h.log_luminance()))
        .collect();
    let dt = (frames[1] - frames[0]) as f32 / 60.0;
    let fast = exposure_curve(Exposure::Adapting { tau: 0.5 }, &targets, dt);
    let slow = exposure_curve(Exposure::Adapting { tau: 2.5 }, &targets, dt);
    let fixed = exposure_curve(Exposure::Fixed(1.0), &targets, dt);
    let rows: [(&str, Tone, &Vec<f32>, &str); 4] = [
        ("today", Tone::None, &fixed, "no curve, fixed (today)"),
        ("fixed", Tone::AgX, &fixed, "AgX-like, exposure fixed at 1"),
        ("fast", Tone::AgX, &fast, "AgX-like, auto-exposure τ 0.5 s"),
        ("slow", Tone::AgX, &slow, "AgX-like, auto-exposure τ 2.5 s"),
    ];
    for (name, tone, curve, label) in rows {
        for (k, h) in images.iter().enumerate() {
            let image = h.develop(tone, curve[k], 0.0);
            save(
                sheet,
                o,
                dir,
                "adapt",
                shot,
                &format!("descent-{name}-{k:02}"),
                format!(
                    "{label} · frame {} · ×{:.2} (settled ×{:.2})",
                    frames[k], curve[k], targets[k]
                ),
                &image,
            )?;
        }
    }
    Ok(())
}
