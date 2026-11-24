//! Throwaway benchmark for ticket #241: stream-power erosion on the real 6 x 2048^2 world map.
//! Not production code. Run: hyd-proto <size> <threads> <steps> <out-dir>

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Barrier;
use std::time::Instant;

use planet_gen::world_map::WorldMap;

const FACES: [[[i8; 3]; 3]; 6] = [
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
    [[0, 1, 0], [1, 0, 0], [0, 0, -1]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 0], [0, 1, 0]],
    [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
];

fn split(size: u32, index: usize) -> (usize, u32, u32) {
    let size = size as usize;
    let (face, rest) = (index / (size * size), index % (size * size));
    (face, (rest % size) as u32, (rest / size) as u32)
}
fn join(size: u32, face: usize, i: u32, j: u32) -> usize {
    (face * size as usize + j as usize) * size as usize + i as usize
}
/// Copied from world_map.rs.
fn step(size: u32, face: usize, i: u32, j: u32, side: usize) -> (usize, u32, u32) {
    let (along, kept_index) = if side < 2 { (i, j) } else { (j, i) };
    let delta = if side.is_multiple_of(2) { -1 } else { 1 };
    if let Some(m) = along.checked_add_signed(delta).filter(|&m| m < size) {
        return if side < 2 { (face, m, j) } else { (face, i, m) };
    }
    let [n, u, v] = FACES[face];
    let (across, kept) = if side < 2 { (u, v) } else { (v, u) };
    let across = across.map(|a| a * delta as i8);
    let next = FACES.iter().position(|f| f[0] == across).unwrap();
    let dot = |a: [i8; 3], b: [i8; 3]| (0..3).map(|k| i32::from(a[k] * b[k])).sum::<i32>();
    let coord = |axis: [i8; 3]| match dot(axis, kept) {
        0 if dot(axis, n) > 0 => size - 1,
        0 => 0,
        d if d > 0 => kept_index,
        _ => size - 1 - kept_index,
    };
    (next, coord(FACES[next][1]), coord(FACES[next][2]))
}

#[derive(Clone, Copy)]
struct Ptr<T>(*mut T);
unsafe impl<T> Send for Ptr<T> {}
unsafe impl<T> Sync for Ptr<T> {}

/// out[i] = f(i), chunked over threads; f sees only i, so the result is thread-count independent.
fn par_fill<T: Send>(out: &mut [T], threads: usize, f: impl Fn(usize) -> T + Sync) {
    let part = out.len().div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        for (n, chunk) in out.chunks_mut(part).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (k, slot) in chunk.iter_mut().enumerate() {
                    *slot = f(n * part + k);
                }
            });
        }
    });
}

/// Runs `body(node)` for every node of every level, level by level (in `dir`), with `threads`
/// persistent workers and a barrier between levels. Nodes of one level are independent.
fn par_levels(order: &[u32], levels: &[usize], reverse: bool, threads: usize, body: impl Fn(usize) + Sync) {
    let nlev = levels.len() - 1;
    let barrier = Barrier::new(threads);
    std::thread::scope(|s| {
        for t in 0..threads {
            let (body, barrier) = (&body, &barrier);
            s.spawn(move || {
                for k in 0..nlev {
                    let l = if reverse { nlev - 1 - k } else { k };
                    let (a, b) = (levels[l], levels[l + 1]);
                    let len = b - a;
                    let part = len.div_ceil(threads);
                    let lo = a + (t * part).min(len);
                    let hi = a + ((t + 1) * part).min(len);
                    for &node in &order[lo..hi] {
                        body(node as usize);
                    }
                    barrier.wait();
                }
            });
        }
    });
}

struct Sim {
    size: u32,
    n: usize,
    nbr: Vec<[u32; 4]>,
    ocean: Vec<bool>,
    dx: f64,
    cell: f64,
}

impl Sim {
    fn receivers(&self, h: &[f64], rec: &mut [u32], threads: usize) {
        par_fill(rec, threads, |i| {
            if self.ocean[i] {
                return i as u32;
            }
            let mut best = (h[i], i as u32);
            for &q in &self.nbr[i] {
                if h[q as usize] < best.0 {
                    best = (h[q as usize], q);
                }
            }
            best.1
        });
    }
    fn donors(&self, rec: &[u32], dn: &mut [[u32; 4]], dc: &mut [u8], threads: usize) {
        let mut both: Vec<([u32; 4], u8)> = vec![([0; 4], 0); self.n];
        par_fill(&mut both, threads, |i| {
            let mut d = [0u32; 4];
            let mut c = 0u8;
            for &q in &self.nbr[i] {
                if q as usize != i && rec[q as usize] as usize == i {
                    d[c as usize] = q;
                    c += 1;
                }
            }
            (d, c)
        });
        for (i, (d, c)) in both.into_iter().enumerate() {
            dn[i] = d;
            dc[i] = c;
        }
    }
    /// Breadth-first levels from the roots (nodes that are their own receiver). Serial.
    fn levels(&self, rec: &[u32], dn: &[[u32; 4]], dc: &[u8]) -> (Vec<u32>, Vec<usize>) {
        let mut order: Vec<u32> = Vec::with_capacity(self.n);
        // Ocean roots without donors are left out: they need no work.
        for i in 0..self.n {
            if rec[i] as usize == i && (dc[i] > 0 || !self.ocean[i]) {
                order.push(i as u32);
            }
        }
        let mut levels = vec![0, order.len()];
        loop {
            let (a, b) = (levels[levels.len() - 2], levels[levels.len() - 1]);
            for k in a..b {
                let i = order[k] as usize;
                for s in 0..dc[i] as usize {
                    order.push(dn[i][s]);
                }
            }
            if order.len() == b {
                break;
            }
            levels.push(order.len());
        }
        (order, levels)
    }
    /// Priority-flood routing (Barnes et al. 2014 style, without changing heights): every land
    /// texel's receiver is the texel the flood reached it from, so depressions drain through their
    /// spill. Ties are broken by index, so it is deterministic. Serial, O(n log n).
    fn flood(&self, h: &[f64], rec: &mut [u32]) -> Vec<u32> {
        let key = |x: f64| {
            let b = x.to_bits() as i64;
            if b < 0 { !b ^ i64::MIN } else { b }
        };
        let mut closed = self.ocean.clone();
        let mut heap = BinaryHeap::new();
        let mut order = Vec::with_capacity(self.n);
        for i in 0..self.n {
            rec[i] = i as u32;
            if self.ocean[i] && self.nbr[i].iter().any(|&q| !self.ocean[q as usize]) {
                heap.push(Reverse((key(h[i]), i as u32)));
                order.push(i as u32);
            }
        }
        let mut level = vec![f64::NEG_INFINITY; self.n];
        while let Some(Reverse((_, p))) = heap.pop() {
            let p = p as usize;
            let lp = level[p].max(h[p]);
            for &q in &self.nbr[p] {
                let q = q as usize;
                if !closed[q] {
                    closed[q] = true;
                    rec[q] = p as u32;
                    level[q] = lp;
                    order.push(q as u32);
                    heap.push(Reverse((key(h[q].max(lp)), q as u32)));
                }
            }
        }
        // Land not reached (no ocean at all) stays its own root.
        order
    }
}

fn hash(h: &[f64]) -> u64 {
    let mut x: u64 = 0xcbf2_9ce4_8422_2325;
    for v in h {
        for b in v.to_bits().to_le_bytes() {
            x = (x ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    x
}

struct Params {
    k: f64,
    dt: f64,
}

/// One FastScape step (Braun & Willett 2013), n = 1, m = 1/2, so F = k dt sqrt(A) / dx: only
/// + - * / and sqrt. Returns the per-stage times in ms and the number of levels.
fn fastscape_step(
    sim: &Sim,
    h: &mut Vec<f64>,
    uplift: &[f64],
    p: &Params,
    threads: usize,
    flood: bool,
    area: &mut Vec<f64>,
) -> ([f64; 5], usize) {
    let n = sim.n;
    let mut rec = vec![0u32; n];
    let mut dn = vec![[0u32; 4]; n];
    let mut dc = vec![0u8; n];
    let t0 = Instant::now();
    let mut flooded: Option<Vec<u32>> = None;
    if flood {
        flooded = Some(sim.flood(h, &mut rec));
    } else {
        sim.receivers(h, &mut rec, threads);
    }
    let t1 = Instant::now();
    sim.donors(&rec, &mut dn, &mut dc, threads);
    let t2 = Instant::now();
    let (order, levels) = match flooded {
        Some(o) => {
            let len = o.len();
            (o, vec![0, len])
        }
        None => sim.levels(&rec, &dn, &dc),
    };
    let t3 = Instant::now();
    // Drainage area: each node gathers its donors' areas (a pull, in a fixed slot order).
    area.iter_mut().for_each(|a| *a = sim.cell);
    let ap = Ptr(area.as_mut_ptr());
    if flood {
        for &i in order.iter().rev() {
            let i = i as usize;
            let r = rec[i] as usize;
            if r != i {
                area[r] += area[i];
            }
        }
    } else {
        par_levels(&order, &levels, true, threads, |i| unsafe {
            let ap = ap;
            let mut a = sim.cell;
            for s in 0..dc[i] as usize {
                a += *ap.0.add(dn[i][s] as usize);
            }
            *ap.0.add(i) = a;
        });
    }
    let t4 = Instant::now();
    // Implicit solve from the outlets upstream.
    let h0 = h.clone();
    let hp = Ptr(h.as_mut_ptr());
    let c = p.k * p.dt / sim.dx;
    let body = |i: usize| unsafe {
        let hp = hp;
        if sim.ocean[i] {
            return;
        }
        let r = rec[i] as usize;
        let up = h0[i] + uplift[i] * p.dt;
        if r == i {
            *hp.0.add(i) = up;
            return;
        }
        // The sea is the base level: an ocean receiver counts as sea level, not its sea floor.
        let hr = if sim.ocean[r] { (*hp.0.add(r)).max(0.0) } else { *hp.0.add(r) };
        if up <= hr {
            // In a lake (flood routing): no erosion.
            *hp.0.add(i) = up;
            return;
        }
        let f = c * area[i].sqrt();
        *hp.0.add(i) = (up + f * hr) / (1.0 + f);
    };
    if flood {
        for &i in &order {
            body(i as usize);
        }
    } else {
        par_levels(&order, &levels, false, threads, body);
    }
    let t5 = Instant::now();
    let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
    ([ms(t0, t1), ms(t1, t2), ms(t2, t3), ms(t3, t4), ms(t4, t5)], levels.len() - 1)
}

/// One Schott et al. 2023 style Jacobi step: drainage relaxed once (multiple flow, weights
/// proportional to slope, p = 1) and explicit stream power on the steepest receiver, clamped to
/// the receiver's height. Pure function of the previous buffers.
fn schott_step(sim: &Sim, h: &mut Vec<f64>, s: &mut Vec<f64>, uplift: &[f64], p: &Params, threads: usize) {
    let n = sim.n;
    let mut next: Vec<(f64, f64)> = vec![(0.0, 0.0); n];
    let (hh, ss) = (&*h, &*s);
    par_fill(&mut next, threads, |i| {
        if sim.ocean[i] {
            return (hh[i], sim.cell);
        }
        let mut water = sim.cell;
        for &q in &sim.nbr[i] {
            let q = q as usize;
            if hh[q] <= hh[i] {
                continue;
            }
            let mut sum = 0.0;
            for &r in &sim.nbr[q] {
                let d = hh[q] - hh[r as usize];
                if d > 0.0 {
                    sum += d;
                }
            }
            water += ss[q] * (hh[q] - hh[i]) / sum;
        }
        let mut low = (hh[i], i);
        for &q in &sim.nbr[i] {
            let hq = if sim.ocean[q as usize] { hh[q as usize].max(0.0) } else { hh[q as usize] };
            if hq < low.0 {
                low = (hq, q as usize);
            }
        }
        let slope = (hh[i] - low.0) / sim.dx;
        let e = p.k * water.sqrt() * slope;
        let nh = (hh[i] - p.dt * e).max(low.0) + p.dt * uplift[i];
        (nh, water)
    });
    for (i, (a, b)) in next.into_iter().enumerate() {
        h[i] = a;
        s[i] = b;
    }
}

fn write_png(path: &str, w: u32, hgt: u32, rgb: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, hgt);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgb).unwrap();
}

/// Hillshade of a crop of one face, with texels draining more than `river` cells in blue.
fn picture(sim: &Sim, h: &[f64], area: Option<&[f64]>, face: usize, x0: u32, y0: u32, w: u32, path: &str) {
    let mut rgb = vec![0u8; (w * w * 3) as usize];
    for y in 0..w {
        for x in 0..w {
            let (i, j) = (x0 + x, y0 + y);
            let at = |i: u32, j: u32| h[join(sim.size, face, i, j)];
            let c = at(i, j);
            let gx = (at(i + 1, j) - at(i - 1, j)) / (2.0 * sim.dx);
            let gy = (at(i, j + 1) - at(i, j - 1)) / (2.0 * sim.dx);
            let len = (gx * gx + gy * gy + 1.0).sqrt();
            let shade = ((-gx * 0.6 - gy * 0.6 + 0.53) / len).clamp(0.0, 1.0);
            let k = ((y * w + x) * 3) as usize;
            let (r, g, b) = if c < 0.0 {
                (40.0, 70.0, 120.0)
            } else {
                let t = (c / 2000.0).clamp(0.0, 1.0);
                (110.0 + 120.0 * t, 140.0 + 80.0 * t, 90.0 + 130.0 * t)
            };
            let s = 0.35 + 0.65 * shade;
            let mut px = [r * s, g * s, b * s];
            if let Some(a) = area {
                let cells = a[join(sim.size, face, i, j)] / sim.cell;
                if c >= 0.0 && cells > 400.0 {
                    px = [30.0, 90.0, 230.0];
                }
            }
            for q in 0..3 {
                rgb[k + q] = px[q].clamp(0.0, 255.0) as u8;
            }
        }
    }
    write_png(path, w, w, &rgb);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let size: u32 = args.get(1).map_or(2048, |s| s.parse().unwrap());
    let threads: usize = args.get(2).map_or(8, |s| s.parse().unwrap());
    let steps: usize = args.get(3).map_or(5, |s| s.parse().unwrap());
    let out = args.get(4).cloned().unwrap_or_else(|| "/tmp/hyd241/out".into());
    std::fs::create_dir_all(&out).unwrap();
    let seed = 0xB4A7_A9AC_u64;

    let t = Instant::now();
    let map = WorldMap::generate(seed, size);
    println!("world map {size}: {:.1} s", t.elapsed().as_secs_f64());

    let n = 6 * (size as usize).pow(2);
    let nbr: Vec<[u32; 4]> = (0..n)
        .map(|idx| {
            let (f, i, j) = split(size, idx);
            [0, 1, 2, 3].map(|s| {
                let (f, i, j) = step(size, f, i, j, s);
                join(size, f, i, j) as u32
            })
        })
        .collect();
    let base: Vec<f64> = (0..n)
        .map(|idx| {
            let (f, i, j) = split(size, idx);
            f64::from(map.texel(f, i, j).elevation) * 0.1
        })
        .collect();
    let ocean: Vec<bool> = base.iter().map(|&x| x < 0.0).collect();
    let radius = 320_000.0 * (f64::from(size) / 2048.0);
    let dx = std::f64::consts::FRAC_PI_2 * radius / f64::from(size);
    let sim = Sim { size, n, nbr, ocean, dx, cell: dx * dx };
    println!("dx {:.1} m, land {:.1}%", dx, 100.0 * sim.ocean.iter().filter(|o| !**o).count() as f64 / n as f64);

    // Face with the most land, for pictures.
    let face = (0..6)
        .max_by_key(|&f| (0..size * size).filter(|&k| !sim.ocean[f * (size * size) as usize + k as usize]).count())
        .unwrap();
    let w = (size * 3 / 8).min(768);
    let (x0, y0) = ((size - w) / 2, (size - w) / 2);
    picture(&sim, &base, None, face, x0, y0, w, &format!("{out}/0-input.png"));

    let zero = vec![0.0; n];
    let mut area = vec![0.0; n];

    let timing = args.get(6).map_or(true, |m| m != "look");
    // 1. Timing and determinism: FastScape, erosion only (no uplift).
    let p = Params { k: 2e-6, dt: 5e4 };
    let runs: &[(&str, bool, usize)] = if timing { &[("levels, 1 thread", false, 1usize), ("levels", false, threads), ("flood (serial)", true, threads)] } else { &[] };
    for &(label, flood, th) in runs {
        let mut h = base.clone();
        let mut sum = [0.0; 5];
        let mut nlev = 0;
        let t = Instant::now();
        for _ in 0..steps {
            let (ms, l) = fastscape_step(&sim, &mut h, &zero, &p, th, flood, &mut area);
            for k in 0..5 {
                sum[k] += ms[k];
            }
            nlev = l;
        }
        let total = t.elapsed().as_secs_f64() * 1e3 / steps as f64;
        println!(
            "fastscape [{label}, {th} threads]: {total:.0} ms/step (receivers {:.0}, donors {:.0}, order {:.0}, area {:.0}, solve {:.0}); levels {nlev}; hash {:016x}",
            sum[0] / steps as f64, sum[1] / steps as f64, sum[2] / steps as f64, sum[3] / steps as f64, sum[4] / steps as f64, hash(&h)
        );
    }

    // 2. Schott-style Jacobi step, timing and determinism.
    let ths: &[usize] = if timing { &[1, threads] } else { &[] };
    for &th in ths {
        let mut h = base.clone();
        let mut s = vec![sim.cell; n];
        let t = Instant::now();
        for _ in 0..steps {
            schott_step(&sim, &mut h, &mut s, &zero, &Params { k: 2e-6, dt: 2e3 }, th);
        }
        println!("schott jacobi [{th} threads]: {:.0} ms/step; hash {:016x}", t.elapsed().as_secs_f64() * 1e3 / steps as f64, hash(&h));
    }

    // 3. Looks: erosion of the existing map (amplification, no uplift), flood routing.
    let look_steps: usize = args.get(5).map_or(40, |s| s.parse().unwrap());
    {
        let mut h = base.clone();
        let t = Instant::now();
        for _ in 0..look_steps {
            fastscape_step(&sim, &mut h, &zero, &Params { k: 5e-7, dt: 2e5 }, threads, true, &mut area);
        }
        let moved: f64 = h.iter().zip(&base).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        println!("erode-only {look_steps} steps: {:.1} s, max change {moved:.0} m", t.elapsed().as_secs_f64());
        picture(&sim, &h, Some(&area), face, x0, y0, w, &format!("{out}/1-erode-only.png"));
    }
    // 4. Looks: uplift proportional to the map's height, towards steady state (Cordonnier 2016).
    {
        let uplift: Vec<f64> = base.iter().map(|&b| 2e-4 * (b / 2000.0).clamp(0.0, 1.0)).collect();
        let mut h: Vec<f64> = base.iter().map(|&b| b.min(0.0)).collect();
        let t = Instant::now();
        let mut last = 0.0;
        for k in 0..look_steps * 4 {
            let before = h.clone();
            fastscape_step(&sim, &mut h, &uplift, &Params { k: 6e-7, dt: 1e6 }, threads, true, &mut area);
            last = h.iter().zip(&before).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
            if k % 5 == 0 {
                let top = h.iter().cloned().fold(f64::MIN, f64::max);
                println!("  uplift step {k}: highest {top:.0} m, largest change {last:.1} m");
            }
        }
        let top = h.iter().cloned().fold(f64::MIN, f64::max);
        println!("uplift {} steps: {:.1} s, highest {top:.0} m, last change {last:.2} m", look_steps * 4, t.elapsed().as_secs_f64());
        picture(&sim, &h, Some(&area), face, x0, y0, w, &format!("{out}/2-uplift.png"));
    }
}
