//! Throwaway microbenchmark for issue #240 (deterministic noise toolkit). Not production code.
//! Compares, per sample, today's trilinear value noise (copied from `crates/planet-gen/src/lib.rs`)
//! with gradient/simplex noise, warps, ridges and splines built from `+ - * /`, `sqrt`-free
//! polynomials and an integer hash only. Prints ns/sample and a digest of every output so that
//! debug, release and `-C target-cpu=native` builds can be compared bit for bit.
#![allow(non_snake_case, static_mut_refs, dead_code, clippy::all)]

mod vendor {
    //! KdotJPG's OpenSimplex2 reference (CC0), unchanged, from
    //! github.com/KdotJPG/OpenSimplex2 at 4cd120d (rust/fast.rs, rust/smooth.rs).
    pub mod fast;
    pub mod smooth;
}

use std::hint::black_box;
use std::num::Wrapping;
use std::time::Instant;

// ---------------------------------------------------------------------------------------------
// Today's value noise, verbatim from crates/planet-gen/src/lib.rs (origin/main 03fc6e3).
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

// ---------------------------------------------------------------------------------------------
// Improved Perlin noise (Perlin 2002, ACM TOG 21(3)), seeded permutation like Minecraft's
// ImprovedNoise; f64::floor is exact.
struct Perlin {
    p: [u8; 512],
}

impl Perlin {
    fn new(seed: u64) -> Perlin {
        let mut perm: [u8; 256] = std::array::from_fn(|i| i as u8);
        for i in (1..256usize).rev() {
            let j = (hash(seed, [i as i32, 0, 0]) % (i as u64 + 1)) as usize;
            perm.swap(i, j);
        }
        Perlin { p: std::array::from_fn(|i| perm[i & 255]) }
    }
    /// Perlin 2002's 12 edge gradients (+4 repeats to make 16), as a table rather than the
    /// reference's if-chain, which mispredicts on random points.
    fn grad(h: u8, x: f64, y: f64, z: f64) -> f64 {
        const G: [[f64; 3]; 16] = [
            [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, -1.0, 0.0],
            [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0],
            [0.0, 1.0, 1.0], [0.0, -1.0, 1.0], [0.0, 1.0, -1.0], [0.0, -1.0, -1.0],
            [1.0, 1.0, 0.0], [0.0, -1.0, 1.0], [-1.0, 1.0, 0.0], [0.0, -1.0, -1.0],
        ];
        let g = G[(h & 15) as usize];
        g[0] * x + g[1] * y + g[2] * z
    }
    fn noise(&self, x: f64, y: f64, z: f64) -> f64 {
        let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
        let (xi, yi, zi) = ((fx as i32 & 255) as usize, (fy as i32 & 255) as usize, (fz as i32 & 255) as usize);
        let (x, y, z) = (x - fx, y - fy, z - fz);
        let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (u, v, w) = (fade(x), fade(y), fade(z));
        let p = &self.p;
        let a = p[xi] as usize + yi;
        let (aa, ab) = (p[a] as usize + zi, p[a + 1] as usize + zi);
        let b = p[xi + 1] as usize + yi;
        let (ba, bb) = (p[b] as usize + zi, p[b + 1] as usize + zi);
        let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
        lerp(w,
            lerp(v, lerp(u, Self::grad(p[aa], x, y, z), Self::grad(p[ba], x - 1.0, y, z)),
                    lerp(u, Self::grad(p[ab], x, y - 1.0, z), Self::grad(p[bb], x - 1.0, y - 1.0, z))),
            lerp(v, lerp(u, Self::grad(p[aa + 1], x, y, z - 1.0), Self::grad(p[ba + 1], x - 1.0, y, z - 1.0)),
                    lerp(u, Self::grad(p[ab + 1], x, y - 1.0, z - 1.0), Self::grad(p[bb + 1], x - 1.0, y - 1.0, z - 1.0))))
    }
}

/// Improved Perlin at an integer point with a power-of-two cell (exact fraction, like today's
/// value_noise), keeping the last cell's 8 gradient indices: the gradient-noise analogue of
/// today's corner cache.
fn perlin_cached(pn: &Perlin, point: [i32; 3], cell: i32, cache: &mut Option<([i32; 3], [u8; 8])>) -> f64 {
    let shift = cell.trailing_zeros();
    let lattice = point.map(|p| p >> shift);
    let inverse = 1.0 / f64::from(cell);
    let [x, y, z] = point.map(|p| f64::from(p & (cell - 1)) * inverse);
    let h = match cache {
        Some((l, h)) if *l == lattice => *h,
        _ => {
            let p = &pn.p;
            let [xi, yi, zi] = lattice.map(|c| (c & 255) as usize);
            let a = p[xi] as usize + yi;
            let (aa, ab) = (p[a] as usize + zi, p[a + 1] as usize + zi);
            let b = p[xi + 1] as usize + yi;
            let (ba, bb) = (p[b] as usize + zi, p[b + 1] as usize + zi);
            let h = [p[aa], p[ba], p[ab], p[bb], p[aa + 1], p[ba + 1], p[ab + 1], p[bb + 1]];
            *cache = Some((lattice, h));
            h
        }
    };
    let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (u, v, w) = (fade(x), fade(y), fade(z));
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    let g = Perlin::grad;
    lerp(w,
        lerp(v, lerp(u, g(h[0], x, y, z), g(h[1], x - 1.0, y, z)), lerp(u, g(h[2], x, y - 1.0, z), g(h[3], x - 1.0, y - 1.0, z))),
        lerp(v, lerp(u, g(h[4], x, y, z - 1.0), g(h[5], x - 1.0, y, z - 1.0)), lerp(u, g(h[6], x, y - 1.0, z - 1.0), g(h[7], x - 1.0, y - 1.0, z - 1.0))))
}

/// Improved Perlin with no period: each corner's gradient index from a one-multiply integer hash
/// of (seed, corner) (OpenSimplex2's prime-multiply hash) instead of a 256-entry permutation.
fn perlin_hashed(seed: i64, point: [i32; 3], cell: i32, cache: &mut Option<([i32; 3], [u8; 8])>) -> f64 {
    let shift = cell.trailing_zeros();
    let lattice = point.map(|p| p >> shift);
    let inverse = 1.0 / f64::from(cell);
    let [x, y, z] = point.map(|p| f64::from(p & (cell - 1)) * inverse);
    let h = match cache {
        Some((l, h)) if *l == lattice => *h,
        _ => {
            let xp = Wrapping(lattice[0] as i64) * Wrapping(PRIME_X);
            let yp = Wrapping(lattice[1] as i64) * Wrapping(PRIME_Y);
            let zp = Wrapping(lattice[2] as i64) * Wrapping(PRIME_Z);
            let h = std::array::from_fn(|i| {
                let c = (if i & 1 != 0 { xp + Wrapping(PRIME_X) } else { xp })
                    ^ (if i & 2 != 0 { yp + Wrapping(PRIME_Y) } else { yp })
                    ^ (if i & 4 != 0 { zp + Wrapping(PRIME_Z) } else { zp });
                let hh = (Wrapping(seed) ^ c) * Wrapping(HASH_MULTIPLIER);
                (hh.0 >> 59) as u8 & 15
            });
            *cache = Some((lattice, h));
            h
        }
    };
    perlin_corners(h, x, y, z)
}

fn perlin_corners(h: [u8; 8], x: f64, y: f64, z: f64) -> f64 {
    let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let (u, v, w) = (fade(x), fade(y), fade(z));
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    let g = Perlin::grad;
    lerp(w,
        lerp(v, lerp(u, g(h[0], x, y, z), g(h[1], x - 1.0, y, z)), lerp(u, g(h[2], x, y - 1.0, z), g(h[3], x - 1.0, y - 1.0, z))),
        lerp(v, lerp(u, g(h[4], x, y, z - 1.0), g(h[5], x - 1.0, y, z - 1.0)), lerp(u, g(h[6], x, y - 1.0, z - 1.0), g(h[7], x - 1.0, y - 1.0, z - 1.0))))
}

/// Value noise at a float point with the quintic fade, hashed corners (for comparison).
fn value_noise_f(seed: u64, p: [f64; 3]) -> f64 {
    let i = p.map(f64::floor);
    let l = i.map(|c| c as i32);
    let f = [p[0] - i[0], p[1] - i[1], p[2] - i[2]];
    let u = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
    let c = |dx: i32, dy: i32, dz: i32| corner_value(seed, [l[0] + dx, l[1] + dy, l[2] + dz]);
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x = |dy, dz| lerp(c(0, dy, dz), c(1, dy, dz), u[0]);
    let y = |dz| lerp(x(0, dz), x(1, dz), u[1]);
    lerp(y(0), y(1), u[2]) * 2.0 - 1.0
}

// ---------------------------------------------------------------------------------------------
// OpenSimplex2 (F) 3D, ported to f64 from the reference, with optional analytic derivatives.
const PRIME_X: i64 = 0x5205402B9270C86F;
const PRIME_Y: i64 = 0x598CD327003817B5;
const PRIME_Z: i64 = 0x5BCC226E9FA0BACB;
const HASH_MULTIPLIER: i64 = 0x53A3F72DEEC546F5;
const SEED_FLIP_3D: i64 = -0x52D547B2E96ED629;
const ROOT3OVER3: f64 = 0.577350269189626;
const UNSKEW_2D: f64 = -0.21132486540518713;
const RSQUARED_3D: f64 = 0.6;
const NORMALIZER_3D: f64 = 0.07969837668935331;
const GRAD3_SRC: &[f64] = &[
     2.22474487139,       2.22474487139,      -1.0,                 0.0,
     2.22474487139,       2.22474487139,       1.0,                 0.0,
     3.0862664687972017,  1.1721513422464978,  0.0,                 0.0,
     1.1721513422464978,  3.0862664687972017,  0.0,                 0.0,
    -2.22474487139,       2.22474487139,      -1.0,                 0.0,
    -2.22474487139,       2.22474487139,       1.0,                 0.0,
    -1.1721513422464978,  3.0862664687972017,  0.0,                 0.0,
    -3.0862664687972017,  1.1721513422464978,  0.0,                 0.0,
    -1.0,                -2.22474487139,      -2.22474487139,       0.0,
     1.0,                -2.22474487139,      -2.22474487139,       0.0,
     0.0,                -3.0862664687972017, -1.1721513422464978,  0.0,
     0.0,                -1.1721513422464978, -3.0862664687972017,  0.0,
    -1.0,                -2.22474487139,       2.22474487139,       0.0,
     1.0,                -2.22474487139,       2.22474487139,       0.0,
     0.0,                -1.1721513422464978,  3.0862664687972017,  0.0,
     0.0,                -3.0862664687972017,  1.1721513422464978,  0.0,
    //--------------------------------------------------------------------//
    -2.22474487139,      -2.22474487139,      -1.0,                 0.0,
    -2.22474487139,      -2.22474487139,       1.0,                 0.0,
    -3.0862664687972017, -1.1721513422464978,  0.0,                 0.0,
    -1.1721513422464978, -3.0862664687972017,  0.0,                 0.0,
    -2.22474487139,      -1.0,                -2.22474487139,       0.0,
    -2.22474487139,       1.0,                -2.22474487139,       0.0,
    -1.1721513422464978,  0.0,                -3.0862664687972017,  0.0,
    -3.0862664687972017,  0.0,                -1.1721513422464978,  0.0,
    -2.22474487139,      -1.0,                 2.22474487139,       0.0,
    -2.22474487139,       1.0,                 2.22474487139,       0.0,
    -3.0862664687972017,  0.0,                 1.1721513422464978,  0.0,
    -1.1721513422464978,  0.0,                 3.0862664687972017,  0.0,
    -1.0,                 2.22474487139,      -2.22474487139,       0.0,
     1.0,                 2.22474487139,      -2.22474487139,       0.0,
     0.0,                 1.1721513422464978, -3.0862664687972017,  0.0,
     0.0,                 3.0862664687972017, -1.1721513422464978,  0.0,
    -1.0,                 2.22474487139,       2.22474487139,       0.0,
     1.0,                 2.22474487139,       2.22474487139,       0.0,
     0.0,                 3.0862664687972017,  1.1721513422464978,  0.0,
     0.0,                 1.1721513422464978,  3.0862664687972017,  0.0,
     2.22474487139,      -2.22474487139,      -1.0,                 0.0,
     2.22474487139,      -2.22474487139,       1.0,                 0.0,
     1.1721513422464978, -3.0862664687972017,  0.0,                 0.0,
     3.0862664687972017, -1.1721513422464978,  0.0,                 0.0,
     2.22474487139,      -1.0,                -2.22474487139,       0.0,
     2.22474487139,       1.0,                -2.22474487139,       0.0,
     3.0862664687972017,  0.0,                -1.1721513422464978,  0.0,
     1.1721513422464978,  0.0,                -3.0862664687972017,  0.0,
     2.22474487139,      -1.0,                 2.22474487139,       0.0,
     2.22474487139,       1.0,                 2.22474487139,       0.0,
     1.1721513422464978,  0.0,                 3.0862664687972017,  0.0,
     3.0862664687972017,  0.0,                 1.1721513422464978,  0.0,
];
const GRADS3: [f64; 1024] = {
    let mut a = [0.0; 1024];
    let mut i = 0;
    while i < 1024 {
        a[i] = GRAD3_SRC[i % GRAD3_SRC.len()] / NORMALIZER_3D;
        i += 1;
    }
    a
};

#[inline(always)]
fn contrib<const D: bool>(a: f64, seed: Wrapping<i64>, xp: Wrapping<i64>, yp: Wrapping<i64>, zp: Wrapping<i64>,
                          d: [f64; 3], value: &mut f64, dv: &mut [f64; 3]) {
    let mut hash = (seed ^ xp) ^ (yp ^ zp);
    hash *= HASH_MULTIPLIER;
    hash ^= hash.0 >> (64 - 8 + 2);
    let gi = (hash.0 as i32 & ((256 - 1) << 2)) as usize;
    let g = [GRADS3[gi], GRADS3[gi | 1], GRADS3[gi | 2]];
    let gd = g[0] * d[0] + g[1] * d[1] + g[2] * d[2];
    let a2 = a * a;
    let a4 = a2 * a2;
    *value += a4 * gd;
    if D {
        // d/dp of a^4 (g.d) with a = r^2 - |d|^2: a^4 g - 8 a^3 (g.d) d.
        let k = -8.0 * a2 * a * gd;
        for i in 0..3 {
            dv[i] += a4 * g[i] + k * d[i];
        }
    }
}

fn round_i(x: f64) -> i32 {
    if x < 0.0 { (x - 0.5) as i32 } else { (x + 0.5) as i32 }
}

fn os2_base<const D: bool>(seed: i64, xr: f64, yr: f64, zr: f64) -> (f64, [f64; 3]) {
    os2_base_r::<D>(seed, xr, yr, zr, RSQUARED_3D)
}

fn os2_base_r<const D: bool>(seed: i64, xr: f64, yr: f64, zr: f64, rsq: f64) -> (f64, [f64; 3]) {
    let mut seed = Wrapping(seed);
    let (xrb, yrb, zrb) = (round_i(xr), round_i(yr), round_i(zr));
    let mut xri = xr - xrb as f64;
    let mut yri = yr - yrb as f64;
    let mut zri = zr - zrb as f64;
    let mut xs = (-1.0 - xri) as i32 | 1;
    let mut ys = (-1.0 - yri) as i32 | 1;
    let mut zs = (-1.0 - zri) as i32 | 1;
    let mut ax0 = xs as f64 * -xri;
    let mut ay0 = ys as f64 * -yri;
    let mut az0 = zs as f64 * -zri;
    let mut xp = Wrapping(xrb as i64) * Wrapping(PRIME_X);
    let mut yp = Wrapping(yrb as i64) * Wrapping(PRIME_Y);
    let mut zp = Wrapping(zrb as i64) * Wrapping(PRIME_Z);
    let (mut value, mut dv) = (0.0, [0.0; 3]);
    let mut a = (rsq - xri * xri) - (yri * yri + zri * zri);
    for l in 0..2 {
        if a > 0.0 {
            contrib::<D>(a, seed, xp, yp, zp, [xri, yri, zri], &mut value, &mut dv);
        }
        if ax0 >= ay0 && ax0 >= az0 {
            let b = a + ax0 + ax0;
            if b > 1.0 {
                contrib::<D>(b - 1.0, seed, xp - Wrapping(xs as i64) * Wrapping(PRIME_X), yp, zp,
                             [xri + xs as f64, yri, zri], &mut value, &mut dv);
            }
        } else if ay0 > ax0 && ay0 >= az0 {
            let b = a + ay0 + ay0;
            if b > 1.0 {
                contrib::<D>(b - 1.0, seed, xp, yp - Wrapping(ys as i64) * Wrapping(PRIME_Y), zp,
                             [xri, yri + ys as f64, zri], &mut value, &mut dv);
            }
        } else {
            let b = a + az0 + az0;
            if b > 1.0 {
                contrib::<D>(b - 1.0, seed, xp, yp, zp - Wrapping(zs as i64) * Wrapping(PRIME_Z),
                             [xri, yri, zri + zs as f64], &mut value, &mut dv);
            }
        }
        if l == 1 {
            break;
        }
        ax0 = 0.5 - ax0;
        ay0 = 0.5 - ay0;
        az0 = 0.5 - az0;
        xri = xs as f64 * ax0;
        yri = ys as f64 * ay0;
        zri = zs as f64 * az0;
        a += (0.75 - ax0) - (ay0 + az0);
        xp += Wrapping((xs as i64 >> 1) & PRIME_X);
        yp += Wrapping((ys as i64 >> 1) & PRIME_Y);
        zp += Wrapping((zs as i64 >> 1) & PRIME_Z);
        xs = -xs;
        ys = -ys;
        zs = -zs;
        seed ^= Wrapping(SEED_FLIP_3D);
    }
    (value, dv)
}

/// OpenSimplex2 3D "ImproveXY" orientation: an orthonormal rotation, then the BCC lattice.
/// Returns the value and, if D, the gradient in the caller's (unrotated) frame.
#[inline(always)]
fn os2<const D: bool>(seed: i64, p: [f64; 3]) -> (f64, [f64; 3]) {
    os2r::<D>(seed, p, RSQUARED_3D, 1.0)
}

/// As [`os2`] with kernel radius^2 `rsq` and the output scaled by `k`.
#[inline(always)]
fn os2r<const D: bool>(seed: i64, p: [f64; 3], rsq: f64, k: f64) -> (f64, [f64; 3]) {
    let [x, y, z] = p;
    let xy = x + y;
    let s2 = xy * UNSKEW_2D;
    let zz = z * ROOT3OVER3;
    let (v, g) = os2_base_r::<D>(seed, x + s2 + zz, y + s2 + zz, xy * -ROOT3OVER3 + zz, rsq);
    let (v, g) = (v * k, g.map(|c| c * k));
    if !D {
        return (v, g);
    }
    // The gradient in rotated coordinates times the rotation's transpose.
    let (s, r) = (UNSKEW_2D, ROOT3OVER3);
    (v, [
        (1.0 + s) * g[0] + s * g[1] - r * g[2],
        s * g[0] + (1.0 + s) * g[1] - r * g[2],
        r * (g[0] + g[1] + g[2]),
    ])
}

/// OS2 with the kernel radius^2 cut from 0.6 to 0.5 (the value that keeps every lattice's
/// contributions to its closest two points), rescaled to the same amplitude.
fn os2_r05(seed: i64, p: [f64; 3]) -> f64 {
    os2r::<false>(seed, p, 0.5, R05_SCALE).0
}

fn os2_r05d(seed: i64, p: [f64; 3]) -> (f64, [f64; 3]) {
    os2r::<true>(seed, p, 0.5, R05_SCALE)
}

/// (0.6 / 0.5)^4: the same peak kernel weight as the reference. The true range must be bounded
/// before production use (see the findings file).
const R05_SCALE: f64 = 2.0736;

fn os2v(seed: i64, p: [f64; 3]) -> f64 {
    os2::<false>(seed, p).0
}

// ---------------------------------------------------------------------------------------------
// Fractal compositions.
const OCT_SALT: i64 = 0x9e37_79b9_7f4a_7c15u64 as i64;

fn fbm(seed: i64, p: [f64; 3], octaves: u32) -> f64 {
    let (mut sum, mut amp, mut f) = (0.0, 1.0, 1.0);
    for o in 0..octaves {
        sum += amp * os2_r05(seed.wrapping_add((o as i64).wrapping_mul(OCT_SALT)), p.map(|c| c * f));
        amp *= 0.5;
        f *= 2.0;
    }
    sum
}

/// Musgrave's ridged multifractal as in libnoise's RidgedMulti (offset 1, gain 2, H = 1,
/// lacunarity 2, so the spectral weights are exactly 2^-i: no pow needed).
fn ridged_multi(seed: i64, p: [f64; 3], octaves: u32) -> f64 {
    let (mut value, mut weight, mut f, mut w) = (0.0, 1.0, 1.0, 1.0);
    for o in 0..octaves {
        let mut signal = os2_r05(seed.wrapping_add((o as i64).wrapping_mul(OCT_SALT)), p.map(|c| c * f));
        signal = 1.0 - signal.abs();
        signal *= signal;
        signal *= weight;
        weight = (signal * 2.0).clamp(0.0, 1.0);
        value += signal * w;
        f *= 2.0;
        w *= 0.5;
    }
    value * 1.25 - 1.0
}

/// Quilez's two-level domain warp in 3D: f(p + 4 r), r = F(p + 4 q), q = F(p): 3*4 + 3*4 + 6
/// octaves = 30 noise samples.
fn warped(seed: i64, p: [f64; 3]) -> f64 {
    let off = |k: i64, p: [f64; 3]| fbm(seed.wrapping_add(k), p, 4);
    let q = [off(1, p), off(2, p), off(3, p)];
    let pq = [p[0] + 4.0 * q[0], p[1] + 4.0 * q[1], p[2] + 4.0 * q[2]];
    let r = [off(4, pq), off(5, pq), off(6, pq)];
    fbm(seed, [p[0] + 4.0 * r[0], p[1] + 4.0 * r[1], p[2] + 4.0 * r[2]], 6)
}

/// One-level warp: q = 3 fbm of 3 octaves, then a 6-octave fbm: 15 samples.
fn warped1(seed: i64, p: [f64; 3]) -> f64 {
    let q = [1, 2, 3].map(|k| fbm(seed.wrapping_add(k), p, 3));
    fbm(seed, [p[0] + 2.0 * q[0], p[1] + 2.0 * q[1], p[2] + 2.0 * q[2]], 6)
}

/// Tectonic's mountain ridges (tectonic: mountain_ridges/ridges.json): base + 0.6 * detail
/// sampled at a point shifted by |finite-difference slope of base| * k. Finite differences as
/// Tectonic does: 3 base evaluations of 2 octaves + a 2-octave detail = 8 samples.
fn tectonic_ridges_fd(seed: i64, p: [f64; 3]) -> f64 {
    let base = |q: [f64; 3]| os2_r05(seed, q) + 0.2 * os2_r05(seed ^ 1, q.map(|c| c * 2.0));
    let b = base(p);
    let sx = b - base([p[0] + 0.01, p[1], p[2]]);
    let sz = b - base([p[0], p[1], p[2] + 0.01]);
    let s = [p[0] * 2.0 + sx.abs() * 30.0, p[1] * 2.0, p[2] * 2.0 + sz.abs() * 30.0];
    let detail = 0.2 * os2_r05(seed ^ 2, s) + os2_r05(seed ^ 3, s.map(|c| c * 2.0));
    b + 0.6 * detail
}

/// The same with analytic derivatives instead of finite differences: 2 + 2 = 4 samples.
fn tectonic_ridges_an(seed: i64, p: [f64; 3]) -> f64 {
    let (v0, g0) = os2_r05d(seed, p);
    let (v1, g1) = os2_r05d(seed ^ 1, p.map(|c| c * 2.0));
    let b = v0 + 0.2 * v1;
    let g = [g0[0] + 0.4 * g1[0], g0[1] + 0.4 * g1[1], g0[2] + 0.4 * g1[2]];
    let s = [p[0] * 2.0 + g[0].abs() * 0.3, p[1] * 2.0 + g[1].abs() * 0.3, p[2] * 2.0 + g[2].abs() * 0.3];
    let detail = 0.2 * os2_r05(seed ^ 2, s) + os2_r05(seed ^ 3, s.map(|c| c * 2.0));
    b + 0.6 * detail
}

/// Quilez's derivative-damped fbm ("erosion-like"): each octave divided by 1 + |sum of grads|^2.
fn eroded_fbm(seed: i64, p: [f64; 3], octaves: u32) -> f64 {
    let (mut a, mut b, mut f, mut d) = (0.0, 1.0, 1.0, [0.0; 3]);
    for o in 0..octaves {
        let (n, g) = os2_r05d(seed.wrapping_add((o as i64).wrapping_mul(OCT_SALT)), p.map(|c| c * f));
        for i in 0..3 {
            d[i] += g[i] * b * f; // chain rule: the octave's gradient wrt p.
        }
        a += b * n / (1.0 + d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
        b *= 0.5;
        f *= 2.0;
    }
    a
}

// ---------------------------------------------------------------------------------------------
// Monotone cubic Hermite spline (PCHIP, Fritsch-Butland 1984 slopes) and a nested spline.
struct Spline {
    x: Vec<f64>,
    y: Vec<f64>,
    d: Vec<f64>,
}

impl Spline {
    fn pchip(x: &[f64], y: &[f64]) -> Spline {
        let n = x.len();
        let h: Vec<f64> = (0..n - 1).map(|k| x[k + 1] - x[k]).collect();
        let m: Vec<f64> = (0..n - 1).map(|k| (y[k + 1] - y[k]) / h[k]).collect();
        let mut d = vec![0.0; n];
        for k in 1..n - 1 {
            if m[k - 1] * m[k] > 0.0 {
                let w1 = 2.0 * h[k] + h[k - 1];
                let w2 = h[k] + 2.0 * h[k - 1];
                d[k] = (w1 + w2) / (w1 / m[k - 1] + w2 / m[k]);
            }
        }
        // End slopes: one-sided, clamped to keep the shape (as SciPy's _edge_case).
        let edge = |h0: f64, h1: f64, m0: f64, m1: f64| {
            let e = ((2.0 * h0 + h1) * m0 - h0 * m1) / (h0 + h1);
            if e * m0 <= 0.0 { 0.0 } else if m0 * m1 <= 0.0 && e.abs() > 3.0 * m0.abs() { 3.0 * m0 } else { e }
        };
        d[0] = edge(h[0], h[1], m[0], m[1]);
        d[n - 1] = edge(h[n - 2], h[n - 3], m[n - 2], m[n - 3]);
        Spline { x: x.to_vec(), y: y.to_vec(), d }
    }

    /// Cubic Hermite between knots, linear past the ends (Minecraft's CubicSpline form, as
    /// cubiomes reimplements it).
    fn eval(&self, f: f64) -> f64 {
        let n = self.x.len();
        let i = self.x.iter().position(|&l| l >= f).unwrap_or(n);
        if i == 0 || i == n {
            let j = if i == 0 { 0 } else { n - 1 };
            return self.y[j] + self.d[j] * (f - self.x[j]);
        }
        hermite(self.x[i - 1], self.x[i], self.y[i - 1], self.y[i], self.d[i - 1], self.d[i], f)
    }
}

#[inline(always)]
fn hermite(g: f64, h: f64, n: f64, o: f64, l: f64, m: f64, f: f64) -> f64 {
    let k = (f - g) / (h - g);
    let p = l * (h - g) - (o - n);
    let q = -m * (h - g) + (o - n);
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    lerp(k, n, o) + k * (1.0 - k) * lerp(k, p, q)
}

/// Vanilla-style nesting: knots of an outer spline over c hold splines over e, whose knots hold
/// splines over pv.
enum Node {
    C(f64),
    S(Box<Nested>),
}

struct Nested {
    axis: usize,
    x: Vec<f64>,
    v: Vec<Node>,
    d: Vec<f64>,
}

impl Node {
    fn eval(&self, c: &[f64; 3]) -> f64 {
        match self {
            Node::C(v) => *v,
            Node::S(s) => s.eval(c),
        }
    }
}

impl Nested {
    fn eval(&self, c: &[f64; 3]) -> f64 {
        let f = c[self.axis];
        let n = self.x.len();
        let i = self.x.iter().position(|&l| l >= f).unwrap_or(n);
        if i == 0 || i == n {
            let j = if i == 0 { 0 } else { n - 1 };
            return self.v[j].eval(c) + self.d[j] * (f - self.x[j]);
        }
        hermite(self.x[i - 1], self.x[i], self.v[i - 1].eval(c), self.v[i].eval(c), self.d[i - 1], self.d[i], f)
    }
}

fn nested(seed: u64) -> Nested {
    let r = |k: i32| corner_value(seed, [k, 7, 7]) * 2.0 - 1.0;
    let mut k = 0;
    let mut next = || {
        k += 1;
        r(k)
    };
    let mk = |axis: usize, n: usize, inner: &mut dyn FnMut() -> Node, next: &mut dyn FnMut() -> f64| Nested {
        axis,
        x: (0..n).map(|i| -1.0 + 2.0 * i as f64 / (n - 1) as f64).collect(),
        v: (0..n).map(|_| inner()).collect(),
        d: (0..n).map(|_| next() * 0.5).collect(),
    };
    let mut leaf_vals = || Node::C(next());
    let mut k2 = 1000;
    let mut next2 = || {
        k2 += 1;
        r(k2)
    };
    let mut pv = || Node::S(Box::new(mk(2, 5, &mut leaf_vals, &mut next2)));
    let mut k3 = 2000;
    let mut next3 = || {
        k3 += 1;
        r(k3)
    };
    let mut ero = || Node::S(Box::new(mk(1, 6, &mut pv, &mut next3)));
    let mut k4 = 3000;
    let mut next4 = || {
        k4 += 1;
        r(k4)
    };
    mk(0, 8, &mut ero, &mut next4)
}

/// Writes 512x512 PGM slices (z = 0.37, 8 noise units across) for a visual check.
fn render() {
    let s = 0xB4A7_A9ACi64;
    let pn = Perlin::new(s as u64);
    let cases: Vec<(&str, Box<dyn Fn([f64; 3]) -> f64>)> = vec![
        ("os2_r06", Box::new(move |p| os2v(s, p))),
        ("os2_r05", Box::new(move |p| os2_r05(s, p))),
        ("perlin", Box::new(move |p| pn.noise(p[0], p[1], p[2]) * 1.4)),
        ("value_today", Box::new(move |p| value_noise_f(s as u64, p))),
        ("ridged_r05", Box::new(move |p| {
            let (mut v, mut w, mut f, mut a) = (0.0, 1.0, 1.0, 1.0);
            for o in 0..6i64 {
                let mut sg = 1.0 - os2_r05(s.wrapping_add(o.wrapping_mul(OCT_SALT)), p.map(|c| c * f / 4.0)).abs();
                sg *= sg; sg *= w; w = (sg * 2.0).clamp(0.0, 1.0); v += sg * a; f *= 2.0; a *= 0.5;
            }
            v * 1.25 - 1.0
        })),
        ("warped_r05", Box::new(move |p| {
            let fb = |sd: i64, q: [f64; 3], n: u32| { let (mut t, mut a, mut f) = (0.0, 0.5, 1.0); for o in 0..n { t += a * os2_r05(sd.wrapping_add((o as i64).wrapping_mul(OCT_SALT)), q.map(|c| c * f / 4.0)); a *= 0.5; f *= 2.0; } t };
            let q = [fb(s + 1, p, 4), fb(s + 2, p, 4), fb(s + 3, p, 4)];
            fb(s, [p[0] + 6.0 * q[0], p[1] + 6.0 * q[1], p[2] + 6.0 * q[2]], 6) * 1.6
        })),
    ];
    for (name, f) in cases {
        let mut out = format!("P5 512 512 255\n").into_bytes();
        for j in 0..512 {
            for i in 0..512 {
                let v = f([i as f64 / 64.0, j as f64 / 64.0, 0.37]);
                out.push(((v * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
        std::fs::write(format!("/tmp/noise240/{name}.pgm"), out).unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// Harness.
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
}

impl Report {
    fn run(&mut self, name: &str, per_call: f64, n: usize, mut f: impl FnMut(usize) -> f64) -> f64 {
        let mut best = f64::INFINITY;
        let mut digest = 0u64;
        for _ in 0..5 {
            let mut d = 0xcbf2_9ce4_8422_2325u64;
            let t = Instant::now();
            for i in 0..n {
                let v = black_box(f(black_box(i)));
                d = (d ^ v.to_bits()).wrapping_mul(0x100_0000_01b3);
            }
            let ns = t.elapsed().as_nanos() as f64 / n as f64;
            best = best.min(ns);
            digest = d;
        }
        self.digest = (self.digest ^ digest).wrapping_mul(0x100_0000_01b3);
        println!("{name:<58} {best:>8.1} ns  {:>8.1} ns/noise  digest {digest:016x}", best / per_call);
        best
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("render") {
        return render();
    }
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1 << 20);
    let seed = 0xB4A7_A9ACu64;
    let iseed = seed as i64;
    // Random points: integers in a +-2^21 lattice box (today's 10 cm points), and the same in
    // noise units for the float-input noises (cell 64 lattice points -> divide by 64).
    let ipts: Vec<[i32; 3]> = (0..n)
        .map(|i| [0u64, 1, 2].map(|a| (splitmix(i as u64 * 3 + a) >> 42) as i32 - (1 << 21)))
        .collect();
    let fpts: Vec<[f64; 3]> = ipts.iter().map(|p| p.map(|c| f64::from(c) / 64.0)).collect();
    // Coherent points: an 8x8x8 brick of Voxel-spaced points (2 per 10 cm lattice point at 5 cm),
    // as Detail::height walks them, at a random brick origin.
    let cpts: Vec<[i32; 3]> = (0..n)
        .map(|i| {
            let b = (i / 512) as u64;
            let o = [0u64, 1, 2].map(|a| (splitmix(b * 3 + a + 99) >> 44) as i32);
            [o[0] + (i % 8) as i32 / 2, o[1] + (i / 8 % 8) as i32 / 2, o[2] + (i / 64 % 8) as i32 / 2]
        })
        .collect();
    let cfpts: Vec<[f64; 3]> = cpts.iter().map(|p| p.map(|c| f64::from(c) / 64.0)).collect();
    let spline_in: Vec<f64> = (0..n).map(|i| unit(i as u64 + 7) * 2.4 - 1.2).collect();
    let nested_in: Vec<[f64; 3]> = (0..n).map(|i| [0u64, 1, 2].map(|a| unit(i as u64 * 3 + a + 11) * 2.2 - 1.1)).collect();

    // Port check: the f64 port of OpenSimplex2 matches the f32 reference to f32 precision.
    let (mut off, mut worst_ok, mut range) = (0usize, 0.0f64, 0.0f64);
    for p in fpts.iter().take(100_000) {
        let r = vendor::fast::noise3_ImproveXY(iseed, p[0], p[1], p[2]) as f64;
        let d = (r - os2v(iseed, *p)).abs();
        if d > 1e-5 { off += 1 } else { worst_ok = worst_ok.max(d) }
    }
    for i in 0..4_000_000u64 {
        let p = [0u64, 1, 2].map(|a| unit(i * 3 + a + 5) * 200.0 - 100.0);
        range = range.max(os2_r05(iseed, p).abs());
    }
    // Derivative check against central differences.
    let mut dworst: f64 = 0.0;
    for p in fpts.iter().take(20_000) {
        let (_, g) = os2_r05d(iseed, *p);
        let e = 1e-6;
        for a in 0..3 {
            let mut hi = *p;
            let mut lo = *p;
            hi[a] += e;
            lo[a] -= e;
            let fd = (os2_r05(iseed, hi) - os2_r05(iseed, lo)) / (2.0 * e);
            dworst = dworst.max((fd - g[a]).abs());
        }
    }
    println!("port check: f64 port vs f32 reference: {off} of 100000 lattice-aligned points differ by > 1e-5 (the tie discontinuity), the rest by <= {worst_ok:.1e}");
    println!("r2=0.5 variant: max |analytic - central diff| = {dworst:.2e}; max |value| over 4M points = {range:.4}");

    let perlin = Perlin::new(seed);
    let spline = Spline::pchip(&[-1.1, -0.5, -0.2, 0.0, 0.1, 0.3, 0.6, 1.0], &[-1.0, -0.6, -0.1, 0.0, 0.05, 0.4, 0.5, 1.0]);
    let nest = nested(seed);
    let mut r = Report { digest: 0 };
    println!("n = {n} samples, best of 5\n{:<58} {:>11} {:>16}", "case", "per call", "per noise eval");
    println!("-- one noise evaluation, random points (no cell reuse)");
    let base = r.run("today: value_noise, int point, cell 64, no cache", 1.0, n, |i| value_noise(seed, ipts[i], 64, &mut None));
    r.run("value noise, float point, quintic, splitmix corners", 1.0, n, |i| value_noise_f(seed, fpts[i]));
    r.run("improved Perlin (2002), permutation table", 1.0, n, |i| { let p = fpts[i]; perlin.noise(p[0], p[1], p[2]) });
    r.run("improved Perlin, int point, cell 64, no cache", 1.0, n, |i| perlin_cached(&perlin, ipts[i], 64, &mut None));
    r.run("improved Perlin, hashed corners (no period), no cache", 1.0, n, |i| perlin_hashed(iseed, ipts[i], 64, &mut None));
    r.run("OpenSimplex2 3D reference (f32, vendored)", 1.0, n, |i| { let p = fpts[i]; vendor::fast::noise3_ImproveXY(iseed, p[0], p[1], p[2]) as f64 });
    r.run("OpenSimplex2S 3D reference (f32, vendored)", 1.0, n, |i| { let p = fpts[i]; vendor::smooth::noise3_ImproveXY(iseed, p[0], p[1], p[2]) as f64 });
    r.run("OpenSimplex2 3D, f64 port, r2 = 0.6 (as reference)", 1.0, n, |i| os2v(iseed, fpts[i]));
    r.run("OpenSimplex2 3D, f64 port, r2 = 0.5 (continuous)", 1.0, n, |i| os2_r05(iseed, fpts[i]));
    r.run("OpenSimplex2 3D, f64 port, r2 = 0.5 + analytic gradient", 1.0, n, |i| { let (v, g) = os2_r05d(iseed, fpts[i]); v + g[0] + g[1] + g[2] });
    println!("-- one noise evaluation, coherent points (8^3 brick walk, today's access pattern)");
    let mut cache = None;
    let cbase = r.run("today: value_noise, int point, cell 64, with corner cache", 1.0, n, |i| value_noise(seed, cpts[i], 64, &mut cache));
    r.run("OpenSimplex2 r2 = 0.5, f64 (no cache possible)", 1.0, n, |i| os2_r05(iseed, cfpts[i]));
    r.run("OpenSimplex2 r2 = 0.5, f64 + analytic gradient", 1.0, n, |i| { let (v, g) = os2_r05d(iseed, cfpts[i]); v + g[0] + g[1] + g[2] });
    r.run("improved Perlin (2002), permutation table", 1.0, n, |i| { let p = cfpts[i]; perlin.noise(p[0], p[1], p[2]) });
    let mut pcache = None;
    r.run("improved Perlin, int point, cell 64, gradient cache", 1.0, n, |i| perlin_cached(&perlin, cpts[i], 64, &mut pcache));
    let mut hcache = None;
    r.run("improved Perlin, hashed corners, gradient cache", 1.0, n, |i| perlin_hashed(iseed, cpts[i], 64, &mut hcache));
    println!("-- compositions (OpenSimplex2 r2 = 0.5, f64), random points");
    r.run("fbm, 7 octaves", 7.0, n, |i| fbm(iseed, fpts[i], 7));
    r.run("ridged multifractal (Musgrave/libnoise), 7 octaves", 7.0, n, |i| ridged_multi(iseed, fpts[i], 7));
    r.run("derivative-damped 'eroded' fbm (Quilez), 7 octaves", 7.0, n, |i| eroded_fbm(iseed, fpts[i], 7));
    r.run("one-level warp: 3x fbm3 offset + fbm6 (15 evals)", 15.0, n, |i| warped1(iseed, fpts[i]));
    r.run("two-level warp (Quilez): 6x fbm4 + fbm6 (30 evals)", 30.0, n, |i| warped(iseed, fpts[i]));
    r.run("Tectonic ridges, finite differences (8 evals)", 8.0, n, |i| tectonic_ridges_fd(iseed, fpts[i]));
    r.run("Tectonic ridges, analytic gradient (2 grad + 2 evals)", 4.0, n, |i| tectonic_ridges_an(iseed, fpts[i]));
    println!("-- splines");
    r.run("PCHIP spline, 8 knots, eval", 1.0, n, |i| spline.eval(spline_in[i]));
    r.run("nested spline 8x6x5 knots (vanilla-style), eval", 1.0, n, |i| nest.eval(&nested_in[i]));
    println!("\nbaseline (today, random) {base:.1} ns, (today, coherent) {cbase:.1} ns");
    println!("ALL-DIGEST {:016x}", r.digest);
}

#[cfg(test)]
mod debug4 {
    use super::*;
    /// Walk random short lines with step h through the vendored f32 reference, and report the
    /// largest step-to-step jump relative to the typical one.
    fn scan(name: &str, f: impl Fn(f64, f64, f64) -> f64) {
        let h = std::env::var("H").ok().and_then(|v| v.parse().ok()).unwrap_or(1e-4);
        let (mut worst, mut at, mut typical) = (0.0f64, [0.0; 3], 0.0f64);
        let mut count = 0u64;
        for line in 0..2000u64 {
            let o = [0u64, 1, 2].map(|a| unit(line * 7 + a) * 40.0 - 20.0);
            let dir = [0u64, 1, 2].map(|a| unit(line * 7 + a + 3) - 0.5);
            let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            let dir = dir.map(|c| c / l);
            let mut prev = f(o[0], o[1], o[2]);
            for k in 1..5000 {
                let t = k as f64 * h;
                let p = [o[0] + dir[0] * t, o[1] + dir[1] * t, o[2] + dir[2] * t];
                let v = f(p[0], p[1], p[2]);
                let d = (v - prev).abs();
                typical += d;
                count += 1;
                if d > worst { worst = d; at = p; }
                prev = v;
            }
        }
        println!("{name}: largest jump over a 1e-4 step {worst:.2e} at {at:?}; mean step {:.2e}", typical / count as f64);
    }
    #[test]
    fn scan_all() {
        let s = 0xB4A7_A9ACi64;
        scan("OpenSimplex2 ref f32", |x, y, z| vendor::fast::noise3_ImproveXY(s, x, y, z) as f64);
        scan("OpenSimplex2S ref f32", |x, y, z| vendor::smooth::noise3_ImproveXY(s, x, y, z) as f64);
        scan("OpenSimplex2 f64 port", |x, y, z| os2v(s, [x, y, z]));
        let pn = Perlin::new(0xB4A7_A9AC);
        scan("improved Perlin", |x, y, z| pn.noise(x, y, z));
        scan("OpenSimplex2 f64 port r2=0.5", |x, y, z| os2_r05(s, [x, y, z]));
    }
}
