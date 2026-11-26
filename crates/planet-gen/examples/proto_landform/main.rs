//! THROWAWAY PROTOTYPE (#246, stage 1): three ways of building the land, world-map renders only.
//!
//!   cargo run --release -p planet-gen --example proto_landform
//!   cargo run --release -p planet-gen --example proto_landform -- --res 128 --only A   (quick look)
//!
//! A: noise + splines   B: tectonic plates   C: plates for continents and range lines, splines within.
//! Same seed, same climate, same stream-power erosion for all three. Outputs: target/landform-proto/.

mod erosion;
mod grid;
mod img;
mod landform;
mod model;
mod noise;
mod render;

use grid::*;
use img::*;
use landform::*;
use model::*;
use noise::*;
use render::*;
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Args {
    res: usize,
    out: PathBuf,
    only: String,
    steps: usize,
    k: f32,
    u: f32,
    skip_views: bool,
}

fn parse() -> Args {
    let mut a = Args { res: 512, out: PathBuf::from("target/landform-proto"), only: "A,B,C,C2".into(), steps: 36, k: 0.02, u: 10.0, skip_views: false };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut v = || it.next().expect("value");
        match x.as_str() {
            "--res" => a.res = v().parse().unwrap(),
            "--out" => a.out = PathBuf::from(v()),
            "--only" => a.only = v().to_uppercase(),
            "--steps" => a.steps = v().parse().unwrap(),
            "--k" => a.k = v().parse().unwrap(),
            "--u" => a.u = v().parse().unwrap(),
            "--skip-views" => a.skip_views = true,
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

struct Land {
    id: usize,
    area: f64,
    centroid: V3,
    extent_km: f64,
    anchor: usize, // a texel of the component nearest its centroid
}

struct Result {
    variant: Variant,
    h: Vec<f32>,
    eroded: erosion::Eroded,
    fam: Vec<u8>,
    biome: Vec<u8>,
    clim: Climate,
    form: Formations,
    shape: Shape,
    lands: Vec<Land>,
    build_s: f64,
    steps: Vec<(&'static str, f64)>,
    numbers: String,
    summary: Summary,
}

#[derive(Default, Clone)]
struct Summary {
    ocean: f64,
    continents: Vec<f64>,
    cores: Vec<f64>,
    islands: usize,
    highest: f64,
    deepest: f64,
    highest_at: (f64, f64),
    massifs_6km: usize,
    ranges_6km: usize,
    fam_share: Vec<f64>,
    range_relief_med: f64,
    range_relief_p90: f64,
    range_crest_p99: f64,
    land_above_4km: f64,
    land_above_6km: f64,
    mean_land_elev: f64,
    t_equator: f64,
    t_pole: f64,
}

fn components(g: &Grid, land: &[bool]) -> Vec<Land> {
    let n = g.total();
    let mut comp = vec![u32::MAX; n];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for s in 0..n {
        if !land[s] || comp[s] != u32::MAX {
            continue;
        }
        let id = out.len();
        comp[s] = id as u32;
        stack.push(s);
        let mut members = Vec::new();
        while let Some(k) = stack.pop() {
            members.push(k);
            for &m in &g.nbr[k] {
                let m = m as usize;
                if land[m] && comp[m] == u32::MAX {
                    comp[m] = id as u32;
                    stack.push(m);
                }
            }
        }
        let mut area = 0.0;
        let mut c = [0.0; 3];
        for &k in &members {
            let a = f64::from(g.area[k]);
            area += a;
            c = add(c, mul(g.dir[k], a));
        }
        let centroid = if len(c) > 1e-9 { norm(c) } else { g.dir[members[0]] };
        let mut anchor = (members[0], f64::MAX);
        let mut extent = 0.0f64;
        for &k in &members {
            let d = dot(g.dir[k], centroid).clamp(-1.0, 1.0).acos() * R;
            if d < anchor.1 {
                anchor = (k, d);
            }
            extent = extent.max(d);
        }
        out.push(Land { id, area, centroid, extent_km: extent, anchor: anchor.0 });
    }
    out.sort_by(|a, b| b.area.total_cmp(&a.area));
    out
}

fn percentile(v: &mut [f32], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    f64::from(v[((v.len() - 1) as f64 * p) as usize])
}

fn run_variant(g: &Grid, plates: &Plates, v: Variant, args: &Args) -> Result {
    let t0 = Instant::now();
    let mut steps = Vec::new();
    let mut lap = |name: &'static str, t: &mut Instant| {
        steps.push((name, t.elapsed().as_secs_f64()));
        *t = Instant::now();
    };
    let mut t = Instant::now();
    let raw = match v {
        Variant::A => raw_a(g),
        Variant::B => raw_plates(g, plates, false),
        Variant::C => raw_c(g, plates),
        Variant::C2 => raw_c2(g, plates),
    };
    let shape = match v {
        Variant::A | Variant::C | Variant::C2 => shape_spline(g, &raw),
        Variant::B => shape_plate(g, &raw),
    };
    lap("land model", &mut t);
    let h0 = shape.h.clone();
    let eroded = erosion::erode(g, &h0, &shape.uplift, &erosion::Params { steps: args.steps, k: args.k, u: args.u, smax: 650.0 });
    if std::env::var("LP_DEBUG").is_ok() {
        let mx0 = h0.iter().cloned().fold(f32::MIN, f32::max);
        let mx1 = eroded.h.iter().cloned().fold(f32::MIN, f32::max);
        let ml = |v: &[f32]| (0..g.total()).filter(|&k| h0[k] > 0.0).map(|k| f64::from(v[k]) * f64::from(g.area[k])).sum::<f64>() / (0..g.total()).filter(|&k| h0[k] > 0.0).map(|k| f64::from(g.area[k])).sum::<f64>();
        eprintln!("erosion: max {mx0:.0} -> {mx1:.0}; mean land {:.0} -> {:.0}", ml(&h0), ml(&eroded.h));
    }
    lap("erosion", &mut t);
    let h = eroded.h.clone();
    let clim = climate(g, &h);
    if std::env::var("LP_DEBUG").is_ok() {
        let mut v: Vec<f32> = (0..g.total()).filter(|&k| h[k] > 0.0).map(|k| clim.moist[k]).collect();
        eprintln!("land moisture p5/25/50/75/95: {:.2} {:.2} {:.2} {:.2} {:.2}", percentile(&mut v, 0.05), percentile(&mut v, 0.25), percentile(&mut v, 0.5), percentile(&mut v, 0.75), percentile(&mut v, 0.95));
        let mut tv: Vec<f32> = (0..g.total()).filter(|&k| h[k] > 0.0).map(|k| clim.temp[k]).collect();
        eprintln!("land temp p5/25/50/75/95: {:.0} {:.0} {:.0} {:.0} {:.0}", percentile(&mut tv, 0.05), percentile(&mut tv, 0.25), percentile(&mut tv, 0.5), percentile(&mut tv, 0.75), percentile(&mut tv, 0.95));
    }
    lap("climate", &mut t);
    let fam = classify(g, &shape, &h);
    let biome = biomes(g, &fam, &clim, &h);
    let form = formations(g, &shape, &fam, &clim, &h);
    lap("landform, biome, formations", &mut t);
    let build_s = t0.elapsed().as_secs_f64();

    // ---- numbers
    let land: Vec<bool> = h.iter().map(|&x| x > 0.0).collect();
    let ocean = 1.0 - g.share(|k| land[k]);
    let lands = components(g, &land);
    if std::env::var("LP_DEBUG").is_ok() {
        eprintln!("components: {:?}", lands.iter().take(12).map(|l| l.area as i64).collect::<Vec<_>>());
    }
    let continents: Vec<f64> = lands.iter().filter(|l| l.area >= 20000.0).map(|l| l.area).collect();
    let islands = lands.iter().filter(|l| l.area < 20000.0 && l.area >= 200.0).count();
    // Continent cores: land at least ~30 km from any water, so isthmuses and spits do not join continents.
    let core_passes = (30.0 / (std::f64::consts::FRAC_PI_2 * R / args.res as f64)).round() as usize;
    let mut core = land.clone();
    for _ in 0..core_passes {
        let prev = core.clone();
        for k in 0..g.total() {
            if prev[k] && g.nbr[k].iter().any(|&m| !prev[m as usize]) {
                core[k] = false;
            }
        }
    }
    let cores: Vec<f64> = components(g, &core).into_iter().filter(|l| l.area >= 3000.0).map(|l| l.area).collect();
    let (hk, hmax) = h.iter().enumerate().fold((0, f32::MIN), |b, (k, &x)| if x > b.1 { (k, x) } else { b });
    let lat = g.dir[hk][1].asin().to_degrees();
    let lon = g.dir[hk][2].atan2(g.dir[hk][0]).to_degrees();
    let high: Vec<bool> = h.iter().map(|&x| x > 6000.0).collect();
    let massif_list: Vec<Land> = components(g, &high).into_iter().filter(|c| c.area > 50.0).collect();
    let massifs_6km = massif_list.len();
    let mut ranges: Vec<V3> = Vec::new();
    for l in &massif_list {
        if !ranges.iter().any(|c| dot(*c, l.centroid).clamp(-1.0, 1.0).acos() * R < 160.0) {
            ranges.push(l.centroid);
        }
    }
    let ranges_6km = ranges.len();
    let land_area: f64 = (0..g.total()).filter(|&k| land[k]).map(|k| f64::from(g.area[k])).sum();
    let fam_share: Vec<f64> = (0..6)
        .map(|f| (0..g.total()).filter(|&k| fam[k] == f).map(|k| f64::from(g.area[k])).sum::<f64>() / land_area)
        .collect();
    let a4: f64 = (0..g.total()).filter(|&k| h[k] > 4000.0).map(|k| f64::from(g.area[k])).sum::<f64>() / land_area;
    let a6: f64 = (0..g.total()).filter(|&k| h[k] > 6000.0).map(|k| f64::from(g.area[k])).sum::<f64>() / land_area;
    let mean_land = (0..g.total()).filter(|&k| land[k]).map(|k| f64::from(g.area[k]) * f64::from(h[k])).sum::<f64>() / land_area;
    // Relief inside ranges: max - min within a +-12 texel window, sampled.
    let mut reliefs: Vec<f32> = Vec::new();
    let mut crest: Vec<f32> = Vec::new();
    let w = (12 * g.n / 512).max(3);
    for f in 0..6 {
        for j in (w..g.n - w).step_by(5) {
            for i in (w..g.n - w).step_by(5) {
                let k = g.index(f, i, j);
                if fam[k] != RANGE {
                    continue;
                }
                crest.push(h[k]);
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for dj in 0..=2 * w {
                    for di in 0..=2 * w {
                        let x = h[g.index(f, i + di - w, j + dj - w)];
                        lo = lo.min(x);
                        hi = hi.max(x);
                    }
                }
                reliefs.push(hi - lo);
            }
        }
    }
    let summary = Summary {
        ocean,
        continents,
        islands,
        cores,
        highest: f64::from(hmax),
        deepest: f64::from(h.iter().cloned().fold(f32::MAX, f32::min)),
        highest_at: (lat, lon),
        massifs_6km,
        ranges_6km,
        fam_share,
        range_relief_med: percentile(&mut reliefs.clone(), 0.5),
        range_relief_p90: percentile(&mut reliefs, 0.9),
        range_crest_p99: percentile(&mut crest, 0.99),
        land_above_4km: a4,
        land_above_6km: a6,
        mean_land_elev: mean_land,
        t_equator: 0.0,
        t_pole: 0.0,
    };
    Result { variant: v, h, eroded, fam, biome, clim, form, shape, lands, build_s, steps, numbers: String::new(), summary }
}

fn numbers_md(r: &Result, res: usize) -> String {
    let s = &r.summary;
    let mut o = String::new();
    o += &format!("### Variant {} ({} per face, {:.2} km texels)\n\n", r.variant.name(), format!("{res}x{res}"), std::f64::consts::FRAC_PI_2 * R / res as f64);
    o += &format!("- Build time: **{:.1} s** (", r.build_s);
    o += &r.steps.iter().map(|(n, t)| format!("{n} {t:.1} s")).collect::<Vec<_>>().join(", ");
    o += ")\n";
    o += &format!("- Ocean share: **{:.1}%**\n", s.ocean * 100.0);
    o += &format!(
        "- Continents (>= 20,000 km2): **{}**, areas {} km2 (equivalent diameters {} km); islands of 200 km2 or more: {}\n",
        s.continents.len(),
        s.continents.iter().map(|a| format!("{:.0}", a)).collect::<Vec<_>>().join(", "),
        s.continents.iter().map(|a| format!("{:.0}", (4.0 * a / std::f64::consts::PI).sqrt())).collect::<Vec<_>>().join(", "),
        s.islands
    );
    o += &format!("- Continent cores (land 30 km from water, >= 3,000 km2, so isthmuses do not join them): **{}**, areas {} km2\n", s.cores.len(), s.cores.iter().map(|a| format!("{a:.0}")).collect::<Vec<_>>().join(", "));
    o += &format!("- Highest point: **{:.0} m** at {:.0} N, {:.0} E; separate massifs above 6 km: {} (in {} ranges, massifs within 160 km counted as one range)\n", s.highest, s.highest_at.0, s.highest_at.1, s.massifs_6km, s.ranges_6km);
    o += &format!("- Deepest point: {:.0} m\n", s.deepest);
    o += &format!("- Land above 4 km: {:.2}% of land; above 6 km: {:.2}%; mean land elevation {:.0} m\n", s.land_above_4km * 100.0, s.land_above_6km * 100.0, s.mean_land_elev);
    o += &format!(
        "- Range family: crest p99 {:.0} m; relief inside a 25-texel window: median {:.0} m, p90 {:.0} m\n",
        s.range_crest_p99, s.range_relief_med, s.range_relief_p90
    );
    o += "- Land share per family: ";
    o += &(0..6).map(|f| format!("{} {:.1}%", FAMILY_NAMES[f], s.fam_share[f] * 100.0)).collect::<Vec<_>>().join(", ");
    o += "\n\n";
    o
}

// ------------------------------------------------------------------ outputs

fn ensure(p: &Path) {
    std::fs::create_dir_all(p).unwrap();
}

fn render_variant(g: &Grid, r: &mut Result, out: &Path, args: &Args) -> (Vec<Img>, Vec<Img>, Img) {
    let dir = out.join(r.variant.name().to_lowercase());
    ensure(&dir);
    let t0 = Instant::now();
    let h = &r.h;
    let lake = &r.eroded.lake;
    let scene = Scene { g, h, temp: &r.clim.temp, area: &r.eroded.area, lake, rivers: false };
    let scene_r = Scene { rivers: true, ..Scene { g, h, temp: &r.clim.temp, area: &r.eroded.area, lake, rivers: true } };
    let title = r.variant.title();

    // ---- orbit
    let big = &r.lands[0];
    let v1 = big.centroid;
    let v2 = mul(v1, -1.0);
    let mut orbit_imgs = Vec::new();
    for (i, v) in [v1, v2].iter().enumerate() {
        let im = orbit(&scene, *v, 3.0, 900, 7.0);
        orbit_imgs.push(label(&im, &format!("{title}  {}", if i == 0 { "HEMISPHERE 1 (BIGGEST CONTINENT)" } else { "OPPOSITE HEMISPHERE" })));
    }
    let mut both = Img::new(1800, 900, [0, 0, 0]);
    both.blit(&orbit_imgs[0], 0, 0);
    both.blit(&orbit_imgs[1], 900, 0);
    both.save(&dir.join("orbit.png"));

    // ---- continent views
    let mut cont_imgs: Vec<Img> = Vec::new();
    if !args.skip_views {
        // The highest peak that has land all round it (not a lone island).
        let mut order: Vec<usize> = (0..g.total()).filter(|&k| h[k] > 3000.0).collect();
        order.sort_by(|&a, &b| h[b].total_cmp(&h[a]));
        let peak = order
            .iter()
            .map(|&k| g.dir[k])
            .find(|&d| {
                let (e, n) = Grid::frame(d);
                let land = (0..16)
                    .filter(|&a| {
                        let az = f64::from(a) * std::f64::consts::TAU / 16.0;
                        let look = add(mul(n, az.cos()), mul(e, az.sin()));
                        let th = 55.0 / R;
                        g.sample(h, add(mul(d, th.cos()), mul(look, th.sin()))) > 0.0
                    })
                    .count();
                land >= 13
            })
            .unwrap_or_else(|| g.dir[order.first().copied().unwrap_or(0)]);
        let targets = [("big", g.dir[big.anchor], 1.0f64, (big.extent_km + 40.0).clamp(130.0, 300.0)), ("range", peak, 1.0, 170.0)];
        for (name, tdir, _, half) in targets {
            // Look along the azimuth with the most high ground 20-110 km ahead.
            let az = (0..16)
                .map(|a| f64::from(a) * std::f64::consts::TAU / 16.0)
                .max_by(|&a, &b| {
                    let score = |az: f64| {
                        let (e, n) = Grid::frame(tdir);
                        let look = add(mul(n, az.cos()), mul(e, az.sin()));
                        (2..12).map(|s| {
                            let th = f64::from(s) * 10.0 / R;
                            let d = add(mul(tdir, th.cos()), mul(look, th.sin()));
                            f64::from(g.sample(h, d)).max(0.0)
                        }).sum::<f64>()
                    };
                    score(a).total_cmp(&score(b))
                })
                .unwrap();
            let cam = camera(tdir, az, if name == "big" { 70.0 } else { 48.0 }, 30.0);
            let p = persp(&scene_r, &cam, 960, 540, 66.0, 2.0);
            let p = label(&p, &format!("{title}  {} CONTINENT, 30 KM UP, HEIGHT X2", if name == "big" { "BIGGEST" } else { "HIGHEST RANGE" }));
            p.save(&dir.join(format!("continent-{name}-persp.png")));
            cont_imgs.push(p);
            let o = ortho(&scene_r, tdir, half, 720, 6.0);
            let o = label(&o, &format!("{title}  {} ({:.0} KM ACROSS)", if name == "big" { "BIGGEST CONTINENT" } else { "HIGHEST RANGE" }, 2.0 * half));
            o.save(&dir.join(format!("continent-{name}-ortho.png")));
            cont_imgs.push(o);
        }
    }

    // ---- maps
    let sc = &scene;
    let elev = equirect(g, MAP_W, MAP_H, |k, d| {
        let sh = map_shade(sc, d, 6.0);
        let c = hypso(h[k]);
        [c[0] * sh, c[1] * sh, c[2] * sh]
    });
    let elev_stops: Vec<(f32, [f32; 3])> = vec![
        (-4000.0, [8.0, 20.0, 60.0]),
        (-2500.0, [16.0, 44.0, 104.0]),
        (-1000.0, [28.0, 76.0, 146.0]),
        (-200.0, [44.0, 118.0, 176.0]),
        (0.0, [66.0, 118.0, 58.0]),
        (1000.0, [186.0, 166.0, 108.0]),
        (2800.0, [140.0, 122.0, 108.0]),
        (5500.0, [220.0, 220.0, 226.0]),
        (8000.0, [255.0, 255.0, 255.0]),
    ];
    legend_bar(&elev, &format!("{title} ELEVATION (HILL SHADED)"), &elev_stops, -4000.0, 8000.0, " M").save(&dir.join("map-elevation.png"));

    let t_stops: Vec<(f32, [f32; 3])> = vec![
        (-40.0, [40.0, 60.0, 190.0]),
        (-20.0, [90.0, 150.0, 235.0]),
        (-5.0, [200.0, 230.0, 245.0]),
        (5.0, [230.0, 240.0, 190.0]),
        (18.0, [240.0, 200.0, 90.0]),
        (30.0, [215.0, 60.0, 40.0]),
        (40.0, [120.0, 20.0, 40.0]),
    ];
    let temp = equirect(g, MAP_W, MAP_H, |k, _| ramp(&t_stops, r.clim.temp[k]));
    legend_bar(&temp, &format!("{title} TEMPERATURE AT SURFACE (6.5 C PER KM LAPSE)"), &t_stops, -40.0, 40.0, " C").save(&dir.join("map-temperature.png"));

    let m_stops: Vec<(f32, [f32; 3])> = vec![
        (0.0, [200.0, 150.0, 90.0]),
        (0.2, [225.0, 205.0, 130.0]),
        (0.4, [170.0, 200.0, 100.0]),
        (0.65, [70.0, 160.0, 90.0]),
        (1.0, [20.0, 80.0, 150.0]),
    ];
    let moist = equirect(g, MAP_W, MAP_H, |k, d| {
        if h[k] <= 0.0 {
            let sh = map_shade(sc, d, 1.0) * 0.5;
            let c = hypso(h[k]);
            [c[0] * sh * 0.5, c[1] * sh * 0.5, c[2] * sh * 0.5 + 12.0]
        } else {
            ramp(&m_stops, r.clim.moist[k])
        }
    });
    legend_bar(&moist, &format!("{title} MOISTURE (EFFECTIVE, 0 DRY TO 1 WET)"), &m_stops, 0.0, 1.0, "").save(&dir.join("map-moisture.png"));

    let famimg = equirect(g, MAP_W, MAP_H, |k, d| {
        if r.fam[k] == OCEAN {
            let c = hypso(h[k]);
            let sh = 0.55 + 0.25 * map_shade(sc, d, 1.0);
            [c[0] * sh * 0.5, c[1] * sh * 0.5, c[2] * sh * 0.5 + 12.0]
        } else {
            f3(FAMILY_COLOURS[r.fam[k] as usize])
        }
    });
    famimg.save(&dir.join("map-family-raw.png"));
    let fam_legend: Vec<(String, Rgb)> = (0..7).map(|f| (FAMILY_NAMES[f].to_string(), FAMILY_COLOURS[f])).collect();
    legend_entries(&famimg, &format!("{title} LANDFORM FAMILY"), &fam_legend, 7).save(&dir.join("map-family.png"));

    let bio = equirect(g, MAP_W, MAP_H, |k, d| {
        let bm = &BIOMES[r.biome[k] as usize];
        if r.fam[k] == OCEAN {
            let sh = 0.7 + 0.3 * map_shade(sc, d, 1.0);
            [f32::from(bm.colour[0]) * sh, f32::from(bm.colour[1]) * sh, f32::from(bm.colour[2]) * sh]
        } else {
            let sh = 0.8 + 0.3 * (map_shade(sc, d, 4.0) - 0.7);
            [f32::from(bm.colour[0]) * sh, f32::from(bm.colour[1]) * sh, f32::from(bm.colour[2]) * sh]
        }
    });
    let bio_legend: Vec<(String, Rgb)> = BIOMES.iter().map(|b| (format!("{} ({})", b.name, FAMILY_NAMES[b.family as usize]), b.colour)).collect();
    legend_entries(&bio, &format!("{title} BIOME (LANDFORM FIRST, THEN CLIMATE; PLACEHOLDER TABLE)"), &bio_legend, 3).save(&dir.join("map-biome.png"));

    // Rivers: dark land, rivers by log drainage area, lakes. Rendered at 2x so 1 km rivers stay joined.
    let area = &r.eroded.area;
    let lakes = &r.eroded.lake;
    let rivers = equirect(g, 2 * MAP_W, 2 * MAP_H, |k, d| {
        if h[k] <= 0.0 {
            return [14.0, 22.0, 40.0];
        }
        let sh = map_shade(sc, d, 5.0);
        let base = 38.0 + 70.0 * sh * (0.6 + 0.4 * (h[k] / 5000.0).min(1.0));
        let mut c = [base, base * 0.98, base * 0.92];
        if lakes[k] {
            c = [110.0, 220.0, 255.0];
        } else if area[k] > 120.0 {
            let s = ((area[k] / 120.0).ln() / (80000f32 / 120.0).ln()).clamp(0.0, 1.0);
            c = lerp3([70.0, 150.0, 255.0], [220.0, 255.0, 255.0], s);
        }
        c
    });
    let river_legend = vec![
        ("RIVER 120 KM2 CATCHMENT".to_string(), [70, 150, 255]),
        ("RIVER 80,000 KM2 CATCHMENT".to_string(), [220, 255, 255]),
        ("LAKE (FILLED BASIN)".to_string(), [110, 220, 255]),
    ];
    legend_entries(&rivers, &format!("{title} RIVERS FROM THE STREAM-POWER PASS (2X SCALE)"), &river_legend, 3).save(&dir.join("map-rivers.png"));

    for (name, arr) in [("cliff", &r.form.cliff), ("spire", &r.form.spire), ("arch", &r.form.arch)] {
        let stops: Vec<(f32, [f32; 3])> = vec![(0.0, [30.0, 30.0, 40.0]), (0.25, [90.0, 40.0, 120.0]), (0.55, [220.0, 70.0, 80.0]), (1.0, [255.0, 235.0, 120.0])];
        let im = equirect(g, MAP_W, MAP_H, |k, d| {
            if h[k] <= 0.0 {
                let sh = 0.6 + 0.2 * map_shade(sc, d, 1.0);
                [10.0 * sh, 16.0 * sh, 34.0 * sh]
            } else {
                ramp(&stops, arr[k])
            }
        });
        let nm = match name {
            "cliff" => "LEANING CLIFF",
            "spire" => "SPIRE",
            _ => "ARCH",
        };
        legend_bar(&im, &format!("{title} FORMATION ALLOWANCE: {nm} (0 NONE, 1 FULL)"), &stops, 0.0, 1.0, "").save(&dir.join(format!("map-formation-{name}.png")));
    }
    eprintln!("  [{}] rendered in {:.1} s", r.variant.name(), t0.elapsed().as_secs_f64());
    (orbit_imgs, cont_imgs, famimg)
}

fn sheet(rows: &[Vec<&Img>], panel_w: usize) -> Img {
    let gap = 6;
    let cols = rows[0].len();
    let rs: Vec<Vec<Img>> = rows
        .iter()
        .map(|row| row.iter().map(|im| im.resized(panel_w, (im.h * panel_w / im.w).max(1))).collect())
        .collect();
    let heights: Vec<usize> = rs.iter().map(|row| row.iter().map(|i| i.h).max().unwrap()).collect();
    let (w, h) = (cols * panel_w + (cols + 1) * gap, heights.iter().sum::<usize>() + (rows.len() + 1) * gap);
    let mut out = Img::new(w, h, [18, 20, 26]);
    let mut y = gap;
    for (ri, row) in rs.iter().enumerate() {
        for (ci, im) in row.iter().enumerate() {
            out.blit(im, gap + ci * (panel_w + gap), y);
        }
        y += heights[ri] + gap;
    }
    out
}

fn main() {
    let args = parse();
    let t_all = Instant::now();
    eprintln!("grid {0}x{0}x6 ...", args.res);
    let g = Grid::new(args.res);
    eprintln!("grid built in {:.1} s", t_all.elapsed().as_secs_f64());
    if std::env::var("LP_DEBUG").is_ok() {
        let mx = g.nbr_d.iter().flat_map(|a| a.iter()).cloned().fold(0f32, f32::max);
        let mean: f64 = g.nbr_d.iter().map(|a| f64::from(a[0])).sum::<f64>() / g.total() as f64;
        let tot: f64 = g.area.iter().map(|&a| f64::from(a)).sum();
        eprintln!("nbr_d max {mx} mean {mean}; total area {tot} (expect {})", 4.0 * std::f64::consts::PI * R * R);
    }
    let plates = make_plates(&g, false);
    let plates2 = make_plates(&g, true);
    let out = args.out.clone();
    ensure(&out);
    let mut results: Vec<Result> = Vec::new();
    let mut orbits: Vec<Vec<Img>> = Vec::new();
    let mut conts: Vec<Vec<Img>> = Vec::new();
    let mut fams: Vec<Img> = Vec::new();
    for v in [Variant::A, Variant::B, Variant::C, Variant::C2] {
        if !args.only.split(',').any(|x| x == v.name()) {
            continue;
        }
        eprintln!("variant {} ...", v.name());
        let mut r = run_variant(&g, if v == Variant::C2 { &plates2 } else { &plates }, v, &args);
        eprintln!("  [{}] built in {:.1} s ({})", v.name(), r.build_s, r.steps.iter().map(|(n, t)| format!("{n} {t:.1}")).collect::<Vec<_>>().join(", "));
        r.numbers = numbers_md(&r, args.res);
        eprint!("{}", r.numbers);
        let (o, c, f) = render_variant(&g, &mut r, &out, &args);
        let _ = (&r.shape, &r.biome);
        orbits.push(o);
        conts.push(c);
        fams.push(f);
        results.push(r);
    }
    // ---- contact sheets
    let titles: Vec<&str> = results.iter().map(|r| r.variant.title()).collect();
    let idx = |name: &str| results.iter().position(|r| r.variant.name() == name);
    if let (Some(c), Some(c2)) = (idx("C"), idx("C2")) {
        let both = [c, c2];
        let mut rows: Vec<Vec<&Img>> = vec![both.iter().map(|&i| &orbits[i][0]).collect(), both.iter().map(|&i| &orbits[i][1]).collect()];
        if !args.skip_views && both.iter().all(|&i| conts[i].len() == 4) {
            rows.push(both.iter().map(|&i| &conts[i][2]).collect());
            rows.push(both.iter().map(|&i| &conts[i][3]).collect());
        }
        let fl: Vec<Img> = both.iter().map(|&i| label(&fams[i].resized(1024, 512), &format!("{}  LANDFORM FAMILY", results[i].variant.title()))).collect();
        rows.push(fl.iter().collect());
        sheet(&rows, 960).save(&out.join("sheet-c-vs-c2.png"));
        for i in both {
            let d = out.join(results[i].variant.name().to_lowercase());
            let _ = std::fs::write(d.join("numbers.md"), &results[i].numbers);
        }
    }
    let abc: Vec<usize> = ["A", "B", "C"].iter().filter_map(|n| idx(n)).collect();
    if abc.len() == 3 {
        let orbits: Vec<&Vec<Img>> = abc.iter().map(|&i| &orbits[i]).collect();
        let conts: Vec<&Vec<Img>> = abc.iter().map(|&i| &conts[i]).collect();
        let fams: Vec<&Img> = abc.iter().map(|&i| &fams[i]).collect();
        let titles: Vec<&str> = abc.iter().map(|&i| results[i].variant.title()).collect();
        let r0: Vec<&Img> = orbits.iter().map(|o| &o[0]).collect();
        let r1: Vec<&Img> = orbits.iter().map(|o| &o[1]).collect();
        sheet(&[r0, r1], 640).save(&out.join("sheet-orbit.png"));
        if !args.skip_views && conts.iter().all(|c| c.len() == 4) {
            let rows: Vec<Vec<&Img>> = [0usize, 2, 1, 3].iter().map(|&i| conts.iter().map(|c| &c[i]).collect()).collect();
            sheet(&rows, 640).save(&out.join("sheet-continent.png"));
        }
        let labelled: Vec<Img> = fams.iter().zip(&titles).map(|(f, t)| label(&f.resized(1024, 512), &format!("{t}  LANDFORM FAMILY"))).collect();
        let strip = sheet(&[labelled.iter().collect()], 1024);
        let legend: Vec<(String, Rgb)> = (0..7).map(|f| (FAMILY_NAMES[f].to_string(), FAMILY_COLOURS[f])).collect();
        let mut with = legend_entries(&strip, "LEGEND", &legend, 7);
        with.rect(0, 0, 0, 0, [0, 0, 0]);
        with.save(&out.join("sheet-families.png"));
    }
    // ---- numbers + README
    let mut md = String::from("# Numbers per variant\n\n");
    for r in &results {
        md += &r.numbers;
    }
    std::fs::write(out.join("numbers.md"), &md).unwrap();
    let _ = std::fs::write(out.join("README.md"), readme(&md, args.res));
    eprintln!("total {:.1} s; outputs in {}", t_all.elapsed().as_secs_f64(), out.display());
}

fn readme(numbers: &str, res: usize) -> String {
    format!(
        "# Landform model prototype (#246, stage 1) - how to read this

Throwaway. Three ways of building the land from the same seed, the same Climate and the same
stream-power erosion. Only the land model differs.

- **A noise and splines** (Tectonic-style): warped continentalness, erosion and ridge noise, mapped through
  monotone splines to height. Families come from regions of those fields.
- **B plates**: 30 hashed plates, 6 continents grown from continental plates. Ranges along colliding edges,
  rifts along separating ones, island arcs and trenches where ocean meets ocean or continent.
- **C hybrid**: B's plates give the continents and the lines of the big ranges (they bias A's erosion field and
  plateau selector); A's splines and ridge noise shape the land inside and along them.

Resolution: {res}x{res} per face of a cube map (~{km:.2} km texels), Planet radius 320 km, sea level 0.
Run: `cargo run --release -p planet-gen --example proto_landform` (add `-- --res 128` for a quick look).

## Files (under `target/landform-proto/`)

- `sheet-orbit.png`: A, B, C side by side from 3 R, sun from the left. Top row: the hemisphere centred on each
  variant's biggest continent. Bottom row: the opposite hemisphere. Hill shading x7, sea blue by depth,
  snow where it is below about -2 C.
- `sheet-continent.png`: rows are (1) biggest continent from 30 km up, height x2; (2) highest range from 30 km up,
  height x2; (3) biggest continent, top-down orthographic crop, hill shading x6; (4) highest range, orthographic
  crop. Rivers (blue lines) and lakes are drawn on these. The camera is 70 km (48 km for the range) from its target, 30 km up.
- `sheet-families.png`: the Landform family maps side by side (equirectangular, flat colour per family).
- `a/`, `b/`, `c/`: `orbit.png`, `continent-*-persp.png`, `continent-*-ortho.png` and the equirectangular maps
  (2048x1024, rivers 4096x2048): `map-elevation`, `map-temperature`, `map-moisture`, `map-family`, `map-biome`,
  `map-rivers`, `map-formation-cliff`, `map-formation-spire`, `map-formation-arch`.
- `numbers.md`: the numbers below.

## What the maps mean

- **Family** is chosen from the shape fields only (before Climate): canyon and valley cut-lines first, then
  range-ness (mountain mask x crest relief), plateau selector, then coast (within a few km of the sea by
  continentalness), else plains. The same values drive the height, so the shape matches the family.
- **Biome** is the nearest box in (temperature, moisture) among the Biomes that allow the family; a placeholder table.
- **Temperature**: 30 C equator to -30 C poles, minus 6.5 C per km of elevation, plus about 15% noise.
- **Moisture**: wind bands (trades, westerlies, polar easterlies) carry moisture off the sea at 4 km texels;
  rain-out on rising ground gives windward wet slopes and rain shadows. Effective moisture saturates at 1.
- **Formation allowance** per kind (0..1), from the family, steepness and ridge crest values, and dryness:
  leaning cliffs on steep range/plateau/canyon walls, spires on sharp crests and dry canyons/mesas,
  arches on coasts and dry canyon/plateau walls.
- **Rivers** are the receiver graph of the last erosion step: drainage area over 120 km2 on the river map (1,200 km2 on the 3D views). Lakes are the basins
  the priority flood had to fill.

{numbers}

## Judgement against the targets (author's own, honest)

Targets: 55% ocean; 4-8 continents of 200-600 km with island arcs and archipelagos; typical ranges 2-5 km,
valleys 1-3 km below the ridgelines, only the greatest one or two ranges at 6-8 km; ranges 100-300 km long with
ridgelines; shelves 50-200 m, open ocean 1.5-3 km, trenches to 4 km.

- **All three** hit the sea share (54.6-56.6%) and have 4-5 continents, island chains, shelves and a
  stream-power drainage with lakes. Range crests are 5.2-5.8 km at p99 and 2.1-3.0 km of relief in a 25 km window
  (that window can include a coast, so read it as an upper bound). None reads as domes with speckle any more:
  ridge noise gives crest lines and the erosion cuts dendritic valleys into them.
- **A (noise and splines)** has the most natural coasts, arcs and archipelagos and the nicest ridged range
  (long crests with spurs). But the continents and the ranges are unrelated blobs of independent noise: one belt of
  mountains on the east side of the biggest continent, and 54% of the land is featureless plains with a round
  inland sea. One 7.2 km massif (right). 4 continents, the smallest 200 km. Families are the most fragmented.
- **B (plates)** has the clearest cause for everything (ranges on collisions, rifts as valleys, arcs and trenches
  at subduction) and the best continent spacing, but it looks like what it is: coasts follow plate edges, with
  straight transform lines and trench slits in the sea, and ranges are blocky snowy plateaus with gullies rather
  than ridge systems. The highest point is only 6.6 km and the biggest 'continent' is two merged at 617 km.
- **C (hybrid)** gives the best ranges: long ridgelines and valleys that follow the collision belts, rivers
  between them, and high ranges where the plates say so (7.8 km). It keeps B's jigsaw coasts, makes plateaus
  too common (21% of land) and puts 4 separate 6 km+ massifs in its one strongest belt (a little more than
  'one or two'). The merged continent is 606 km.
- **Weak in all three**: canyons are noise cut-lines gated to tablelands (2-3% of land) and closed loops show as
  circular ditches/lakes; trenches and island arcs are crude (the deepest trench bottoms are only 3.1-3.7 km, short of 4 km); the Climate is plausible but un-tuned (wet equator,
  dry subtropics and rain shadows show; the poles are mostly ice sheet); the Biome table is a placeholder;
  Formation allowances are heuristics from family, steepness, crest and dryness; the erosion is a 36-step
  transient (not steady state) with weak uplift, so valleys are shallower than a real run's; 1 km texels make the
  30 km views soft, with a ray-march washboard on slopes.
",
        km = std::f64::consts::FRAC_PI_2 * R / res as f64
    )
}
