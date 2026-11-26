//! Climate (temperature, wind-band moisture), Landform family, Biome, Formation allowance.

use super::grid::*;
use super::model::{Shape, SEED};
use super::noise::*;

pub const RANGE: u8 = 0;
pub const PLATEAU: u8 = 1;
pub const CANYON: u8 = 2;
pub const VALLEY: u8 = 3;
pub const PLAINS: u8 = 4;
pub const COAST: u8 = 5;
pub const OCEAN: u8 = 6;
pub const FAMILY_NAMES: [&str; 7] = ["RANGE", "PLATEAU", "CANYON", "VALLEY", "PLAINS", "COAST", "OCEAN"];
pub const FAMILY_COLOURS: [[u8; 3]; 7] = [
    [150, 80, 60],   // range: rust brown
    [230, 190, 80],  // plateau: ochre
    [190, 50, 70],   // canyon: crimson
    [60, 160, 90],   // valley: green
    [190, 215, 120], // plains: light lime
    [240, 140, 200], // coast: pink
    [30, 55, 110],   // ocean
];

// ------------------------------------------------------------------ climate

pub struct Climate {
    pub temp: Vec<f32>,  // deg C
    pub moist: Vec<f32>, // 0..1 effective moisture
    pub precip: Vec<f32>, // raw rain index
}

fn temp_sea(lat: f64) -> f64 {
    -30.0 + 60.0 * lat.cos().max(0.0).powf(1.5)
}
fn qsat(t: f64) -> f64 {
    0.06 + 0.94 * ((t + 30.0) / 60.0).clamp(0.0, 1.0).powi(2)
}

pub fn climate(g: &Grid, h: &[f32]) -> Climate {
    let n = g.n;
    let nc_side = (n / 4).max(32);
    let gc = Grid::new(nc_side);
    let r = n / nc_side;
    // Coarse heights: block means.
    let hc: Vec<f32> = par_map(gc.total(), |k| {
        let (f, i, j) = gc.unpack(k);
        let mut s = 0.0;
        for dj in 0..r {
            for di in 0..r {
                s += h[g.index(f, i * r + di, j * r + dj)];
            }
        }
        s / (r * r) as f32
    });
    let tc: Vec<f32> = (0..gc.total())
        .map(|k| (temp_sea(f64::from(gc.lat[k])) - 6.5 * f64::from(hc[k].max(0.0)) / 1000.0) as f32)
        .collect();
    // Wind: Earth-like bands. u east, v poleward-signed meridional.
    let wind: Vec<([f32; 8], f32)> = par_map(gc.total(), |k| {
        let d = gc.dir[k];
        let (e, nn) = Grid::frame(d);
        let lat = f64::from(gc.lat[k]);
        let al = lat.abs();
        let u = if al < 60f64.to_radians() { -(3.0 * al).cos() } else { (6.0 * (al - 60f64.to_radians())).cos() };
        let v = -0.35 * (6.0 * al).sin() * lat.signum();
        let w = add(mul(e, u), mul(nn, v));
        let speed = len(w);
        let wh = if speed > 0.02 { mul(w, 1.0 / speed) } else { [0.0; 3] };
        let mut ws = [0f32; 8];
        let mut sum = 0.0;
        for m in 0..8 {
            let nb = gc.dir[gc.nbr[k][m] as usize];
            let t = norm(sub(nb, mul(d, dot(nb, d))));
            let wgt = if speed > 0.02 { (-dot(t, wh)).max(0.0).powi(2) } else { 1.0 };
            ws[m] = wgt as f32;
            sum += wgt;
        }
        if sum < 1e-9 {
            ws = [1.0; 8];
            sum = 8.0;
        }
        for x in ws.iter_mut() {
            *x /= sum as f32;
        }
        (ws, speed.clamp(0.3, 1.0) as f32)
    });
    let hop = (std::f64::consts::FRAC_PI_2 * R / nc_side as f64) as f32;
    let s = hop / 4.0;
    let zon: Vec<f32> = (0..gc.total())
        .map(|k| {
            let deg = f64::from(gc.lat[k]).to_degrees().abs();
            (1.0 + 2.5 * gauss(deg, 0.0, 9.0) - 0.65 * gauss(deg, 28.0, 9.0) + 0.8 * gauss(deg, 60.0, 8.0)).max(0.2) as f32
        })
        .collect();
    let mut q = vec![0.3f32; gc.total()];
    let mut rain = vec![0f32; gc.total()];
    for _ in 0..(6 * nc_side) {
        let out: Vec<(f32, f32)> = par_map(gc.total(), |k| {
            let (ws, a) = &wind[k];
            let mut qin = 0.0;
            let mut hup = 0.0;
            for m in 0..8 {
                let nb = gc.nbr[k][m] as usize;
                qin += ws[m] * q[nb];
                hup += ws[m] * hc[nb].max(0.0);
            }
            let qa = (1.0 - a) * q[k] + a * qin;
            let qs = qsat(f64::from(tc[k])) as f32;
            if hc[k] <= 0.0 {
                let nq = if qa < qs { qa + 0.09 * s * (qs - qa) } else { qs };
                (nq.max(0.0), 0.0)
            } else {
                let lift = (hc[k] - hup).max(0.0);
                let mut rate = 0.012 * s * zon[k] + 0.22 * (1.0 - (-lift / (900.0 * s)).exp());
                if qa > qs {
                    rate = rate.max(0.6 * (qa - qs) / qa);
                }
                let rate = rate.min(0.95);
                let rr = qa * rate;
                // Land gives a little moisture back (evapotranspiration).
                ((qa - rr + 0.007 * s * (qs - qa).max(0.0)).max(0.0), rr)
            }
        });
        for k in 0..gc.total() {
            q[k] = out[k].0;
            rain[k] = out[k].1;
        }
    }
    // Upsample, add noise.
    let seed = SEED;
    let res: Vec<(f32, f32, f32)> = par_map(g.total(), |k| {
        let p = g.position(k);
        let d = g.dir[k];
        let lat = f64::from(g.lat[k]);
        let t0 = temp_sea(lat);
        let hh = f64::from(h[k]);
        let tn = fbm1(p, 1.0 / 170.0, 3, 0.5, seed + 201);
        let t = t0 - 6.5 * hh.max(0.0) / 1000.0 + 9.0 * tn;
        let pr = f64::from(gc.sample(&rain, d)).max(0.0);
        let p0 = 0.004 + 0.011 * qsat(t0);
        let mn = fbm1(p, 1.0 / 120.0, 3, 0.5, seed + 203);
        let m = (1.0 - (-pr / p0).exp() + 0.22 * mn).clamp(0.0, 1.0);
        (t as f32, m as f32, pr as f32)
    });
    Climate {
        temp: res.iter().map(|x| x.0).collect(),
        moist: res.iter().map(|x| x.1).collect(),
        precip: res.iter().map(|x| x.2).collect(),
    }
}

// ------------------------------------------------------------------ family

pub fn classify(g: &Grid, s: &Shape, h: &[f32]) -> Vec<u8> {
    (0..g.total())
        .map(|k| {
            if h[k] <= 0.0 {
                OCEAN
            } else if s.canyon[k] > 0.4 {
                CANYON
            } else if s.valley[k] > 0.45 {
                VALLEY
            } else if s.range[k] > 0.32 {
                RANGE
            } else if s.plateau[k] > 0.5 {
                PLATEAU
            } else if s.c[k] < 0.08 {
                COAST
            } else {
                PLAINS
            }
        })
        .collect()
}

// ------------------------------------------------------------------ biomes

pub struct Biome {
    pub name: &'static str,
    pub family: u8,
    pub t: (f32, f32),
    pub m: (f32, f32), // for ocean entries: depth in km
    pub colour: [u8; 3],
}

const fn b(name: &'static str, family: u8, t: (f32, f32), m: (f32, f32), colour: [u8; 3]) -> Biome {
    Biome { name, family, t, m, colour }
}

pub const BIOMES: &[Biome] = &[
    b("GLACIAL PEAKS", RANGE, (-60.0, -4.0), (0.0, 1.0), [235, 240, 250]),
    b("ALPINE TUNDRA", RANGE, (-4.0, 6.0), (0.0, 1.0), [150, 160, 150]),
    b("HIGHLAND FOREST", RANGE, (6.0, 20.0), (0.4, 1.0), [50, 110, 70]),
    b("ROCKY BARRENS", RANGE, (6.0, 40.0), (0.0, 0.4), [140, 115, 95]),
    b("CLOUD MOUNTAINS", RANGE, (20.0, 45.0), (0.55, 1.0), [30, 140, 110]),
    b("ICE PLATEAU", PLATEAU, (-60.0, -8.0), (0.0, 1.0), [215, 230, 240]),
    b("COLD STEPPE PLATEAU", PLATEAU, (-8.0, 8.0), (0.0, 1.0), [170, 170, 120]),
    b("MESA BADLANDS", PLATEAU, (8.0, 45.0), (0.0, 0.3), [205, 120, 70]),
    b("HIGHLAND SAVANNA", PLATEAU, (8.0, 45.0), (0.3, 0.6), [200, 175, 90]),
    b("CLOUD FOREST PLATEAU", PLATEAU, (8.0, 45.0), (0.6, 1.0), [70, 150, 100]),
    b("FROZEN CANYON", CANYON, (-60.0, -5.0), (0.0, 1.0), [180, 200, 225]),
    b("TEMPERATE CANYON", CANYON, (-5.0, 14.0), (0.0, 1.0), [170, 120, 95]),
    b("BADLANDS CANYON", CANYON, (14.0, 45.0), (0.0, 0.35), [200, 85, 55]),
    b("JUNGLE GORGE", CANYON, (14.0, 45.0), (0.5, 1.0), [30, 120, 60]),
    b("GLACIAL VALLEY", VALLEY, (-60.0, -3.0), (0.0, 1.0), [190, 215, 235]),
    b("MEADOW VALLEY", VALLEY, (-3.0, 18.0), (0.2, 1.0), [110, 175, 95]),
    b("DRY VALLEY", VALLEY, (6.0, 45.0), (0.0, 0.3), [185, 160, 105]),
    b("WET RIVER VALLEY", VALLEY, (18.0, 45.0), (0.45, 1.0), [40, 150, 90]),
    b("ICE SHEET", PLAINS, (-60.0, -14.0), (0.0, 1.0), [245, 248, 255]),
    b("TUNDRA", PLAINS, (-14.0, -2.0), (0.0, 1.0), [165, 175, 150]),
    b("TAIGA", PLAINS, (-2.0, 8.0), (0.35, 1.0), [55, 100, 85]),
    b("COLD STEPPE", PLAINS, (-2.0, 8.0), (0.0, 0.35), [175, 170, 115]),
    b("GRASSLAND", PLAINS, (8.0, 22.0), (0.25, 0.55), [150, 190, 90]),
    b("TEMPERATE FOREST", PLAINS, (8.0, 22.0), (0.55, 1.0), [60, 140, 60]),
    b("DESERT", PLAINS, (10.0, 45.0), (0.0, 0.2), [225, 200, 130]),
    b("SAVANNA", PLAINS, (22.0, 45.0), (0.2, 0.55), [195, 180, 80]),
    b("RAINFOREST", PLAINS, (22.0, 45.0), (0.55, 1.0), [20, 110, 50]),
    b("POLAR COAST", COAST, (-60.0, -4.0), (0.0, 1.0), [210, 225, 235]),
    b("COLD SHORE", COAST, (-4.0, 6.0), (0.0, 1.0), [150, 160, 140]),
    b("TEMPERATE MARSH", COAST, (6.0, 20.0), (0.3, 1.0), [100, 150, 110]),
    b("DUNE COAST", COAST, (6.0, 45.0), (0.0, 0.3), [230, 210, 150]),
    b("MANGROVE COAST", COAST, (20.0, 45.0), (0.3, 1.0), [60, 140, 95]),
    b("FROZEN SEA", OCEAN, (-60.0, -8.0), (0.0, 1.2), [190, 215, 235]),
    b("COLD SEA", OCEAN, (-8.0, 8.0), (0.0, 1.2), [60, 100, 150]),
    b("TEMPERATE SEA", OCEAN, (8.0, 20.0), (0.0, 1.2), [40, 100, 170]),
    b("WARM SEA", OCEAN, (20.0, 45.0), (0.0, 1.2), [30, 150, 190]),
    b("DEEP OCEAN", OCEAN, (-60.0, 45.0), (1.2, 9.0), [15, 35, 85]),
];

fn range_dist(x: f32, lo: f32, hi: f32, scale: f32) -> f32 {
    ((lo - x).max(0.0) + (x - hi).max(0.0)) / scale
}

pub fn pick_biome(family: u8, t: f32, m: f32) -> u8 {
    let mut best = (0usize, f32::MAX);
    for (i, bm) in BIOMES.iter().enumerate() {
        if bm.family != family {
            continue;
        }
        let d = range_dist(t, bm.t.0, bm.t.1, 20.0) + range_dist(m, bm.m.0, bm.m.1, 0.4);
        // Narrow boxes win on ties so the table reads as "nearest box".
        let d = d + 0.001 * (bm.t.1 - bm.t.0 + 50.0 * (bm.m.1 - bm.m.0)) / 100.0;
        if d < best.1 {
            best = (i, d);
        }
    }
    best.0 as u8
}

pub fn biomes(g: &Grid, fam: &[u8], c: &Climate, h: &[f32]) -> Vec<u8> {
    (0..g.total())
        .map(|k| {
            if fam[k] == OCEAN {
                let depth = -h[k] / 1000.0;
                if depth > 1.2 {
                    return BIOMES.len() as u8 - 1;
                }
            }
            pick_biome(fam[k], c.temp[k], c.moist[k])
        })
        .collect()
}

// ------------------------------------------------------------------ formations

pub struct Formations {
    pub cliff: Vec<f32>,
    pub spire: Vec<f32>,
    pub arch: Vec<f32>,
}

pub fn slope(g: &Grid, h: &[f32]) -> Vec<f32> {
    par_map(g.total(), |k| {
        let mut s = 0.0f32;
        for m in 0..8 {
            let nb = g.nbr[k][m] as usize;
            s = s.max((h[nb] - h[k]).abs() / g.nbr_d[k][m]);
        }
        s
    })
}

pub fn formations(g: &Grid, s: &Shape, fam: &[u8], c: &Climate, h: &[f32]) -> Formations {
    let sl = slope(g, h);
    let n = g.total();
    let mut f = Formations { cliff: vec![0.0; n], spire: vec![0.0; n], arch: vec![0.0; n] };
    for k in 0..n {
        if fam[k] == OCEAN {
            continue;
        }
        let steep = smoothstep(70.0, 240.0, f64::from(sl[k]));
        let w_cliff = [0.55, 0.95, 1.0, 0.25, 0.03, 0.5][fam[k] as usize];
        let dry = 1.0 - smoothstep(0.22, 0.5, f64::from(c.moist[k]));
        let warm = smoothstep(-5.0, 10.0, f64::from(c.temp[k]));
        let peak = smoothstep(0.5, 0.9, f64::from(s.ridge[k]));
        let cliff = w_cliff * steep;
        let spire = match fam[k] {
            RANGE => 0.95 * peak,
            CANYON => 0.9 * dry * warm,
            PLATEAU => 0.6 * dry * warm * (0.4 + 0.6 * steep),
            _ => 0.0,
        };
        let arch = match fam[k] {
            COAST => 0.65 * (0.45 + 0.55 * steep),
            CANYON => 0.8 * dry * warm,
            PLATEAU => 0.55 * dry * warm * steep,
            RANGE => 0.15 * dry * steep,
            _ => 0.0,
        };
        f.cliff[k] = cliff.clamp(0.0, 1.0) as f32;
        f.spire[k] = spire.clamp(0.0, 1.0) as f32;
        f.arch[k] = arch.clamp(0.0, 1.0) as f32;
    }
    f
}
