//! The noise toolkit of the terrain spec (`docs/terrain.md`, "Noise toolkit (#240)"): gradient
//! noise with analytic derivatives, fBm over it, a domain warp, ridges, monotone cubic splines,
//! and the bounds that let a brick decide a whole box without sampling it. Nothing in the
//! generator calls it yet, so the generator's output does not change.
//!
//! **Arithmetic (ADR 0003).** `+ - * /`, `sqrt`, `abs`, comparisons, casts and an integer hash
//! made of `wrapping_*`; no transcendentals, no `powi`, no `mul_add`. A power-of-two scale is
//! exact, so the in-cell fraction of an integer lattice point is exact too.
//!
//! **The noise** is improved Perlin noise (Perlin 2002) with hashed corners, so it has no period:
//! a quintic fade, the 12 edge gradients of a cube (length √2), `f64`. Its [`gradient`] takes a
//! float point; its [`lattice_gradient`] takes an integer lattice point and a power-of-two cell
//! like `value_noise`, and keeps the last cell's eight corner gradients in a [`GradientCache`].
//! Both give the same bits for the same point. Derivatives are analytic.
//!
//! # Proven bounds
//!
//! Over one cell, with the fraction `f` in `[0, 1]³`, `u = fade(f)` and corner weights
//! `W_c = Π (u or 1 − u)`, the noise is `n(f) = Σ_c W_c (g_c · d_c)` with `d_c = f − c`.
//!
//! **Value: `|n| ≤ VALUE_BOUND = √(3/2)`.**
//! - `|g_c · d_c| ≤ √2 |d_c|`, so `|n| ≤ √2 Σ_c W_c |d_c|`.
//! - `Σ_c W_c = 1`, so Jensen gives `Σ_c W_c |d_c| ≤ √(Σ_c W_c |d_c|²) = √(Σ_axes e(f_i))`, with
//!   `e(x) = (1 − u) x² + u (1 − x)²`.
//! - `e(x) ≤ 1/4`. Put `t = x − ½` and `u = ½ + s(t)`: `e = ¼ + t² − 2 s(t) t`. `s' = 30 (¼ − t²)²`
//!   falls on `[0, ½]`, so `s` is concave there, `s(0) = 0` and `s(½) = ½`, and `s(t) ≥ t` (the
//!   chord); so `e ≤ ¼ + t² − 2t² ≤ ¼`. `e` is even in `t`.
//! - So `|n| ≤ √2 √(3/4) = √(3/2)` (about 1.2247; the largest value found by search is 1.036).
//!
//! **Gradient: `|∂ₓ n| ≤ AXIS_GRADIENT_BOUND` per axis, `|∇n| ≤ GRADIENT_BOUND = √3 ·` that.**
//! The bound is per unit of `f`: an octave of cell `c` lattice points has `GRADIENT_BOUND / c`
//! per lattice point.
//! - Group the corners in pairs that differ in x. Then `∂ₓ n = Σ_yz W_yz · T`, with
//!   `T = g₀ · q₀ + g₁ · q₁`, `q₀ = ((1 − u) − u′x, −u′a, −u′b)`, `q₁ = (u − u′(1 − x), u′a, u′b)`,
//!   `u′ = fade′(x) = 30 x² (1 − x)²`, `a`, `b` the y and z offsets of that pair's corner.
//! - The 12 gradients are every vector with two entries `±1` and one `0`, so the largest `g · q`
//!   is the sum of the two largest `|q_i|`: `top2(q)`. `T* = top2(q₀) + top2(q₁)` bounds `|T|`.
//! - `T*` depends on `s = |a|` and `t = |b|`, which lie in `[0, 1]`; it is convex and
//!   non-decreasing in each (a maximum of sums of them). A convex function lies under its chord, so
//!   `T*(s, t)` is at most the multilinear interpolation of its four corner values.
//! - `s` is `y` or `1 − y` with weights `1 − u_y` and `u_y`, so its mean is `y + u_y (1 − 2y) ≤ ½`
//!   (the sign of `1 − 2y` is that of `½ − u_y`). The interpolation is non-decreasing in those
//!   means, so `∂ₓ n ≤ ¼ Σ_corners T*` at `s, t ∈ {0, 1}`: a function of `x` alone.
//! - Its largest value on `[0, 1]` is 141/32 = 4.40625 at `x = ½`. The test
//!   `the_gradient_bound_is_proved_on_a_grid` evaluates it on a grid of 2¹⁴ points and adds the
//!   function's Lipschitz bound, so the constant holds between grid points as well.
//! - The axes are interchangeable, so `|∇n| ≤ √3 · AXIS_GRADIENT_BOUND`. The largest gradient found
//!   by search is 3.75 (at the cell's centre), so this is about 2× loose in the worst case.
//!
//! **A box.** For `x` in a box with centre `c` and half-diagonal `r`, `|F(x) − F(c)| ≤ G r` where
//! `G` bounds `|∇F|` over the box (the mean value theorem on the segment from `c`, which the box
//! contains). [`centre_plus_gradient`] is that. A sum of octaves has `G = Σ amplitude · G_octave`,
//! and a rotation of an octave's input does not change `|∇|`.
//!
//! **A spline** is monotone on each segment between its knots (a constructor checks it), so over an
//! interval it takes its extremes at the interval's ends and at the knots inside it: the bound
//! comes from its knots. A spline whose knots hold splines has a sound but looser bound.

/// `|noise| ≤ VALUE_BOUND` everywhere: `√(3/2)`, rounded up. See "Proven bounds".
pub const VALUE_BOUND: f64 = 1.224_744_871_391_59;

/// `|∂ noise / ∂ axis| ≤ AXIS_GRADIENT_BOUND` per unit of the in-cell fraction.
pub const AXIS_GRADIENT_BOUND: f64 = 4.41;

/// `|∇ noise| ≤ GRADIENT_BOUND` per unit of the in-cell fraction: `√3 ·` [`AXIS_GRADIENT_BOUND`],
/// rounded up.
pub const GRADIENT_BOUND: f64 = 7.64;

/// Room for the rounding of the sums in the bounds, relative to the margin and the amplitudes.
const SLACK: f64 = 1e-9;

// ---------------------------------------------------------------------------------------------
// The hash and the gradients.

const PRIME_X: u64 = 0x5205_402B_9270_C86F;
const PRIME_Y: u64 = 0x598C_D327_0038_17B5;
const PRIME_Z: u64 = 0x5BCC_226E_9FA0_BACB;
const HASH_MULTIPLIER: u64 = 0x53A3_F72D_EEC5_46F5;

/// Perlin's 12 edge gradients, and the first four again to make 16 (a table, not an if-chain, so
/// that nothing mispredicts). Every vector has two entries of `±1` and one of 0.
const GRADIENTS: [[f64; 3]; 16] = [
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
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

/// The eight corner gradients of the lattice cell at `lattice`, x fastest: a table index each.
/// One wrapping multiply per corner (the hash of OpenSimplex2), the top four bits.
fn corner_gradients(seed: u64, lattice: [i64; 3]) -> [u8; 8] {
    let [x, y, z] = lattice.map(|c| c as u64);
    let corner = |x: u64, y: u64, z: u64| {
        let h =
            (seed ^ x.wrapping_mul(PRIME_X) ^ y.wrapping_mul(PRIME_Y) ^ z.wrapping_mul(PRIME_Z))
                .wrapping_mul(HASH_MULTIPLIER);
        (h >> 60) as u8
    };
    std::array::from_fn(|i| {
        corner(
            x.wrapping_add((i & 1) as u64),
            y.wrapping_add((i >> 1 & 1) as u64),
            z.wrapping_add((i >> 2) as u64),
        )
    })
}

/// The seed of octave `k` of a noise with `seed`.
pub fn salt(seed: u64, k: u64) -> u64 {
    seed ^ (k + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// The quintic fade `6t⁵ − 15t⁴ + 10t³` and its derivative `30 t² (t − 1)²`.
fn fade(t: f64) -> (f64, f64) {
    (
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0),
        30.0 * t * t * (t - 1.0) * (t - 1.0),
    )
}

/// The offsets from the eight corners of a cell to the point at fraction `f`, x fastest.
fn corner_offsets([x, y, z]: [f64; 3]) -> [[f64; 3]; 8] {
    let (x1, y1, z1) = (x - 1.0, y - 1.0, z - 1.0);
    [
        [x, y, z],
        [x1, y, z],
        [x, y1, z],
        [x1, y1, z],
        [x, y, z1],
        [x1, y, z1],
        [x, y1, z1],
        [x1, y1, z1],
    ]
}

/// The gradient of table index `h`. The mask lets the compiler drop the bounds check.
fn gradient_of(h: u8) -> [f64; 3] {
    GRADIENTS[usize::from(h & 15)]
}

fn dot3(g: [f64; 3], d: [f64; 3]) -> f64 {
    g[0] * d[0] + g[1] * d[1] + g[2] * d[2]
}

/// The gradient noise of one cell, from its corner gradients and the fraction `f` in the cell.
fn cell_value(grads: [u8; 8], f: [f64; 3]) -> f64 {
    let [u, v, w] = f.map(|t| fade(t).0);
    let d = corner_offsets(f);
    let a: [f64; 8] = std::array::from_fn(|i| dot3(gradient_of(grads[i]), d[i]));
    trilerp([u, v, w], a)
}

/// A value and its gradient, for [`cell_value_d`].
type ValueGradient = (f64, [f64; 3]);

/// The trilinear blend of eight values, x fastest, at the weights `[u, v, w]`.
fn trilerp([u, v, w]: [f64; 3], a: [f64; 8]) -> f64 {
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    lerp(
        w,
        lerp(v, lerp(u, a[0], a[1]), lerp(u, a[2], a[3])),
        lerp(v, lerp(u, a[4], a[5]), lerp(u, a[6], a[7])),
    )
}

/// The gradient noise of one cell and its derivatives with respect to `f`, analytic. The value is
/// the trilinear blend of the corner values `g · d` with the fade `(u, v, w)`. Its derivative is
/// the blend of the corners' gradients `g` plus, along each axis, the fade's slope times the
/// difference of the blends of the two faces across it.
fn cell_value_d(grads: [u8; 8], f: [f64; 3]) -> ValueGradient {
    let [(u, du), (v, dv), (w, dw)] = f.map(fade);
    let d = corner_offsets(f);
    let g: [[f64; 3]; 8] = std::array::from_fn(|i| gradient_of(grads[i]));
    let a: [f64; 8] = std::array::from_fn(|i| dot3(g[i], d[i]));
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    // The value, by stages, keeping the ones the slopes need.
    let bx = [
        lerp(u, a[0], a[1]),
        lerp(u, a[2], a[3]),
        lerp(u, a[4], a[5]),
        lerp(u, a[6], a[7]),
    ];
    let by = [lerp(v, bx[0], bx[1]), lerp(v, bx[2], bx[3])];
    let value = lerp(w, by[0], by[1]);
    // The slope of each fade times the difference across that axis.
    let across_x = lerp(
        w,
        lerp(v, a[1] - a[0], a[3] - a[2]),
        lerp(v, a[5] - a[4], a[7] - a[6]),
    );
    let across_y = lerp(w, bx[1] - bx[0], bx[3] - bx[2]);
    let across_z = by[1] - by[0];
    let weights = [u, v, w];
    let blend = |axis: usize| trilerp(weights, std::array::from_fn(|i| g[i][axis]));
    (
        value,
        [
            blend(0) + du * across_x,
            blend(1) + dv * across_y,
            blend(2) + dw * across_z,
        ],
    )
}

/// The lattice cell of a float point and the fraction in it. Inputs beyond ±2³⁰ are clamped, which
/// is defined and deterministic.
fn split(p: [f64; 3]) -> ([i64; 3], [f64; 3]) {
    let floor = p.map(|c| c.clamp(-1.073_741_824e9, 1.073_741_824e9).floor());
    let lattice = floor.map(|c| c as i64);
    (lattice, [p[0] - floor[0], p[1] - floor[1], p[2] - floor[2]])
}

/// Gradient noise at a float point, in `[-VALUE_BOUND, VALUE_BOUND]`. The cell is one unit.
pub fn gradient(seed: u64, p: [f64; 3]) -> f64 {
    let (lattice, f) = split(p);
    cell_value(corner_gradients(seed, lattice), f)
}

/// Gradient noise at a float point and its gradient with respect to `p`, analytic.
pub fn gradient_d(seed: u64, p: [f64; 3]) -> ValueGradient {
    let (lattice, f) = split(p);
    cell_value_d(corner_gradients(seed, lattice), f)
}

/// The last lattice cell of one octave and its eight corner gradients, so that neighbouring
/// points, which nearly always share a cell, hash its corners once instead of once each. It only
/// saves work: results are identical with or without it.
#[derive(Clone, Copy, Debug, Default)]
pub struct GradientCache(Option<([i32; 3], [u8; 8])>);

/// The cell, fraction and corner gradients of an integer lattice point (`cell` a power of two).
fn lattice_cell(
    seed: u64,
    point: [i32; 3],
    cell: i32,
    cache: &mut GradientCache,
) -> ([u8; 8], [f64; 3]) {
    debug_assert!(cell > 0 && cell & (cell - 1) == 0);
    let shift = cell.trailing_zeros();
    let lattice = point.map(|p| p >> shift);
    let inverse = 1.0 / f64::from(cell);
    let f = point.map(|p| f64::from(p & (cell - 1)) * inverse);
    let grads = match cache.0 {
        Some((cached, grads)) if cached == lattice => grads,
        _ => {
            let grads = corner_gradients(seed, lattice.map(i64::from));
            cache.0 = Some((lattice, grads));
            grads
        }
    };
    (grads, f)
}

/// Gradient noise at an integer lattice point, with a cell of `cell` points (a power of two, so
/// the fraction is exact). The same bits as [`gradient`] at `point / cell`.
pub fn lattice_gradient(seed: u64, point: [i32; 3], cell: i32, cache: &mut GradientCache) -> f64 {
    let (grads, f) = lattice_cell(seed, point, cell, cache);
    cell_value(grads, f)
}

/// [`lattice_gradient`] and its gradient per lattice point.
pub fn lattice_gradient_d(
    seed: u64,
    point: [i32; 3],
    cell: i32,
    cache: &mut GradientCache,
) -> ValueGradient {
    let (grads, f) = lattice_cell(seed, point, cell, cache);
    let (value, g) = cell_value_d(grads, f);
    let inverse = 1.0 / f64::from(cell);
    (value, g.map(|c| c * inverse))
}

// ---------------------------------------------------------------------------------------------
// Bounds over a box.

/// The bounds of a field over a box: its value at the box's centre plus and minus the greatest
/// gradient magnitude over the box times the box's half-diagonal. Sound for any field whose
/// gradient is within `max_gradient` over the box (see "Proven bounds").
pub fn centre_plus_gradient(
    centre_value: f64,
    max_gradient: f64,
    half_diagonal: f64,
) -> (f64, f64) {
    let margin = max_gradient * half_diagonal * (1.0 + SLACK) + SLACK * (1.0 + centre_value.abs());
    (centre_value - margin, centre_value + margin)
}

/// The half-diagonal of the box with corners `low` and `high`: `sqrt` is correctly rounded.
fn half_diagonal(low: [f64; 3], high: [f64; 3]) -> f64 {
    let d = [0, 1, 2].map(|i| high[i] - low[i]);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 0.5
}

// ---------------------------------------------------------------------------------------------
// Rotations.

/// A fixed rotation of a noise's input, to hide the cubic lattice's axes. Built from an integer
/// quaternion, so it is orthogonal exactly in real arithmetic and each entry is one correctly
/// rounded division.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rotation([[f64; 3]; 3]);

impl Rotation {
    /// The rotation of the quaternion `a + bi + cj + dk`, which must not be zero.
    pub fn from_quaternion([a, b, c, d]: [i32; 4]) -> Rotation {
        let [a, b, c, d] = [a, b, c, d].map(i64::from);
        let n = (a * a + b * b + c * c + d * d) as f64;
        let m = |v: i64| v as f64 / n;
        Rotation([
            [
                m(a * a + b * b - c * c - d * d),
                m(2 * (b * c - a * d)),
                m(2 * (b * d + a * c)),
            ],
            [
                m(2 * (b * c + a * d)),
                m(a * a - b * b + c * c - d * d),
                m(2 * (c * d - a * b)),
            ],
            [
                m(2 * (b * d - a * c)),
                m(2 * (c * d + a * b)),
                m(a * a - b * b - c * c + d * d),
            ],
        ])
    }

    /// The rotation for octave `k`: one of a few fixed ones, none aligned with an axis.
    pub fn for_octave(k: usize) -> Rotation {
        const QUATERNIONS: [[i32; 4]; 4] = [[3, 1, 2, 1], [2, 2, 1, 3], [1, 3, 2, 2], [2, 1, 3, 2]];
        Rotation::from_quaternion(QUATERNIONS[k % QUATERNIONS.len()])
    }

    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        self.0.map(|r| r[0] * p[0] + r[1] * p[1] + r[2] * p[2])
    }

    /// The transpose, which is the inverse: takes a gradient back to the unrotated input.
    pub fn apply_transpose(&self, g: [f64; 3]) -> [f64; 3] {
        let r = &self.0;
        [0, 1, 2].map(|j| r[0][j] * g[0] + r[1][j] * g[1] + r[2][j] * g[2])
    }
}

// ---------------------------------------------------------------------------------------------
// fBm.

#[derive(Clone, Debug)]
struct Octave {
    frequency: f64,
    amplitude: f64,
    rotation: Option<Rotation>,
}

/// Fractal Brownian motion over [`gradient`]: octaves at doubling frequency (exact) and amplitude
/// times `gain`, each with its own seed and, optionally, its own fixed rotation. The gradient is
/// summed analytically.
#[derive(Clone, Debug)]
pub struct Fbm {
    seed: u64,
    octaves: Vec<Octave>,
}

impl Fbm {
    /// `octaves` octaves from `base_frequency` (cells per unit) with amplitude 1, each octave's
    /// frequency twice and its amplitude `gain` times the last. `rotated` gives each octave a
    /// fixed rotation.
    pub fn new(seed: u64, base_frequency: f64, octaves: u32, gain: f64, rotated: bool) -> Fbm {
        let (mut frequency, mut amplitude) = (base_frequency, 1.0);
        let octaves = (0..octaves as usize)
            .map(|k| {
                let octave = Octave {
                    frequency,
                    amplitude,
                    rotation: rotated.then(|| Rotation::for_octave(k)),
                };
                frequency *= 2.0;
                amplitude *= gain;
                octave
            })
            .collect();
        Fbm { seed, octaves }
    }

    pub fn octaves(&self) -> usize {
        self.octaves.len()
    }

    fn octave_input(octave: &Octave, p: [f64; 3]) -> [f64; 3] {
        let q = octave.rotation.map_or(p, |r| r.apply(p));
        q.map(|c| c * octave.frequency)
    }

    pub fn value(&self, p: [f64; 3]) -> f64 {
        let mut total = 0.0;
        for (k, o) in (0u64..).zip(&self.octaves) {
            total += o.amplitude * gradient(salt(self.seed, k), Self::octave_input(o, p));
        }
        total
    }

    /// The value and its gradient with respect to `p`.
    pub fn value_gradient(&self, p: [f64; 3]) -> ValueGradient {
        let (mut total, mut grad) = (0.0, [0.0; 3]);
        for (k, o) in (0u64..).zip(&self.octaves) {
            let (n, g) = gradient_d(salt(self.seed, k), Self::octave_input(o, p));
            let g = o.rotation.map_or(g, |r| r.apply_transpose(g));
            total += o.amplitude * n;
            for i in 0..3 {
                grad[i] += o.amplitude * o.frequency * g[i];
            }
        }
        (total, grad)
    }

    /// `|value| ≤ max_value()` everywhere.
    pub fn max_value(&self) -> f64 {
        let sum: f64 = self.octaves.iter().map(|o| o.amplitude.abs()).sum();
        sum * VALUE_BOUND * (1.0 + SLACK)
    }

    /// `|∇ value| ≤ max_gradient()` everywhere.
    pub fn max_gradient(&self) -> f64 {
        let sum: f64 = self
            .octaves
            .iter()
            .map(|o| o.amplitude.abs() * o.frequency)
            .sum();
        sum * GRADIENT_BOUND * (1.0 + SLACK)
    }

    /// Bounds on the value over the box with corners `low` and `high`: the centre-plus-gradient
    /// bound inside the global one.
    pub fn bounds(&self, low: [f64; 3], high: [f64; 3]) -> (f64, f64) {
        let centre = [0, 1, 2].map(|i| (low[i] + high[i]) * 0.5);
        let (lo, hi) = centre_plus_gradient(
            self.value(centre),
            self.max_gradient(),
            half_diagonal(low, high),
        );
        let max = self.max_value();
        (lo.max(-max), hi.min(max))
    }
}

/// [`Fbm`] over an integer lattice, for a brick's detail: octaves of a power-of-two cell in
/// lattice points and an amplitude, each with a [`GradientCache`] in a [`LatticeCache`].
#[derive(Clone, Debug)]
pub struct LatticeFbm {
    seed: u64,
    octaves: Vec<(i32, f64)>,
}

/// One [`GradientCache`] per octave of a [`LatticeFbm`].
#[derive(Clone, Debug, Default)]
pub struct LatticeCache(Vec<GradientCache>);

impl LatticeFbm {
    /// Octaves as `(cell, amplitude)`; each cell a power of two.
    pub fn new(seed: u64, octaves: &[(i32, f64)]) -> LatticeFbm {
        assert!(
            octaves
                .iter()
                .all(|&(cell, _)| cell > 0 && cell & (cell - 1) == 0)
        );
        LatticeFbm {
            seed,
            octaves: octaves.to_vec(),
        }
    }

    pub fn cache(&self) -> LatticeCache {
        LatticeCache(vec![GradientCache::default(); self.octaves.len()])
    }

    pub fn value(&self, point: [i32; 3], cache: &mut LatticeCache) -> f64 {
        let mut total = 0.0;
        for (k, (&(cell, amplitude), cached)) in (0u64..).zip(self.octaves.iter().zip(&mut cache.0))
        {
            total += amplitude * lattice_gradient(salt(self.seed, k), point, cell, cached);
        }
        total
    }

    /// The value and its gradient per lattice point.
    pub fn value_gradient(&self, point: [i32; 3], cache: &mut LatticeCache) -> ValueGradient {
        let (mut total, mut grad) = (0.0, [0.0; 3]);
        for (k, (&(cell, amplitude), cached)) in (0u64..).zip(self.octaves.iter().zip(&mut cache.0))
        {
            let (n, g) = lattice_gradient_d(salt(self.seed, k), point, cell, cached);
            total += amplitude * n;
            for i in 0..3 {
                grad[i] += amplitude * g[i];
            }
        }
        (total, grad)
    }

    /// The value at a point between lattice points: the same noise.
    pub fn value_at(&self, p: [f64; 3]) -> f64 {
        let mut total = 0.0;
        for (k, &(cell, amplitude)) in (0u64..).zip(&self.octaves) {
            let inverse = 1.0 / f64::from(cell);
            total += amplitude * gradient(salt(self.seed, k), p.map(|c| c * inverse));
        }
        total
    }

    pub fn max_value(&self) -> f64 {
        let sum: f64 = self.octaves.iter().map(|&(_, a)| a.abs()).sum();
        sum * VALUE_BOUND * (1.0 + SLACK)
    }

    /// `|∇ value|` per lattice point is at most this.
    pub fn max_gradient(&self) -> f64 {
        let sum: f64 = self
            .octaves
            .iter()
            .map(|&(cell, a)| a.abs() / f64::from(cell))
            .sum();
        sum * GRADIENT_BOUND * (1.0 + SLACK)
    }

    /// Bounds on the value over the box of lattice points from `low` to `high` (every point
    /// between, whole or not).
    pub fn bounds(&self, low: [i32; 3], high: [i32; 3]) -> (f64, f64) {
        let (low, high) = (low.map(f64::from), high.map(f64::from));
        let centre = [0, 1, 2].map(|i| (low[i] + high[i]) * 0.5);
        let (lo, hi) = centre_plus_gradient(
            self.value_at(centre),
            self.max_gradient(),
            half_diagonal(low, high),
        );
        let max = self.max_value();
        (lo.max(-max), hi.min(max))
    }
}

// ---------------------------------------------------------------------------------------------
// The sphere: tangent parts, the domain warp and ridges.

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The part of `v` along the sphere at a point with unit normal `normal`: `v − (v · n) n`. It is
/// never longer than `v`.
pub fn tangent(v: [f64; 3], normal: [f64; 3]) -> [f64; 3] {
    let along = dot(v, normal);
    [0, 1, 2].map(|i| v[i] - along * normal[i])
}

/// A two-level domain warp for the world map, after Quilez: `q = F₁(p)`, then `r = F₂(p + k₁ q)`,
/// and the warped point is `p + k₂ r`, where each `F` is three [`Fbm`] (the x, y and z of the
/// vector) and each vector is first cut to its tangent part, so a point on the sphere stays on its
/// tangent plane. Octaves of 4 and 6 are 30 noise evaluations (about 1 µs): for the world map
/// only.
#[derive(Clone, Debug)]
pub struct Warp {
    levels: [[Fbm; 3]; 2],
    strength: [f64; 2],
}

impl Warp {
    /// A warp whose fBms start at `base_frequency` with `octaves` octaves each level.
    pub fn new(seed: u64, base_frequency: f64, octaves: [u32; 2], strength: [f64; 2]) -> Warp {
        let level = |l: u64| {
            [0, 1, 2].map(|c| {
                Fbm::new(
                    salt(seed, 3 * l + c),
                    base_frequency,
                    octaves[l as usize],
                    0.5,
                    true,
                )
            })
        };
        Warp {
            levels: [level(0), level(1)],
            strength,
        }
    }

    fn vector(level: &[Fbm; 3], p: [f64; 3], normal: [f64; 3]) -> [f64; 3] {
        tangent(
            [level[0].value(p), level[1].value(p), level[2].value(p)],
            normal,
        )
    }

    /// The warped point. `normal` is the unit direction of `p` from the Planet's centre (for a
    /// texel, its direction).
    pub fn apply(&self, p: [f64; 3], normal: [f64; 3]) -> [f64; 3] {
        let q = Self::vector(&self.levels[0], p, normal);
        let p1 = [0, 1, 2].map(|i| p[i] + self.strength[0] * q[i]);
        let r = Self::vector(&self.levels[1], p1, normal);
        [0, 1, 2].map(|i| p[i] + self.strength[1] * r[i])
    }

    /// The warped point is never further than this from `p`. A field sampled at the warped point
    /// keeps its own global bounds.
    pub fn max_displacement(&self) -> f64 {
        let m = self.levels[1]
            .iter()
            .map(Fbm::max_value)
            .fold(0.0, f64::max);
        self.strength[1].abs() * m * 3f64.sqrt() * (1.0 + SLACK)
    }
}

/// Vanilla's peaks-and-valleys fold `−3 (||w| − ⅔| − ⅓)`: a ridge along `|w| = ⅔`. Its slope is
/// `±3`.
pub fn fold(w: f64) -> f64 {
    -3.0 * ((w.abs() - 2.0 / 3.0).abs() - 1.0 / 3.0)
}

/// `fold`'s slope at `w`.
fn fold_slope(w: f64) -> f64 {
    let side = if w.abs() >= 2.0 / 3.0 { 3.0 } else { -3.0 };
    if w >= 0.0 { -side } else { side }
}

/// The bounds of [`fold`] over `|w| ≤ max_abs`: it is `−1` at 0, `1` at `⅔` and falls with slope 3
/// past it.
pub fn fold_bounds(max_abs: f64) -> (f64, f64) {
    let lo = (-1.0f64).min(fold(max_abs));
    let hi = if max_abs >= 2.0 / 3.0 {
        1.0
    } else {
        fold(max_abs)
    };
    (lo, hi)
}

/// Ridgelines: a folded low-frequency base with detail dragged along the base's slope, as
/// Tectonic's `base + 0.6 · detail(p + 3000 · |∇base|)`. The base's gradient is analytic, and cut
/// to its tangent part on the sphere, so it is four noise evaluations for two octaves each.
#[derive(Clone, Debug)]
pub struct Ridges {
    base: Fbm,
    detail: Fbm,
    drag: f64,
    detail_weight: f64,
}

impl Ridges {
    pub fn new(base: Fbm, detail: Fbm, drag: f64, detail_weight: f64) -> Ridges {
        Ridges {
            base,
            detail,
            drag,
            detail_weight,
        }
    }

    /// The ridge height at `p`, whose unit normal is `normal`: `fold(base) + weight · detail(p + shift)`
    /// with `shift = drag · |tangent(∇ fold(base))|` along each axis.
    pub fn value(&self, p: [f64; 3], normal: [f64; 3]) -> f64 {
        let (w, grad) = self.base.value_gradient(p);
        let slope = fold_slope(w);
        let tangential = tangent(grad.map(|c| c * slope), normal);
        let shifted = [0, 1, 2].map(|i| p[i] + self.drag * tangential[i].abs());
        fold(w) + self.detail_weight * self.detail.value(shifted)
    }

    /// The ridge height is within these bounds everywhere.
    pub fn bounds(&self) -> (f64, f64) {
        let (lo, hi) = fold_bounds(self.base.max_value());
        let d = self.detail_weight.abs() * self.detail.max_value();
        (lo - d, hi + d)
    }
}

// ---------------------------------------------------------------------------------------------
// Splines.

/// Why a [`Spline`] was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum SplineError {
    /// Fewer than two knots.
    TooFewKnots,
    /// A position, value or slope is not finite.
    NotFinite { knot: usize },
    /// Knot positions must strictly increase.
    NotIncreasing { knot: usize },
    /// The slopes at the ends of `segment` leave the Fritsch–Carlson box `0 ≤ α, β ≤ 3`, so the
    /// segment may not be monotone.
    SlopeBreaksMonotone {
        segment: usize,
        alpha: f64,
        beta: f64,
    },
}

/// What a knot holds: a value, or a spline over another coordinate (nesting).
#[derive(Clone, Debug, PartialEq)]
pub enum Child {
    Value(f64),
    Spline(Spline),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Knot {
    pub at: f64,
    pub child: Child,
    /// The slope of the spline along its own coordinate at this knot.
    pub slope: f64,
}

/// A cubic Hermite spline in vanilla's form: Hermite between knots, and linear past the ends with
/// the end knot's slope. Each spline reads one coordinate of the point it is evaluated at, its
/// `axis`; a knot may hold a spline over another axis.
///
/// The constructor checks Fritsch–Carlson's sufficient condition for monotonicity on every
/// segment between knots that both hold plain values, so a spline of plain values lies within
/// the range of the two knots on each segment, and the bound over an interval comes from its
/// knots. Splines with nested knots are checked for finiteness and order only.
#[derive(Clone, Debug, PartialEq)]
pub struct Spline {
    axis: usize,
    knots: Vec<Knot>,
}

impl Spline {
    /// A spline over coordinate `axis` with authored slopes.
    pub fn new(axis: usize, knots: Vec<Knot>) -> Result<Spline, SplineError> {
        if knots.len() < 2 {
            return Err(SplineError::TooFewKnots);
        }
        for (i, k) in knots.iter().enumerate() {
            let finite = k.at.is_finite()
                && k.slope.is_finite()
                && !matches!(k.child, Child::Value(v) if !v.is_finite());
            if !finite {
                return Err(SplineError::NotFinite { knot: i });
            }
            if i > 0 && k.at <= knots[i - 1].at {
                return Err(SplineError::NotIncreasing { knot: i });
            }
        }
        for (segment, pair) in knots.windows(2).enumerate() {
            if let (Child::Value(y0), Child::Value(y1)) = (&pair[0].child, &pair[1].child) {
                let secant = (y1 - y0) / (pair[1].at - pair[0].at);
                let (alpha, beta) = if secant == 0.0 {
                    // A flat segment is monotone only with flat ends.
                    let flat = |m: f64| if m == 0.0 { 0.0 } else { f64::INFINITY };
                    (flat(pair[0].slope), flat(pair[1].slope))
                } else {
                    (pair[0].slope / secant, pair[1].slope / secant)
                };
                if !(0.0..=3.0).contains(&alpha) || !(0.0..=3.0).contains(&beta) {
                    return Err(SplineError::SlopeBreaksMonotone {
                        segment,
                        alpha,
                        beta,
                    });
                }
            }
        }
        Ok(Spline { axis, knots })
    }

    /// A spline through `points` (`(at, value)`, strictly increasing in `at`) with the slopes of
    /// PCHIP (Fritsch–Butland): zero where the neighbouring secants differ in sign, otherwise a
    /// weighted harmonic mean, and shape-preserving one-sided slopes at the ends.
    pub fn pchip(axis: usize, points: &[(f64, f64)]) -> Result<Spline, SplineError> {
        let n = points.len();
        if n < 2 {
            return Err(SplineError::TooFewKnots);
        }
        let h: Vec<f64> = points.windows(2).map(|w| w[1].0 - w[0].0).collect();
        if let Some(i) = h.iter().position(|&h| !(h > 0.0 && h.is_finite())) {
            return Err(SplineError::NotIncreasing { knot: i + 1 });
        }
        let m: Vec<f64> = points
            .windows(2)
            .zip(&h)
            .map(|(w, h)| (w[1].1 - w[0].1) / h)
            .collect();
        let mut slope = vec![0.0; n];
        if n == 2 {
            slope = vec![m[0]; 2];
        } else {
            for k in 1..n - 1 {
                if m[k - 1] * m[k] > 0.0 {
                    let w1 = 2.0 * h[k] + h[k - 1];
                    let w2 = h[k] + 2.0 * h[k - 1];
                    slope[k] = (w1 + w2) / (w1 / m[k - 1] + w2 / m[k]);
                }
            }
            // The one-sided end slope, kept to the shape (as SciPy's `_edge_case`).
            let edge = |h0: f64, h1: f64, m0: f64, m1: f64| {
                let e = ((2.0 * h0 + h1) * m0 - h0 * m1) / (h0 + h1);
                if e * m0 <= 0.0 {
                    0.0
                } else if m0 * m1 <= 0.0 && e.abs() > 3.0 * m0.abs() {
                    3.0 * m0
                } else {
                    e
                }
            };
            slope[0] = edge(h[0], h[1], m[0], m[1]);
            slope[n - 1] = edge(h[n - 2], h[n - 3], m[n - 2], m[n - 3]);
        }
        let knots = points
            .iter()
            .zip(slope)
            .map(|(&(at, y), slope)| Knot {
                at,
                child: Child::Value(y),
                slope,
            })
            .collect();
        Spline::new(axis, knots)
    }

    pub fn axis(&self) -> usize {
        self.axis
    }

    pub fn knots(&self) -> &[Knot] {
        &self.knots
    }

    /// The spline at `coords`: this spline reads `coords[axis]`, and nested ones read theirs.
    pub fn eval(&self, coords: &[f64]) -> f64 {
        self.eval_at(coords[self.axis], coords)
    }

    fn child_value(child: &Child, coords: &[f64]) -> f64 {
        match child {
            Child::Value(v) => *v,
            Child::Spline(s) => s.eval(coords),
        }
    }

    fn eval_at(&self, x: f64, coords: &[f64]) -> f64 {
        let knots = &self.knots;
        let n = knots.len();
        let i = knots.iter().position(|k| k.at >= x).unwrap_or(n);
        if i == 0 || i == n {
            let k = &knots[if i == 0 { 0 } else { n - 1 }];
            return Self::child_value(&k.child, coords) + k.slope * (x - k.at);
        }
        let (a, b) = (&knots[i - 1], &knots[i]);
        let (y0, y1) = (
            Self::child_value(&a.child, coords),
            Self::child_value(&b.child, coords),
        );
        let width = b.at - a.at;
        let k = (x - a.at) / width;
        let rise = y1 - y0;
        let p = a.slope * width - rise;
        let q = -b.slope * width + rise;
        let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
        lerp(k, y0, y1) + k * (1.0 - k) * lerp(k, p, q)
    }

    /// The bounds of the spline over the box with corners `low` and `high` (each indexed by
    /// coordinate, as `eval`'s `coords`).
    ///
    /// Plain-valued: the extremes are at the ends of the interval and at its knots, exactly. Nested:
    /// on each segment the value is `(1 − S(k)) n + S(k) o + width · τ(k)`, a blend of the two
    /// children's values (`S = 3k² − 2k³`) and a term of the slopes alone, so the children's
    /// bounds and `τ`'s range (`|τ| ≤ 4/27` times the slopes) bound it.
    pub fn bounds(&self, low: &[f64], high: &[f64]) -> (f64, f64) {
        let (a, b) = (low[self.axis], high[self.axis]);
        let knots = &self.knots;
        if knots.iter().all(|k| matches!(k.child, Child::Value(_))) {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            let mut see = |x: f64| {
                let v = self.eval_at(x, &[]);
                lo = lo.min(v);
                hi = hi.max(v);
            };
            see(a);
            see(b);
            for k in knots.iter().filter(|k| k.at > a && k.at < b) {
                see(k.at);
            }
            return (lo, hi);
        }
        let child = |c: &Child| match c {
            Child::Value(v) => (*v, *v),
            Child::Spline(s) => s.bounds(low, high),
        };
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let mut join = |l: f64, h: f64| {
            lo = lo.min(l);
            hi = hi.max(h);
        };
        let first = &knots[0];
        let last = &knots[knots.len() - 1];
        // Linear past the ends.
        for (k, from, to) in [(first, a, b.min(first.at)), (last, a.max(last.at), b)] {
            if from <= to {
                let (cl, ch) = child(&k.child);
                let (d0, d1) = (k.slope * (from - k.at), k.slope * (to - k.at));
                join(cl + d0.min(d1), ch + d0.max(d1));
            }
        }
        for pair in knots.windows(2) {
            let (n, o) = (&pair[0], &pair[1]);
            let (from, to) = (a.max(n.at), b.min(o.at));
            if from > to {
                continue;
            }
            let width = o.at - n.at;
            let smooth = |x: f64| {
                let k = (x - n.at) / width;
                k * k * (3.0 - 2.0 * k)
            };
            let ((nl, nh), (ol, oh)) = (child(&n.child), child(&o.child));
            let blend = |s: f64| ((1.0 - s) * nl + s * ol, (1.0 - s) * nh + s * oh);
            let (b0, b1) = (blend(smooth(from)), blend(smooth(to)));
            let tau_lo = (n.slope.min(0.0) - o.slope.max(0.0)) * (4.0 / 27.0) * width;
            let tau_hi = (n.slope.max(0.0) - o.slope.min(0.0)) * (4.0 / 27.0) * width;
            join(
                b0.0.min(b1.0) + tau_lo - SLACK * width,
                b0.1.max(b1.1) + tau_hi + SLACK * width,
            );
        }
        (lo, hi)
    }
}

#[cfg(test)]
mod tests;
