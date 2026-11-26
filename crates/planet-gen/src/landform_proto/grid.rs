//! Cube-sphere grid (equal-angle), neighbour tables, bilinear sampling, helpers.

use super::noise::*;

pub const R: f64 = 320.0; // planet radius, km

const FACES: [(V3, V3, V3); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
    ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
    ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
    ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
];

pub const OFFS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (-1, 1),
    (1, -1),
    (-1, -1),
];

pub struct Grid {
    pub n: usize,
    pub dir: Vec<V3>,
    pub area: Vec<f32>,  // km^2
    pub nbr: Vec<[u32; 8]>,
    pub nbr_d: Vec<[f32; 8]>, // km
    pub lat: Vec<f32>,   // radians
}

pub fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

/// Parallel map over 0..n with chunking.
pub fn par_map<T: Send + Default + Clone>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let mut out = vec![T::default(); n];
    let nt = threads();
    let chunk = n.div_ceil(nt * 4).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out_ptr = SendPtr(out.as_mut_ptr());
    let (f, next_ref, out_ref) = (&f, &next, &out_ptr);
    std::thread::scope(|s| {
        for _ in 0..nt {
            s.spawn(move || loop {
                let c = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let lo = c * chunk;
                if lo >= n {
                    break;
                }
                for i in lo..(lo + chunk).min(n) {
                    // SAFETY: each index is written by exactly one thread.
                    unsafe { *out_ref.0.add(i) = f(i) };
                }
            });
        }
    });
    out
}
struct SendPtr<T>(*mut T);
unsafe impl<T> Sync for SendPtr<T> {}
unsafe impl<T> Send for SendPtr<T> {}

impl Grid {
    pub fn new(n: usize) -> Grid {
        let total = 6 * n * n;
        let da = std::f64::consts::FRAC_PI_2 / n as f64;
        let dir: Vec<V3> = (0..total).map(|k| Self::texel_dir(n, k)).collect();
        let area: Vec<f32> = (0..total)
            .map(|k| {
                let (_, i, j) = (k / (n * n), (k % (n * n)) % n, (k % (n * n)) / n);
                let a = ((i as f64 + 0.5) / n as f64 * 2.0 - 1.0) * std::f64::consts::FRAC_PI_4;
                let b = ((j as f64 + 0.5) / n as f64 * 2.0 - 1.0) * std::f64::consts::FRAC_PI_4;
                let (s, t) = (a.tan(), b.tan());
                (R * R * da * da * (1.0 + s * s) * (1.0 + t * t) / (1.0 + s * s + t * t).powf(1.5)) as f32
            })
            .collect();
        let lat: Vec<f32> = dir.iter().map(|d| d[1].asin() as f32).collect();
        let mut g = Grid { n, dir, area, nbr: Vec::new(), nbr_d: Vec::new(), lat };
        let nb: Vec<([u32; 8], [f32; 8])> = par_map(total, |k| {
            let (f, i, j) = g.unpack(k);
            let mut ids = [0u32; 8];
            let mut ds = [0f32; 8];
            for (m, (di, dj)) in OFFS.iter().enumerate() {
                let (ni, nj) = (i as i32 + di, j as i32 + dj);
                let idx = g.resolve(f, ni, nj);
                ids[m] = idx as u32;
                let c = dot(g.dir[k], g.dir[idx]).clamp(-1.0, 1.0);
                ds[m] = (c.acos() * R) as f32;
            }
            (ids, ds)
        });
        g.nbr = nb.iter().map(|x| x.0).collect();
        g.nbr_d = nb.iter().map(|x| x.1).collect();
        g
    }

    pub fn total(&self) -> usize {
        6 * self.n * self.n
    }
    pub fn unpack(&self, k: usize) -> (usize, usize, usize) {
        let nn = self.n * self.n;
        (k / nn, (k % nn) % self.n, (k % nn) / self.n)
    }
    pub fn index(&self, f: usize, i: usize, j: usize) -> usize {
        f * self.n * self.n + j * self.n + i
    }

    fn face_point(n: usize, f: usize, fi: f64, fj: f64) -> V3 {
        let a = ((fi + 0.5) / n as f64 * 2.0 - 1.0) * std::f64::consts::FRAC_PI_4;
        let b = ((fj + 0.5) / n as f64 * 2.0 - 1.0) * std::f64::consts::FRAC_PI_4;
        let (nn, r, u) = FACES[f];
        norm(add(nn, add(mul(r, a.tan()), mul(u, b.tan()))))
    }
    fn texel_dir(n: usize, k: usize) -> V3 {
        let nn = n * n;
        Self::face_point(n, k / nn, ((k % nn) % n) as f64, ((k % nn) / n) as f64)
    }

    /// Continuous texel coordinates (texel centres at integers) for a direction.
    pub fn locate(&self, d: V3) -> (usize, f64, f64) {
        let ax = [d[0].abs(), d[1].abs(), d[2].abs()];
        let m = if ax[0] >= ax[1] && ax[0] >= ax[2] { 0 } else if ax[1] >= ax[2] { 1 } else { 2 };
        let f = 2 * m + usize::from(d[m] < 0.0);
        let (nn, r, u) = FACES[f];
        let dn = dot(d, nn);
        let a = (dot(d, r) / dn).atan();
        let b = (dot(d, u) / dn).atan();
        let q = std::f64::consts::FRAC_PI_4;
        let fi = (a / q + 1.0) * 0.5 * self.n as f64 - 0.5;
        let fj = (b / q + 1.0) * 0.5 * self.n as f64 - 0.5;
        (f, fi, fj)
    }
    pub fn nearest(&self, d: V3) -> usize {
        let (f, fi, fj) = self.locate(d);
        let n = self.n as i32;
        let i = (fi.round() as i32).clamp(0, n - 1) as usize;
        let j = (fj.round() as i32).clamp(0, n - 1) as usize;
        self.index(f, i, j)
    }
    /// Index of texel (i, j) on face f, where i/j may be one or two outside the face.
    pub fn resolve(&self, f: usize, i: i32, j: i32) -> usize {
        let n = self.n as i32;
        if i >= 0 && j >= 0 && i < n && j < n {
            return self.index(f, i as usize, j as usize);
        }
        self.nearest(Self::face_point(self.n, f, f64::from(i), f64::from(j)))
    }

    pub fn sample(&self, v: &[f32], d: V3) -> f32 {
        let (f, fi, fj) = self.locate(d);
        let (i0, j0) = (fi.floor(), fj.floor());
        let (tx, ty) = ((fi - i0) as f32, (fj - j0) as f32);
        let (i0, j0) = (i0 as i32, j0 as i32);
        let at = |di: i32, dj: i32| v[self.resolve(f, i0 + di, j0 + dj)];
        let a = at(0, 0) * (1.0 - tx) + at(1, 0) * tx;
        let b = at(0, 1) * (1.0 - tx) + at(1, 1) * tx;
        a * (1.0 - ty) + b * ty
    }

    pub fn position(&self, k: usize) -> V3 {
        mul(self.dir[k], R)
    }

    /// East and north unit vectors at a direction (north = +Y pole).
    pub fn frame(d: V3) -> (V3, V3) {
        let e = norm(cross([0.0, 1.0, 0.0], d));
        let e = if len(e) < 1e-6 { [1.0, 0.0, 0.0] } else { e };
        let nrth = norm(cross(d, e));
        (e, nrth)
    }

    /// Area-weighted fraction where mask is true.
    pub fn share(&self, mask: impl Fn(usize) -> bool) -> f64 {
        let mut s = 0.0;
        let mut t = 0.0;
        for k in 0..self.total() {
            let a = f64::from(self.area[k]);
            t += a;
            if mask(k) {
                s += a;
            }
        }
        s / t
    }

    /// Replace values with their area-weighted rank in [lo, hi] (uniform distribution).
    pub fn rank_normalize(&self, v: &mut [f32], lo: f32, hi: f32) {
        let mut idx: Vec<u32> = (0..v.len() as u32).collect();
        idx.sort_unstable_by(|&a, &b| v[a as usize].total_cmp(&v[b as usize]));
        let total: f64 = self.area.iter().map(|&a| f64::from(a)).sum();
        let mut acc = 0.0;
        for &k in &idx {
            let a = f64::from(self.area[k as usize]);
            let r = (acc + 0.5 * a) / total;
            acc += a;
            v[k as usize] = lo + (hi - lo) * r as f32;
        }
    }

    /// Value at an area-weighted quantile.
    pub fn quantile(&self, v: &[f32], q: f64) -> f32 {
        let mut idx: Vec<u32> = (0..v.len() as u32).collect();
        idx.sort_unstable_by(|&a, &b| v[a as usize].total_cmp(&v[b as usize]));
        let total: f64 = self.area.iter().map(|&a| f64::from(a)).sum();
        let mut acc = 0.0;
        for &k in &idx {
            acc += f64::from(self.area[k as usize]);
            if acc >= q * total {
                return v[k as usize];
            }
        }
        v[*idx.last().unwrap() as usize]
    }
}
