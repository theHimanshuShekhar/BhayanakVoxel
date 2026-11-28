use super::*;

/// A well-mixed 64-bit value of `i`.
fn mix(i: u64) -> u64 {
    let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A value in `[0, 1)` from `i`.
fn unit(i: u64) -> f64 {
    (mix(i) >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

fn point(i: u64, extent: f64) -> [f64; 3] {
    [0, 1, 2].map(|a| (unit(i * 3 + a) * 2.0 - 1.0) * extent)
}

fn lattice_point(i: u64, extent: i32) -> [i32; 3] {
    [0, 1, 2].map(|a| (mix(i * 3 + a) >> 33) as i32 % extent)
}

fn norm(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

fn unit_vector(i: u64) -> [f64; 3] {
    let v = point(i, 1.0);
    let n = norm(v);
    if n < 1e-3 {
        [1.0, 0.0, 0.0]
    } else {
        v.map(|c| c / n)
    }
}

// ---- the proofs behind the constants ----

/// `top2` of three non-negative numbers: their sum less the smallest.
fn top2(a: f64, b: f64, c: f64) -> f64 {
    a + b + c - a.min(b).min(c)
}

/// `¼ Σ T*` of "Proven bounds": the bound on `|∂ₓ n|` at `x`.
fn axis_gradient_at(x: f64) -> f64 {
    let (u, du) = fade(x);
    let (a1, a2) = ((1.0 - u - du * x).abs(), (u - du * (1.0 - x)).abs());
    let phi = |s: f64, t: f64| top2(a1, du * s, du * t) + top2(a2, du * s, du * t);
    0.25 * (phi(0.0, 0.0) + phi(1.0, 0.0) + phi(0.0, 1.0) + phi(1.0, 1.0))
}

#[test]
fn the_gradient_bound_is_proved_on_a_grid() {
    // The function is a sum of `top2` (1-Lipschitz in each of three arguments up to a factor 2)
    // of `α₁`, `α₂` and `u′`; `|α′| ≤ 2·u′max + |u″|max·1 = 2 · 1.875 + 6` and `|u″| ≤ 6`
    // (`60 w √(1 − 4w)`, `w = x (1 − x)`, is at most 5.78), so it is `4 · 9.75`-Lipschitz.
    const LIPSCHITZ: f64 = 4.0 * 9.75;
    const STEPS: u32 = 1 << 14;
    let mut sup = 0.0f64;
    for i in 0..=STEPS {
        sup = sup.max(axis_gradient_at(f64::from(i) / f64::from(STEPS)));
    }
    let proved = sup + LIPSCHITZ * 0.5 / f64::from(STEPS);
    assert!(proved <= AXIS_GRADIENT_BOUND, "sup {sup}, proved {proved}");
    // The constant is not much above the proof.
    assert!(proved > AXIS_GRADIENT_BOUND - 0.01, "proved {proved}");
    assert!(GRADIENT_BOUND >= 3f64.sqrt() * AXIS_GRADIENT_BOUND);
    assert!(GRADIENT_BOUND < 3f64.sqrt() * AXIS_GRADIENT_BOUND + 0.01);
}

#[test]
fn the_value_bound_follows_from_the_mean_square_distance() {
    // e(x) = (1 − u) x² + u (1 − x)² ≤ 1/4, so |n| ≤ √2 √(3/4) = √(3/2).
    let mut e_max = 0.0f64;
    let mut mean_max = 0.0f64;
    for i in 0..=20_000 {
        let x = f64::from(i) / 20_000.0;
        let u = fade(x).0;
        e_max = e_max.max((1.0 - u) * x * x + u * (1.0 - x) * (1.0 - x));
        mean_max = mean_max.max((1.0 - u) * x + u * (1.0 - x));
    }
    assert!(
        e_max <= 0.25 + 1e-12 && mean_max <= 0.5 + 1e-12,
        "{e_max} {mean_max}"
    );
    assert!(VALUE_BOUND >= (1.5f64).sqrt());
    assert!(VALUE_BOUND < (1.5f64).sqrt() + 1e-9);
}

/// The corner gradients that make `v · ∇n` greatest at the fraction `f` (the largest `g · q` per
/// corner), as the proof does it, and that greatest value from the proof's formula.
fn worst_gradients(f: [f64; 3], v: [f64; 3]) -> ([u8; 8], f64) {
    let [(u, du), (w, dw), (z, dz)] = f.map(fade);
    let weights = [(1.0 - u, u), (1.0 - w, w), (1.0 - z, z)];
    let slopes = [du, dw, dz];
    let mut grads = [0u8; 8];
    let mut total = 0.0;
    for (i, grad) in grads.iter_mut().enumerate() {
        let bits = [i & 1, i >> 1 & 1, i >> 2];
        let weight_of = |a: usize| {
            if bits[a] == 1 {
                weights[a].1
            } else {
                weights[a].0
            }
        };
        let corner_weight = weight_of(0) * weight_of(1) * weight_of(2);
        // The derivative of this corner's weight along `v`.
        let slope_along: f64 = (0..3)
            .map(|a| {
                let others: f64 = (0..3).filter(|&b| b != a).map(weight_of).product();
                let sign = if bits[a] == 1 { 1.0 } else { -1.0 };
                v[a] * sign * slopes[a] * others
            })
            .sum();
        let d = [0, 1, 2].map(|a| f[a] - bits[a] as f64);
        let q = [0, 1, 2].map(|a| slope_along * d[a] + corner_weight * v[a]);
        let (best, value) = (0..12)
            .map(|g| (g as u8, dot(GRADIENTS[g], q)))
            .fold((0, f64::MIN), |a, b| if b.1 > a.1 { b } else { a });
        *grad = best;
        total += value;
    }
    (grads, total)
}

#[test]
fn the_worst_gradients_reach_the_formula_and_stay_under_the_bounds() {
    let mut greatest = 0.0f64;
    for i in 0..4000u64 {
        let f = [0, 1, 2].map(|a| unit(i * 3 + a));
        let v = unit_vector(i + 99);
        let (grads, formula) = worst_gradients(f, v);
        let (_, g) = cell_value_d(grads, f);
        // The analytic derivative agrees with the proof's formula for the worst corners.
        assert!(
            (dot(v, g) - formula).abs() < 1e-9,
            "{} {formula}",
            dot(v, g)
        );
        assert!(norm(g) <= GRADIENT_BOUND, "{}", norm(g));
        for c in g {
            assert!(c.abs() <= AXIS_GRADIENT_BOUND, "{c}");
        }
        greatest = greatest.max(norm(g));
        let (value, _) = cell_value_d(grads, f);
        assert!(value.abs() <= VALUE_BOUND);
    }
    // At the cell's centre, along an axis, the greatest is 3.75.
    let centre = [0.5; 3];
    let (grads, formula) = worst_gradients(centre, [1.0, 0.0, 0.0]);
    assert!((formula - 3.75).abs() < 1e-9, "{formula}");
    assert!((cell_value_d(grads, centre).1[0] - 3.75).abs() < 1e-9);
    assert!(greatest > 2.5 && greatest < GRADIENT_BOUND, "{greatest}");
}

#[test]
fn the_value_is_within_its_bound_at_the_worst_corners_and_at_random() {
    let mut greatest = 0.0f64;
    for i in 0..4000u64 {
        let f = [0, 1, 2].map(|a| unit(i * 3 + a + 7));
        // Each corner's gradient that points most along its offset from the point.
        let grads: [u8; 8] = std::array::from_fn(|c| {
            let d = [0, 1, 2].map(|a| f[a] - ((c >> a) & 1) as f64);
            (0..12u8)
                .max_by(|&a, &b| {
                    dot(GRADIENTS[a as usize], d).total_cmp(&dot(GRADIENTS[b as usize], d))
                })
                .unwrap()
        });
        greatest = greatest.max(cell_value(grads, f).abs());
    }
    assert!(greatest <= VALUE_BOUND && greatest > 0.95, "{greatest}");
    for i in 0..100_000u64 {
        let v = gradient(mix(i / 1000), point(i, 1000.0));
        assert!(v.abs() <= VALUE_BOUND, "{v}");
    }
}

// ---- the noise itself ----

#[test]
fn the_noise_is_the_same_on_the_lattice_and_between_lattice_points() {
    for cell in [1, 2, 16, 64, 1024] {
        let mut cache = GradientCache::default();
        for i in 0..2000u64 {
            let p = lattice_point(i, 1 << 20);
            let uncached = lattice_gradient(3, p, cell, &mut GradientCache::default());
            let cached = lattice_gradient(3, p, cell, &mut cache);
            let float = gradient(3, p.map(|c| f64::from(c) / f64::from(cell)));
            assert_eq!(uncached.to_bits(), cached.to_bits());
            assert_eq!(uncached.to_bits(), float.to_bits(), "{p:?} cell {cell}");
            let (value, g) = lattice_gradient_d(3, p, cell, &mut cache);
            let (fv, fg) = gradient_d(3, p.map(|c| f64::from(c) / f64::from(cell)));
            assert_eq!(value.to_bits(), fv.to_bits());
            for a in 0..3 {
                assert!((g[a] - fg[a] / f64::from(cell)).abs() < 1e-12);
            }
        }
    }
}

#[test]
fn a_cached_walk_gives_the_same_bits_as_an_uncached_one() {
    let mut cache = GradientCache::default();
    for i in 0..4096u64 {
        let p = [
            i as i32 % 16 - 8,
            (i / 16) as i32 % 16 - 8,
            (i / 256) as i32 - 8,
        ];
        let a = lattice_gradient(9, p, 8, &mut cache);
        let b = lattice_gradient(9, p, 8, &mut GradientCache::default());
        assert_eq!(a.to_bits(), b.to_bits(), "{p:?}");
    }
}

#[test]
fn the_noise_does_not_repeat_and_is_not_flat() {
    // Hashed corners have no 256-cell period.
    let same = (0..200u64)
        .filter(|&i| {
            let p = point(i, 100.0);
            gradient(5, p).to_bits() == gradient(5, [p[0] + 256.0, p[1], p[2]]).to_bits()
        })
        .count();
    assert_eq!(same, 0);
    let n = 20_000;
    let (mut sum, mut square) = (0.0, 0.0);
    for i in 0..n {
        let v = gradient(5, point(i, 500.0));
        sum += v;
        square += v * v;
    }
    let mean = sum / n as f64;
    assert!(mean.abs() < 0.02, "{mean}");
    assert!(square / n as f64 > 0.05, "{}", square / n as f64);
    // Another seed is another noise.
    assert_ne!(gradient(5, [0.3, 0.4, 0.5]), gradient(6, [0.3, 0.4, 0.5]));
}

#[test]
fn the_noise_is_continuous_a_tiny_step_never_jumps_more_than_the_gradient_allows() {
    // The test that caught OpenSimplex2's reference code: 3e-3 over a step of 1e-6.
    let h = 1e-6;
    let mut worst = 0.0f64;
    for line in 0..4000u64 {
        let o = point(line * 2, 40.0);
        let dir = unit_vector(line * 2 + 1);
        let mut prev = gradient(1, o);
        for k in 1..64 {
            let t = k as f64 * h;
            let p = [0, 1, 2].map(|a| o[a] + dir[a] * t);
            let v = gradient(1, p);
            let jump = (v - prev).abs();
            assert!(jump <= GRADIENT_BOUND * h * (1.0 + 1e-6), "{jump} at {p:?}");
            worst = worst.max(jump);
            prev = v;
        }
    }
    // The steps are about a gradient's worth and no more.
    assert!(worst > 0.2 * h, "{worst}");
}

fn finite_difference(f: impl Fn([f64; 3]) -> f64, p: [f64; 3]) -> [f64; 3] {
    let e = 1e-6;
    [0, 1, 2].map(|a| {
        let (mut hi, mut lo) = (p, p);
        hi[a] += e;
        lo[a] -= e;
        (f(hi) - f(lo)) / (2.0 * e)
    })
}

#[test]
fn the_analytic_derivatives_match_finite_differences() {
    for i in 0..2000u64 {
        let p = point(i, 30.0);
        let (value, g) = gradient_d(4, p);
        assert_eq!(value, gradient(4, p));
        let fd = finite_difference(|q| gradient(4, q), p);
        for a in 0..3 {
            assert!(
                (g[a] - fd[a]).abs() < 1e-5,
                "{p:?} axis {a}: {} vs {}",
                g[a],
                fd[a]
            );
        }
        let fbm = Fbm::new(4, 0.7, 5, 0.5, true);
        let (value, g) = fbm.value_gradient(p);
        assert!((value - fbm.value(p)).abs() < 1e-12);
        let fd = finite_difference(|q| fbm.value(q), p);
        for a in 0..3 {
            assert!(
                (g[a] - fd[a]).abs() < 1e-4 * (1.0 + fd[a].abs()),
                "{} vs {}",
                g[a],
                fd[a]
            );
        }
    }
    // The lattice fBm's gradient is per lattice point.
    let fbm = LatticeFbm::new(8, &[(256, 24.0), (64, 8.0), (16, 2.0)]);
    let mut cache = fbm.cache();
    for i in 0..2000u64 {
        let p = lattice_point(i, 1 << 18);
        let (value, g) = fbm.value_gradient(p, &mut cache);
        assert_eq!(value.to_bits(), fbm.value(p, &mut fbm.cache()).to_bits());
        let at = p.map(f64::from);
        assert!((fbm.value_at(at) - value).abs() < 1e-9);
        let fd = finite_difference(|q| fbm.value_at(q), at);
        for a in 0..3 {
            assert!((g[a] - fd[a]).abs() < 1e-5, "{} vs {}", g[a], fd[a]);
        }
    }
}

#[test]
fn a_rotation_is_orthogonal_and_undone_by_its_transpose() {
    for k in 0..8 {
        let r = Rotation::for_octave(k);
        for i in 0..200u64 {
            let p = point(i, 1000.0);
            let q = r.apply(p);
            assert!((norm(q) - norm(p)).abs() < 1e-10 * (1.0 + norm(p)));
            let back = r.apply_transpose(q);
            for a in 0..3 {
                assert!((back[a] - p[a]).abs() < 1e-10 * (1.0 + norm(p)));
            }
        }
        // None of them keeps an axis where it is.
        assert!(r.apply([1.0, 0.0, 0.0])[0].abs() < 0.95);
    }
}

// ---- the centre-plus-gradient bounds against sampling ----

/// Samples a box's corners, centre and `n` hashed points, and says whether any is outside.
fn escapes(
    (lo, hi): (f64, f64),
    low: [f64; 3],
    high: [f64; 3],
    n: u64,
    seed: u64,
    f: impl Fn([f64; 3]) -> f64,
) -> Option<f64> {
    let inside = |v: f64| v >= lo && v <= hi;
    for c in 0..8usize {
        let p = [0, 1, 2].map(|a| if (c >> a) & 1 == 1 { high[a] } else { low[a] });
        let v = f(p);
        if !inside(v) {
            return Some(v);
        }
    }
    for i in 0..n {
        let p = [0, 1, 2]
            .map(|a| low[a] + unit(seed * 1_000_003 + i * 3 + a as u64) * (high[a] - low[a]));
        let v = f(p);
        if !inside(v) {
            return Some(v);
        }
    }
    None
}

#[test]
fn each_octaves_bound_holds_over_thousands_of_hashed_boxes_of_several_sizes() {
    for (n, cell) in [1, 4, 16, 64, 256, 4096].into_iter().enumerate() {
        let fbm = LatticeFbm::new(11 + n as u64, &[(cell, 1.0)]);
        for (s, size) in [1, 2, 7, 32, 100, 700, 5000].into_iter().enumerate() {
            for b in 0..400u64 {
                let low = lattice_point(b * 7 + s as u64, 1 << 22);
                let high = low.map(|c| c + size);
                let bounds = fbm.bounds(low, high);
                let (lowf, highf) = (low.map(f64::from), high.map(f64::from));
                let out = escapes(bounds, lowf, highf, 64, b, |p| fbm.value_at(p));
                assert_eq!(
                    out, None,
                    "cell {cell} size {size} box {low:?} bounds {bounds:?}"
                );
            }
        }
    }
}

#[test]
fn the_bound_of_a_small_box_is_tight() {
    // A box of one lattice point in a cell of 64 spans little more than the noise's own change.
    let fbm = LatticeFbm::new(2, &[(64, 1.0)]);
    for b in 0..200u64 {
        let low = lattice_point(b, 1 << 20);
        let (lo, hi) = fbm.bounds(low, low.map(|c| c + 1));
        assert!(
            hi - lo < 2.0 * GRADIENT_BOUND * 0.87 / 64.0 + 1e-6,
            "{}",
            hi - lo
        );
    }
}

#[test]
fn the_fbm_bounds_hold_over_hashed_boxes_with_and_without_rotation() {
    for rotated in [false, true] {
        let fbm = Fbm::new(21, 0.05, 6, 0.5, rotated);
        for (s, size) in [0.01, 0.2, 1.0, 5.0, 40.0, 300.0].into_iter().enumerate() {
            for b in 0..300u64 {
                let low = point(b * 5 + s as u64, 200.0);
                let high = low.map(|c| c + size);
                let bounds = fbm.bounds(low, high);
                let out = escapes(bounds, low, high, 64, b, |p| fbm.value(p));
                assert_eq!(out, None, "rotated {rotated} size {size} bounds {bounds:?}");
                assert!(bounds.0 >= -fbm.max_value() && bounds.1 <= fbm.max_value());
            }
        }
    }
}

#[test]
fn the_lattice_fbm_bounds_hold_in_a_brick_sized_box_and_in_large_ones() {
    let octaves = [(256, 24.0), (64, 8.0), (16, 2.0), (8, 1.0)];
    let fbm = LatticeFbm::new(3, &octaves);
    let mut tight = 0usize;
    let boxes = 3000u64;
    for b in 0..boxes {
        let size = [4, 8, 16, 64, 400, 3000][(b % 6) as usize];
        let low = lattice_point(b, 1 << 21);
        let high = low.map(|c| c + size);
        let bounds = fbm.bounds(low, high);
        let (lowf, highf) = (low.map(f64::from), high.map(f64::from));
        assert_eq!(
            escapes(bounds, lowf, highf, 48, b, |p| fbm.value_at(p)),
            None
        );
        assert!(bounds.0 >= -fbm.max_value() && bounds.1 <= fbm.max_value());
        // A brick-sized box (8 points) is much narrower than the whole range.
        if size <= 8 && bounds.1 - bounds.0 < 0.5 * 2.0 * fbm.max_value() {
            tight += 1;
        }
    }
    assert!(
        tight > 0,
        "no small box was tighter than half the global range"
    );
}

// ---- the warp and the ridges ----

#[test]
fn the_tangent_part_is_perpendicular_to_the_normal_and_no_longer() {
    for i in 0..500u64 {
        let n = unit_vector(i);
        let v = point(i + 1000, 3.0);
        let t = tangent(v, n);
        assert!(dot(t, n).abs() < 1e-12);
        assert!(norm(t) <= norm(v) * (1.0 + 1e-12));
    }
}

#[test]
fn the_warp_moves_a_point_along_the_sphere_within_its_proven_distance() {
    let warp = Warp::new(31, 0.4, [3, 3], [2.0, 3.0]);
    let max = warp.max_displacement();
    let mut greatest = 0.0f64;
    for i in 0..1500u64 {
        let n = unit_vector(i);
        let p = n.map(|c| c * 5.0);
        let moved = warp.apply(p, n);
        let shift = [0, 1, 2].map(|a| moved[a] - p[a]);
        assert!(
            dot(shift, n).abs() < 1e-9,
            "the shift is off the tangent plane"
        );
        assert!(norm(shift) <= max, "{} > {max}", norm(shift));
        greatest = greatest.max(norm(shift));
    }
    assert!(
        greatest > 0.2 * max.min(1.0) && greatest > 0.05,
        "{greatest}"
    );
    // The same warp twice gives the same point.
    let n = unit_vector(7);
    assert_eq!(
        warp.apply([1.0, 2.0, 3.0], n),
        warp.apply([1.0, 2.0, 3.0], n)
    );
}

fn ridges() -> Ridges {
    Ridges::new(
        Fbm::new(41, 0.3, 2, 0.2, true),
        Fbm::new(42, 0.6, 2, 5.0, true),
        0.3,
        0.6,
    )
}

#[test]
fn the_ridges_stay_within_their_bounds_and_the_detail_follows_the_slope() {
    let ridges = ridges();
    let (lo, hi) = ridges.bounds();
    let (mut seen_lo, mut seen_hi) = (f64::MAX, f64::MIN);
    for i in 0..20_000u64 {
        let n = unit_vector(i);
        let v = ridges.value(n.map(|c| c * 4.0), n);
        assert!(v >= lo && v <= hi, "{v} not in [{lo}, {hi}]");
        seen_lo = seen_lo.min(v);
        seen_hi = seen_hi.max(v);
    }
    assert!(seen_hi > 0.0 && seen_lo < 0.0, "{seen_lo} {seen_hi}");
    // With no drag the detail is not dragged.
    let flat = Ridges::new(
        Fbm::new(41, 0.3, 2, 0.2, true),
        Fbm::new(42, 0.6, 2, 5.0, true),
        0.0,
        0.6,
    );
    let n = unit_vector(3);
    let p = n.map(|c| c * 4.0);
    assert_ne!(flat.value(p, n), ridges.value(p, n));
}

#[test]
fn the_fold_is_a_ridge_along_two_thirds_with_slope_three() {
    assert!((fold(0.0) + 1.0).abs() < 1e-15);
    assert!((fold(2.0 / 3.0) - 1.0).abs() < 1e-15);
    assert!(fold(1.0).abs() < 1e-15);
    for i in 0..2000u64 {
        let w = (unit(i) * 2.0 - 1.0) * 2.0;
        let (lo, hi) = fold_bounds(w.abs());
        assert!(fold(w) >= lo - 1e-12 && fold(w) <= hi + 1e-12);
        // Away from its kinks (0, ±⅔) the slope is the formula's.
        if w.abs() > 1e-3 && (w.abs() - 2.0 / 3.0).abs() > 1e-3 {
            let e = 1e-7;
            let slope = (fold(w + e) - fold(w - e)) / (2.0 * e);
            assert!((slope - fold_slope(w)).abs() < 1e-5, "{slope} at {w}");
        }
    }
}

// ---- splines ----

fn knot(at: f64, y: f64, slope: f64) -> Knot {
    Knot {
        at,
        child: Child::Value(y),
        slope,
    }
}

#[test]
fn a_spline_passes_through_its_knots_and_is_linear_past_the_ends() {
    let s = Spline::new(
        0,
        vec![
            knot(0.0, 0.0, 0.5),
            knot(1.0, 1.0, 1.0),
            knot(3.0, 2.0, 0.0),
        ],
    )
    .unwrap();
    for k in s.knots() {
        let Child::Value(y) = k.child else {
            unreachable!()
        };
        assert_eq!(s.eval(&[k.at]), y);
    }
    assert_eq!(s.eval(&[-2.0]), -1.0);
    assert_eq!(s.eval(&[7.0]), 2.0);
    // Continuous and flat where the slopes say so.
    assert!((s.eval(&[3.0 - 1e-9]) - 2.0).abs() < 1e-8);
    assert!(s.eval(&[0.5]) > 0.0 && s.eval(&[0.5]) < 1.0);
}

#[test]
fn authored_slopes_that_leave_the_fritsch_carlson_box_are_rejected() {
    let ok = |a: f64, b: f64| Spline::new(0, vec![knot(0.0, 0.0, a), knot(1.0, 1.0, b)]);
    assert!(ok(0.0, 0.0).is_ok());
    assert!(
        ok(3.0, 3.0).is_ok(),
        "the corner of the box is just monotone"
    );
    assert!(matches!(
        ok(3.01, 1.0),
        Err(SplineError::SlopeBreaksMonotone { segment: 0, .. })
    ));
    assert!(matches!(
        ok(1.0, -0.1),
        Err(SplineError::SlopeBreaksMonotone { .. })
    ));
    // A falling segment has falling slopes.
    assert!(Spline::new(0, vec![knot(0.0, 1.0, -2.0), knot(1.0, 0.0, -1.0)]).is_ok());
    assert!(Spline::new(0, vec![knot(0.0, 1.0, 2.0), knot(1.0, 0.0, -1.0)]).is_err());
    // A flat segment needs flat ends.
    assert!(Spline::new(0, vec![knot(0.0, 1.0, 0.0), knot(1.0, 1.0, 0.0)]).is_ok());
    assert!(Spline::new(0, vec![knot(0.0, 1.0, 0.1), knot(1.0, 1.0, 0.0)]).is_err());
    assert_eq!(
        Spline::new(0, vec![knot(0.0, 0.0, 0.0)]),
        Err(SplineError::TooFewKnots)
    );
    assert_eq!(
        Spline::new(0, vec![knot(0.0, 0.0, 0.0), knot(0.0, 1.0, 0.0)]),
        Err(SplineError::NotIncreasing { knot: 1 })
    );
    assert_eq!(
        Spline::new(0, vec![knot(0.0, f64::NAN, 0.0), knot(1.0, 1.0, 0.0)]),
        Err(SplineError::NotFinite { knot: 0 })
    );
}

fn random_points(i: u64, n: usize) -> Vec<(f64, f64)> {
    let mut at = -1.0;
    let mut y = -1.0;
    let mut up = i.is_multiple_of(2);
    (0..n as u64)
        .map(|k| {
            at += 0.05 + unit(i * 100 + k);
            // Mostly rising, with flat runs and a turn now and then.
            let step = unit(i * 100 + k + 50);
            if step < 0.15 {
                up = !up;
            }
            if step > 0.3 {
                y += if up { step } else { -step };
            }
            (at, y)
        })
        .collect()
}

#[test]
fn pchip_slopes_pass_the_check_and_the_spline_never_leaves_its_knots_range_on_a_segment() {
    for i in 0..400u64 {
        let points = random_points(i, 2 + (i % 9) as usize);
        let s = Spline::pchip(0, &points).unwrap_or_else(|e| panic!("{points:?}: {e:?}"));
        for w in points.windows(2) {
            let (lo, hi) = (w[0].1.min(w[1].1), w[0].1.max(w[1].1));
            for k in 0..=50 {
                let x = w[0].0 + (w[1].0 - w[0].0) * f64::from(k) / 50.0;
                let v = s.eval(&[x]);
                assert!(
                    v >= lo - 1e-12 && v <= hi + 1e-12,
                    "{v} outside [{lo}, {hi}] on {w:?}"
                );
            }
        }
    }
    assert_eq!(
        Spline::pchip(0, &[(0.0, 0.0), (0.0, 1.0)]),
        Err(SplineError::NotIncreasing { knot: 1 })
    );
}

#[test]
fn a_spline_bound_comes_from_its_knots_and_holds_over_hashed_intervals() {
    for i in 0..300u64 {
        let points = random_points(i, 3 + (i % 8) as usize);
        let s = Spline::pchip(0, &points).unwrap();
        let (first, last) = (points[0].0 - 1.0, points[points.len() - 1].0 + 1.0);
        for b in 0..20u64 {
            let a = first + unit(i * 31 + b) * (last - first);
            let c = a + unit(i * 37 + b) * (last - first) * 0.5;
            let (lo, hi) = s.bounds(&[a], &[c]);
            for k in 0..=100 {
                let v = s.eval(&[a + (c - a) * f64::from(k) / 100.0]);
                assert!(
                    v >= lo - 1e-12 && v <= hi + 1e-12,
                    "{v} outside [{lo}, {hi}]"
                );
            }
            // And it is reached: the bound is the extremes of the samples at the knots and ends.
            assert!(lo <= s.eval(&[a]) && hi >= s.eval(&[c]).min(hi));
        }
        // Across all the knots a monotone-per-segment spline stays within its knots' values.
        let (lo, hi) = s.bounds(&[points[0].0], &[points[points.len() - 1].0]);
        let ys = points.iter().map(|p| p.1);
        assert_eq!(lo, ys.clone().fold(f64::MAX, f64::min));
        assert_eq!(hi, ys.fold(f64::MIN, f64::max));
    }
}

/// A spline over axis 0 whose knots hold splines over axis 1, whose knots hold splines over axis 2
/// of plain values (rising, PCHIP slopes), with hashed slopes above them (nested knots are not
/// checked for monotonicity).
fn nested(i: u64) -> Spline {
    fn build(i: u64, counter: &mut u64, axis: usize) -> Spline {
        let n = 3 + axis;
        let at = |j: usize| -1.0 + 2.0 * j as f64 / (n - 1) as f64;
        let next = |counter: &mut u64| {
            *counter += 1;
            unit(i * 100_000 + *counter) * 2.0 - 1.0
        };
        if axis == 2 {
            let points: Vec<_> = (0..n).map(|j| (at(j), j as f64 + next(counter))).collect();
            return Spline::pchip(axis, &points).unwrap();
        }
        let mut knots = Vec::new();
        for j in 0..n {
            let child = Child::Spline(build(i, counter, axis + 1));
            let slope = next(counter) * 0.5;
            knots.push(Knot {
                at: at(j),
                child,
                slope,
            });
        }
        Spline::new(axis, knots).unwrap()
    }
    build(i, &mut 0, 0)
}

#[test]
fn nested_splines_evaluate_continuously_and_their_bounds_hold_over_boxes() {
    for i in 0..40u64 {
        let s = nested(i);
        // Continuous across the outer knots and the ends.
        for edge in [-1.0, 1.0] {
            let c = [edge, 0.3, -0.2];
            let near = [edge + 1e-9, 0.3, -0.2];
            assert!((s.eval(&c) - s.eval(&near)).abs() < 1e-6);
        }
        for b in 0..60u64 {
            let low = [0, 1, 2].map(|a| unit(i * 7919 + b * 3 + a as u64) * 3.0 - 1.5);
            let high = [0, 1, 2].map(|a| low[a] + unit(i * 104_729 + b * 3 + a as u64) * 1.5);
            let (lo, hi) = s.bounds(&low, &high);
            for k in 0..120u64 {
                let c = [0, 1, 2].map(|a| {
                    low[a] + unit(i * 31 + b * 977 + k * 3 + a as u64) * (high[a] - low[a])
                });
                let v = s.eval(&c);
                assert!(
                    v >= lo - 1e-9 && v <= hi + 1e-9,
                    "{v} outside [{lo}, {hi}] at {c:?}"
                );
            }
        }
    }
}
