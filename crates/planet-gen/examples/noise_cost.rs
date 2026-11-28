//! Measures what the noise toolkit (`planet_gen::noise`, #289) costs per sample, against today's
//! `value_noise`, cached and uncached, on one core. It prints a table of ns per sample and a
//! digest of every output, so that two builds can be compared bit for bit.
//!
//! `cargo run --release -p planet-gen --example noise_cost -- [SAMPLES]`
//!
//! "Random" points are spread with no cell reuse, so the cache never hits. "Coherent" points are an
//! 8³ walk of Voxels at 5 cm over a 10 cm lattice, the pattern of `Detail::height`, so nearly
//! every sample shares its cell with the last. The research for #240 measured 11.9 against 7.9 ns
//! cached and 17.9 against 29.0 ns uncached for a single octave.

use std::hint::black_box;
use std::time::Instant;

use planet_gen::noise::{
    Child, Fbm, GradientCache, Knot, LatticeFbm, Ridges, Spline, Warp, gradient, gradient_d,
    lattice_gradient, lattice_gradient_d,
};

// ---------------------------------------------------------------------------------------------
// Today's value noise, copied from `crates/planet-gen/src/lib.rs` (private there).

fn hash(seed: u64, point: [i32; 3]) -> u64 {
    fn mix(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    point
        .into_iter()
        .fold(mix(seed), |h, c| mix(h ^ u64::from(c as u32)))
}

#[derive(Clone, Copy)]
struct Corners {
    lattice: [i32; 3],
    values: [f64; 8],
}

fn corner_value(seed: u64, lattice: [i32; 3]) -> f64 {
    (hash(seed, lattice) >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

fn value_noise(seed: u64, point: [i32; 3], cell: i32, cache: &mut Option<Corners>) -> f64 {
    let shift = cell.trailing_zeros();
    let lattice = point.map(|p| p >> shift);
    let inverse = 1.0 / f64::from(cell);
    let frac = point.map(|p| f64::from(p & (cell - 1)) * inverse);
    let fade = frac.map(|t| t * t * (3.0 - 2.0 * t));
    let corners = match cache {
        Some(corners) if corners.lattice == lattice => *corners,
        _ => {
            let values = std::array::from_fn(|i| {
                let [dx, dy, dz] = [i & 1, i >> 1 & 1, i >> 2].map(|d| d as i32);
                corner_value(seed, [lattice[0] + dx, lattice[1] + dy, lattice[2] + dz])
            });
            *cache.insert(Corners { lattice, values })
        }
    };
    let corner = |dx: usize, dy: usize, dz: usize| corners.values[dx | dy << 1 | dz << 2];
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x = |dy, dz| lerp(corner(0, dy, dz), corner(1, dy, dz), fade[0]);
    let y = |dz| lerp(x(0, dz), x(1, dz), fade[1]);
    lerp(y(0), y(1), fade[2]) * 2.0 - 1.0
}

/// The detail's 7 octaves as today's value noise makes them: (cell in lattice points, amplitude).
const OCTAVES: [(i32, f64); 7] = [
    (1024, 64.0),
    (512, 32.0),
    (256, 16.0),
    (128, 8.0),
    (64, 4.0),
    (32, 2.0),
    (16, 1.0),
];

fn value_fbm(seed: u64, point: [i32; 3], caches: &mut [Option<Corners>; 7]) -> f64 {
    let mut total = 0.0;
    for (k, ((cell, amplitude), cache)) in (0u64..).zip(OCTAVES.into_iter().zip(caches)) {
        total += amplitude * value_noise(planet_gen::noise::salt(seed, k), point, cell, cache);
    }
    total
}

// ---------------------------------------------------------------------------------------------
// The harness.

fn splitmix(i: u64) -> u64 {
    let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn unit(i: u64) -> f64 {
    (splitmix(i) >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

struct Report {
    digest: u64,
    baseline: f64,
}

impl Report {
    /// Times `f` over `n` samples, best of 5, and returns ns per sample and the digest of its
    /// outputs.
    fn time(&mut self, n: usize, mut f: impl FnMut(usize) -> f64) -> (f64, u64) {
        let mut best = f64::INFINITY;
        let mut digest = 0u64;
        for _ in 0..5 {
            let mut d = 0xcbf2_9ce4_8422_2325u64;
            let started = Instant::now();
            for i in 0..n {
                let v = black_box(f(black_box(i)));
                d = (d ^ v.to_bits()).wrapping_mul(0x0000_0100_0000_01b3);
            }
            best = best.min(started.elapsed().as_nanos() as f64 / n as f64);
            digest = d;
        }
        self.digest = (self.digest ^ digest).wrapping_mul(0x0000_0100_0000_01b3);
        (best, digest)
    }

    fn print(&self, name: &str, (ns, digest): (f64, u64)) {
        println!(
            "{name:<62} {ns:>9.1} ns {:>6.2}x  {digest:016x}",
            ns / self.baseline
        );
    }

    /// A row, divided by the group's baseline row.
    fn run(&mut self, name: &str, n: usize, f: impl FnMut(usize) -> f64) {
        let row = self.time(n, f);
        self.print(name, row);
    }

    /// Starts a group whose first row is the baseline the others are divided by.
    fn baseline(&mut self, name: &str, n: usize, f: impl FnMut(usize) -> f64) {
        let row = self.time(n, f);
        self.baseline = row.0;
        self.print(name, row);
    }
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .map_or(1 << 20, |s| s.parse().expect("SAMPLES"));
    let seed = 0xB4A7_A9ACu64;
    let random: Vec<[i32; 3]> = (0..n)
        .map(|i| [0u64, 1, 2].map(|a| (splitmix(i as u64 * 3 + a) >> 42) as i32 - (1 << 21)))
        .collect();
    let coherent: Vec<[i32; 3]> = (0..n)
        .map(|i| {
            let brick = (i / 512) as u64;
            let o = [0u64, 1, 2].map(|a| (splitmix(brick * 3 + a + 99) >> 44) as i32);
            [
                o[0] + (i % 8) as i32 / 2,
                o[1] + (i / 8 % 8) as i32 / 2,
                o[2] + (i / 64 % 8) as i32 / 2,
            ]
        })
        .collect();
    let floats: Vec<[f64; 3]> = random
        .iter()
        .map(|p| p.map(|c| f64::from(c) / 64.0))
        .collect();
    let sphere: Vec<[f64; 3]> = (0..n)
        .map(|i| {
            let v = [0u64, 1, 2].map(|a| unit(i as u64 * 3 + a + 17) * 2.0 - 1.0);
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
            v.map(|c| c / len)
        })
        .collect();
    let spline_in: Vec<f64> = (0..n).map(|i| unit(i as u64 + 7) * 2.4 - 1.2).collect();

    let mut r = Report {
        digest: 0,
        baseline: 1.0,
    };
    println!(
        "{n} samples, best of 5, one thread\n{:<62} {:>12} {:>7}  digest",
        "case", "per sample", "vs today"
    );

    println!("-- one octave, random points (the cache never hits), cell 64");
    r.baseline("today: value_noise", n, |i| {
        value_noise(seed, random[i], 64, &mut None)
    });
    r.run("gradient noise, hashed corners (no cache)", n, |i| {
        lattice_gradient(seed, random[i], 64, &mut GradientCache::default())
    });
    r.run("gradient noise + analytic gradient (no cache)", n, |i| {
        let (v, g) = lattice_gradient_d(seed, random[i], 64, &mut GradientCache::default());
        v + g[0] + g[1] + g[2]
    });
    r.run("gradient noise at a float point", n, |i| {
        gradient(seed, floats[i])
    });
    r.run(
        "gradient noise at a float point + analytic gradient",
        n,
        |i| {
            let (v, g) = gradient_d(seed, floats[i]);
            v + g[0] + g[1] + g[2]
        },
    );

    println!("-- one octave, coherent points (an 8³ brick walk), cell 64");
    let mut today = None;
    r.baseline("today: value_noise with its corner cache", n, |i| {
        value_noise(seed, coherent[i], 64, &mut today)
    });
    let mut cache = GradientCache::default();
    r.run("gradient noise with its gradient cache", n, |i| {
        lattice_gradient(seed, coherent[i], 64, &mut cache)
    });
    let mut cache = GradientCache::default();
    r.run(
        "gradient noise + analytic gradient with its cache",
        n,
        |i| {
            let (v, g) = lattice_gradient_d(seed, coherent[i], 64, &mut cache);
            v + g[0] + g[1] + g[2]
        },
    );

    println!("-- the detail's 7 octaves (cells 1024 to 16)");
    let mut caches = [None; 7];
    r.baseline("today: 7 value_noise octaves, coherent, cached", n, |i| {
        value_fbm(seed, coherent[i], &mut caches)
    });
    let lattice = LatticeFbm::new(seed, &OCTAVES);
    let mut cache = lattice.cache();
    r.run("7 gradient octaves, coherent, cached", n, |i| {
        lattice.value(coherent[i], &mut cache)
    });
    let mut cache = lattice.cache();
    r.run("7 gradient octaves + gradient, coherent, cached", n, |i| {
        let (v, g) = lattice.value_gradient(coherent[i], &mut cache);
        v + g[0] + g[1] + g[2]
    });
    r.baseline("today: 7 value_noise octaves, random, uncached", n, |i| {
        value_fbm(seed, random[i], &mut [None; 7])
    });
    r.run("7 gradient octaves, random, uncached", n, |i| {
        lattice.value(random[i], &mut lattice.cache())
    });
    // The bound a brick would take: one value at the box's centre.
    r.run(
        "the centre-plus-gradient bound of a brick (8 points)",
        n,
        |i| {
            let low = coherent[i];
            let (lo, hi) = lattice.bounds(low, low.map(|c| c + 4));
            lo + hi
        },
    );

    println!("-- the world map's pieces, on the sphere (per texel)");
    let fbm = Fbm::new(seed, 2.0, 7, 0.5, true);
    r.baseline("7-octave fBm, rotated (7 noise evaluations)", n, |i| {
        fbm.value(sphere[i])
    });
    r.run("the same with its analytic gradient", n, |i| {
        let (v, g) = fbm.value_gradient(sphere[i]);
        v + g[0] + g[1] + g[2]
    });
    let ridges = Ridges::new(
        Fbm::new(seed ^ 1, 1.5, 2, 0.2, true),
        Fbm::new(seed ^ 2, 3.0, 2, 5.0, true),
        0.3,
        0.6,
    );
    r.run(
        "ridges: folded base + dragged detail (4 evaluations)",
        n,
        |i| ridges.value(sphere[i], sphere[i]),
    );
    let warp = Warp::new(seed, 2.0, [4, 6], [0.5, 0.5]);
    r.run(
        "two-level warp, 4 and 6 octaves, three fBms a level",
        n,
        |i| {
            let q = warp.apply(sphere[i], sphere[i]);
            q[0] + q[1] + q[2]
        },
    );

    println!("-- splines");
    let points = [
        (-1.1, -1.0),
        (-0.5, -0.6),
        (-0.2, -0.1),
        (0.0, 0.0),
        (0.1, 0.05),
        (0.3, 0.4),
        (0.6, 0.5),
        (1.0, 1.0),
    ];
    let pchip = Spline::pchip(0, &points).expect("increasing knots");
    r.baseline("PCHIP spline, 8 knots", n, |i| pchip.eval(&[spline_in[i]]));
    let nested = Spline::new(
        0,
        vec![
            Knot {
                at: -1.0,
                child: Child::Spline(pchip.clone()),
                slope: 0.25,
            },
            Knot {
                at: 1.0,
                child: Child::Spline(pchip.clone()),
                slope: 0.5,
            },
        ],
    )
    .expect("finite knots");
    r.run("a spline of two PCHIP splines (nested)", n, |i| {
        nested.eval(&[spline_in[i], spline_in[i]])
    });
    r.run("a PCHIP spline's bound over an interval", n, |i| {
        let (lo, hi) = pchip.bounds(&[spline_in[i]], &[spline_in[i] + 0.2]);
        lo + hi
    });

    println!("\nALL-DIGEST {:016x}", r.digest);
}
