//! Gradient noise with analytic derivatives, fBm, domain warp, monotone splines.
//! Prototype code: f64, integer hash, nothing here is production-grade.

pub type V3 = [f64; 3];

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn mul(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub fn norm(a: V3) -> V3 {
    let l = len(a).max(1e-300);
    mul(a, 1.0 / l)
}

pub fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
pub fn gauss(x: f64, mu: f64, sigma: f64) -> f64 {
    let z = (x - mu) / sigma;
    (-0.5 * z * z).exp()
}

pub fn hash3(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let mut h = seed.wrapping_mul(0x9E37_79B1)
        ^ (x as u32).wrapping_mul(0x85EB_CA6B)
        ^ (y as u32).wrapping_mul(0xC2B2_AE35)
        ^ (z as u32).wrapping_mul(0x27D4_EB2F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

/// Hash to a float in [0, 1).
pub fn hash01(a: u32, b: u32, seed: u32) -> f64 {
    f64::from(hash3(a as i32, b as i32, 7, seed) >> 8) / f64::from(1u32 << 24)
}

const GRADS: [V3; 12] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
];

fn grad(ix: i32, iy: i32, iz: i32, seed: u32) -> V3 {
    GRADS[(hash3(ix, iy, iz, seed) % 12) as usize]
}

/// Improved-Perlin gradient noise with analytic gradient. Roughly in [-1, 1].
pub fn gnoise(p: V3, seed: u32) -> (f64, V3) {
    let (fx, fy, fz) = (p[0].floor(), p[1].floor(), p[2].floor());
    let (ix, iy, iz) = (fx as i32, fy as i32, fz as i32);
    let f = [p[0] - fx, p[1] - fy, p[2] - fz];
    let u = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
    let du = f.map(|t| 30.0 * t * t * (t - 1.0) * (t - 1.0));
    let g = |dx: i32, dy: i32, dz: i32| grad(ix + dx, iy + dy, iz + dz, seed);
    let (ga, gb, gc, gd) = (g(0, 0, 0), g(1, 0, 0), g(0, 1, 0), g(1, 1, 0));
    let (ge, gf, gg, gh) = (g(0, 0, 1), g(1, 0, 1), g(0, 1, 1), g(1, 1, 1));
    let va = dot(ga, f);
    let vb = dot(gb, sub(f, [1.0, 0.0, 0.0]));
    let vc = dot(gc, sub(f, [0.0, 1.0, 0.0]));
    let vd = dot(gd, sub(f, [1.0, 1.0, 0.0]));
    let ve = dot(ge, sub(f, [0.0, 0.0, 1.0]));
    let vf = dot(gf, sub(f, [1.0, 0.0, 1.0]));
    let vg = dot(gg, sub(f, [0.0, 1.0, 1.0]));
    let vh = dot(gh, sub(f, [1.0, 1.0, 1.0]));
    let k1 = vb - va;
    let k2 = vc - va;
    let k3 = ve - va;
    let k4 = va - vb - vc + vd;
    let k5 = va - vc - ve + vg;
    let k6 = va - vb - ve + vf;
    let k7 = -va + vb + vc - vd + ve - vf - vg + vh;
    let v = va + u[0] * k1 + u[1] * k2 + u[2] * k3 + u[0] * u[1] * k4 + u[1] * u[2] * k5
        + u[2] * u[0] * k6
        + u[0] * u[1] * u[2] * k7;
    let mut d = [0.0; 3];
    for a in 0..3 {
        d[a] = ga[a]
            + u[0] * (gb[a] - ga[a])
            + u[1] * (gc[a] - ga[a])
            + u[2] * (ge[a] - ga[a])
            + u[0] * u[1] * (ga[a] - gb[a] - gc[a] + gd[a])
            + u[1] * u[2] * (ga[a] - gc[a] - ge[a] + gg[a])
            + u[2] * u[0] * (ga[a] - gb[a] - ge[a] + gf[a])
            + u[0] * u[1] * u[2] * (-ga[a] + gb[a] + gc[a] - gd[a] + ge[a] - gf[a] - gg[a] + gh[a]);
    }
    d[0] += du[0] * (k1 + u[1] * k4 + u[2] * k6 + u[1] * u[2] * k7);
    d[1] += du[1] * (k2 + u[0] * k4 + u[2] * k5 + u[0] * u[2] * k7);
    d[2] += du[2] * (k3 + u[1] * k5 + u[0] * k6 + u[0] * u[1] * k7);
    (v * 0.8, mul(d, 0.8))
}

/// A fixed rotation per octave hides the lattice axes.
const ROT: [V3; 3] = [
    [0.8000, 0.6000, 0.0000],
    [-0.4800, 0.6400, 0.6000],
    [0.3600, -0.4800, 0.8000],
];
fn rot(p: V3) -> V3 {
    [dot(ROT[0], p), dot(ROT[1], p), dot(ROT[2], p)]
}
fn rot_t(g: V3) -> V3 {
    [
        ROT[0][0] * g[0] + ROT[1][0] * g[1] + ROT[2][0] * g[2],
        ROT[0][1] * g[0] + ROT[1][1] * g[1] + ROT[2][1] * g[2],
        ROT[0][2] * g[0] + ROT[1][2] * g[1] + ROT[2][2] * g[2],
    ]
}

/// fBm of gradient noise. `freq` in 1/km. Result normalised by the amplitude sum;
/// the gradient is d/dp of that value in 1/km.
pub fn fbm(p: V3, freq: f64, octaves: u32, gain: f64, seed: u32) -> (f64, V3) {
    let mut q = mul(p, freq);
    let mut amp = 1.0;
    let mut sum = 0.0;
    let mut total = 0.0;
    let mut g = [0.0; 3];
    let mut scale = freq;
    let mut path = 0u32;
    for o in 0..octaves {
        let (v, d) = gnoise(q, seed.wrapping_add(o * 101));
        sum += amp * v;
        // d is w.r.t. q; q = scale * R^path p  =>  d/dp = scale * (R^T)^path d
        let mut dd = d;
        for _ in 0..path {
            dd = rot_t(dd);
        }
        g = add(g, mul(dd, amp * scale));
        total += amp;
        amp *= gain;
        q = mul(rot(q), 2.03);
        scale *= 2.03;
        path += 1;
    }
    (sum / total, mul(g, 1.0 / total))
}

pub fn fbm1(p: V3, freq: f64, octaves: u32, gain: f64, seed: u32) -> f64 {
    fbm(p, freq, octaves, gain, seed).0
}

/// Domain warp: p + amp * (three noises).
pub fn warp(p: V3, amp: f64, freq: f64, seed: u32) -> V3 {
    let q = mul(p, freq);
    let a = gnoise(q, seed).0;
    let b = gnoise(add(q, [31.4, 17.2, 5.9]), seed.wrapping_add(1)).0;
    let c = gnoise(add(q, [-12.1, 44.3, 23.7]), seed.wrapping_add(2)).0;
    add(p, mul([a, b, c], amp * 1.25))
}

/// Monotone cubic Hermite spline (PCHIP slopes), clamped at both ends.
pub struct Spline {
    x: Vec<f64>,
    y: Vec<f64>,
    m: Vec<f64>,
}

impl Spline {
    pub fn new(knots: &[(f64, f64)]) -> Spline {
        let n = knots.len();
        let x: Vec<f64> = knots.iter().map(|k| k.0).collect();
        let y: Vec<f64> = knots.iter().map(|k| k.1).collect();
        let d: Vec<f64> = (0..n - 1).map(|i| (y[i + 1] - y[i]) / (x[i + 1] - x[i])).collect();
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            if d[i - 1] * d[i] <= 0.0 {
                m[i] = 0.0;
            } else {
                let (h0, h1) = (x[i] - x[i - 1], x[i + 1] - x[i]);
                let (w1, w2) = (2.0 * h1 + h0, h1 + 2.0 * h0);
                m[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
            }
        }
        Spline { x, y, m }
    }
    pub fn eval(&self, t: f64) -> f64 {
        let n = self.x.len();
        if t <= self.x[0] {
            return self.y[0];
        }
        if t >= self.x[n - 1] {
            return self.y[n - 1];
        }
        let mut i = 0;
        while t > self.x[i + 1] {
            i += 1;
        }
        let h = self.x[i + 1] - self.x[i];
        let s = (t - self.x[i]) / h;
        let (s2, s3) = (s * s, s * s * s);
        (2.0 * s3 - 3.0 * s2 + 1.0) * self.y[i]
            + (s3 - 2.0 * s2 + s) * h * self.m[i]
            + (-2.0 * s3 + 3.0 * s2) * self.y[i + 1]
            + (s3 - s2) * h * self.m[i + 1]
    }
}
