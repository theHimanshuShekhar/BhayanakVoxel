# The reference estimator: what to change, in what order

**Ticket:** [#96 — Research: unbiased estimator variance for sun, sky and emissive Voxels](https://github.com/theHimanshuShekhar/Bhayanakvoxel/issues/96), under the reference-tracer map [#94](https://github.com/theHimanshuShekhar/Bhayanakvoxel/issues/94).
**Feeds:** the decision ticket [#100](https://github.com/theHimanshuShekhar/Bhayanakvoxel/issues/100) (sample sequence and estimator).
**Engine state read:** `crates/render/src/shaders/pathtrace.wgsl`, `sampling.wgsl`, `common.wgsl`, `trace.wgsl`, `crates/render/src/{tracer,accumulation,reference,window,sun}.rs`, `crates/materials/materials.toml`, `bins/client/bench-budgets.toml`.
**No GPU was run.** The agent machine has none, so every number below is either derived here from the code and `materials.toml`, or quoted from the cited source. Measurements that need the 4090 are marked **[needs bench]**.

---

## 1. The question

Given a Lambertian voxel tracer lit by (a) a sun cone, (b) a sky gradient, and, after #54, (c) emissive Voxels, **which unbiased estimator changes reduce variance per sample the most**, and what does the estimator have to look like before M1b adds gloss and glass?

## 2. What the estimator does today

`radiance()` in `pathtrace.wgsl:42-67`, one loop, five surface events:

| step | line | what it adds |
| --- | --- | --- |
| escape | `:49-52` | `throughput * sky(ray.dir)`, once, wherever the path happens to escape |
| hit | `:56` | `throughput * emission(material)` — a visible emitter's own light |
| sun | `:58-62` | one shadow ray to a cone sample around the sun; `throughput * albedo * SUN_RADIANCE * cos` if unblocked |
| continue | `:64-66` | `throughput *= albedo`; cosine-weighted hemisphere direction |
| stop | `:47` | `bounce <= MAX_BOUNCES` with `MAX_BOUNCES = 4` (`:18`) |

Facts that matter for the analysis:

- **Sun NEE is the only light-sampling strategy.** Sky light and Voxel emitters are reachable only by a BSDF-sampled path happening to hit them.
- **No MIS.** Every strategy is used alone, so the estimator is the sum of one-strategy estimates.
- **The depth cap is a hard truncation**, not Russian roulette, so the loop does not rescale the throughput of the paths it kills.
- **The sky is a pure function of direction** (`common.wgsl:93-95`), with no occlusion of its own: `mix((0.75,0.82,0.92), (0.25,0.45,0.85), clamp(dir.y,0,1))`. The sun is deliberately *not* in it (`pathtrace.wgsl:11`), so there is no double count today.
- **Materials are Lambertian with albedo = `materials.toml` colour**, and `emissive_intensity` is "a multiple of the sun's radiance", i.e. `emission()` returns color × intensity in the same units as `SUN_RADIANCE` (`common.wgsl:56-61`, `materials.toml:19`).
- **Emitters are also ordinary surfaces**: `white light` has `colour = [1,1,1]`, so a path can bounce off an emitter with albedo 1.0.

### 2.1 Two audit notes that fell out of reading the shader

1. **The sun term is 0.04 % low, and that is the only photometric error left besides truncation.** `SUN_RADIANCE` is documented as "the sun's irradiance divided by π" so that a head-on surface returns exactly `albedo * SUN_RADIANCE` (`pathtrace.wgsl:20-22`). But `facing` (`:60`) is the cosine of the *sampled* direction, and over a cone of angular radius α the mean of that cosine is `(1+cos α)/2`, not 1. With the engine's α = 0.04 rad (2.29°, 8.6× the real sun's 0.267° radius; `sun.rs:6-7`) the deficit is `α²/4 = 4.0e-4`. Below the noise floor, but it should be written down or divided out, because it is the kind of thing that silently survives into a golden image.
2. **Shadow rays treat every solid Voxel as opaque** (`:59`), including materials with `transparency > 0`. Harmless in M1a (nothing is transparent yet); a hard bias the moment M1b shades glass or water.

---

## 3. Findings

### 3.1 Where the variance actually is

At one sample per pixel the pixel is a sum of three independently sampled terms. Their per-sample relative standard deviations, computed for the engine's actual constants:

| term | estimator | per-sample relative std | how it is computed |
| --- | --- | --- | --- |
| sun (lit, no blocker) | one cone sample, weight `ρ·SUN_RADIANCE·cos` | **~0.02 %** | `cos θ` spans `1−α²/2` to 1 across the cap, so its rel. std is `α²/(4√3) = 2.3e-4` |
| sun (penumbra) | one binary shadow ray | **100 % at half-coverage**, unbounded at the edge | `√((1-p)/p)` with `p` = visible fraction of the disc |
| sky (open, level ground) | one cosine-sampled escape | **28 % / 15 % / 2 % (R/G/B)** | mean of the gradient under a cosine-weighted hemisphere about +y is `H + (Z−H)·(2/3)`, variance `(Z−H)²·(1/2 − 4/9)` |
| emissive Voxel (no NEE) | one cosine-sampled path that must *hit* it | **340 % at 2 Voxels, 880 % at 5, 1770 % at 10** | `p = Ω/π ≈ 1/(πd²)` for a 1-Voxel face at distance d; std `= √(1/p − 1)` |

Two conclusions follow directly.

- **In open terrain the sky gradient, not the sun, is the dominant noise source at 1 spp.** The sun is nearly deterministic when it is unoccluded.
- **An emissive Voxel that is not NEE-sampled is not a variance problem, it is a non-starter.** A room lit only by a 1-Voxel emitter at 5 Voxels needs ~79 samples per pixel before a single path hits the emitter. That is #54's acceptance test ("an enclosed room lit only by an emissive block"), and it cannot pass without emitter NEE.

### 3.2 Russian roulette versus `MAX_BOUNCES = 4`

**The bias is large and it is exactly computable.** With cosine-weighted sampling the throughput after k bounces is the product of the albedos, so a path that survives k bounces carries at most `ρ^k` of a path that starts there. Truncating after N surface events therefore loses at most `ρ^N` of the true value, *relative*:

> |error| ≤ ρ^N, with ρ the largest albedo on the path.

That is a rigorous bound (it is the ratio of the dropped tail of the Neumann series to the whole), and for a closed cavity of albedo ρ it is attained exactly: the analytic answer is `L_e/(1−ρ)` and the truncated one is `L_e(1−ρ^N)/(1−ρ)`.

The engine's N is **5** (`bounce <= 4`, `pathtrace.wgsl:47`: the fifth vertex still gets sun NEE and emission, but loses its sky and everything beyond). Evaluated on `materials.toml`, using the max colour channel:

| Material | albedo | error bound ρ⁵ |
| --- | --- | --- |
| basalt / coal / obsidian / peat | 0.07–0.20 | < 0.1 % |
| rock, soil, grass, leaves | 0.38–0.50 | 1.3 % – 3.1 % |
| sand, sandstone, dry grass | 0.60–0.76 | 7.8 % – 25.4 % |
| limestone | 0.78 | 28.9 % |
| ice, crystal | 0.92–0.95 | 66 % – 77 % |
| white cloth, marble | 0.88 | 52.8 % |
| **snow** | **0.98** | **90.4 %** |
| `white light` (an emitter you can bounce off) | 1.00 | 100 % |

So the one "deliberate bias" in the header comment is a *few percent* error over most of the Planet and a **90 % darkening inside a snow cave or behind a limestone overhang** — exactly the enclosed, high-albedo places the game is about. This contradicts the map's framing of `MAX_BOUNCES = 4` as a minor, acceptable truncation.

**Raising the cap is not a fix.** ρ⁶⁴ is still 27 % at ρ = 0.98 and 2 % at ρ = 0.94; you would need D ≈ 256 to get snow under 0.6 %, at which point a fixed cap costs 256 events per path in a cave.

**Russian roulette is the fix, and it is unbiased by construction.** Arvo and Kirk introduced it to graphics for exactly this ("a technique known as Russian roulette which can be used to terminate the recursive tracing of rays without introducing statistical bias", Arvo & Kirk, *Particle Transport and Image Synthesis*, SIGGRAPH '90, [history.siggraph.org summary and DOI](https://history.siggraph.org/learning/particle-transport-and-image-synthesis-by-arvo-and-kirk/), [doi:10.1145/97879.97886](https://doi.org/10.1145/97879.97886)). The mechanics: pick a termination probability q, and with probability 1−q keep the estimator scaled by `1/(1−q)`; the expectation is unchanged (PBRT 4ed §2.2.4, <https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency.html#RussianRoulette>). PBRT is blunt about the trade: **"Russian roulette never reduces variance. In fact, unless somehow c = F, it will always increase variance. However, it does improve Monte Carlo efficiency if the probabilities are chosen so that samples that are likely to make a small contribution to the final result are skipped."** (same page.)

**The policy to copy** is PBRT's `PathIntegrator` (PBRT 4ed §13.4, <https://pbr-book.org/4ed/Light_Transport_I_Surface_Reflection/A_Better_Path_Tracer.html>):

```cpp
SampledSpectrum rrBeta = beta * etaScale;
if (rrBeta.MaxComponentValue() < 1 && depth > 1) {
    Float q = std::max<Float>(0, 1 - rrBeta.MaxComponentValue());
    if (sampler.Get1D() < q) break;
    beta /= 1 - q;
}
```

Three details are load-bearing and easy to get wrong:

- **`depth > 1`**: never roulette the first bounces; they carry most of the energy and rouletting them only adds variance.
- **`MaxComponentValue`, not luminance**: the rescale must be conservative per channel, or one channel goes dark.
- **`etaScale`**: for glass (M1b) the throughput has already been divided by η² at each refraction; rouletting on the raw `beta` would terminate refracting paths at the wrong rate. PBRT carries that factor separately for the same reason (same page).

PBRT attributes the albedo-driven choice of q to Szécsi, Szirmay-Kalos and Kelemen, *Variance reduction for Russian roulette*, WSCG 11(1), 2003, and notes that a better q also accounts for incident lighting (Vorba & Křivánek, *Adjoint-driven Russian roulette and splitting in light transport simulation*, TOG 35(4), 2016); both are cited from PBRT 4ed §13 Further Reading, <https://pbr-book.org/4ed/Light_Transport_I_Surface_Reflection/Further_Reading.html>.

**Does RR win at equal cost here?** With throughput = albedo, PBRT's rule gives q = 1−ρ, so the expected number of surface events is `1/(1−ρ)`:

| scene | fixed cap (5 events) | RR expected events | verdict |
| --- | --- | --- | --- |
| rock cave (max 0.45, lum 0.43) | 5, biased ≤ 1.9 % | 1.8 | RR is **2.8× cheaper and unbiased** |
| limestone room (max 0.78, lum 0.75) | 5, biased ≤ 28.9 % | 4.0 | neutral cost, bias gone |
| snow cave (max 0.98, lum 0.94) | 5, biased ≤ 90.4 % | 16.7 | **3.3× more expensive, and correct** |

(The bias bound uses the max colour channel, as in the table above. The expected path length is quoted from the luminance albedo for brevity; the shader itself must key the roulette on `MaxComponentValue`, per the note above, so its real cost sits between these two numbers.)

So RR is cheaper than the cap in the average voxel scene, because the average albedo is ~0.45, and only more expensive where the physics genuinely needs the extra bounces. And "equal cost" is the wrong frame for a ground truth anyway: the truncated estimator's 90 % error in a snow cave cannot be bought back with samples at any cost, while RR's extra variance is exactly what averaging is for. The cost of deep interreflection belongs to the *real-time* path (#54's radiance cache, #52/#55), not to the yardstick.

**A hard cap is still required, for termination, not for noise.** `white light` and any material at albedo 1.0 give q = 0, so RR would never stop; and a scene of pure emitters is reachable. Keep `MAX_BOUNCES` as `MAX_DEPTH = 256` and document the bound it re-introduces: ≤ ρ²⁵⁶ = 0.57 % at snow, 1.3e-7 at ρ = 0.94, ~0 at rock. A cap of 256 is reached by ~0.6 % of snow-cave paths, so it costs nothing measurable.

### 3.3 Emissive Voxels as area lights

#### Sample the solid angle, not the area

This is the single most important design choice for #54's emitters, and it decides whether the reference gets fireflies at all.

A Voxel emitter is a cube. A receiver sees at most three of its six faces. Two ways to sample it:

- **Area sampling**: pick a point uniformly on the exposed faces, pdf `1/A`. The estimator is `(ρ/π)·L_e·cos_l·cos_r/(r²·p_A)`. As the receiver approaches the emitter, `r → 0` and the weight diverges as `1/r²`. PBRT's `DiffuseAreaLight` samples the shape's area and converts to a solid-angle pdf by dividing by `cos/r²`, and the shape guards the degenerate case explicitly with `if (IsInf(pdf)) return {}` (PBRT 4ed §6.2, <https://pbr-book.org/4ed/Shapes/Spheres.html>). A guard is a bias; the divergence is real.
- **Solid-angle sampling**: sample a direction uniformly over the emitter's *subtended solid angle Ω*, pdf `1/Ω`. The estimator is `(ρ/π)·L_e·cos_l·Ω`. Since `Ω ≤ 2π` and `cos_l ≤ 1`, the weight is **bounded by `2ρ·L_e`**. There is no near-field spike, ever.

PBRT's sphere light does the solid-angle thing for the same reason: it samples "uniformly inside the subtended cone" with pdf `1/(2π(1−cos θ_max))` in solid angle, and only falls back to area sampling when the shading point is inside the sphere (PBRT 4ed §6.2, §12.4, <https://pbr-book.org/4ed/Shapes/Spheres.html>, <https://pbr-book.org/4ed/Light_Sources/Area_Lights.html>).

For a rectangular emitter face, uniform solid-angle sampling with a stable parametrisation is a solved problem: Ureña, Fajardo and King, *An Area-Preserving Parametrization for Spherical Rectangles*, CGF 32(4) (EGSR 2013), [paper PDF](https://blogs.autodesk.com/media-and-entertainment/wp-content/uploads/sites/162/egsr2013_spherical_rectangle.pdf), [doi:10.1111/cgf.12151](https://doi.org/10.1111/cgf.12151). Their own comparison is the same ranking we need: naive area sampling "yields very high variance", solid-angle sampling of the rectangle is the best of the three they test (their slides, [UGR PDF](https://www.ugr.es/~curena/publ/2013-egsr/ufk-2013-spherical-rectangles-slides.pdf)). For a Voxel emitter: compute the solid angle of each face that faces the receiver (closed form exists for a rectangle), pick a face with probability proportional to its solid angle, then sample that spherical rectangle. Total pdf = 1/Ω_total, weight bounded, no rejection loop.

#### MIS does not save area sampling

Worth stating because it is a natural assumption: with the balance heuristic, a light sample contributes `f/(p_light + p_bsdf)`. Near-touching an area-sampled emitter, both `f → ∞` (as `1/r²`) and `p_light → 0` (as `r²/A`, since `p_ω = p_A·r²/cos_l`), while `p_bsdf` stays finite. The weighted contribution still diverges as `1/r²`. **MIS bounds the damage from a *mismatched* strategy; it does not bound a strategy whose own pdf vanishes where the integrand explodes.** Only the sampling measure does. (MIS itself: Veach & Guibas, *Optimally Combining Sampling Techniques for Monte Carlo Rendering*, SIGGRAPH '95, 419–428, [Stanford page](https://graphics.stanford.edu/papers/combine/), [doi:10.1145/218380.218498]; balance and power heuristic as given in PBRT 4ed §2.2.3.)

#### MIS weights against BSDF sampling

Once emitters are NEE-sampled, a BSDF-sampled path that *hits* an emitter must not count it in full. The rule is PBRT's: emission at the first hit, or after a specular bounce, is added unweighted; otherwise it is weighted by the MIS weight against the light pdf (PBRT 4ed §13.4). Concretely, for this engine:

```
w_light = balance(1, p_light) / (balance(1, p_light) + balance(1, p_bsdf_los))
w_bsdf  = 1 - w_light
```

with `p_bsdf` the cosine pdf (`cos/π`) and `p_light` the same `1/Ω` the NEE path used, both in solid angle. The engine is a pure Lambertian tracer, so both pdfs are one line; there is no excuse for skipping this. The per-sample contribution with the balance heuristic is `f/(p_light + p_bsdf)`, which is at most `min(f/p_light, f/p_bsdf)` — i.e. MIS takes the *better* of the two weights for every sample. That bound, not the pdfs separately, is why a solid-angle-sampled estimator has bounded samples.

**Expected gain.** For a small emitter in a closed room, the BSDF-only estimator needs `1/p ≈ πd²` samples just to hit it, with a relative std of `√(1/p−1)`. NEE makes it near-deterministic in an unoccluded room (zero variance for a Lambertian receiver and a uniform emitter, once both are sampled in solid angle). This is a 10²–10⁴× reduction in the samples needed for the room test, against one extra ray per vertex. There is no other change in this document with that ratio.

### 3.4 Sun cone sampling: the cone is fine, the visibility is the whole problem

**Is the uniform cone optimal for a disc?** Effectively yes, and the argument is short. For a Lambertian receiver the integrand is `L_sun·cos θ_l`, with `θ_l` measured at the receiver. Sampling `dω` uniformly inside the cone gives an estimator whose mean is `L·(1+cos α)/2` and whose relative standard deviation over the cone is `α²/(4√3) = 2.3e-4`. The *optimal* distribution is `∝ cos θ_l` (projected solid angle) — that one is exactly zero-variance for an unoccluded uniform emitter, because `p ∝ cos` cancels the `cos` in the integrand. The difference between the two is `α²/4` in the mean and `~1e-7` in variance. **At α = 0.04 the choice of cone distribution is irrelevant.** This is also why the sphere-light sampling in PBRT (uniform over the subtended cone) is good enough in practice even though it is not projected-solid-angle sampling.

(For a genuinely wide emitter the two do differ; the projected-solid-angle answer is Ureña & Georgiev, *Stratified Sampling of Projected Spherical Caps*, CGF 37(4) (EGSR 2018), [PDF](https://blogs.autodesk.com/media-and-entertainment/wp-content/uploads/sites/162/egsr2018_projected_spherical_caps.pdf), and for a disc, Guillén et al., *Area-Preserving Parameterizations for Spherical Ellipses*, CGF 36(4) (EGSR 2017), [arXiv:1805.09048](https://arxiv.org/abs/1805.09048). Not needed at α = 0.04; needed if the sun's radius ever becomes art-directable and large, or for a large emissive panel.)

**Where the penumbra variance comes from.** With the geometric term effectively constant, the estimator's randomness is entirely the binary visibility: per sample the sun term is `c·Bernoulli(p)` with `p` = the fraction of the disc visible from the shading point. Relative std is `√((1−p)/p)`: 100 % at half-coverage, 300 % at p = 0.1, 995 % at p = 0.01. That is a *pure* visibility signal, so no change to the cone distribution can touch it, and the shader's `if facing > 0.0 && !trace(...)` (`:59-61`) already spends exactly one ray on it.

Levers, in order of value per line of code:

1. **The sample sequence.** A stratified/QMC sequence across samples turns the penumbra from independent coin flips into an ordered sweep of the disc, which is the same variance reduction as "more samples" for free. This is the sampling ticket's job (see #94); the estimator only has to keep the cone's two dimensions well separated from the pixel and hemisphere dimensions, which the current `seed_rng(pixel, sample, seed)`-then-consume-in-order scheme already does.
2. **Splitting** the sun when it is cheap: m shadow rays per vertex cuts the penumbra variance by m and costs m−1 extra rays (PBRT 4ed §2.2.5, <https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency.html#Splitting>). Worth it only in the penumbra band, i.e. conditional on the surface being near a shadow edge — which the reference can detect with the first shadow ray plus a neighbouring one. **Recommendation: do not do this in M1a.** It buys 2–4× in a thin band and costs branchy divergence and a second ray budget.
3. **Correlated/analytic shadowing** (analytic direct lighting with a ratio estimator) is the real answer for production, and is explicitly out of scope for a ground truth that must be strategy-oblivious.

### 3.5 Sky NEE and MIS: small now, required after #53

The map lists "reaches the sky only through cosine-weighted bounces" as a deficiency. **For the current placeholder gradient, closing that gap is worth much less than it looks**, and the reason is a trap worth naming:

> Adding a sky NEE sample with a *cosine* pdf adds nothing. The NEE estimator with `p = cos/π` is statistically identical to the BSDF-sampled escape it was already getting; it is just a second sample of the same distribution. The gain from sky NEE comes entirely from using a *better* pdf than the BSDF's, not from the fact that it is called NEE.

How much better can the pdf be, given the actual sky? The gradient's cosine-sampled relative std is 28/15/2 % (R/G/B) on level ground (§3.1), so the sky term is a real noise source in open terrain. Removing it needs `p ∝ L_sky·cos`. For a function that is *linear* in `dir.y` that distribution is derivable in closed form, but it is fiddly, direction-dependent, and worth ~1 branch versus just letting the sample sequence handle a smooth 2-D integral — and the smooth gradient is a placeholder that #53 deletes.

After **#53 (Hillaire atmosphere)** the situation changes qualitatively, in three ways:

1. **The sky stops being smooth.** A Hillaire sky has a sharp horizon, a bright solar aureole, and strong colour variation with elevation (Hillaire, *A Scalable and Production Ready Sky and Atmosphere Rendering Technique*, CGF 39(4) (EGSR 2020) 13–22, [author PDF](https://sebh.github.io/publications/egsr2020.pdf), [EGSR PDF](https://diglib.eg.org/bitstream/handle/10.1111/cgf14050/v39i4pp013-022.pdf)). PBRT's measurement of exactly this problem in an environment map is brutal: uniform direction sampling versus image-proportional sampling is **38,000× MSE** on a sky-lit scene, and a further 1.52× from MIS compensation (PBRT 4ed §12.5, <https://pbr-book.org/4ed/Light_Sources/Infinite_Area_Lights.html>). Hillaire's sky-view LUT is already a 2-D image over direction, so the importance table is free: build a piecewise-constant CDF over the LUT, exactly as `ImageInfiniteLight` does over an environment map.
2. **The sun must stay out of the sky, or the estimator silently double counts.** The current code is careful about this (`pathtrace.wgsl:11`); a Hillaire sky bakes the sun disc into the LUT. Either subtract the analytic sun contribution when building the LUT used by the reference, or use a sun-free LUT variant for the reference and keep the analytic sun NEE.
3. **Visibility becomes the limiting factor, not the sky's shape.** PBRT's `PortalImageInfiniteLight` — sampling only the part of the sky visible through a window — is **2.82× lower MSE** than sampling the whole map, on a room lit through one opening (PBRT 4ed §12.5). Caves, overhangs and doors are the voxel equivalent, and the reference cannot afford per-point portals. Expect sky NEE to help in open terrain and to help much less under an overhang, which is the opposite of where the light is scarce.

**And a warning for #100 before #53 lands:** Hillaire's atmosphere is *not* an infinite light. Radiance along a ray that grazes the horizon changes over kilometres of participating air, and terrain shadows that air. A `sky(dir)` lookup at the last vertex assumes an infinitely distant, unoccluded environment, which is exactly right for the current gradient and only approximately right for a full atmosphere. The reference must keep using the same "distant sky" approximation as Primary, or the FLIP/MSE comparison of #55 is measuring two different skies. Flag it, do not fix it by adding volumetrics to the reference.

**Recommendation:** keep cosine-only sky until #53; when #53 lands, add sky NEE with a LUT-based `∝ L·cos` pdf and MIS against the BSDF-sampled sky, sun excluded, and *not* before.

### 3.6 Cosine-weighted hemisphere sampling: nothing better, and that is the point

For a Lambertian BSDF, `pdf = cos/π` gives `f·cos/pdf = ρ` exactly: zero BRDF variance, which is the theoretical optimum for a single-bounce estimator. Every remaining photon of noise is in the *incident radiance field*, so the only ways to reduce it are to sample the field directly (NEE for sun, sky, emitters — §3.2–3.5) or to guide toward it (path guiding, out of scope for the reference per #94). Do not replace cosine sampling.

**What M1b (glossy/glass) needs from the estimator.** The estimator must grow from "albedo, cosine" to a BSDF interface, and four specific things become mandatory, not optional:

1. **`f`, `pdf` and `sample` per BSDF**, with `pdf` in solid angle, and MIS between BSDF and light samples at every non-specular vertex (PBRT 4ed §13.4). Roughness in `materials.toml` already spans 0.02 (water, clear glass) to 1.0 (soil), so the same scene will contain both "BSDF sampling is hopeless" and "light sampling is hopeless" cases — the exact case MIS exists for (Veach & Guibas 1995; PBRT 4ed §13.4 figure 13.8).
2. **GGX visible-normal sampling**, not NDF sampling: Heitz, *Sampling the GGX Distribution of Visible Normals*, JCGT 7(4):1–13, 2018, <https://jcgt.org/published/0007/04/01/>. The paper's own summary is that VNDF sampling "yields significant variance reduction", and its reference implementation is ~10 lines of WGSL-shaped code.
3. **A specular flag.** A delta BSDF has `pdf = 0` for every direction another strategy can generate, so a path that reaches a light *through* a specular bounce must add the emission unweighted — PBRT's `depth == 0 || specularBounce` test (PBRT 4ed §13.4). Getting this wrong double-counts or loses energy at every mirror.
4. **Transport mode and `etaScale` for glass.** A refractive BSDF is not symmetric, and the estimator is only unbiased if the path remembers whether it is transporting radiance or importance and applies the same η factor to both the BSDF and the pdf. This is PBRT's `TransportMode` parameter and its `etaScale` bookkeeping (PBRT 4ed §9.1, <https://pbr-book.org/4ed/Reflection_Models/BSDF_Representation.html>; §9.5.2), and Veach's thesis §3.7.5 "Sampling and evaluation of non-symmetric BSDF's" is the canonical statement of why (Veach, *Robust Monte Carlo Methods for Light Transport Simulation*, PhD thesis, Stanford, 1997, [thesis PDF](https://cg.informatik.uni-freiburg.de/intern/seminar/raytracing%20-%20Veach%20-%201997%20-%20PhD%20Thesis%20Monte%20Carlo.pdf)).

And the honest limit: **unidirectional path tracing cannot sample caustics.** Light that reaches a diffuse surface *through* glass has no NEE strategy and no BSDF strategy that finds it often; it converges, but with the fattest tail in the renderer. Manifold next event estimation (Hanika, Droske, Fascione, EGSR 2015), photon mapping or specular manifold sampling are the answers, and all are out of scope. For the reference, that means **allowing the noise and never clamping it** (§3.7), and making sure the M1b golden images are converged enough that the caustic noise is below the FLIP threshold rather than "cleaned up".

### 3.7 Fireflies and suppression

**M1a has none, if §3.3 is followed.** With sun, sky and Voxel emitters all sampled in solid angle, every weight in the estimator is bounded: `ρ·SUN_RADIANCE·cos ≤ 1.6ρ`, `ρ·L_sky ≤ ρ·max(sky)`, `ρ·L_e·Ω/π ≤ 2ρL_e`. A bounded weight means bounded samples: no fireflies. This is a property of the estimator, not of a post-process, and it is worth preserving deliberately.

**Where they will come from in M1b**, all in one place: a narrow specular or glossy lobe finding a small bright emitter. The BSDF-sample weight is `f·cos/pdf`, which is fine when the lobe and the light agree in size and enormous when a near-mirror lobe happens to land on a 1-Voxel emitter. That is the classic Veach firefly, and the answer is MIS (which weights it down when the light strategy could have found the same direction cheaply), not clamping.

**Clamping, quantified, and rejected.** Replacing a sample X by `min(X, c)` changes the mean by exactly `E[(X−c)⁺] > 0`: clamping darkens the image by precisely the tail it cuts, and there is no way to make that small by taking more samples. Concretely, for a pixel whose true value is `L̄`, with the clamp at c = 10·L̄: one sample in 10⁴ carrying `1000·L̄` (a plausible glass caustic) removes `0.1·L̄` — **10 % of the pixel**. At a 10³ rate of `10⁴·L̄` samples the same arithmetic removes `10·L̄`, i.e. an order of magnitude, not a correction. The bias scales with the firefly rate × firefly size, and both are properties of the scene, not of the clamp.

The same arithmetic applies to rejecting samples outright (c = 0, the most common "firefly filter"): it is the most biased clamp there is. Related and citable: PBRT's own variance-reduction escape hatch, path *regularization*, is described in the book as an "admittedly *ad hoc* roughening to all non-diffuse BSDFs" that, unlike the formulation it cites, is not "designed to not lose energy" — PBRT flags it as capable of losing energy and keeps it opt-in (PBRT 4ed §13 Further Reading, <https://pbr-book.org/4ed/Light_Transport_I_Surface_Reflection/Further_Reading.html>). It also notes, of Arvo and Kirk, that "in some situations bias and less noise may be the more desirable artifact" — which is exactly the argument *for* denoising the real-time image and *against* doing it in the reference.

**There is one unbiased way to cut a firefly's cost**, and it is worth knowing because it looks like clamping: raise the *roulette probability* on the offending path instead of the sample value. RR rescaling is unbiased (§3.2); clamping the sample is not. If a specific configuration (glass caustics) turns out to dominate the reference's time-to-converge, the correct move is a higher termination probability on paths that have already refracted, not a clamp.

**Also worth fixing before it bites:** the reference's float mean lives in `accum` (`tracer.rs:171-178`, one `vec4` per pixel) but is *displayed* through `encode()` into `rgba8unorm` (`pathtrace.wgsl:86-98`, `tracer.rs:21`), which clips at 1.0 — sunlit snow is `0.98 × 1.6 = 1.57` before gamma. Any convergence metric (#55's FLIP/MSE) must read the float buffer or be measured offscreen in float, or it will score a clipped image, hiding exactly the fireflies the metric exists to catch. Not an estimator bias; a measurement hazard.

---

## 4. Ranked recommendation

Ranked by *error removed per unit of implementation and review risk*, with the variance story stated for each. "Expected gain" is what the derivation above supports; nothing here is a measured 4090 number.

| # | Change | Removes | Expected gain | Cost | Risk for a beginner team |
| --- | --- | --- | --- | --- | --- |
| **1** | **Russian roulette** (PBRT's `q = 1 − max(β·etaScale)`, `depth > 1`), plus rename the cap to `MAX_DEPTH = 256` and document its bound | up to **90 % energy bias** in snow/ice caves (ρ⁵ table) | bias → ≤0.6 % worst case; ~2.8× *cheaper* per path in rock terrain, 3.3× costlier in a snow cave | ~8 lines of WGSL, one extra RNG draw per bounce after the second | **Low.** The one classic bug is forgetting `beta /= 1−q`; a furnace test catches it immediately. Needs a CPU/golden test, not a bench. |
| **2** | **Emissive Voxel NEE, sampled in solid angle, with MIS against the BSDF** | 340–1770 % per-sample noise, and the 1/r² near-field spike | **10²–10⁴× fewer samples** for a small emitter in a room; required for #54's acceptance test | emitter list + spherical-rectangle sampling (Ureña 2013) + two pdfs + MIS weights | **Medium.** The maths is a page, the integration (finding emitters in the brick store, face visibility) is the work. Do *not* ship area sampling "for now": it is the firefly generator. |
| **3** | **Sky NEE + MIS with a `∝ L·cos` importance table, after #53** | the sky term's 28/15/2 % per-sample noise in open terrain | removes the sky's share of the noise: most of it in open terrain, ~none under an overhang | LUT CDF (the Hillaire sky-view LUT is already one) + MIS + sun-exclusion | **Medium.** The trap is that a *cosine*-pdf sky NEE buys nothing (§3.5). Sequence it after #53, not before. |
| **4** | **Keep the uniform cone; fix the `(1+cos α)/2` factor; let the sample sequence stratify the cone** | 4e-4 photometric error (documentation, not noise) | none expected in variance; penumbra noise is then whatever the sequence delivers | a one-line divisor, or a comment | **Low.** The interesting finding is that there is nothing to win here: penumbra variance is 100 % visibility. Do not spend the budget on the cone. |
| **5** | **Do not clamp, do not filter, do not reject fireflies** | would *add* bias | keeps the ground truth a ground truth | zero | **Low**, if written into the fidelity contract (#100) as a rule rather than a preference. |
| **6** | **BSDF interface + VNDF + TransportMode/`etaScale` + transparent shadow rays, for M1b** | future biases (double-counted mirrors, lost refraction energy, glass that casts an opaque shadow) | unlocks glossy/glass at all | a real refactor of `radiance()` and `leave()` | **High.** Four coupled invariants (§3.6); needs the furnace test and a glass golden before it is trusted. |

**The estimator, bounce by bounce, after 1–2 (the M1a recommendation):**

```
throughput = 1, light = 0, depth = 0
trace camera ray
loop:
  if miss:            light += throughput * sky(dir)            // MIS-weighted once #3 has landed
  else:
    light += throughput * emission(material)                     // unweighted at depth 0 and after a
                                                                 // specular bounce; MIS-weighted otherwise (#2)
    light += sun_NEE(...)                                        // one cone sample, as today
    light += emitter_NEE(...)                                    // solid angle, MIS-weighted  (#2)
    throughput *= albedo
    if depth > 1 and max(throughput) < 1:
        q = 1 - max(throughput); if rng < q: break; throughput /= 1 - q
    dir = cosine_hemisphere(normal)
  if depth++ == MAX_DEPTH (256): break
```

---

## 5. Which biases are acceptable in a ground truth

Explicit list, so #100 can adopt or reject each one.

**Acceptable, because they are documented and bounded below the measurement noise:**

1. **f32 accumulation and the running-mean recurrence** (`pathtrace.wgsl:86-91`). Rounding, not bias; the recurrence is chosen precisely so it does not drift.
2. **The flat-albedo simplification** — `albedo()` not `varied_albedo()` (`pathtrace.wgsl:55`, documented at `:53-54`). A deliberate contract: the image must be derivable from the material table alone.
3. **8-bit display encoding** (`pow(x, 1/2.2)` into `rgba8unorm`). Not part of the estimator *if* the metric reads the float buffer; see §3.7.
4. **The `(1+cos α)/2 = 1 − 4e-4` sun factor.** Acceptable only if written down (or divided out). It is 0.04 % of the sun term, i.e. below any FLIP threshold, but a silent 0.04 % is still a silent error.
5. **A hard `MAX_DEPTH` of 256**, with the bound stated: ≤ ρ²⁵⁶ (0.57 % at snow, ~0 at everything else) and reached by ~0.6 % of the deepest paths. This is a termination guard, not a noise control.

**Not acceptable:**

6. **`MAX_BOUNCES = 4`.** ≤90 % error at snow, ~29 % at limestone, attained in exactly the enclosed scenes the game is built around. This is not a truncation you can budget for; it changes the answer.
7. **Clamping, sample rejection, or any "firefly filter".** Biased by construction, and biased by the amount of the tail it removes (§3.7).
8. **A shadow ray that treats a transparent material as opaque.** Currently vacuous; a hard bias from the first glass Voxel.
9. **Denoising, reprojection, sample reuse** — already excluded by #94's out-of-scope, restated because RR's higher per-sample variance will make them tempting.

**Acceptable only outside the reference:** path regularization, radiance caching, GPU-timing-motivated depth cuts, and the caches that #54 builds. Those are the real-time path's business (#52, #54, #55); the reference stays the yardstick.

---

## 6. What contradicts the map's assumptions

Four things worth carrying back to #94/#100:

1. **"`MAX_BOUNCES = 4` … the one deliberate bias" understates it by two orders of magnitude.** The map treats it as acceptable truncation. It is ≤90 % energy loss for snow (0.98), 53 % for marble and white cloth (0.88), 29 % for limestone — and the loss is *largest in exactly the enclosed high-albedo places* (caves, overhangs, interiors) that the Planet is made of. This is the single largest error in the current ground truth, larger than every variance question in this document combined, and it is a ten-line fix.
2. **"Reaches the sky only through cosine-weighted bounces" is not the next win.** While the sky is the placeholder gradient, a *cosine-pdf* sky NEE is mathematically the same estimator as the BSDF-sampled escape, so it buys nothing; the win is in the importance function, and the placeholder is about to be deleted by #53. Order the work: RR → emitters → sky-after-#53.
3. **"Sun cone sampling" has almost nothing left in it.** Uniform-cone sampling is within 4e-4 in the mean and ~1e-7 in variance of the optimal projected-solid-angle distribution at α = 0.04. All penumbra variance is binary visibility, which no cone change can touch. If the map expects a win here, it is not there.
4. **The `(1+cos α)/2` factor and the "solid Voxels block shadow rays" rule are undocumented behaviours that will outlive M1a.** The first is a 4e-4 photometric error that will be baked into goldens; the second becomes a real bias the moment M1b shades the `transparency` values already sitting in `materials.toml` (0.3–0.95 across six materials).

Also worth noting for #100, which the map does not mention: **there is no test of the estimator's unbiasedness anywhere in the crate.** `sampling_tests.rs` verifies the sampling distributions, and `reference.rs` is a CPU copy of `trace.wgsl` (the *visible-surface* path, not the radiance estimator), so nothing checks that `radiance()` converges to the right value. The cheapest possible guard is a **white-furnace test**: a closed Lambertian box of albedo ρ emitting `L_e`, whose exact answer is `L_e/(1−ρ)` (the geometric series), rendered on lavapipe through the existing golden infrastructure. At ρ = 0.9 the furnace renders at 41 % of its true value under `MAX_BOUNCES = 4` (ρ⁵ = 0.59 of the light is missing) and to rounding under RR — one image, no GPU budget, and it would have caught the current bias on day one.

---

## 7. Open questions handed to #100

- RR's `q` from throughput only, or adjoint-driven (Vorba & Křivánek 2016)? Throughput-only is 8 lines and unbiased; the literature's better `q` needs an estimate of incident light at the vertex, which this engine does not have until #54's cache exists — and the reference must not read the cache.
- Does splitting the sun in the penumbra band pay for its divergence once the sample sequence lands? **[needs bench]** on the 4090, 1440p flyover.
- Where does the emitter list live: a per-scene array built at upload (`SceneBuffers`), or a scan of `bricks` for emissive palette entries at NEE time? The former is a small `scene.rs` change; the latter is a per-vertex cost.
- Do the goldens need regenerating in float (not `rgba8unorm`) before the FLIP/MSE gate of #55 is meaningful? Recommended yes, for the clipping reason in §3.7.
- After #53, is the reference's `sky(dir)` allowed to stay infinite-distance while Primary gets aerial perspective? If not, the two views are not comparable and the fidelity contract needs a stated tolerance.

---

## 8. Sources

1. M. Pharr, W. Jakob, G. Humphreys. *Physically Based Rendering: From Theory to Implementation*, 4th ed., MIT Press, 2023. §2.2.1 (stratification), §2.2.3 (MIS, balance and power heuristic, single-sample model, MIS compensation), §2.2.4 (Russian roulette), §2.2.5 (splitting). <https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency.html>
2. Same, §13.4 "A Better Path Tracer" — the `PathIntegrator` reference implementation: MIS direct lighting, `specularBounce` handling, and the `rrBeta`/`etaScale` roulette rule. <https://pbr-book.org/4ed/Light_Transport_I_Surface_Reflection/A_Better_Path_Tracer.html>
3. Same, §13 Further Reading — Arvo & Kirk's roulette is unbiased, the albedo-driven probability (Szécsi et al. 2003), adjoint-driven roulette (Vorba & Křivánek 2016), and path regularization's energy loss. <https://pbr-book.org/4ed/Light_Transport_I_Surface_Reflection/Further_Reading.html>
4. Same, §6.2 "Spheres" — sampling a sphere light uniformly inside the subtended cone, solid-angle pdf `1/(2π(1−cosθ_max))`, and the area→solid-angle conversion with its `1/r²` guard. <https://pbr-book.org/4ed/Shapes/Spheres.html>
5. Same, §12.4 "Area Lights" — `DiffuseAreaLight::SampleLi`/`PDF_Li` and the area-to-solid-angle conversion. <https://pbr-book.org/4ed/Light_Sources/Area_Lights.html>
6. Same, §12.5 "Infinite Area Lights" — image importance sampling (38,000× MSE over uniform), MIS compensation (1.52×), and the portal variant (2.82×) for sky visible through an opening. <https://pbr-book.org/4ed/Light_Sources/Infinite_Area_Lights.html>
7. Same, §12.6 "Light Sampling" — sampling one light out of many, and why power-based selection is not optimal. <https://pbr-book.org/4ed/Light_Sources/Light_Sampling.html>
8. Same, §9.1 "BSDF Representation" — `TransportMode`, non-symmetric BSDFs, and delta-distribution pdf conventions. <https://pbr-book.org/4ed/Reflection_Models/BSDF_Representation.html>
9. E. Veach, L. J. Guibas. "Optimally Combining Sampling Techniques for Monte Carlo Rendering." *SIGGRAPH '95 Proceedings*, 419–428. <https://graphics.stanford.edu/papers/combine/> · [doi:10.1145/218380.218498](https://doi.org/10.1145/218380.218498)
10. E. Veach. *Robust Monte Carlo Methods for Light Transport Simulation*. PhD thesis, Stanford University, 1997. §3.7.5 "Sampling and evaluation of non-symmetric BSDF's". [Thesis PDF](https://cg.informatik.uni-freiburg.de/intern/seminar/raytracing%20-%20Veach%20-%201997%20-%20PhD%20Thesis%20Monte%20Carlo.pdf)
11. J. Arvo, D. Kirk. "Particle Transport and Image Synthesis." *Computer Graphics (SIGGRAPH '90)* 24(4):63–66. [SIGGRAPH history record](https://history.siggraph.org/learning/particle-transport-and-image-synthesis-by-arvo-and-kirk/) · [doi:10.1145/97879.97886](https://doi.org/10.1145/97879.97886)
12. C. Ureña, M. Fajardo, A. King. "An Area-Preserving Parametrization for Spherical Rectangles." *CGF* 32(4) (EGSR 2013), 59–66. [PDF](https://blogs.autodesk.com/media-and-entertainment/wp-content/uploads/sites/162/egsr2013_spherical_rectangle.pdf) · [doi:10.1111/cgf.12151](https://doi.org/10.1111/cgf.12151)
13. C. Ureña, I. Georgiev. "Stratified Sampling of Projected Spherical Caps." *CGF* 37(4) (EGSR 2018). [PDF](https://blogs.autodesk.com/media-and-entertainment/wp-content/uploads/sites/162/egsr2018_projected_spherical_caps.pdf)
14. I. Guillén, C. Ureña, A. King, M. Fajardo, I. Georgiev, J. López-Moreno, A. Jarabo. "Area-Preserving Parameterizations for Spherical Ellipses." *CGF* 36(4) (EGSR 2017). [arXiv:1805.09048](https://arxiv.org/abs/1805.09048)
15. E. Heitz. "Sampling the GGX Distribution of Visible Normals." *JCGT* 7(4):1–13, 2018. <https://jcgt.org/published/0007/04/01/>
16. S. Hillaire. "A Scalable and Production Ready Sky and Atmosphere Rendering Technique." *CGF* 39(4) (EGSR 2020), 13–22. [Author PDF](https://sebh.github.io/publications/egsr2020.pdf) · [EGSR PDF](https://diglib.eg.org/bitstream/handle/10.1111/cgf14050/v39i4pp013-022.pdf)
