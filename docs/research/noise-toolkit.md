# Research: a deterministic noise toolkit for Tectonic-like landforms

Resolves [issue #240](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/240), part of map [#238](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/238). Researched 2026-11-24 against `origin/main` at `03fc6e3`. Terms follow [`CONTEXT.md`](../../CONTEXT.md).

This file lives on the throwaway branch `research/noise-toolkit` and is never merged. The microbenchmark it quotes is in [`noise-toolkit-bench/`](noise-toolkit-bench/) beside it.

Labels used below, as in [`deterministic-generation.md`](deterministic-generation.md):
- **[source]**: a primary source states it, and the link follows.
- **[measured]**: I ran it on the dev box (Ryzen 7 5700G, rustc 1.99.1), pinned to one core, best of 3 runs × 5 repeats. Other sessions loaded the box heavily during these runs (load average 39–52 on 8 cores), so absolute ns swing by about ±15%. The ratios are steadier than the absolute numbers.
- **[calc]**: my own arithmetic, with the steps shown.
- **[inference]**: my reading of the sources; no source says it directly.
- **[recommendation]**: a judgement call.

## TL;DR

1. **Every primitive Tectonic needs can be built from `+ - * /`, `abs`, comparisons, casts and an integer hash.** None needs a transcendental, and none needs `sqrt` except to normalise a direction. Tectonic's terrain is just four ingredients: multi-octave gradient noise; vanilla's `shifted_noise` domain warp; a slope-driven warp that turns a 2-octave noise into ridgelines; and cubic Hermite splines with author-set slopes. [source: Tectonic's density functions, §1]
2. **Measured determinism:** every primitive below gives one digest (`1a2538d014076107`) in the debug build, the release build and a `-C target-cpu=native` build. The native binary contains no FMA instruction. Integer arithmetic in hashes and salts must be written as `wrapping_*`. One plain `*` panicked in the debug build ("attempt to multiply with overflow"), while release silently wrapped. [measured]
3. **Gradient noise: use 3D improved Perlin noise with hashed corners.** It is quintic-faded and f64, takes the same integer lattice input and power-of-two cells as today's value noise, and gets a per-cell gradient cache and analytic derivatives. Hashing each corner gives it no period, which the 256-entry permutation table would have. Cost: **11.9 ns cached vs today's 7.9 ns (1.5×); 17.9 ns uncached vs today's 29.0 ns (0.6×)**. [measured, recommendation]
4. **Do not use the OpenSimplex2 or OpenSimplex2S reference code as it is.** Both are **discontinuous**: they jump by 3.0e-3 and 1.15e-3 (about 0.3% and 0.1% of their range) over a 1e-6 step, more than 1,000× their mean step. I traced the OS2 jump to its kernel radius² of 0.6: in one cubic lattice, two neighbours tie and both lie in range, but the loop takes only one. **With radius² 0.5 it is provably continuous** (§2.2), isotropic, and costs 23.6 ns cached / 49.3 ns uncached. That makes it the fallback if Perlin's axis bias shows after rotation. [measured, calc]
5. **Domain warping** is `f(p + k·q(p))` with `q` a 3-vector of noises (Quilez's article). Its cost is the sum of the noises it evaluates, about 43 ns per noise evaluation here. A two-level warp of 30 evaluations costs 1.3 µs per sample. That is affordable on the 245 m world map (+4 s on 8 cores for all 25 M texels) but not per Voxel. [source, measured, calc]
6. **Ridgelines, not speckle,** come from three things together:
   - **A low-frequency base, folded:** ridges along the zero set of a 1–2 octave noise, as in Musgrave's ridged multifractal or vanilla's peaks-and-valleys fold `-3(||w| − 2/3| − 1/3)`.
   - **Detail warped along the base's slope:** Tectonic's `ridges = base + 0.6·detail(p + shift)`, where the shift is 3000 × |base(p) − base(p + 10 blocks)| along x and z.
   - **A spline mask:** a jaggedness spline over continentalness and erosion confines the ridges to belts.

   With analytic gradients, Tectonic's ridge costs 4 noise evaluations instead of its 8 finite-difference ones (209 vs 312 ns). [source, measured]
7. **Splines: monotone cubic Hermite**, in vanilla's form: Hermite between knots and linear past the ends, with nesting allowed. Two ways to get the slopes: PCHIP (Fritsch–Butland) computes them, or an author sets them and a test checks them against the Fritsch–Carlson box `0 ≤ αₖ, βₖ ≤ 3`. Evaluation costs 13 ns for 8 knots and 71 ns for a vanilla-style 3-level nested spline. [source, measured]
8. **Sphere:** sample 3D noise at the 3D point, as `planet-gen` already does. It has no seams and no poles, and it needs no cube-map stitching. Warp with a 3D vector (radial components are harmless) or its tangent part. Use the tangential gradient for slope-driven warps. [inference]
9. **The real cost risk is not per-sample cost; it is the brick bounds.** `Detail::bounds` relies on value noise being multilinear in each cell, which makes the exact range over a box sit at its corners. That lever took #166's uncertain bricks from 31% to about 11%. Gradient noise loses that property. A centre value plus an analytic-gradient Taylor bound should stay tight (§4.3), but it has to be built and tested before any octave of the detail switches. [source: `docs/measurements.md`; inference]

---

## 1. What Tectonic and vanilla actually use

**Vanilla's base noise.** From Minecraft 1.18, a "noise" is a `DoublePerlinNoise`: two octave sums of improved Perlin noise, the second sampled at coordinates scaled by 337/331 (1.0181…), with their sum scaled by `(5/3)·n/(n+1)` [source: [cubiomes `noise.c`](https://github.com/Cubitect/cubiomes/blob/e61f90580cbdd883214a8054670dacae655e59c0/noise.c), `sampleDoublePerlin` and `xDoublePerlinInit`]. cubiomes is a third-party C reimplementation, not Mojang's source; I used it because minecraft.wiki's [Noise](https://minecraft.wiki/w/Noise) page does not describe how the noise is computed.
- **Octaves:** each octave is improved Perlin with the quintic fade `t³(t(6t − 15) + 10)` and Perlin's 16-entry edge-gradient switch (`indexedLerp`). The octaves sit at `2^firstOctave` frequency, weighted by `amplitudes[i]·2^(n−1−i)/(2^n − 1)`.
- **Distribution:** the wiki says noises "define a fractal Brownian motion (fBm) configuration, producing approximate output values fitting a normal distribution" [source: [minecraft.wiki: Noise](https://minecraft.wiki/w/Noise)].

**Vanilla's domain warp.**
- **The shift functions:** `shift_a` "samples a noise at (x/4, 0, z/4), then multiplies it by 4", and `shift_b` samples at (z/4, x/4, 0). `shifted_noise` "first shifts the input coordinates" by density functions [source: [minecraft.wiki: Density function](https://minecraft.wiki/w/Density_function)]. In 26.3 snapshot 10 the shift fields moved into `noise` itself, described as "a domain warp" (same page).
- **The climate warp:** cubiomes's biome sampler offsets the climate lookups by `4 · offset(x, 0, z)` and `4 · offset(z, x, 0)`, a 3-octave DoublePerlin named `minecraft:offset` (cubiomes [`biomenoise.c`](https://github.com/Cubitect/cubiomes/blob/e61f90580cbdd883214a8054670dacae655e59c0/biomenoise.c) lines 1154–1157 and 854–859).

**Vanilla's peaks-and-valleys** is a fold of the weirdness noise `w`: `pv = −3(||w| − 0.6667| − 0.3333)` (cubiomes `biomenoise.c` line 1167). That is a ridge function made of `abs` and `+ − *` only. [source]

**Vanilla's splines.**
- **What the wiki says:** `spline` "computes a cubic spline", with points of `location`, `value` (a number or a nested spline) and `derivative` ("the slope at this point"). The wiki doesn't say how it interpolates [source: [Density function](https://minecraft.wiki/w/Density_function)].
- **What the code does:** cubiomes's `getSpline` is cubic Hermite in the form `lerp(k, n, o) + k(1 − k)·lerp(k, p, q)`, with `p = l·Δx − Δy` and `q = −m·Δx + Δy`, and linear extrapolation with the end slope outside the knots. Nested splines evaluate both neighbouring children (`biomenoise.c` lines 1074–1109) [source].
- **Monotonicity:** nothing enforces it; the data's authors choose the slopes. [inference]

**Tectonic's terrain** (read at [`34241bd`](https://github.com/Apollounknowndev/tectonic/tree/34241bdb35acda67b5367d49f354c66c05e098e2), under `src/common/main/resources/resourcepacks/tectonic/data/tectonic/worldgen/`):
- **Mountain ridges** (`density_function/mountain_ridges/ridges.json`) are `base + 0.6 · shifteddetail`:
  - **`base`:** a noise with `firstOctave −8` and `amplitudes [1, 0.2]`, at `xz_scale 1.7`.
  - **`shifteddetail`:** a second noise (`firstOctave −7`, `[0.2, 1]`, `xz_scale 2`), shifted in x by `3000 · |base(p) − base(p + 10 x̂)|` and in z likewise (`shifteddetail.json`, `noiseshift/slopex.json`).
  - **So:** the detail is warped by the absolute finite-difference slope of the base, which drags its features along the base's ridges. The result is cached per column (`flat_cache`, `cache_2d`). [source]
- **Weathering** (`weathering.json`) is `1 − clamp(spline(|noise|))`, with knots `(0.025, value 0, slope 20)` and `(1, value 1, slope 0)` [source].
- **Use in the surface:** the ridges and weathering enter the surface through `jaggedness_continents · (0.6 − ridges − 0.1·weathering)` (`density_function/sloped_cheese.json`). The jaggedness is itself a spline over continentalness, erosion and ridges, so ridges appear only in mountain belts. [source]
- **Other uses of the warp:** the region selector, island ridges, badlands ridges and dunes use `shifted_noise` with vanilla's `minecraft:shift_x`/`shift_z` (`noise/region_selector.json` and others) [source].
- **Continents** are a spline over `|continents noise|`, with knots at 0, 0.4 and 0.48 (`noise/raw_continents.json`) [source].

**So the inventory is short** [inference]:
- gradient-noise fBm;
- an additive domain warp;
- `abs` folds;
- a slope-driven warp;
- cubic Hermite splines, possibly nested;
- per-column caching.

## 2. The primitives, and how to build each deterministically

### 2.1 Today's value noise, the baseline

`crates/planet-gen/src/lib.rs` `value_noise`:
- **Input:** an integer 10 cm lattice point and a power-of-two cell, so the in-cell fraction is exact.
- **Corners:** each takes four splitmix64 finaliser rounds over (seed, x, y, z).
- **Interpolation:** trilinear, with the *cubic* fade `t²(3 − 2t)`.
- **Cache:** one cell's corners per octave.
- **Users:** the world map's `fractal` and `continents`, and the 7-octave `Detail::height` (`detail.rs`) [source].

Its weaknesses for landforms:
- **Blockiness:** value noise shows the lattice as axis-aligned blocks (figure below, top left). [measured: rendered]
- **Derivatives:** the cubic fade is only C¹, so its second derivative jumps on every cell plane. Quilez shows "discontinuity artefacts" when derivatives feed an fBm with the cubic fade, and none with the quintic [source: [Quilez, "Value noise derivatives"](https://iquilezles.org/articles/morenoise/)]. That matters as soon as slopes drive warps or erosion-like damping.

![Slices z = 0.37 across 8 noise units. Top: today's value noise (cubic fade), improved Perlin (hashed corners), OpenSimplex2 with radius² 0.6. Bottom: OpenSimplex2 with radius² 0.5, a 6-octave ridged multifractal, a one-level warped fBm. All from the bench's `render` mode.](noise-toolkit/slices.png)

*Top row: today's value noise, improved Perlin, OS2 with radius² 0.6. Bottom row: OS2 with radius² 0.5, a ridged multifractal (radius² 0.5), a warped fBm (radius² 0.5).*

### 2.2 Gradient noise

**Improved Perlin noise** [source: Perlin, ["Improving noise"](https://doi.org/10.1145/566654.566636), ACM TOG 21(3), 2002]:
- **Gradients:** 12 edge vectors (16 with repeats).
- **Fade:** quintic, so the noise is C².
- **Evaluation:** 8 corners, trilinear blend of `g·d`.
- **Determinism:** everything is `+ − * floor` and table lookups.
- **The permutation table:** Perlin's 256-entry table repeats every 256 cells. A 3.2 m octave would repeat every 819 m [calc: 256 × 3.2 m]. Instead, hash each corner with one wrapping multiply and take 4 high bits; this is OS2's hash: `(seed ^ xp ^ yp ^ zp) · 0x53A3F72DEEC546F5` with `xp = x · PRIME_X` and so on, and the bench takes bits 59–62. It has no period and is 4.5 ns slower uncached [measured].
- **The gradient switch mispredicts:** Perlin's reference `grad` is an if-chain. On random points it cost 107 ns a sample, and a 16-entry table brought it to 21 ns [measured]. Use the table.
- **Axis bias:** cubic-lattice noise has axis-aligned bias, which vanilla hides with the 337/331 second copy [source: cubiomes]. A cheaper fix is a fixed orthonormal rotation of each octave's input (9 multiplies, 6 adds), as Quilez does between octaves (`p = m·p·2.0` with `m = [[0.8, −0.6], [0.6, 0.8]]`) [source: [morenoise](https://iquilezles.org/articles/morenoise/)].
- **Rotation and caching:** a rotation gives up the exact integer fraction and the per-cell cache, because rotated cells no longer line up with Voxels. So rotate the macro octaves on the map, and judge whether the brick detail even needs rotating; ridges and warps already hide the axes. [recommendation]

**OpenSimplex2 (F) and OpenSimplex2S** [source: [KdotJPG/OpenSimplex2](https://github.com/KdotJPG/OpenSimplex2/tree/4cd120d35bfc27096698de90d1bcbf4f9d359a3b), CC0, `rust/fast.rs` and `rust/smooth.rs`]:
- **3D construction:** a "rotated body-centered-cubic grid as the offset union of two rotated cubic grids". OS2 finds "the closest two points on each" lattice, with kernel `(r² − |d|²)⁴` and r² = 0.6. OS2S finds "the closest four points" with r² = 0.75 (README).
- **Determinism:** the reference uses only `+ − *`, casts and a 64-bit wrapping multiply hash. Its gradient table is normalised by one division at start-up, which can be a `const` [source: `fast.rs` lines 146–330 and 516–620].
- **Recommended use:** the README recommends OS2S "for ridged noise (if passing individual layers into `abs(x)`)" [source].

**Finding: both OS2 references are discontinuous.** [measured]
- **The scan:** I walked 2,000 random lines × 5,000 steps through the vendored f32 references. With a step of 1e-6, the largest step-to-step change was **3.03e-3 for OS2 and 1.15e-3 for OS2S**, against mean steps of 1.3e-6 and 1.1e-6. Improved Perlin's largest was 2.3e-6, four times its mean, as a smooth function's should be. (Test `scan_all` in the bench.)
- **The cause, for OS2** [calc]:
  - **Where it jumped:** in the port at (12551.296875, −13345.703125, 19509.515625). The second cubic lattice there has `|x|, |y|, |z|` offsets of (0.49877, 0.49877, 0.02524) from its closest point, where `|x|` and `|y|` tie to 2e-12.
  - **Both neighbours are in range:** the closest point's falloff is `a = 0.6 − (0.49877² + 0.49877² + 0.02524²) = 0.1018`. The x and the y neighbours both have `b = a + 2·0.49877 − 1 = 0.0994 > 0`.
  - **The loop drops one:** it takes only the larger (`if ax0 >= ay0 && ax0 >= az0`). The dropped point's contribution, `b⁴·(g·d)` ≈ 1e-4 × 28 × 1 ≈ 3e-3, is exactly the size of the jump.
- **Why r² ≤ 0.5 is exact** [calc]: within such a radius, one cubic lattice holds at most the closest point and one axis neighbour.
  - **Two axis neighbours:** if both the x and the y neighbour were within r² = 0.5, adding the two conditions gives `2(ax−½)² + 2(ay−½)² + 2az² < 0`, which is impossible.
  - **A diagonal neighbour:** its distance² is `(1−ax)² + (1−ay)² + az² ≥ 0.5`.
  - **So:** "closest plus best axis neighbour", which is exactly what the loop evaluates, is complete.

  The r² = 0.5 port has a largest 1e-6 step of 4.6e-6, so it is continuous [measured]. Its analytic gradient matches central differences to 2.2e-5 (the r² = 0.6 port's "gradient" missed by 1.4e3 at the jumps) [measured].
- **Upstream:** neither the README nor the repository's issues mention it (the searched issues are #2, #5 and #9) [source].
- **Look:** r² = 0.5 looks very like 0.6, slightly more dotted (figure). Its amplitude is smaller: rescaled by (0.6/0.5)⁴, the largest |value| over 4 M points was 0.79. A production port needs a *proven* bound, for the brick bounds.
- **The port:** to f64, the reference stays within 1e-5 at all but 91 of 100,000 lattice-aligned points; those 91 sit on such ties. [measured]

**Analytic derivatives** cost little for both.
- **Perlin:** the quintic fade's derivative is `30t²(t−1)²` [source: [morenoise](https://iquilezles.org/articles/morenoise/)].
- **OS2:** each point adds `a⁴·g − 8a³(g·d)·d`, with the result rotated back by the transpose of OS2's orthonormal rotation. The +gradient variant costs 54 vs 49 ns uncached [measured].

**Precision:**
- **f64 throughout:** planet-gen is f64, so keep the port f64.
- **Where the reference is safe:** it converts the in-cell offset to f32 only *after* subtracting the base point in f64. That keeps it fine at planet-scale lattice coordinates (up to 3.2 × 10⁶ points at 10 cm).
- **Casts:** `as i32` casts saturate in Rust, which is defined and deterministic; clamp the input range anyway.

[source: reference code; inference]

**Recommendation.** One gradient primitive, improved Perlin with hashed corners:
- **Input:** an integer lattice point and power-of-two cell, as today, so fractions stay exact and the per-cell cache keeps working.
- **Arithmetic:** quintic fade, f64.
- **Derivatives:** analytic.
- **Rotation:** a fixed rotation where the axes show (the macro octaves on the map).
- **Fallback:** keep OS2 with r² = 0.5 in case a visual check of rotated Perlin still shows the lattice.
- **Avoid:** OS2 at 0.6 and OS2S.

[recommendation]

### 2.3 Domain warping

**The construction.** Quilez defines a warp as `f(p + h(p))`.
- **One level:** `fbm(p + 4q)`, with `q = (fbm(p), fbm(p + (5.2, 1.3)))`.
- **Two levels:** `fbm(p + 4r)`, with `r = (fbm(p + 4q + (1.7, 9.2)), fbm(p + 4q + (8.3, 2.8)))`.
- **The offsets** "don't have any special meaning, they are used to get different fBM values by using one single fbm() implementation".

[source: [Quilez, "Domain warping"](https://iquilezles.org/articles/warp/)]

**Under ADR 0003** it is pure `+ − *` [inference]:
- in 3D, `q` has three components (three salted fBms);
- in place of the offsets, use a different hash salt per component.

**Cost** is the sum of the noises evaluated: 42–44 ns per noise evaluation in fBm [measured].
- **One level:** 3 × fBm3 + fBm6 = 15 evaluations, 0.64 µs per sample.
- **Two levels:** 30 evaluations, 1.32 µs per sample.

**On the world map**, two levels over all 6 × 2048² = 25.2 M texels cost 25.2 M × 1.32 µs = 33 s of CPU, about 4 s on 8 cores [calc]. That is against today's 26 s map build (`docs/measurements.md`, "The 320 km generator").

**Per brick it is too dear** [calc]: a two-level warp at each of the ~16–25 height columns of a level-0 brick (§4.2) costs 21–33 µs, +40–65% on #166's 50–54 µs a brick. Warp on the map, or with one or two low octaves per brick. [recommendation]

**Vanilla's own warp is cheap:** a 3-octave noise of ±4 quart-blocks feeds the climate lookups (§1). A small-amplitude, low-octave warp breaks up lattice-aligned shapes without the cost of Quilez's full pattern. [source, inference]

### 2.4 Ridge noise that forms ridgelines

- **The basic fold:** `1 − |n|` peaks along the zero set of `n`. On a slice that zero set is a network of closed curves at the noise's feature scale, so a single high-frequency fold reads as cells or speckle, not ranges. [inference; the figure's ridged panel shows the network]
- **Musgrave's ridged multifractal,** as libnoise implements it (`RidgedMulti::GetValue`):
  - each octave's `signal = (offset − |n|)²` is multiplied by `weight`;
  - the next weight is `clamp(signal·gain, 0, 1)`;
  - offset is 1, gain 2, and the spectral weights are `frequency^(−H)`.

  The weighting puts high-octave detail only on the crests and leaves the valleys smooth. libnoise credits it: "Multifractal code originally written by F. Kenton 'Doc Mojo' Musgrave, 1998" [source: [libnoise `ridgedmulti.cpp`](https://github.com/qknight/libnoise/blob/9ce0737b55812f7de907e86dc633724524e3a8e8/src/module/ridgedmulti.cpp)].
- **The only transcendental:** libnoise's spectral weights use `pow(frequency, −H)`. With lacunarity 2 and H = 1 they are exactly `2^−i` and need no `pow`. For other H, compute them once as constants or with pinned `libm` (the clippy ban allows that) [source: `clippy.toml`; inference].
- **How Tectonic makes ranges** (§1):
  1. a very low-frequency base, 2 octaves at `2^−8` × 1.7;
  2. detail dragged along the base's slope, by 3000 × the base's change over 10 blocks, so detail features stretch and fold along the base's ridges;
  3. jaggedness splines over continentalness and erosion, so ridges exist only in belts;
  4. weathering, a spline of `|noise|` that roughens some ridges.

  [source]
- **With analytic gradients:** the slope can come from analytic gradients instead of Tectonic's two extra finite-difference samples per octave. That is 4 noise evaluations (2 with gradients) instead of 8, **209 ns vs 312 ns** [measured].
- **Derivative-damped fBm:** Quilez's version divides each octave by `1 + |Σ∇|²`, which gives "erosion-like effects… with flat areas as well as more rough areas". It is pure `+ − * /` and costs 381 ns for 7 octaves [source: [morenoise](https://iquilezles.org/articles/morenoise/); measured]. It is a cheap stand-in for weathered slopes, not hydraulic erosion.
- **Recommendation for #246:** mountain ranges as `jaggedness(c, e) · ridged(base_lowfreq, warped by its own tangential gradient)` on the world map. Detail per brick should keep the existing ridged octaves (`detail.rs` already folds the three widest with `2|n| − 1`), moved to gradient noise. [recommendation]

### 2.5 Monotone cubic and Hermite splines

**Evaluation.** Use vanilla's form (§1): Hermite between knots, linear past the ends with the end slope.
- **Nesting:** knot values may be nested splines over another coordinate.
- **Arithmetic:** `+ − * /` only.
- **The knot search:** a linear scan for up to about 12 knots (cubiomes caps a spline at 11). [source]

**Slopes, two ways:**
- **PCHIP** (Fritsch–Butland, as SciPy's `PchipInterpolator`):
  - **interior slopes:** 0 where the neighbouring secants differ in sign or one is zero; otherwise the weighted harmonic mean `(w₁ + w₂)/(w₁/dₖ₋₁ + w₂/dₖ)`, with `w₁ = 2hₖ + hₖ₋₁` and `w₂ = hₖ + 2hₖ₋₁`;
  - **end slopes:** one-sided and shape-preserving;
  - **result:** "preserves monotonicity in the interpolation data and does not overshoot".

  [source: [SciPy `_cubic.py`](https://github.com/scipy/scipy/blob/main/scipy/interpolate/_cubic.py), citing Fritsch & Butland, SIAM J. Sci. Stat. Comput. 5(2) 300–304, 1984, [doi:10.1137/0905021](https://doi.org/10.1137/0905021)]
- **Author-set slopes** (vanilla, Tectonic): check them with Fritsch–Carlson's sufficient condition.
  - **The condition:** with `αₖ = mₖ/Δₖ` and `βₖ = mₖ₊₁/Δₖ`, a segment is monotone if `0 ≤ αₖ, βₖ ≤ 3` [source: Fritsch & Carlson, "Monotone Piecewise Cubic Interpolation", SIAM J. Numer. Anal. 17(2) 238–246, 1980, [doi:10.1137/0717021](https://doi.org/10.1137/0717021); the paper is paywalled and I verified the title and pages through Crossref].
  - **The box's corner** [calc]: for y from 0 to 1 on [0, 1], `p′(t) = 6t(1−t) + α(1 − 4t + 3t²) + β(3t² − 2t)`. At α = β = 3, `p′(½) = 1.5 − 0.75 − 0.75 = 0`, so the box's corner is just monotone.
- **When monotonicity matters:** for climate → height maps, so that more continental never means lower. Not every map should be monotone: Tectonic's ocean/land blend splines rise and fall on purpose (`sloped_cheese.json`). [inference]

**Cost** [measured]:
- 13 ns for an 8-knot spline;
- 71 ns for a vanilla-style nested 8 × 6 × 5 spline, which evaluates 1 + 2 + 4 segments.

So splines are negligible next to the noise that feeds them.

## 3. Determinism under ADR 0003

**The rules.** `planet-gen`'s doc comment and `clippy.toml` already state them:
- only `+ − * /`, `sqrt` and conversions on floats;
- integer hashes;
- no std transcendentals, `powi` or `powf` (all banned by clippy);
- no `HashMap` or `HashSet`;
- a `GENERATOR_VERSION` bump for any level-0 change.

[source: [`crates/planet-gen/src/lib.rs`](../../crates/planet-gen/src/lib.rs), [`crates/planet-gen/clippy.toml`](../../crates/planet-gen/clippy.toml)]

**Why that suffices:** Rust's IEEE 754 guarantees for these operations, and its refusal to fuse `a·b + c`, are covered in [`deterministic-generation.md`](deterministic-generation.md) §1.1 (RFC 3514).

**What each primitive above needs** [inference, checked by the measurement below]:
- **Arithmetic:** `+ − * /`, `abs`, `floor` (or casts), `min`, `max`, `clamp` and comparisons.
- **Hashing:** `wrapping_mul` and xor on `i64`/`u64`.
- **Constants:** literals, or `const` arithmetic such as the gradient normalisation.
- **Not needed:** `sqrt`, except to normalise a direction (unit sphere points, tangent projection), and it is correctly rounded. No transcendental at all.

**Pitfalls found while building them:**
- **Overflow in debug builds.** `(o as i64) * SALT` panicked in the debug build and wrapped in release. Use `wrapping_mul` and `wrapping_add` for every hash and salt; clippy's `arithmetic_side_effects` lint could enforce this. [measured; recommendation]
- **Ties are deterministic, not continuous.** OS2's tie-break (`>=`) gives the same result everywhere, but the noise jumps there (§2.2). Determinism and continuity are separate properties to test. [measured]
- **`pow` in reference code.** libnoise's spectral weights use `pow`; with lacunarity 2, replace them with exact powers of two (§2.4). [source]
- **One-time tables** (gradient normalisation, permutation tables) must be built from integers or `const` float arithmetic, never from `sin`/`cos`. A gradient table can be literal values, as OS2's is. [source: `fast.rs`]

**Measured** [measured]:
- **One digest everywhere:** the bench folds every output of every case into one digest. With `n = 32768` it was `1a2538d014076107` in the **debug, release and `-C target-cpu=native`** builds.
- **No fusion:** the native binary contains **0** `vfmadd`/`vfmsub`/`vfnmadd` instructions.
- **Scope:** this machine only. The cross-platform proof is CI's matrix with golden hashes, from #12's research.

## 4. Cost per sample, against today's value noise

### 4.1 The table

All rows are [measured]: ns per call, best of 3 runs × 5 repeats over 1,048,576 samples, one pinned core, under the load noted at the top. "Random" points are spread with no cell reuse. "Coherent" is an 8³ brick walk at 5 cm Voxels over a 10 cm lattice, the access pattern of `Detail::height`. "Per noise" divides by the number of noise evaluations in the call.

| Case | ns per call | ns per noise | vs today |
|---|---:|---:|---:|
| **Random points** | | | |
| today: `value_noise`, int point, cell 64, no cache | 29.0 | 29.0 | 1.0× |
| value noise, float point, quintic | 31.7 | 31.7 | 1.1× |
| improved Perlin, permutation table, float point | 21.7 | 21.7 | 0.75× |
| improved Perlin, int point, cell 64, permutation | 13.4 | 13.4 | 0.46× |
| **improved Perlin, int point, hashed corners (no period)** | **17.9** | 17.9 | **0.62×** |
| OpenSimplex2 reference (f32, vendored) | 55.0 | 55.0 | 1.9× |
| OpenSimplex2S reference (f32, vendored) | 61.1 | 61.1 | 2.1× |
| OS2 f64 port, r² 0.6 | 42.6 | 42.6 | 1.5× |
| **OS2 f64 port, r² 0.5 (continuous)** | **49.3** | 49.3 | **1.7×** |
| OS2 r² 0.5 + analytic gradient | 54.3 | 54.3 | 1.9× |
| **Coherent brick walk** | | | |
| today: `value_noise` with its corner cache | 7.9 | 7.9 | 1.0× |
| **improved Perlin, hashed corners, gradient cache** | **11.9** | 11.9 | **1.5×** |
| improved Perlin, permutation, gradient cache | 11.9 | 11.9 | 1.5× |
| improved Perlin, float point, no cache | 20.8 | 20.8 | 2.6× |
| OS2 r² 0.5 (no cache possible) | 23.6 | 23.6 | 3.0× |
| OS2 r² 0.5 + analytic gradient | 37.1 | 37.1 | 4.7× |
| **Compositions (OS2 r² 0.5), random points** | | | |
| fBm, 7 octaves | 303.5 | 43.4 | |
| ridged multifractal, 7 octaves | 303.3 | 43.3 | |
| derivative-damped fBm, 7 octaves | 381.1 | 54.4 | |
| one-level warp (15 evaluations) | 639.3 | 42.6 | |
| two-level warp (30 evaluations) | 1323.5 | 44.1 | |
| Tectonic ridges, finite differences (8 evaluations) | 312.1 | 39.0 | |
| Tectonic ridges, analytic gradient (4 evaluations) | 209.1 | 52.3 | |
| **Splines** | | | |
| PCHIP, 8 knots | 13.3 | | |
| nested 8 × 6 × 5 | 71.1 | | |

**Why today's noise is expensive uncached:** it hashes 8 corners × 4 splitmix rounds. A per-corner single-multiply hash makes gradient noise *cheaper* than today's value noise on random points. [measured; inference about the cause]

**Where today's noise is cheap:** in the coherent walk it is only 7.9 ns because nearly every sample reuses the cell's 8 cached corners, leaving the fade and 7 lerps. Cached Perlin adds 8 three-term dot products, hence 1.5×. [measured; inference]

Compositions use OS2 r² 0.5, the dearer candidate, so they are an upper bound. With Perlin, multiply them by about 0.4 on random points [calc: 17.9 / 43].

### 4.2 What that means per brick and per map

These estimates are [calc] from the table, not measured in the generator:
- **Per brick:** `Detail::height` sums 7 octaves per distinct 10 cm lattice point under the brick. At level 0 that is about 4 × 4 to 5 × 5 columns, roughly 16–25 points, so 112–175 noise evaluations [inference from `detail.rs`: `lattice` projects to the sea-level sphere, and the `sums` cache keys on the lattice point].
  - **With cached Perlin:** about 4 ns more each, so +0.5 to +0.7 µs a brick, **about 1% of #166's 50–54 µs**.
  - **With OS2 r² 0.5:** +16 ns each, so +1.8 to +2.8 µs, about 5%.
- **Per map:** the 6 × 2048² map holds 25.2 M texels.
  - **Today's elevation:** 7 uncached value-noise octaves at about 29 ns, about 5.1 s of CPU.
  - **Perlin fBm instead:** 7 × 17.9 ns, about 3.2 s.
  - **Adding mountain ridges (209 ns), a two-level warp (1.3 µs) and three splines:** about 1.7 µs a texel, 43 s of CPU, **about 5–6 s on 8 cores, +20% on today's 26 s build**.

### 4.3 The bounds, which is the real cost risk

#166 measured the brick bounds. Its lever was making `detail::noise_range` exact: "a cell's noise is multilinear in the faded fractions… so over a box it is at its extremes at the box's corners". That replaced a Lipschitz bound and cut uncertain bricks from 2,894 to 1,050 of 9,216; the uncertain bricks are worked Voxel by Voxel [source: `docs/measurements.md`, "The 320 km generator"].
- **What gradient noise loses:** it is not multilinear, so the corner trick does not carry over. [inference]
- **A bound that should stay tight:** per octave, `n(c) ± (Σᵢ|∂ᵢn(c)|·rᵢ + ½·M₂·|r|²)`, where `c` is the box centre, `r` its half-extent, and `M₂` a proven bound on the octave's second derivatives (a constant per noise, scaled by `1/cell²`). A level-0 brick is 0.4 m against cells of 3.2 m or more, so `|r|/cell ≤ 0.11`. The quadratic term is then under 1% of the octave's amplitude per unit of `M₂`, and the linear term is exact to first order. [inference; must be built and tested]
- **Before switching:** this bound and the global range of the chosen noise need proofs, plus a test like `the_bounds_on_the_ground_over_a_brick_hold_and_are_tight`, before any detail octave switches. Otherwise the per-brick cost can jump far more than the per-sample table suggests. This affects #244's `Detail::bounds` interaction directly. [recommendation]

## 5. Use on the sphere

**3D noise at points on the sphere** is what `planet-gen` already does:
- **The map:** `fractal` samples the lattice point of a sphere of `noise` points in `direction`.
- **The detail:** sampled on the sphere of sea level.
- **Continents:** jittered balls in a 3D lattice, "in 3D, so that there is no seam on the sphere".

[source: `world_map.rs`, `detail.rs`]

All the primitives above are 3D functions of position, so restricting them to the sphere has no seams, no pole pinch and no cube-face edges. [inference]

**Scale:** distances in the noise are chords, not arcs. At the 52 km continent scale on a 320 km Planet the chord is 0.11% shorter [calc: θ = 52/320 = 0.1625 rad; chord/arc = sin(θ/2)/(θ/2) ≈ 1 − θ²/24 = 0.9989]. That is negligible.

**Warps:**
- A 3D warp vector also moves the point radially, onto another shell of the 3D noise. That is harmless but wastes one of three components. [inference]
- **The cheaper tangent warp:** `q − (q·n)n`, with `n = p/R` for a point on a sphere of known radius `R`. That needs no `sqrt`; on the map, use the texel's direction. [inference]
- **Slope-driven warps:** for Tectonic's warp, use the tangential gradient `∇n − (∇n·n)n`. Tectonic's world is flat, so it uses the x and z slopes. [inference]

**Lattice orientation:** OS2's "ImproveXY" and "ImproveXZ" orientations favour one axis as "vertical" (`fast.rs` comments). A sphere has no global vertical, so either orientation, or `Fallback`, is equally good. [source; inference]

**Splines** act on scalar fields and have no geometry. Climate fields sampled from 3D noise inherit the noise's seamlessness. [inference]

## 6. Recommendations for #244 and #246

1. **The noise module.** Add a small `noise` module to `planet-gen` with:
   - `gradient3(seed, lattice point, cell, &mut cache) -> f64` and `gradient3_d`, which also returns the gradient: improved Perlin with hashed corners and the quintic fade;
   - `fbm`, `ridged` (Musgrave weighting, exact 2⁻ⁱ weights) and `warp(p, salts, octaves, k)`;
   - `Spline` (Hermite, linear ends, nesting, PCHIP or checked author slopes).

   Golden hashes for each, in the CI matrix. Keep `value_noise` for `continents` and for anything whose output must not change until a `GENERATOR_VERSION` bump. [recommendation]
2. **Macro landforms on the world map:**
   - climate fields from rotated Perlin fBm with a small vanilla-style warp;
   - mountain belts as `jaggedness_spline(c, e) · ridged(low-frequency base warped along its tangential gradient)`;
   - height from nested splines over continentalness, erosion and pv, with vanilla's `−3(||w| − 2/3| − 1/3)` fold for pv.

   That is about +20% on the map build [calc §4.2]. [recommendation]
3. **Per-brick detail:** move the 7 octaves to cached gradient noise (+1% a brick) only together with the Taylor bound of §4.3. Without the bound, keep value noise. [recommendation]
4. **Prototype first:** judge Perlin's axis bias on a world-map render, with and without per-octave rotation, before choosing between Perlin and OS2 r² 0.5. If the lattice shows, use OS2 r² 0.5, whose 1.7–3× cost is still small on the map. [recommendation]
5. **Not recommended:**
   - OS2 at r² 0.6 and OS2S: measured discontinuous;
   - 2D noise on cube faces: seams;
   - Quilez's two-level warp per Voxel: its cost would double the brick.

   [measured, calc]

## Reproducing

```sh
cd docs/research/noise-toolkit-bench
cargo build --release && taskset -c 5 ./target/release/noise-toolkit-bench   # table; last line ALL-DIGEST
./target/release/noise-toolkit-bench 32768 | tail -1                          # digest at n = 32768
cargo build && ./target/debug/noise-toolkit-bench 32768 | tail -1             # same digest in debug
RUSTFLAGS="-C target-cpu=native" cargo build --release --target-dir target-native
./target-native/release/noise-toolkit-bench 32768 | tail -1                   # same digest, native
cargo test --release scan_all -- --nocapture                                  # discontinuity scan (H=1e-6 for the fine step)
./target/release/noise-toolkit-bench render                                   # PGM slices to /tmp/noise240/
```

`src/vendor/` holds KdotJPG's `fast.rs` and `smooth.rs` unchanged (CC0). They are kept only to compare against.

## Sources

- **ADR and generator:** [ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md); [`crates/planet-gen/src/lib.rs`](../../crates/planet-gen/src/lib.rs), [`detail.rs`](../../crates/planet-gen/src/detail.rs), [`world_map.rs`](../../crates/planet-gen/src/world_map.rs) and [`clippy.toml`](../../crates/planet-gen/clippy.toml) at `03fc6e3`; [`docs/measurements.md`](../measurements.md), "The 320 km generator"; [`deterministic-generation.md`](deterministic-generation.md).
- **Tectonic:** [Apollounknowndev/tectonic @ 34241bd](https://github.com/Apollounknowndev/tectonic/tree/34241bdb35acda67b5367d49f354c66c05e098e2), the `worldgen/density_function` and `worldgen/noise` JSON.
- **Minecraft:** [minecraft.wiki: Density function](https://minecraft.wiki/w/Density_function); [Noise](https://minecraft.wiki/w/Noise); [Noise router](https://minecraft.wiki/w/Noise_router) (roles only, no definitions).
- **cubiomes** (a C reimplementation of vanilla generation, not Mojang source): [Cubitect/cubiomes @ e61f905](https://github.com/Cubitect/cubiomes/tree/e61f90580cbdd883214a8054670dacae655e59c0), `noise.c` and `biomenoise.c`.
- **OpenSimplex2:** [KdotJPG/OpenSimplex2 @ 4cd120d](https://github.com/KdotJPG/OpenSimplex2/tree/4cd120d35bfc27096698de90d1bcbf4f9d359a3b), README, `rust/fast.rs` and `rust/smooth.rs`; issues searched for "discontinuity", "seam" and "artifact".
- **Perlin:** K. Perlin, "Improving noise", ACM TOG 21(3) 681–682, 2002, [doi:10.1145/566654.566636](https://doi.org/10.1145/566654.566636); "An image synthesizer", SIGGRAPH '85, [doi:10.1145/325165.325247](https://doi.org/10.1145/325165.325247).
- **Inigo Quilez:** ["Domain warping"](https://iquilezles.org/articles/warp/); ["Value noise derivatives"](https://iquilezles.org/articles/morenoise/).
- **libnoise:** `RidgedMulti` (Musgrave's ridged multifractal), [qknight/libnoise @ 9ce0737](https://github.com/qknight/libnoise/blob/9ce0737b55812f7de907e86dc633724524e3a8e8/src/module/ridgedmulti.cpp), a mirror of the SourceForge project.
- **Monotone splines:** F. N. Fritsch & R. E. Carlson, "Monotone Piecewise Cubic Interpolation", SIAM J. Numer. Anal. 17(2) 238–246, 1980, [doi:10.1137/0717021](https://doi.org/10.1137/0717021) (metadata checked through Crossref; the text is paywalled); F. N. Fritsch & J. Butland, 1984, [doi:10.1137/0905021](https://doi.org/10.1137/0905021), as implemented in [SciPy `PchipInterpolator`](https://github.com/scipy/scipy/blob/main/scipy/interpolate/_cubic.py).
