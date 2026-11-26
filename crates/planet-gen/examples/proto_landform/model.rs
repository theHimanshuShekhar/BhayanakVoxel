//! The three land models. Each fills the same `Raw` fields; two shape functions turn
//! them into pre-erosion elevation and the per-texel Landform inputs.

use crate::grid::*;
use crate::noise::*;

pub const SEED: u32 = 0xB4A7_A9AC;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    A,
    B,
    C,
    C2,
}
impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::A => "A",
            Variant::B => "B",
            Variant::C => "C",
            Variant::C2 => "C2",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Variant::A => "A NOISE AND SPLINES",
            Variant::B => "B PLATES",
            Variant::C => "C HYBRID",
            Variant::C2 => "C2 HYBRID, A COASTS",
        }
    }
}

/// Fields every variant provides before the shape function runs.
#[derive(Default)]
pub struct Raw {
    pub c: Vec<f32>,       // continentalness, 0 = coast, -1 deep ocean, +1 far inland
    pub e: Vec<f32>,       // erosion field: low = young rugged ranges, high = worn flat
    pub psel: Vec<f32>,    // plateau selector, [-1, 1]
    pub arc: Vec<f32>,     // island-arc strength 0..1
    pub trench: Vec<f32>,  // trench strength 0..1
    pub oridge: Vec<f32>,  // mid-ocean ridge strength 0..1
    pub range_b: Vec<f32>, // plate collision belt 0..1 (B, C)
    pub plateau_b: Vec<f32>,
    pub rift_b: Vec<f32>,
    pub giant: Vec<f32>,   // C2: the one or two greatest ranges
    pub v2: bool,          // C2 tuning
    pub conv: Vec<f32>, // strength of the plate collision, for the giant-range term
}

/// Pre-erosion result: elevation plus the values that chose the Landform.
#[derive(Default)]
pub struct Shape {
    pub h: Vec<f32>,       // m
    pub uplift: Vec<f32>,  // 0..1, drives the erosion's uplift
    pub range: Vec<f32>,   // range-ness 0..1 (mountain mask x crest relief)
    pub plateau: Vec<f32>, // 0..1
    pub canyon: Vec<f32>,  // 0..1
    pub valley: Vec<f32>,  // 0..1
    pub ridge: Vec<f32>,   // crest value 0..1, for spires
    pub c: Vec<f32>,
}

// ---------------------------------------------------------------- shared noise

fn std_of(v: &[f32]) -> f32 {
    let n = v.len() as f64;
    let m: f64 = v.iter().map(|&x| f64::from(x)).sum::<f64>() / n;
    (v.iter().map(|&x| (f64::from(x) - m).powi(2)).sum::<f64>() / n).sqrt() as f32
}

/// Calibrate a continentalness-like field so that `ocean` of the area is below zero;
/// each side is rank-normalised into [-1, 0] and [0, 1].
fn calibrate_c(g: &Grid, c: &mut [f32], ocean: f64) {
    let c0 = g.quantile(c, ocean);
    let (mut lo, mut hi): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
    for (k, &x) in c.iter().enumerate() {
        if x < c0 {
            lo.push(k as u32);
        } else {
            hi.push(k as u32);
        }
    }
    let side = |ids: &mut Vec<u32>, c: &mut [f32], sign: f32| {
        ids.sort_unstable_by(|&a, &b| c[a as usize].total_cmp(&c[b as usize]));
        let total: f64 = ids.iter().map(|&k| f64::from(g.area[k as usize])).sum();
        let mut acc = 0.0;
        let vals: Vec<(u32, f32)> = ids
            .iter()
            .map(|&k| {
                let a = f64::from(g.area[k as usize]);
                let r = ((acc + 0.5 * a) / total) as f32;
                acc += a;
                (k, if sign < 0.0 { -1.0 + r } else { r })
            })
            .collect();
        for (k, v) in vals {
            c[k as usize] = v;
        }
    };
    side(&mut lo, c, -1.0);
    side(&mut hi, c, 1.0);
}

pub fn ridge_fn(p: V3, seed: u32) -> f64 {
    // Main spines: warped low-frequency ridged noise (crest = zero crossing of the noise).
    let pw = warp(p, 28.0, 1.0 / 140.0, seed + 5);
    let (n1, g1) = fbm(pw, 1.0 / 190.0, 2, 0.4, seed + 11);
    let r1 = 1.0 - (n1 * 2.6).abs().min(1.0);
    // Spurs: detail dragged along the slope of the base (Tectonic's trick).
    let q = add(pw, mul(g1, 2600.0));
    let (n2, g2) = fbm(q, 1.0 / 62.0, 2, 0.45, seed + 23);
    let r2 = 1.0 - (n2 * 2.8).abs().min(1.0);
    let q3 = add(q, mul(g2, 700.0));
    let n3 = fbm1(q3, 1.0 / 24.0, 2, 0.5, seed + 37);
    let r3 = 1.0 - (n3 * 3.0).abs().min(1.0);
    let r = r1 * (0.62 + 0.38 * r2 * (0.4 + 0.6 * r1)) * (0.88 + 0.12 * r3);
    ((r - 0.12) / 0.8).clamp(0.0, 1.0).powf(1.25)
}

// ---------------------------------------------------------------- A: noise + splines

/// A's coasts: warped continentalness noise (unit std) and the island-arc chains.
pub fn noise_land(g: &Grid) -> (Vec<f32>, Vec<f32>) {
    let n = g.total();
    let seed = SEED;
    let cn: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        let q = warp(warp(p, 70.0, 1.0 / 250.0, seed + 1), 24.0, 1.0 / 90.0, seed + 4);
        fbm1(q, 1.0 / 205.0, 4, 0.5, seed + 7) as f32
    });
    let arcs: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        let q = warp(p, 70.0, 1.0 / 190.0, seed + 31);
        let a = 1.0 - (fbm1(q, 1.0 / 165.0, 2, 0.4, seed + 33) * 3.6).abs().min(1.0);
        let m = fbm1(p, 1.0 / 90.0, 2, 0.5, seed + 35);
        (smoothstep(0.78, 0.93, a) * smoothstep(-0.25, 0.08, m)) as f32
    });
    let sd = std_of(&cn);
    (cn.iter().map(|x| x / sd).collect(), arcs)
}

pub fn raw_a(g: &Grid) -> Raw {
    let n = g.total();
    let seed = SEED;
    let (cstd, arcs) = noise_land(g);
    let mut c: Vec<f32> = (0..n)
        .map(|k| {
            let x = f64::from(cstd[k]);
            let band = smoothstep(-1.7, -1.1, x) * (1.0 - smoothstep(-0.55, -0.15, x));
            (x + 2.2 * band * f64::from(arcs[k])) as f32
        })
        .collect();
    let arc: Vec<f32> = (0..n)
        .map(|k| {
            let x = f64::from(cstd[k]);
            let band = smoothstep(-1.7, -1.1, x) * (1.0 - smoothstep(-0.55, 0.3, x));
            (band * f64::from(arcs[k])) as f32
        })
        .collect();
    calibrate_c(g, &mut c, 0.55);
    let mut e: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        let q = warp(p, 80.0, 1.0 / 260.0, seed + 41);
        fbm1(q, 1.0 / 270.0, 3, 0.5, seed + 43) as f32
    });
    g.rank_normalize(&mut e, -1.0, 1.0);
    let mut psel: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        let q = warp(p, 70.0, 1.0 / 250.0, seed + 51);
        fbm1(q, 1.0 / 230.0, 3, 0.5, seed + 53) as f32
    });
    g.rank_normalize(&mut psel, -1.0, 1.0);
    let trench: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        let m = fbm1(warp(p, 50.0, 1.0 / 200.0, seed + 61), 1.0 / 210.0, 2, 0.5, seed + 63);
        smoothstep(0.12, 0.3, m as f64) as f32
    });
    Raw {
        c,
        e,
        psel,
        arc,
        trench,
        oridge: vec![0.0; n],
        range_b: vec![0.0; n],
        plateau_b: vec![0.0; n],
        rift_b: vec![0.0; n],
        conv: vec![0.0; n],
        giant: vec![0.0; n],
        v2: false,
    }
}

// ---------------------------------------------------------------- plates (B, C)

pub struct Plates {
    pub sites: Vec<V3>, // on the sphere, radius R
    pub cont: Vec<bool>,
    pub omega: Vec<V3>,
    pub rank: Vec<f64>,
    pub group: Vec<usize>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
    fn unit(&mut self) -> V3 {
        loop {
            let v = [self.next() * 2.0 - 1.0, self.next() * 2.0 - 1.0, self.next() * 2.0 - 1.0];
            if len(v) > 0.1 && len(v) <= 1.0 {
                return norm(v);
            }
        }
    }
}

const N_PLATES: usize = 36;

fn plate_warp(p: V3) -> V3 {
    warp(warp(p, 70.0, 1.0 / 230.0, SEED + 71), 22.0, 1.0 / 75.0, SEED + 75)
}

fn envf(k: &str, d: f64) -> f64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn plate_warp2(p: V3) -> V3 {
    warp(warp(p, 78.0, 1.0 / 240.0, SEED + 71), 26.0, 1.0 / 78.0, SEED + 75)
}

pub fn make_plates(g: &Grid, compact: bool) -> Plates {
    let mut rng = Rng(u64::from(SEED) * 7919 + 13);
    // Best-candidate sites: evenly spread but not a lattice.
    let mut sites: Vec<V3> = vec![mul(rng.unit(), R)];
    while sites.len() < N_PLATES {
        let mut best = ([0.0; 3], -1.0);
        for _ in 0..24 {
            let c = mul(rng.unit(), R);
            let d = sites.iter().map(|s| len(sub(*s, c))).fold(f64::MAX, f64::min);
            if d > best.1 {
                best = (c, d);
            }
        }
        sites.push(best.0);
    }
    // Plate areas.
    let own: Vec<u8> = par_map(g.total(), |k| {
        let q = plate_warp(g.position(k));
        let mut bi = 0;
        let mut bd = f64::MAX;
        for (i, s) in sites.iter().enumerate() {
            let d = len(sub(q, *s));
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        bi as u8
    });
    let mut area = vec![0.0f64; N_PLATES];
    for k in 0..g.total() {
        area[own[k] as usize] += f64::from(g.area[k]);
    }
    let total: f64 = area.iter().sum();
    let mut adj = vec![vec![false; N_PLATES]; N_PLATES];
    for k in 0..g.total() {
        for &m in &g.nbr[k] {
            let (a, b) = (own[k] as usize, own[m as usize] as usize);
            if a != b {
                adj[a][b] = true;
            }
        }
    }
    // Continents: grow K seed plates through their nearest unassigned neighbours.
    let k_cont = if compact { envf("LP_K", 8.0) as usize } else { 7usize };
    let mut cont = vec![false; N_PLATES];
    let mut group = vec![usize::MAX; N_PLATES];
    let mut seeds: Vec<usize> = Vec::new();
    while seeds.len() < k_cont {
        let mut best = (0, -1.0);
        for i in 0..N_PLATES {
            if seeds.contains(&i) || seeds.iter().any(|&s| adj[s][i]) {
                continue;
            }
            let d = seeds.iter().map(|&s| len(sub(sites[s], sites[i]))).fold(f64::MAX, f64::min);
            let d = if seeds.is_empty() { rng.next() } else { d * (0.7 + 0.6 * rng.next()) };
            if d > best.1 {
                best = (i, d);
            }
        }
        seeds.push(best.0);
        cont[best.0] = true;
        group[best.0] = seeds.len() - 1;
    }
    let target = if compact { envf("LP_T", 0.48) } else { 0.58 } * total;
    let mut have: f64 = seeds.iter().map(|&s| area[s]).sum();
    let mut turn = 0;
    while have < target {
        let gi = turn % k_cont;
        turn += 1;
        let mut best = (usize::MAX, f64::MAX);
        // Compact continents: grow towards the group's centroid, and stop a group at its area cap.
        let members: Vec<usize> = (0..N_PLATES).filter(|&j| group[j] == gi).collect();
        let garea: f64 = members.iter().map(|&j| area[j]).sum();
        if compact && garea > envf("LP_CAP", 1.0) * target / k_cont as f64 {
            if turn > 200 {
                break;
            }
            continue;
        }
        let cen = norm(members.iter().fold([0.0; 3], |a, &j| add(a, norm(sites[j]))));
        for i in 0..N_PLATES {
            if cont[i] {
                continue;
            }
            for j in 0..N_PLATES {
                if group[j] == gi {
                    let d = if compact { len(sub(sites[i], mul(cen, R))) + 0.25 * len(sub(sites[i], sites[j])) } else { len(sub(sites[i], sites[j])) };
                    if d < best.1 {
                        best = (i, d);
                    }
                }
            }
        }
        if best.0 == usize::MAX {
            break;
        }
        cont[best.0] = true;
        group[best.0] = gi;
        have += area[best.0];
        if turn > 200 {
            break;
        }
    }
    if std::env::var("LP_DEBUG").is_ok() {
        eprintln!("plates cont={} groups={:?} cont area share {:.3}", cont.iter().filter(|&&c| c).count(), group.iter().enumerate().filter(|(_, gr)| **gr != usize::MAX).map(|(i, &gr)| (i, gr)).collect::<Vec<_>>(), (0..N_PLATES).filter(|&i| cont[i]).map(|i| area[i]).sum::<f64>() / total);
    }
    let omega: Vec<V3> = (0..N_PLATES).map(|_| mul(rng.unit(), 0.45 + 0.55 * rng.next())).collect();
    let rank: Vec<f64> = (0..N_PLATES).map(|_| rng.next()).collect();
    Plates { sites, cont, omega, rank, group }
}

pub fn raw_plates(g: &Grid, pl: &Plates, v2: bool) -> Raw {
    let n = g.total();
    let seed = SEED;
    #[derive(Clone, Default)]
    struct Out {
        sc: f32,
        range: f32,
        plateau: f32,
        rift: f32,
        arc: f32,
        trench: f32,
        oridge: f32,
        conv: f32,
    }
    let strait = envf("LP_S", 60.0);
    let out: Vec<Out> = par_map(n, |k| {
        let p = g.position(k);
        let q = if v2 { plate_warp2(p) } else { plate_warp(p) };
        let qh = norm(q);
        let mut d2 = [0.0f64; N_PLATES];
        let mut own = 0;
        for (i, s) in pl.sites.iter().enumerate() {
            let d = sub(q, *s);
            d2[i] = dot(d, d);
            if d2[i] < d2[own] {
                own = i;
            }
        }
        let mut o = Out::default();
        let mut coast = f64::MAX;
        let own_cont = pl.cont[own];
        let v_own = cross(pl.omega[own], qh);
        for j in 0..N_PLATES {
            if j == own {
                continue;
            }
            let sep = len(sub(pl.sites[j], pl.sites[own]));
            let db = (d2[j] - d2[own]) / (2.0 * sep);
            if pl.cont[j] != own_cont {
                coast = coast.min(db);
            } else if own_cont && pl.group[j] != pl.group[own] {
                // Touching continents get a strait between them.
                coast = coast.min(db - if v2 { strait } else { 38.0 });
            }
            if db > 380.0 {
                continue;
            }
            let dirn = sub(pl.sites[j], pl.sites[own]);
            let t = norm(sub(dirn, mul(qh, dot(dirn, qh))));
            let v_j = cross(pl.omega[j], qh);
            let conv = (dot(sub(v_own, v_j), t) / 1.25).clamp(-1.0, 1.0);
            let over = pl.rank[own] > pl.rank[j]; // own plate overrides
            let (cn, dv) = (conv.max(0.0), (-conv).max(0.0));
            match (own_cont, pl.cont[j]) {
                (true, true) => {
                    o.range = o.range.max((cn * gauss(db, 0.0, 52.0)) as f32);
                    if over {
                        o.plateau = o.plateau.max((cn * gauss(db, 150.0, 85.0)) as f32);
                    }
                    o.rift = o.rift.max((dv * gauss(db, 0.0, 26.0)) as f32);
                    o.conv = o.conv.max(cn as f32);
                }
                (true, false) => {
                    o.range = o.range.max((cn * gauss(db, 40.0, 38.0)) as f32);
                    o.plateau = o.plateau.max((0.7 * cn * gauss(db, 150.0, 80.0)) as f32);
                    o.conv = o.conv.max((0.7 * cn) as f32);
                }
                (false, true) => {
                    o.trench = o.trench.max((cn * gauss(db, 22.0, 30.0)) as f32);
                }
                (false, false) => {
                    if over {
                        o.arc = o.arc.max((cn * gauss(db, 72.0, 17.0)) as f32);
                    } else {
                        o.trench = o.trench.max((cn * gauss(db, 20.0, 28.0)) as f32);
                    }
                    o.oridge = o.oridge.max((dv * gauss(db, 0.0, 45.0)) as f32);
                }
            }
        }
        let sc = if own_cont { coast } else { -coast };
        o.sc = sc.clamp(-420.0, 420.0) as f32;
        o
    });
    let noise: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        let a = fbm1(warp(p, 45.0, 1.0 / 150.0, seed + 81), 1.0 / 150.0, 3, 0.5, seed + 83);
        let b = fbm1(p, 1.0 / 50.0, 2, 0.5, seed + 85);
        (a * 0.9 + b * 0.45) as f32
    });
    // Continentalness: signed distance to the coast, roughened; then calibrated to 55% sea.
    let (_, arcs_n) = if v2 { noise_land(g) } else { (Vec::new(), Vec::new()) };
    // C2: A's coast-shaping noise (same warps, finer scale), so coasts wander like A's.
    let cstd: Vec<f32> = if v2 {
        let hf: Vec<f32> = par_map(n, |k| {
            let p = g.position(k);
            let q = warp(warp(p, 70.0, 1.0 / 250.0, seed + 1), 24.0, 1.0 / 90.0, seed + 4);
            fbm1(q, 1.0 / 115.0, 4, 0.5, seed + 8) as f32
        });
        let sd = std_of(&hf);
        hf.iter().map(|x| x / sd).collect()
    } else {
        Vec::new()
    };
    let (wn, bh0, bh1, ah) = (envf("LP_W", 0.25), envf("LP_B0", -0.6), envf("LP_B1", -0.3), envf("LP_AH", 1.5));
    let mut c: Vec<f32> = (0..n)
        .map(|k| {
            let sc = f64::from(out[k].sc);
            // Arcs and island chains lift themselves clear of the sea.
            let isl = f64::from(out[k].arc);
            if v2 {
                // Plates place the continents; A's warped noise draws the coasts and the archipelagos.
                let base = 0.9 * (sc / 190.0).clamp(-1.4, 1.4) + wn * f64::from(cstd[k]);
                let band = smoothstep(-1.1, -0.6, base) * (1.0 - smoothstep(bh0, bh1, base));
                (base + ah * band * f64::from(arcs_n[k]) + 2.0 * isl) as f32
            } else {
                (sc / 130.0 + f64::from(noise[k]) * 0.8 + 2.0 * isl) as f32
            }
        })
        .collect();
    calibrate_c(g, &mut c, 0.55);
    // Erosion field and plateau selector: noise biased by the plates.
    let mut en: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        fbm1(warp(p, 80.0, 1.0 / 260.0, seed + 41), 1.0 / 270.0, 3, 0.5, seed + 43) as f32
    });
    g.rank_normalize(&mut en, -1.0, 1.0);
    let mut ps: Vec<f32> = par_map(n, |k| {
        let p = g.position(k);
        fbm1(warp(p, 70.0, 1.0 / 250.0, seed + 51), 1.0 / 230.0, 3, 0.5, seed + 53) as f32
    });
    g.rank_normalize(&mut ps, -1.0, 1.0);
    Raw {
        c,
        e: en,
        psel: ps,
        arc: (0..n)
            .map(|k| {
                let plate = out[k].arc * smoothstep(-0.12, 0.2, fbm1(g.position(k), 1.0 / 85.0, 2, 0.5, seed + 91)) as f32;
                if v2 {
                    let base = 0.9 * f64::from(out[k].sc / 190.0).clamp(-1.4, 1.4) + wn * f64::from(cstd[k]);
                    let band = smoothstep(-1.1, -0.6, base) * (1.0 - smoothstep(-0.3, 0.2, base));
                    plate.max((band * f64::from(arcs_n[k])) as f32)
                } else {
                    plate
                }
            })
            .collect(),
        trench: out.iter().map(|o| o.trench).collect(),
        oridge: out.iter().map(|o| o.oridge).collect(),
        range_b: out.iter().map(|o| o.range).collect(),
        plateau_b: out.iter().map(|o| o.plateau).collect(),
        rift_b: out.iter().map(|o| o.rift).collect(),
        conv: out.iter().map(|o| o.conv).collect(),
        giant: vec![0.0; n],
        v2,
    }
}

/// C: plates give the continents and the lines of the big ranges; noise gives the rest.
pub fn raw_c(g: &Grid, pl: &Plates) -> Raw {
    let mut r = raw_plates(g, pl, false);
    let n = g.total();
    for k in 0..n {
        let belt = f64::from(r.range_b[k]).clamp(0.0, 1.0);
        let e = f64::from(r.e[k]) * 0.8 - 1.5 * belt * (0.5 + 0.5 * f64::from(r.conv[k])) + 0.1;
        r.e[k] = e.clamp(-2.2, 1.0) as f32;
        let ps = f64::from(r.psel[k]) + 1.5 * f64::from(r.plateau_b[k]);
        r.psel[k] = ps.clamp(-1.0, 1.4) as f32;
    }
    r
}

/// C2: C with A's coasts and archipelagos, fewer plateaus, and only the greatest one or two ranges
/// reaching 6-8 km.
pub fn raw_c2(g: &Grid, pl: &Plates) -> Raw {
    let mut r = raw_plates(g, pl, true);
    let n = g.total();
    for k in 0..n {
        let belt = f64::from(r.range_b[k]).clamp(0.0, 1.0);
        let e = f64::from(r.e[k]) * 0.8 - 1.5 * belt * (0.5 + 0.5 * f64::from(r.conv[k])) + 0.1;
        r.e[k] = e.clamp(-2.2, 1.0) as f32;
        let ps = f64::from(r.psel[k]) + envf("LP_PW", 1.2) * f64::from(r.plateau_b[k]);
        r.psel[k] = ps.clamp(-1.0, 1.4) as f32;
    }
    // The two strongest collision points, at least a quarter of the planet apart, mark the giants.
    let score: Vec<f32> = (0..n).map(|k| r.range_b[k] * r.conv[k] * smoothstep(-0.02, 0.1, f64::from(r.c[k])) as f32).collect();
    let k1 = (0..n).max_by(|&a, &b| score[a].total_cmp(&score[b])).unwrap();
    let k2 = (0..n)
        .filter(|&k| dot(g.dir[k], g.dir[k1]) < 0.15)
        .max_by(|&a, &b| score[a].total_cmp(&score[b]))
        .unwrap();
    if std::env::var("LP_DEBUG").is_ok() {
        for &k in &[k1, k2] {
            eprintln!("giant at lat {:.0} lon {:.0}: c {:.2} e {:.2} range_b {:.2} conv {:.2}", g.dir[k][1].asin().to_degrees(), g.dir[k][2].atan2(g.dir[k][0]).to_degrees(), r.c[k], r.e[k], r.range_b[k], r.conv[k]);
        }
    }
    for k in 0..n {
        let near = |c: usize| {
            let d = dot(g.dir[k], g.dir[c]).clamp(-1.0, 1.0).acos() * R;
            gauss(d, 0.0, 85.0)
        };
        let m = near(k1).max(near(k2));
        r.giant[k] = (m * f64::from(r.range_b[k]).sqrt().min(1.0)) as f32;
    }
    r
}

// ---------------------------------------------------------------- shape functions

struct Splines {
    base: Spline,
    amp: Spline,
}
fn splines() -> Splines {
    Splines {
        // continentalness -> sea floor / lowland offset, metres
        base: Spline::new(&[
            (-1.0, -3000.0),
            (-0.62, -2550.0),
            (-0.34, -1900.0),
            (-0.20, -330.0),
            (-0.09, -130.0),
            (0.0, 0.0),
            (0.12, 110.0),
            (0.4, 260.0),
            (0.75, 420.0),
            (1.0, 520.0),
        ]),
        // erosion field -> crest amplitude of a range, metres
        amp: Spline::new(&[
            (-2.2, 8300.0),
            (-1.6, 7500.0),
            (-1.2, 6100.0),
            (-0.92, 4900.0),
            (-0.7, 3700.0),
            (-0.45, 2500.0),
            (-0.2, 1200.0),
            (0.1, 300.0),
            (1.0, 0.0),
        ]),
    }
}

/// A and C: continentalness, erosion and ridge noise through monotone splines.
pub fn shape_spline(g: &Grid, r: &Raw) -> Shape {
    let n = g.total();
    let sp = splines();
    #[derive(Clone, Default)]
    struct O {
        h: f32,
        up: f32,
        range: f32,
        plateau: f32,
        canyon: f32,
        valley: f32,
        ridge: f32,
    }
    let seed = SEED;
    let o: Vec<O> = par_map(n, |k| {
        let p = g.position(k);
        let c = f64::from(r.c[k]);
        let e = f64::from(r.e[k]);
        let ps = f64::from(r.psel[k]);
        let land = smoothstep(-0.02, 0.05, c);
        let inland = smoothstep(0.015, 0.22, c);
        // Mountain mask: rugged erosion field, away from the sea.
        let rugged = 1.0 - smoothstep(-0.8, -0.1, e);
        let mt = rugged * inland;
        let rid = ridge_fn(p, seed + 100);
        let amp = if r.v2 { sp.amp.eval(e).min(4600.0) } else { sp.amp.eval(e) };
        let giant = if r.v2 { envf("LP_G", 4700.0) * f64::from(r.giant[k]) * (0.15 + 0.85 * rid) * smoothstep(0.0, 0.08, c) } else { 0.0 };
        let crest = mt * amp * (0.06 + 0.94 * rid) + giant;
        // Plateau: a raised tableland with a steep rim and terraces.
        let (plo, phi) = if r.v2 { (0.42, 0.78) } else { (0.26, 0.62) };
        let pm = smoothstep(plo, phi, ps) * smoothstep(0.12, 0.35, c) * (1.0 - smoothstep(0.35, 0.8, mt));
        let terr = smoothstep(0.62, 0.74, ps);
        let plat = pm * (1050.0 + 450.0 * terr + 120.0 * fbm1(p, 1.0 / 40.0, 2, 0.5, seed + 120));
        // Canyons: narrow cuts along the zero line of a warped noise, inside tablelands and uplands.
        let cq = warp(p, 45.0, 1.0 / 150.0, seed + 131);
        let cg = fbm1(cq, 1.0 / 170.0, 2, 0.5, seed + 133);
        let cgate = smoothstep(-0.3, -0.05, fbm1(p, 1.0 / 110.0, 2, 0.5, seed + 135));
        let cmask = smoothstep(0.25, 0.6, pm.max(0.7 * smoothstep(0.3, 0.7, c) * (1.0 - mt))) * cgate;
        let cprof = 1.0 - smoothstep(0.0, 0.05, cg.abs());
        let canyon = cmask * cprof;
        // Valleys: broad cuts along the foot and through the middle of the ranges.
        let vq = warp(p, 55.0, 1.0 / 170.0, seed + 141);
        let vg = fbm1(vq, 1.0 / 230.0, 2, 0.5, seed + 143);
        let vmask = smoothstep(0.03, 0.2, rugged * inland) * (1.0 - smoothstep(0.55, 0.95, mt));
        let valley = vmask * (1.0 - smoothstep(0.0, 0.06, vg.abs()));
        let base = sp.base.eval(c);
        let hills = fbm1(p, 1.0 / 26.0, 3, 0.5, seed + 150) + 1.6 * fbm1(p, 1.0 / 110.0, 2, 0.5, seed + 151);
        let rough = 70.0 + 220.0 * (1.0 - smoothstep(-0.3, 0.2, e));
        let mut h = base + crest + plat * land + hills * rough * land - 1300.0 * canyon - 750.0 * valley * (1.0 - canyon);
        // Island arcs (A: from noise; C: from plates) and trenches.
        let arc = f64::from(r.arc[k]);
        h += arc * (700.0 + 900.0 * rid) * smoothstep(-0.2, 0.02, c).max(0.5);
        let ocean = 1.0 - smoothstep(-0.12, -0.02, c);
        let tr = if r.v2 {
            f64::from(r.trench[k]) * ocean * smoothstep(-0.85, -0.4, c) * (1.0 - smoothstep(-0.3, -0.15, c))
        } else {
            f64::from(r.trench[k]) * ocean * smoothstep(-0.42, -0.3, c) * (1.0 - smoothstep(-0.25, -0.12, c))
        };
        h -= if r.v2 { 1750.0 } else { 1900.0 } * tr;
        h += 1300.0 * f64::from(r.oridge[k]) * ocean * smoothstep(-0.9, -0.5, c);
        O {
            h: h as f32,
            up: (mt * (0.2 + 0.8 * rid) + 0.5 * pm).clamp(0.0, 1.0) as f32,
            range: (mt * (amp / 3200.0).min(1.0) * (0.3 + 0.7 * rid)).max(arc * 0.9).min(1.0) as f32,
            plateau: pm as f32,
            canyon: canyon as f32,
            valley: valley as f32,
            ridge: (rid * mt) as f32,
        }
    });
    collect(&o, r, |x| (x.h, x.up, x.range, x.plateau, x.canyon, x.valley, x.ridge))
}

fn collect<T>(o: &[T], r: &Raw, f: impl Fn(&T) -> (f32, f32, f32, f32, f32, f32, f32)) -> Shape {
    let mut s = Shape { c: r.c.clone(), ..Default::default() };
    for x in o {
        let (h, u, ra, pl, ca, va, ri) = f(x);
        s.h.push(h);
        s.uplift.push(u);
        s.range.push(ra);
        s.plateau.push(pl);
        s.canyon.push(ca);
        s.valley.push(va);
        s.ridge.push(ri);
    }
    s
}

/// B: heights straight from the plate belts; no splines, only simple profiles.
pub fn shape_plate(g: &Grid, r: &Raw) -> Shape {
    let n = g.total();
    let seed = SEED;
    #[derive(Clone, Default)]
    struct O {
        h: f32,
        up: f32,
        range: f32,
        plateau: f32,
        canyon: f32,
        valley: f32,
        ridge: f32,
    }
    let o: Vec<O> = par_map(n, |k| {
        let p = g.position(k);
        let c = f64::from(r.c[k]);
        let ocean = 1.0 - smoothstep(-0.03, 0.0, c);
        let rid = ridge_fn(p, seed + 100);
        // Belts: a ridged-noise crest laid along the belt line.
        let belt = f64::from(r.range_b[k]);
        let conv = f64::from(r.conv[k]);
        let on_land = smoothstep(-0.03, 0.04, c);
        let bmask = smoothstep(0.04, 0.5, belt) * on_land;
        let bamp = 1500.0 + 7200.0 * conv.powf(1.5);
        let crest = bmask * bamp * (0.1 + 0.9 * rid) * (0.35 + 0.65 * smoothstep(0.1, 0.9, belt));
        let hills_f = fbm1(p, 1.0 / 28.0, 3, 0.5, seed + 150);
        let pm = f64::from(r.plateau_b[k]).clamp(0.0, 1.0) * smoothstep(0.05, 0.3, c) * (1.0 - smoothstep(0.5, 0.9, bmask));
        let plat = pm * (1700.0 + 260.0 * fbm1(p, 1.0 / 45.0, 2, 0.5, seed + 120));
        let cq = warp(p, 45.0, 1.0 / 150.0, seed + 131);
        let cg = fbm1(cq, 1.0 / 170.0, 2, 0.5, seed + 133);
        let cmask = smoothstep(0.2, 0.5, pm) * smoothstep(-0.3, -0.05, fbm1(p, 1.0 / 110.0, 2, 0.5, seed + 135));
        let canyon = cmask * (1.0 - smoothstep(0.0, 0.05, cg.abs()));
        let rift = f64::from(r.rift_b[k]).clamp(0.0, 1.0) * on_land;
        let vq = warp(p, 55.0, 1.0 / 170.0, seed + 141);
        let vg = fbm1(vq, 1.0 / 230.0, 2, 0.5, seed + 143);
        let vmask = smoothstep(0.03, 0.25, belt) * (1.0 - smoothstep(0.5, 0.9, belt)) * on_land;
        let valley_line = vmask * (1.0 - smoothstep(0.0, 0.06, vg.abs()));
        let valley = rift.max(valley_line);
        // Profiles.
        let shelf = -140.0 - 40.0 * smoothstep(-0.2, 0.0, c);
        let ocean_floor = shelf + (-2600.0 - shelf) * smoothstep(-0.14, -0.4, c);
        let cont_base = 90.0 + 420.0 * c.clamp(0.0, 1.0).powf(0.7);
        let mut h = if c < 0.0 { ocean_floor } else { cont_base };
        h += (hills_f * (90.0 + 160.0 * smoothstep(0.0, 0.6, bmask.max(pm)))) * on_land;
        h += crest + plat * on_land - 1300.0 * canyon - 800.0 * rift - 650.0 * valley_line * (1.0 - canyon);
        let arc = f64::from(r.arc[k]);
        h += arc * (900.0 + 1700.0 * rid);
        h -= 2000.0 * f64::from(r.trench[k]) * ocean * smoothstep(-0.75, -0.2, c);
        h += 1500.0 * f64::from(r.oridge[k]) * ocean;
        O {
            h: h as f32,
            up: (bmask * (0.3 + 0.7 * rid) + 0.5 * pm).clamp(0.0, 1.0) as f32,
            range: (bmask * (0.35 + 0.65 * conv.max(0.3)) * (0.3 + 0.7 * rid)).max(arc * 0.9).min(1.0) as f32,
            plateau: pm as f32,
            canyon: canyon as f32,
            valley: valley as f32,
            ridge: (rid * bmask) as f32,
        }
    });
    collect(&o, r, |x| (x.h, x.up, x.range, x.plateau, x.canyon, x.valley, x.ridge))
}
