# Reference sampling: which sequence should the reference path tracer use?

Research for [#95](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/95), under the map
[#94](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/94). This answers the question for
*this* engine, not for path tracing in the abstract. Every number below is either read out of the
engine's own source, or quoted from a primary source linked in the reference list.

The machine this was written on has no GPU, so no engine timings were taken. The only measurement
made here is a CPU microbenchmark of the sample arithmetic (§5), which is a proxy for lavapipe, not
for the 4090: the 4090 numbers must come from `client bench` under the `needs-benchmark` flow.

## The question

The reference path tracer draws one sample per pixel per frame from a white-noise PCG stream
(`crates/render/src/shaders/sampling.wgsl`, `seed_rng` + `next_f32`). At 1–64 samples that is
visibly noisy, and the noise is uncorrelated from frame to frame. Which sample sequence should it
use instead, how is that sequence seeded deterministically in f32-only WGSL, how many dimensions
does one sample need, what does it cost, and how much less noise should we expect at equal spp?

## What the engine does today

Read from the checkout this ticket works in:

- `pathtrace.wgsl` draws one sample per pixel per dispatch, adds it to a running mean in `accum`,
  and writes the mean. `MAX_BOUNCES = 4` but the loop runs `bounce <= MAX_BOUNCES`, so a path visits
  up to 5 hit points.
- Per hit: one 2D sun-cone sample (`sample_cone`) and one 2D cosine-hemisphere sample
  (`cosine_hemisphere`). Per sample: one 2D pixel offset. All drawn sequentially from one PCG
  stream seeded by `seed_rng(pixel, sample_index, seed)`.
- The sun has an angular radius of 0.04 rad by default (`sun.rs`), so the sun cone is a near-delta:
  its 2D sample is low-variance; the visible sun noise is along penumbrae.
- The sky is a smooth gradient (`common.wgsl`), albedo is flat, there are no textures, and there is
  no MIS and no Russian roulette. The integrand is a Lambertian interreflection integral with binary
  voxel visibility — smooth almost everywhere, discontinuous on voxel silhouettes and shadow edges.
- The accumulator resets on any camera, sun, size or scene change and counts samples from 0
  (`accumulation.rs`). At 60 fps interactive, one sample per frame means 64 spp takes ~1.07 s.
- The bench traces 1280×720 internally and measured 1.10 ms per reference sample (mean; p95 2.69)
  against a 3.0 ms budget (`bins/client/bench-budgets.toml`). At that rate 64 spp is 70 ms of GPU
  work at 720p, ~282 ms at native 1440p.
- The reference golden is 64 samples at 256×144 on lavapipe, compared with MSE ≤ 1.0 and ≤ 0.2 %
  far-off pixels (`crates/render/tests/common/mod.rs`, `golden.rs`). Any change of sample positions
  changes that image and needs re-approval.

## Findings

### 1. The candidates, and what published measurements say

**White noise (status quo).** Independent samples converge as `O(N^-0.5)` for every integrand; the
error spectrum is flat, so low-frequency clumps survive ([PMJ 2018 §3.1, Fig. 6–7][pmj]). That is
the perceptual problem the ticket describes.

**Stratified pixel jitter only.** Subdividing the pixel and taking one jittered sample per stratum
makes the image-plane integral converge faster and turns aliasing into noise ([PBRT 3ed §7.3][pbrt]).
But stratification does not survive into high dimensions: doing it in every dimension costs
`4^5 = 1024` samples for 5 dimensions, so renderers stratify low-dimensional subsets and pad the
rest ([PBRT 3ed §7.3][pbrt]). Our noise is dominated by the bounce and sun dimensions, not the pixel
dimensions, so stratifying only the pixel offset buys little. It is a cheap first step, not the
answer.

**Owen-scrambled Sobol.** Sobol's sequence is a base-2 digital net ([Sobol' 1967][sobol]); Owen's
nested uniform scrambling randomises it while preserving its `(t,m,s)`-net structure
([Owen 1995][owen95], [Owen 1997][owen97], summarised in [Kollig & Keller 2002 §3.2.2][kk02]).
Laine and Karras gave an efficient hash-based base-2 nested permutation ([Laine & Karras 2011][lk11]);
Burley made it practical for Sobol of indeterminate length and dimension, evaluated on the fly in
arbitrary order, with no table of samples ([Burley 2020][burley]). It is the sequence the primary
sources measure as best or tied-best on 2D test integrals: in [PMJ 2018 §3.3][pmj] the authors
conclude "the Owen-scrambled Sobol' sequence is the best of the tested sequences", with no
pathological behaviour on discontinuous functions and "extraordinarily fast convergence" on smooth
ones. PMJ's own best sequence only *matches* it.

Measured image-level numbers from [Burley 2020, Fig. 1][burley] (RMSE of a detail crop; independent
vs Owen-scrambled Sobol):

| integrand | spp | independent | Owen-Sobol | RMSE ratio |
|---|---|---|---|---|
| direct lighting | 16 | 0.020 | 0.0017 | 11.8× |
| GI with participating media and depth of field | 16 | 0.017 | 0.0040 | 4.3× |
| GI with a subpixel checkerboard texture | 64 | 0.046 | 0.043 | 1.07× |

The pattern is the important part: the gain is large where the integrand is smooth, and essentially
gone where subpixel geometry dominates. Burley also notes Owen scrambling is the only one of the
tested Sobol variants that does *not* produce structured artefacts on the unfavourable integrand.

**PMJ / PMJ02.** Progressive multi-jittered sequences fill in strata diagonally as samples are
added, so every prefix is well distributed, and PMJ02 "fully matches Owen-scrambled Sobol's error for
all sample counts" ([PMJ 2018, abstract and §6–§7][pmj]). PMJ02 has a (0,2) structure like Sobol and
is what RenderMan uses, truncated at 4096 samples in pregenerated tables, with a few hundred tables
and each pixel index and ray depth hashed to a table ([PMJ 2018 §11][pmj]). The cost is that
implementation shape: tables, 2D only, and pairs of 2D sequences shuffled for higher dimensions.
For our 20 dimensions that is a larger asset and more code than hash-based Sobol for no measured
error advantage. The non-progressive alternative, correlated multi-jittered, is competitive but
needs the sample count up front ([Kensler 2013][kensler]), which a still-frame accumulator does not
have.

**Production practice.** The renderers a beginner team is most likely to read do not agree on one
answer, but they bracket the choice. Cycles uses hash-based Sobol–Burley with a per-pixel hash and
shuffled 4D groups ([Cycles `pattern.h`][cycles_pattern]); pbrt-v4 ships `PaddedSobolSampler`,
`ZSobolSampler` (Morton-ordered Owen-Sobol) and `PMJ02BNSampler` ([pbrt-v4][pbrt4]); Mitsuba 3's
documented samplers are independent, stratified, correlated multi-jittered, orthogonal-array and
low-discrepancy, not hash-based Sobol ([Mitsuba][mitsuba]); Falcor ships Halton and stratified
patterns ([Falcor][falcor]). The common thread is that the high-end path tracers use Owen-scrambled
Sobol or PMJ02, and the real-time research renderers use simpler patterns because their sample counts
and budgets are different.

**R2 / Kronecker.** The R2 sequence is a rank-1 lattice: `fract(n·α)` with α from the plastic
constant ([Roberts 2018][r2], a non-peer-reviewed blog; the peer-reviewed review is
[Patel, JCGT 2022][kronecker]; golden-ratio sequences are an older peer-reviewed relative
[Schretter et al. 2012][schretter]). It is by far the cheapest generator — one integer add, one
int-to-float conversion and one multiply per dimension, 515 M 2D samples/s versus 154 M/s for
uniform random and 10 M/s for Burley's reference Sobol implementation on one 3.2 GHz Xeon core
([Patel 2022, Fig. 14][kronecker]). But Kronecker sequences are only uniformly distributed in the
limit; short subsequences are not always well distributed, their spectrum has lattice impulses
rather than a good blue spectrum ([Patel 2022, §2 and Fig. 14][kronecker]), and single-precision
arithmetic bounds the usable sequence length ([Patel 2022 §4][kronecker]). R2 is not a `(0,2)`
sequence and has no 1D multi-jittered stratification, so it cannot stand in for the pixel-jitter pair
the way a (0,2) sequence can. It is a good candidate for cheap per-frame offsets, not for the sample
sequence itself.

**Blue-noise masks (BNDS / STBN).** Blue-noise dithered sampling tiles a precomputed mask over the
image and uses its value as the per-pixel offset of the sample pattern, so neighbouring pixels get
very different offsets and the error's low-frequency content is removed ([Georgiev & Fajardo
2016][bnds]). The abstract is explicit: this produces "substantially more faithful images" *without
reducing the amount of error*. Masks are 64² in the figures, 128² in the renders ([BNDS][bnds]).
The spatiotemporal extension gives each pixel a sequence that is blue over time as well as space
([Wolfe et al. 2022][stbn]); the paper's Figure 1 reports, for an EMA-filtered volumetric render,
RMSE 0.25 → 0.16, 1-SSIM 0.21 → 0.12 and FLIP 0.10 → 0.07 versus independently animated 2D blue
noise. Its masks are 64×64 per frame ([Wolfe 2022, Fig. 4 caption][stbn]).

The decisive caveat for us is [Heitz & Belcour 2019][hb19]: BNDS "excels at one sample per pixel
with simple rendering integrals", but "the blue-noise properties of BNDS vanish rapidly as the number
of samples or the dimensionality of the integrals increase", and it "fails to provide significant
improvements to production Monte Carlo rendering with many rays per pixel that bounce multiple
times". Our reference is 5 bounces and up to 64 spp, exactly the regime where BNDS stops helping.
The Blender manual says the same thing about its blue-noise mode from the other side: the image
"typically appears smoother than Classic despite not actually reducing the overall noise"
([Blender manual, Cycles Sampling → Pattern][blender]).

**Hybrids.** Two published hybrids are worth naming:

- *Owen-Sobol with blue-noise pixel ordering.* Instead of an independent scramble per pixel, derive
  the sample index from the pixel's Morton (Z-order) position so that neighbouring pixels use
  far-apart blocks of one sequence. The error is then blue in screen space while each pixel's
  block stays a low-discrepancy net ([Ahmed & Wonka 2020][ahmed]). pbrt-v4 ships this as
  `ZSobolSampler`, with the index `morton(pixel) << log2(spp) | sample` and a base-4 digit scramble
  ([pbrt-v4 `samplers.h`][pbrt4]); Ahmed & Wonka's paper covers resolutions and sample counts that
  are not powers of two.
- *Blue-noise offset on top of Sobol.* Use an Owen-Sobol sequence per pixel and add a blue-noise
  Cranley–Patterson offset to the 2D pixel samples; Heitz et al. implement this "with a small
  texture and two xor operations" and report state-of-the-art convergence plus a blue-noise error
  distribution ([Heitz et al. 2019][heitz19]). pbrt-v4's `PMJ02BNSampler` does the same with PMJ02
  ([pbrt-v4][pbrt4]).

For our engine the first hybrid is the interesting one: it is the same integer-only Owen-Sobol
arithmetic, plus a Morton code and a shift, and no asset. The second needs a texture and a binding.

### 2. What "better" means at low spp

The ticket is right that the asymptotic order is not the whole story, but the candidates split into
two groups that optimise different things:

- **Error magnitude (MSE/RMSE).** Stratified and low-discrepancy sequences reduce it; blue-noise
  dithering explicitly does not ([BNDS][bnds]). Measured convergence on 2D test integrals: random
  `O(N^-0.5)`; jittered `O(N^-0.75)` at discontinuities and `O(N^-1)` on smooth functions;
  Owen-Sobol and PMJ02 `O(N^-0.75)` at discontinuities and `O(N^-1.5)` on smooth functions
  ([PMJ 2018 §3.1, §7][pmj]).
- **Error distribution (FLIP/SSIM and the spectrum).** A flat (white) spectrum has low-frequency
  clumps, which the eye picks out; blue noise removes them, so FLIP and SSIM improve at the same
  MSE ([BNDS][bnds], [HB19][hb19], [STBN][stbn]). FLIP is defined in [Andersson et al. 2020][flip];
  SSIM in [Wang et al. 2004][ssim].

The two can disagree, and for a *ground truth* that matters. The reference is the yardstick that
Primary is scored against with FLIP and MSE (#55, and the map's "Metric" note). A sequence that
merely redistributes its error into a nicer pattern improves the displayed image but leaves the
yardstick no more accurate; it spends the effort on how the reference *looks* rather than on how
close it *is*. So the reference should be chosen for error magnitude first, and any blue-noise trick
should be justified as display polish, never as accuracy. This is the main reason the recommendation
below is Owen-Sobol and not a blue-noise mask.

There is a further measured caveat that the ticket should know about. [Patel 2022 §5.4][kronecker]
path-traced a simple scene with seven sample generators and found "perhaps the least dynamic result
of all the testing; the various sample generators produced very similar results" in RMS error. In
high-dimensional path tracing the sequence gain is real but much smaller than the 2D numbers above
suggest; it is largest for direct lighting, soft shadows and AO, and shrinks as the integrand's
effective dimension grows.

### 3. Determinism and seeding in f32-only WGSL

WGSL is a good fit for hash-based Owen-Sobol because the whole sampler can be integer-only, and
integer arithmetic in WGSL is exactly specified: expressions that overflow produce a result modulo
2^bitwidth ([WGSL §6.2.3][wgsl]). `reverseBits`, `countOneBits` and shifts are exact. The only
floating-point steps are the conversion of at most 24 bits to f32 (exact) and a multiply by 2^-24
(exact). So the *sample positions* are bit-identical on every device, including lavapipe, which is
what the golden tests want. Floating-point expressions are a different matter: WGSL says an
implementation may reassociate and fuse operations, `x / y` is 2.5 ULP, and `sin`/`cos` have
absolute error up to 2^-11 on [-π, π] ([WGSL §15.7.4, §15.7.5][wgsl]). That already applies to the
current cone and hemisphere sampling, so the golden tolerance must stay; but a float-based sequence
such as R2 (`fract(n * α)` with a float multiply) would add avoidable cross-device drift on top.

The seeding recipe is Burley's [`shuffled_scrambled_sobol4d`][burley], with the scramble seed
derived from the pixel and the seed instead of a global seed:

```wgsl
// Once per (pixel, dispatch). This is today's seed chain, unchanged.
fn pixel_hash(pixel: vec2<u32>, seed: u32) -> u32 {
    return pcg(pixel.x + pcg(pixel.y + pcg(seed)));
}

// Burley 2020, Listing 2: a nested uniform scramble, and (applied to the index) a shuffle.
fn nested_uniform_scramble(x: u32, seed: u32) -> u32 {
    return reverseBits(laine_karras_permutation(reverseBits(x), seed));
}

// Any 32-bit mix of a seed and a small index; Burley uses hash_combine.
fn hash_combine(seed: u32, i: u32) -> u32 {
    return pcg(seed ^ (i * 0x9E3779B9u));
}

// One 4D group of a sample: the group's slots 0..3, already Owen-scrambled.
fn sobol_group(sample: u32, group: u32, pixel_seed: u32) -> vec4<f32> {
    let seed = hash_combine(pixel_seed, group);
    let index = nested_uniform_scramble(sample, seed);       // shuffles the sample order
    let x = sobol4d(index);                                  // the 4D Sobol point, dims 0..3
    return vec4<f32>(
        f32(nested_uniform_scramble(x.x, hash_combine(seed, 0u)) >> 8u) * (1.0 / 16777216.0),
        f32(nested_uniform_scramble(x.y, hash_combine(seed, 1u)) >> 8u) * (1.0 / 16777216.0),
        f32(nested_uniform_scramble(x.z, hash_combine(seed, 2u)) >> 8u) * (1.0 / 16777216.0),
        f32(nested_uniform_scramble(x.w, hash_combine(seed, 3u)) >> 8u) * (1.0 / 16777216.0),
    );
}
```

`sobol4d` needs the direction numbers for Sobol dimensions 0–3 only — dimension 0 is van der Corput
and needs none, so 3 × 32 `u32`, 384 bytes, or 96 constants in the shader — because every group
reuses the same 4D sequence with a different scramble seed. Burley: "For padding to higher
dimensions, one merely needs to supply a different seed to each group of four dimensions. This
effectively decorrelates each group of four dimensions against all others while preserving the
stratification within each group" ([Burley 2020 §4][burley]). The direction numbers are the standard
Sobol' set; use the Joe–Kuo file for dimensions 0–3 to avoid transcription risk
([Joe & Kuo][joekuo]).

This matters because a naive implementation that draws the dimensions in sequence, as the current
`next_f32` loop does, would give every group the same index and correlate them. Burley's shuffle of
the index is what prevents that.

Cycles does exactly this shape in production: `sobol_burley_sample_2D(index.x, dimension, index.y,
index.z)` where `index = (sample, per_pixel_hash ^ seed, length_mask)`, and the per-pixel hash is
`hash_iqnt2d(x, y) ^ seed` ([Cycles `pattern.h`][cycles_pattern], [`sobol_burley.h`][cycles_sobol]).
Blender's default "Classic" pattern is "pre-computed tables of Owen-scrambled Sobol"; its
"Blue-Noise" pattern is the hybrid in §1 ([Blender manual][blender]).

**Determinism checklist.** Same `(pixel, sample_index, seed)` gives the same point on every device
because only `u32` ops and exact `f32` conversions are involved. Keep two things separate: the QMC
index is the frame's sample number and nothing else, while any sequential random stream (the
existing `next_f32`) is for values that are not sample dimensions. Mixing them — drawing the sample
index from the PCG stream, or advancing the QMC index per bounce — reintroduces the correlations
QMC exists to remove. Because each dimension is looked up statelessly, an early-terminating path
simply does not read its later groups; that is free and unbiased.

### 4. Dimensions and decorrelation

Count the dimensions the kernel actually needs:

- 1 camera jitter pair = 2 dimensions.
- 5 hit points × 1 sun-cone pair = 10 dimensions.
- 5 hit points × 1 cosine pair = 10, but the last one is sampled and never traced (the loop's final
  iteration assigns `ray` and exits), so 8 are useful.

That is **20 useful dimensions out of the 22 the code currently consumes**. The unused final cosine
pair is a small free win: skip it, and 20 dimensions fit exactly into five 4D groups. A natural
mapping, putting each group's `(0,2)`-stratified pair on its most important integral:

| group | slots 0,1 | slots 2,3 |
|---|---|---|
| 0 | pixel jitter | sun cone, bounce 0 |
| 1 | cosine, bounce 0 | sun cone, bounce 1 |
| 2 | cosine, bounce 1 | sun cone, bounce 2 |
| 3 | cosine, bounce 2 | sun cone, bounce 3 |
| 4 | cosine, bounce 3 | sun cone, bounce 4 |

Sobol's first two dimensions are a perfect `(0,2)`-sequence ([Burley 2020 §2.2][burley]), so every
group's slots 0,1 are a stratified 2D pair — each bounce's cosine sample gets the best 2D
stratification available, and so does the pixel. Give every group a different seed (§3); that is
the padding decorrelation. Assign dimensions in order of importance and keep adjacent dimensions
for the most correlated integrals: Cycles' source states the rule directly — "earlier sets of
dimensions are better stratified" and "dimensions that are closer to each other are better
stratified than dimensions that are far" ([Cycles `pattern.h`][cycles_pattern]).

Reserve for growth: glossy lobes add 2–3 dimensions per bounce, glass 2–3, volumetrics 3+ per
segment. Budget 64 dimensions (16 groups) now, so adding M1b terms appends groups instead of
shifting every existing dimension. Shifting dimensions invalidates the stratification of everything
after the insertion point, and padding preserves the pairing only for the dimensions that were
padded together — not for arbitrary pairs ([Jarosz et al. 2019][jarosz]). A documented slot map is
worth more than a clever one.

**Neighbouring pixels.** Derive the per-pixel seed from `(pixel, seed)` (§3). Two pixels then use
different Owen scrambles of the same sequence; the probability that their first `N` points coincide
is negligible, and the 2D strata are independently rotated. This is the decorrelation Cycles uses.

**Successive frames.** Sample `n` and `n+1` are consecutive points of the same scrambled sequence.
That is the point of a progressive sequence: the running mean sees a `(0,m,2)`-net at every power of
two, not a fresh random set. Frame-to-frame independence is not what makes the mean unbiased — each
sample being marginally uniform does that — and it buys nothing once samples are averaged; what the
eye sees is the spatial pattern of the accumulated error ([HB19][hb19]).

**If a blue-noise spatial error is wanted**, change only the pixel→index map, not the sequence:
`index = (morton(pixel) << 6) | sample_index` for up to 64 spp, as pbrt-v4's `ZSobolSampler` does,
with the base-4 digit scramble from Ahmed & Wonka ([pbrt-v4][pbrt4], [Ahmed & Wonka 2020][ahmed]).
It costs a Morton encode and a few shifts, uses no texture, and turns the spatially white error of
independent per-pixel scrambles into a blue-noise distribution. It needs care with non-power-of-two
image sizes and sample caps; treat it as a second step, measured with FLIP, not as part of the first
change.

### 5. Cost in the compute kernel, and on lavapipe

Per dimension, hash-based Owen-Sobol costs one Sobol digit-XOR per set bit of the sample index
(at most 6 for 1–64 spp), one bit reversal pair, the four-multiply Laine–Karras permutation, and one
hash for the per-dimension seed. That is roughly 15–25 integer instructions per dimension, so
**~350–500 integer instructions per 20-dimension path**, plus 5 index shuffles. For comparison, one
voxel DDA step is a handful of instructions and a path takes hundreds of them across up to five
bounces and five shadow rays.

To get a number rather than an estimate, this ticket benchmarked the arithmetic on the research
machine (AMD Ryzen 7 5700G, `gcc -O3 -march=native`, one thread; the harness is
`/tmp/opencode/sampler_cost.c`, a standalone 22-dimension loop, **not** the renderer):

| generator | ns per 22-dim path | M paths/s | 720p sample, 1 thread |
|---|---|---|---|
| white noise (today) | 25.5 | 39.3 | 23.5 ms |
| Sobol, no Owen scramble | 39.4 | 25.4 | 36.3 ms |
| Owen-scrambled Sobol (portable bit reversal) | 141.9 | 7.0 | 130.7 ms |
| Owen-scrambled Sobol (byte-table reversal) | 116.6 | 8.6 | 107.5 ms |
| R2 (Kronecker) | 16.5 | 60.5 | 15.2 ms |

Three runs varied by about ±5 %. Read this as a CPU upper bound, not a GPU number. On a GPU `reverseBits` is one instruction, so the
reversal cost that dominates the CPU column largely disappears; the remaining cost is the Sobol
XORs, the permutation and the hashes. As a GPU estimate, ~0.5 G integer instructions per 720p
sample against a measured 1.10 ms/sample on the 4090 is on the order of 1–2 % of the sample if the
kernel were ALU-bound, and less where it is latency-bound on the voxel DDA. The 3.0 ms budget has
1.9 ms of headroom over the measured 1.10 ms, so the change should pass the budget; the bench must
confirm it, and if it does not, the fallback is a precomputed table of Owen-scrambled Sobol values
(one table lookup per dimension) rather than a cheaper sequence.

The published CPU rates bracket the same conclusion: Burley's reference implementation generates
10 M 2D Sobol–Owen samples/s on a 3.2 GHz Xeon, versus 154 M/s for uniform random, 139 M/s for
Heitz et al.'s table-driven Sobol and 515 M/s for a 2D Kronecker sequence
([Patel 2022, Fig. 14][kronecker]). Hash-based Owen-Sobol is the most expensive of the candidates
by arithmetic, and still small next to ray tracing.

**Lavapipe.** Mesa's llvmpipe is an LLVM-JIT software rasteriser; lavapipe is its Vulkan front end
([Mesa llvmpipe][mesa]). There are no vendor intrinsics to rely on, but the sampler needs none: it is
`u32` add/xor/shift/multiply, `reverseBits`, and exact int→float conversion, all of which LLVM lowers
to plain integer instructions. Nothing in the sampler is transcendental, so the absence of GPU
`sin`/`cos` units is irrelevant. The CPU proxy above is therefore a fair *upper* bound for the
sampler's share on lavapipe: ~0.1 µs per path, which for the 256×144×64-sample golden is
2.36 M paths ≈ 0.25–0.35 s single-threaded, or a few tens of milliseconds across lavapipe's threads.
The golden test may get slower, but should not become a problem; if it does, the byte-table variant
or a precomputed table is the lever.

The shading is a different story: `sin`/`cos` in `sample_cone` and `cosine_hemisphere` already differ
across implementations, so lavapipe goldens will keep needing the current MSE/far-off tolerance
(`common/mod.rs`). Do not tighten it because the sampler is now exact.

### 6. Batching and the mean reset

The sequence is random-access by `(sample_index, group, pixel_seed)`, so the still-frame batching
policy is independent of it:

- **k samples per dispatch.** Loop `j` in `0..k`, evaluate the path with `sample_index = n + j`, and
  update the running mean exactly as today (`mean += (estimate - mean) / (n + j + 1)`). Sobol is
  progressive, so the prefix `n..n+k` is as well distributed as `k` separate frames; there is no
  statistical difference, only one fewer submit and one more uniform write per frame.
- **Do not** index with `frame * k + j` and then re-scramble per frame: it breaks the prefix
  property. Use consecutive indices.
- **Mean reset on camera move.** Restarting at index 0 is exactly right: the sequence's prefixes are
  what make the progressive mean converge fast, and index 0 is well defined per pixel because of the
  per-pixel scramble (without a scramble, every pixel would jump to the same corner of its pixel at
  sample 0 and the first frame would show a fixed pattern).
- **Determinism.** A fixed `k` gives bit-identical images; changing `k` changes only the order of the
  float mean updates, by rounding, not the sample positions. If the reference must be bit-identical
  for a fixed sample count, fix `k` in the policy.
- **Per-tile reset**, if #94 later invalidates only edited tiles, is free: the counter is per tile,
  and the sequence is stateless.

One caveat for the crossfade policy: at 1 spp an Owen-Sobol image is spatially white (the per-pixel
scramble), so it will not look meaningfully cleaner than today in the first frame or two. The
visible gain starts around 4–16 spp, which at 60 fps and one sample per frame is 65–260 ms — the
tail of that range is outside the map's ~150 ms crossfade window. If 1–4 spp quality matters, the
Morton/blue-noise index map (§4) is the only lever that helps there, and it helps perception, not
MSE.

## Recommendation (ranked)

1. **Use hash-based Owen-scrambled Sobol for every dimension** (Burley 2020), evaluated per
   dimension from `(pixel, sample_index, seed)`, in 4D groups with an independent scramble seed per
   group and per pixel. Assign groups as in §4: pixel jitter first, then each bounce's cosine, with
   the near-delta sun cone in slots 2,3. Twenty dimensions today, sixty-four reserved. This is the
   only candidate that is top-tier on error magnitude *and* unbiased *and* bit-reproducible from
   integer arithmetic *and* needs no table of samples *and* is what Cycles and pbrt both ship.
2. **Morton-ordered pixel indexing (pbrt-v4 `ZSobolSampler`, Ahmed & Wonka 2020)** as a follow-up
   once the bench exists: the same sequence, a different pixel→index map, a few extra instructions,
   no texture. It buys a blue-noise spatial error distribution and therefore better FLIP at equal
   MSE. Measure it; do not assume it.
3. **A blue-noise mask offset (BNDS/STBN) only for the 2D pixel dimension**, and only if a measured
   FLIP improvement at 1–4 spp justifies a texture, a binding and an asset. It does not reduce MSE
   and its benefit vanishes by ~16 spp and in high-dimensional integrals.
4. **PMJ02** if the team ever wants a table-driven sampler. It matches Owen-Sobol on 2D, but it is
   2D-only, needs tables, and offers no measured advantage for our dimensions.
5. **Stratified pixel jitter alone** as a one-line interim if the full change must wait. Small gain;
   it fixes edge AA, not light-transport noise.
6. **R2** only as a cheap per-frame offset source, never as the sample sequence: no 2D
   stratification, lattice-shaped spectrum, f32 length limits.
7. **White noise**: keep as the rollback path only.

## Expected gain at equal spp

- **Direct lighting, soft shadows, AO (low-dimensional):** 11.8× lower RMSE for direct lighting at
  16 spp in [Burley 2020, Fig. 1a][burley]; convergence improves from `O(N^-0.5)` to `O(N^-0.75)` at
  discontinuities and `O(N^-1.5)` on smooth 2D integrals ([PMJ 2018][pmj]).
- **Full GI path tracing (our case):** 1.2–4.3× lower RMSE. The 4.3× figure is Burley's GI-with-media
  case ([Burley 2020, Fig. 1b][burley]); the path-tracing comparison in [Patel 2022 §5.4][kronecker]
  found the generators "very similar", which is the honest lower bound. Expect the visible win to be
  concentrated in smooth lit areas and penumbrae.
- **Pixels dominated by subpixel geometry or voxel silhouettes:** ~1× (0.046 → 0.043 in
  [Burley 2020, Fig. 1c][burley]). Do not promise cleaner silhouettes.
- **Perceptually**, the error also loses its low-frequency clumps, so FLIP improves by more than the
  RMSE ratio suggests — but that part is unquantified for our scene, and MSE is the ground-truth
  metric (§2).

Translated to samples: a 2× RMSE reduction is the same image at 4× fewer samples; 4× is 16× fewer.
So a realistic claim for our engine is **"the same noise at 2–8× fewer samples"**, i.e. 64 spp of
Owen-Sobol looking like 128–512 spp of white noise on lit areas, and no better on silhouettes. At
1.10 ms/sample, 64 spp at 1440p is 282 ms; the sequence change is what makes 16–32 spp (70–140 ms)
sufficient, and batching is what removes the interactive wait.

This is an estimate from other people's scenes. The acceptance measurement is `client bench` with
FLIP and MSE against a converged reference, at 4, 16 and 64 spp, before and after — the map already
requires exactly that, and the map's "measure on the 4090 only" rule applies.

## Risks for a graphics-beginner team

1. **Owen scrambling is easy to get subtly wrong.** Wrong bit order, a seed reused across groups, or
   scrambling the value instead of the index all produce images that look plausible but are
   correlated or badly stratified. Mitigate with property tests, not eyeballing (below).
2. **The direction numbers are a new asset.** A transcription error in the 4D table produces
   structured artefacts that look like a bug in the tracer. Take dimensions 0–3 from the Joe–Kuo
   file ([Joe & Kuo][joekuo]) and test the first 16 points against the published Sobol values
   ([Burley 2020 §2.2][burley] prints them).
3. **Goldens must be regenerated and re-approved.** 64 samples at 256×144 is the reference golden
   (`golden.rs`); changing the sample positions changes every pixel, so the existing MSE gate will
   fail loudly. That is expected. Regenerate with `UPDATE_GOLDEN=1`, inspect the (cleaner) image, and
   land it in the same PR. #100 is blocked by this work; the map already lists "how the lavapipe
   goldens are regenerated" as unspecified.
4. **Do not promise "clean at 1 spp".** At one sample the image is spatially white. The gain starts
   at 4–16 spp. The crossfade policy should be tuned knowing that.
5. **Power-of-two sample counts are where the magic is.** The `(0,m,s)`-net property and the deep
   error valleys hold at 1, 2, 4, 8, 16, 32, 64 ([PMJ 2018, Fig. 7][pmj] describes the "deep error
   valleys" for Owen-scrambled Sobol at powers of two). Between powers of two the error is still
   unbiased but a little higher. The engine's 1–64 range and the bench's 4 spp are fine; do not
   advertise the same gain at, say, 48 spp as at 32 or 64.
6. **The 4090 bench is the only proof of cost.** The sampler is ~0.5 G integer instructions per
   720p sample; this machine can only show that the arithmetic is small in CPU terms. The 3.0 ms
   budget has headroom, but a driver that does not vectorise `reverseBits` well could surprise us.
   Keep the precomputed-table fallback in the plan.
7. **Scope creep into the real-time path.** #94 says determinism may be relaxed only for the
   real-time path. Owen-Sobol strengthens determinism, which is the right direction; do not "share"
   the sampler with the real-time upscaler's Halton jitter (`upscale.rs`) in this change — that is a
   separate decision with its own budget.
8. **Metric interaction.** A FLIP+MSE gate will reward this change mostly through MSE and the
   removal of low-frequency clumps; a blue-noise mask would move FLIP without moving MSE. Decide
   which one the gate weights before adding a mask, or the two changes will argue with each other.

## How to verify before trusting it

- Unit/property tests on the GPU, in the style of `sampling_tests.rs`:
  - each dimension is uniform on [0,1) (mean ≈ 0.5, variance ≈ 1/12) over many pixels and samples;
  - the first 2D pair is stratified: at 64 samples, every pixel's 8×8 cells hold exactly one point
    (this is the `(0,2)` property, and a direct test of the direction table);
  - no correlation between group 0 and group 1, or between adjacent pixels (sample correlation
    coefficient near 0 over a large grid);
  - the same `(pixel, sample, seed)` gives bit-identical values on a second run and on lavapipe.
- A convergence test: render the test patch at 4, 16, 64 and 256 spp with the old and the new sampler
  and score each against a converged (1024-spp) render of the same patch made with the new sampler on
  the 4090. The new curve should sit below the old one from 4 spp onward, and the two should meet by
  256 spp.
- `client bench` on the 4090 for the reference pass and `gpu_total.reference`; the pass must stay
  under 3.0 ms and the sample budget under 3.0 ms.

## Appendix: the CPU cost harness

The numbers in §5 come from this standalone C program, run on the research machine. It is not the
renderer and it does not touch the engine; it exists so the arithmetic comparison can be reproduced
without a GPU. Save it, build it, and run it.

```c
// A machine-local proxy for the sampler cost inside the reference kernel. Not the renderer, not
// the bench: it only compares the arithmetic of the sample generators per 22-dimension path
// (1 camera jitter pair + 5 sun-cone pairs + 5 cosine pairs, as pathtrace.wgsl draws today).
//
// Build: gcc -O3 -march=native -o sampler_cost sampler_cost.c && ./sampler_cost
#include <stdint.h>
#include <stdio.h>
#include <time.h>

static inline uint32_t pcg(uint32_t v) {
    uint32_t state = v * 747796405u + 2891336453u;
    uint32_t word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

static inline uint32_t rev(uint32_t x) {
    x = ((x >> 1) & 0x55555555u) | ((x & 0x55555555u) << 1);
    x = ((x >> 2) & 0x33333333u) | ((x & 0x33333333u) << 2);
    x = ((x >> 4) & 0x0f0f0f0fu) | ((x & 0x0f0f0f0fu) << 4);
    x = ((x >> 8) & 0x00ff00ffu) | ((x & 0x00ff00ffu) << 8);
    return (x >> 16) | (x << 16);
}

static inline uint32_t lk(uint32_t x, uint32_t seed) {
    x += seed;
    x ^= x * 0x6c50b47cu;
    x ^= x * 0xb82f1e52u;
    x ^= x * 0xc7afe638u;
    x ^= x * 0x8d22f6e6u;
    return x;
}

static uint8_t revtab[256];
static inline uint32_t rev8(uint32_t x) {
    return (uint32_t)revtab[x & 255u] << 24 | (uint32_t)revtab[(x >> 8) & 255u] << 16 |
           (uint32_t)revtab[(x >> 16) & 255u] << 8 | (uint32_t)revtab[x >> 24];
}
static inline uint32_t owen(uint32_t x, uint32_t seed) { return rev(lk(rev(x), seed)); }
static inline uint32_t owen8(uint32_t x, uint32_t seed) { return rev8(lk(rev8(x), seed)); }

// Direction numbers stand in as a table read with the same access pattern as Sobol evaluation:
// one XOR per set bit of the sample index (<= 6 bits for 1..64 spp).
static uint32_t dir[22][6];
static inline uint32_t sobol(uint32_t index, uint32_t dim) {
    uint32_t v = 0;
    int b = 0;
    while (index) {
        if (index & 1u) v ^= dir[dim][b];
        index >>= 1u;
        b++;
    }
    return v;
}

// Current kernel: one PCG step per 32 bits, two per 2D dimension.
static inline float pcg_f32(uint32_t *s) {
    uint32_t old = *s;
    *s = old * 747796405u + 2891336453u;
    uint32_t word = ((old >> ((old >> 28u) + 4u)) ^ old) * 277803737u;
    return (float)(((word >> 22u) ^ word) >> 8u) * (1.0f / 16777216.0f);
}

static inline double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec + (double)t.tv_nsec * 1e-9;
}

#define PATHS (1u << 20)
#define DIMS 22u

volatile float sink;

static void report(const char *name, double seconds) {
    printf("%-24s %7.1f ns/path  %6.1f M paths/s  720p sample: %6.1f ms (1 thread)\n", name,
           seconds / PATHS * 1e9, PATHS / seconds / 1e6, 921600.0 / PATHS * seconds * 1e3);
}

int main(void) {
    for (int k = 0; k < 256; k++) revtab[k] = (uint8_t)rev((uint32_t)k) >> 24;
    for (int d = 0; d < 22; d++)
        for (int b = 0; b < 6; b++) dir[d][b] = pcg((uint32_t)(d * 131 + b + 1));

    double t0;
    float acc;

    t0 = now();
    acc = 0.0f;
    for (uint32_t i = 0; i < PATHS; i++) {
        uint32_t s = pcg(i);
        for (unsigned d = 0; d < DIMS; d += 2) {
            acc += pcg_f32(&s);
            acc += pcg_f32(&s);
        }
    }
    report("white noise (today)", now() - t0);
    sink = acc;

    t0 = now();
    acc = 0.0f;
    for (uint32_t i = 0; i < PATHS; i++) {
        uint32_t pixel_seed = pcg(i * 7u + 3u);
        for (unsigned d = 0; d < DIMS; d += 2) {
            uint32_t idx = i & 63u;
            uint32_t seed = pcg(pixel_seed ^ (d * 0x9e3779b9u));
            acc += (float)(owen(sobol(idx, d), seed) >> 8) * (1.0f / 16777216.0f);
            acc += (float)(owen(sobol(idx, d + 1), seed ^ 0x85ebca6bu) >> 8) * (1.0f / 16777216.0f);
        }
    }
    report("Owen-scrambled Sobol", now() - t0);
    sink = acc;

    t0 = now();
    acc = 0.0f;
    for (uint32_t i = 0; i < PATHS; i++) {
        for (unsigned d = 0; d < DIMS; d += 2) {
            uint32_t idx = i & 63u;
            acc += (float)(sobol(idx, d) >> 8) * (1.0f / 16777216.0f);
            acc += (float)(sobol(idx, d + 1) >> 8) * (1.0f / 16777216.0f);
        }
    }
    report("Sobol, no Owen", now() - t0);
    sink = acc;

    t0 = now();
    acc = 0.0f;
    for (uint32_t i = 0; i < PATHS; i++) {
        uint32_t pixel_seed = pcg(i * 7u + 3u);
        for (unsigned d = 0; d < DIMS; d += 2) {
            uint32_t idx = i & 63u;
            uint32_t seed = pcg(pixel_seed ^ (d * 0x9e3779b9u));
            acc += (float)(owen8(sobol(idx, d), seed) >> 8) * (1.0f / 16777216.0f);
            acc += (float)(owen8(sobol(idx, d + 1), seed ^ 0x85ebca6bu) >> 8) * (1.0f / 16777216.0f);
        }
    }
    report("Owen (byte-table reverse)", now() - t0);
    sink = acc;

    const float g = 1.32471795724474602596f;
    const float a1 = 1.0f / g, a2 = 1.0f / (g * g);
    t0 = now();
    acc = 0.0f;
    for (uint32_t i = 0; i < PATHS; i++) {
        for (unsigned d = 0; d < DIMS; d += 2) {
            float x = (float)i * a1 + (float)d * 0.5f;
            float y = (float)i * a2 + (float)d * 0.5f;
            acc += x - (float)(int)x;
            acc += y - (float)(int)y;
        }
    }
    report("R2 (Kronecker)", now() - t0);
    sink = acc;
    return 0;
}
```

## What this changes for the map

- The map's "white noise ... uncorrelated from frame to frame, which is the worst case for the eye"
  conflates two things. Frame-to-frame independence is one way to make a progressive mean unbiased,
  not a requirement, and it is not what the eye objects to; spatially white (low-frequency-clumped)
  accumulated error is. Owen-Sobol fixes the spatial part while remaining progressive, and it does
  not make frames "less uncorrelated".
- The map's metric note (FLIP + MSE) needs an explicit weighting for this decision: a blue-noise
  mask moves FLIP and not MSE, and the reference is the MSE yardstick. Recommend MSE as the gate for
  the sample sequence, FLIP as the gate for presentation polish.
- The map's still-first policy should budget for the fact that the sequence gain is ~1× at 1 spp and
  ~2–4× by 16–64 spp; batching, not the sequence, is what makes the first 150 ms feel fast.
- The map's "output resolution for Reference" quadruples the per-sample pixel count; the sampler cost
  scales with it too, but stays inside the budget at the measured rate.
- Nothing found contradicts the map's constraints (unbiased, denoiser-free, bit-reproducible,
  bench-per-sample). If anything the integer-only sampler makes the determinism constraint easier to
  keep across lavapipe and the 4090.

## References

Every source was fetched or its abstract read while writing this, except where an automated fetch
was blocked (the Owen 1995 Springer page, the ACM page for FLIP, and the R2 page); those are cited
by DOI or URL for the concept, and the claims attributed to them are also stated in an open source
that was read in full, noted where so.

1. [Sobol 1967] I. Sobol', "On the distribution of points in a cube and the approximate evaluation
   of integrals", *USSR Computational Mathematics and Mathematical Physics* 7(4), 86–112, 1967.
   https://doi.org/10.1016/0041-5553(67)90144-9
2. [Owen 1995] A. B. Owen, "Randomly permuted (t,m,s)-nets and (t,s)-sequences", in *Monte Carlo and
   Quasi-Monte Carlo Methods in Scientific Computing*, Lecture Notes in Statistics 106, Springer,
   299–317, 1995. https://doi.org/10.1007/978-1-4612-2552-2_19 (paywalled; the property used here is
   stated and used in [Burley 2020 §2–3] and [Kollig & Keller 2002 §3.2.2].)
3. [Owen 1997] A. B. Owen, "Monte Carlo variance of scrambled net quadrature", *SIAM Journal on
   Numerical Analysis* 34(5), 1884–1910, 1997. https://doi.org/10.1137/S0036142994277468
4. [LK11] S. Laine, T. Karras, "Stratified Sampling for Stochastic Transparency", *Computer Graphics
   Forum* 30(4) (EGSR 2011). https://doi.org/10.1111/j.1467-8659.2011.01978.x ;
   https://research.nvidia.com/publication/2011-06_stratified-sampling-stochastic-transparency
5. [Burley] B. Burley, "Practical Hash-based Owen Scrambling", *Journal of Computer Graphics
   Techniques* 9(4), 1–20, 2020. https://jcgt.org/published/0009/04/01/ (PDF:
   https://jcgt.org/published/0009/04/01/paper.pdf)
6. [PMJ] P. Christensen, A. Kensler, C. Kilpatrick, "Progressive Multi-Jittered Sample Sequences",
   *Computer Graphics Forum* 37(4) (EGSR 2018), 21–33. https://doi.org/10.1111/cgf.13472 ; PDF:
   http://eastfarthing.com/publications/pmj.pdf
7. [Kensler 2013] A. Kensler, "Correlated Multi-Jittered Sampling", Pixar Technical Memo 13-01,
   2013. http://eastfarthing.com/publications/cmj.pdf
8. [KK02] T. Kollig, A. Keller, "Efficient Multidimensional Sampling", *Computer Graphics Forum*
   21(3) (Eurographics 2002), 557–563. https://doi.org/10.1111/1467-8659.00706
9. [Joe & Kuo] S. Joe, F. Y. Kuo, "Constructing Sobol sequences with better two-dimensional
   projections", *SIAM Journal on Scientific Computing* 30(5), 2635–2654, 2008; direction numbers
   and the 21201-dimension file at https://web.maths.unsw.edu.au/~fkuo/sobol/
10. [Heitz 2019] E. Heitz, L. Belcour, V. Ostromoukhov, D. Coeurjolly, J.-C. Iehl, "A
    Low-Discrepancy Sampler that Distributes Monte Carlo Errors as a Blue Noise in Screen Space",
    *ACM SIGGRAPH 2019 Talks*. https://doi.org/10.1145/3306307.3328191 ; PDF:
    https://perso.liris.cnrs.fr/david.coeurjolly/publication/heitz-19/heitz-19.pdf
11. [HB19] E. Heitz, L. Belcour, "Distributing Monte Carlo Errors as a Blue Noise in Screen Space by
    Permuting Pixel Seeds Between Frames", *Computer Graphics Forum* 38(4) (EGSR 2019), 149–158.
    https://doi.org/10.1111/cgf.13778
12. [BNDS] I. Georgiev, M. Fajardo, "Blue-noise Dithered Sampling", *ACM SIGGRAPH 2016 Talks*.
    https://doi.org/10.1145/2897839.2927430 ; abstract:
    http://www.iliyan.com/publications/DitheredSampling (full abstract PDF:
    https://blogs.autodesk.com/media-and-entertainment/wp-content/uploads/sites/162/dither_abstract.pdf)
13. [STBN] A. Wolfe, N. Morrical, T. Akenine-Möller, R. Ramamoorthi, "Spatiotemporal Blue Noise
    Masks", *Eurographics Symposium on Rendering (DL-only)*, 2022.
    https://doi.org/10.2312/sr.20221161 ; PDF: https://cseweb.ucsd.edu/~ravir/stbn.pdf ; code and
    masks: https://github.com/NVIDIAGameWorks/SpatiotemporalBlueNoiseSDK
14. [Ahmed] A. G. M. Ahmed, P. Wonka, "Screen-Space Blue-Noise Diffusion of Monte Carlo Sampling
    Error via Hierarchical Ordering of Pixels", *ACM Transactions on Graphics* 39(6), 244, 2020.
    https://doi.org/10.1145/3414685.3417881 ; https://abdallagafar.com/publications/zsampler/
15. [Kronecker] M. Patel, "Optimizing Kronecker Sequences for Multidimensional Sampling", *Journal
    of Computer Graphics Techniques* 11(1), 2022. https://jcgt.org/published/0011/01/04/
16. [R2] M. Roberts, "The Unreasonable Effectiveness of Quasirandom Sequences", 2018.
    http://extremelearning.com.au/unreasonable-effectiveness-of-quasirandom-sequences/
    (author's own page, not peer-reviewed; used only for the definition and for the fact that R2 is
    a rank-1 lattice.)
17. [Schretter 2012] C. Schretter, L. Kobbelt, P.-O. Dehaye, "Golden Ratio Sequences for
    Low-Discrepancy Sampling", *Journal of Graphics Tools* 16(2), 95–104, 2012.
    https://doi.org/10.1080/2165347X.2012.679555
18. [PBRT] M. Pharr, W. Jakob, G. Humphreys, *Physically Based Rendering*, 3rd ed., §7.3
    "Stratified Sampling". https://pbr-book.org/3ed-2018/Sampling_and_Reconstruction/Stratified_Sampling
19. [pbrt4] pbrt-v4 source, `src/pbrt/samplers.h` (`ZSobolSampler`, `PMJ02BNSampler`, `PaddedSobolSampler`).
    https://github.com/mmp/pbrt-v4/blob/master/src/pbrt/samplers.h
20. [Cycles pattern] Blender Cycles, `src/kernel/sample/pattern.h`.
    https://github.com/blender/cycles/blob/main/src/kernel/sample/pattern.h
21. [Cycles sobol] Blender Cycles, `src/kernel/sample/sobol_burley.h`.
    https://github.com/blender/cycles/blob/main/src/kernel/sample/sobol_burley.h
22. [Blender] Blender Manual, "Cycles → Render Settings → Sampling" (Pattern: Automatic, Classic,
    Blue-Noise; Seed; Scrambling Distance).
    https://docs.blender.org/manual/en/latest/render/cycles/render_settings/sampling.html
23. [Mitsuba] Mitsuba 3 documentation, "Samplers" (independent/PCG32, stratified, correlated
    multi-jittered, orthogonal array, low-discrepancy).
    https://mitsuba.readthedocs.io/en/latest/src/generated/plugins_samplers.html
24. [Falcor] Falcor sample generators (`HaltonSamplePattern`, `StratifiedSamplePattern`, DX sample
    pattern). https://github.com/NVIDIAGameWorks/Falcor/tree/master/Source/Falcor/Utils/SampleGenerators
25. [WGSL] W3C, *WebGPU Shading Language*, §6.2.3 integer overflow (modulo 2^bitwidth), §15.7.4
    floating-point accuracy, §15.7.5 reassociation and fusion.
    https://www.w3.org/TR/WGSL/
26. [Mesa] Mesa documentation, "LLVMpipe". https://docs.mesa3d.org/drivers/llvmpipe.html
27. [FLIP] P. Andersson, J. Nilsson, T. Akenine-Möller, M. Oskarsson, K. Åström, M. D. Fairchild,
    "FLIP: A Difference Evaluator for Alternating Images", *Proc. ACM on Computer Graphics and
    Interactive Techniques* 3(2), 15:1–15:23, 2020. https://doi.org/10.1145/3406183
28. [SSIM] Z. Wang, A. C. Bovik, H. R. Sheikh, E. P. Simoncelli, "Image quality assessment: from
    error visibility to structural similarity", *IEEE Transactions on Image Processing* 13(4),
    600–612, 2004. https://doi.org/10.1109/TIP.2003.819861
29. [Jarosz 2019] W. Jarosz et al., "Orthogonal Array Sampling for Monte Carlo Rendering",
    *Computer Graphics Forum* 38(4) (EGSR 2019). https://doi.org/10.1111/cgf.13777 (padding loses
    stratification for dimension pairs other than the padded ones; motivates the fixed dimension
    slots in §4.)
30. Engine sources read: `crates/render/src/shaders/sampling.wgsl`, `pathtrace.wgsl`, `common.wgsl`,
    `crates/render/src/tracer.rs`, `accumulation.rs`, `window.rs`, `sun.rs`, `upscale.rs`,
    `crates/render/tests/golden.rs`, `crates/render/tests/common/mod.rs`,
    `bins/client/bench-budgets.toml`, `bins/client/src/bench/mod.rs`.

[pmj]: http://eastfarthing.com/publications/pmj.pdf
[pbrt]: https://pbr-book.org/3ed-2018/Sampling_and_Reconstruction/Stratified_Sampling
[owen95]: https://doi.org/10.1007/978-1-4612-2552-2_19
[owen97]: https://doi.org/10.1137/S0036142994277468
[kk02]: https://doi.org/10.1111/1467-8659.00706
[lk11]: https://doi.org/10.1111/j.1467-8659.2011.01978.x
[burley]: https://jcgt.org/published/0009/04/01/paper.pdf
[r2]: http://extremelearning.com.au/unreasonable-effectiveness-of-quasirandom-sequences/
[kronecker]: https://jcgt.org/published/0011/01/04/
[bnds]: http://www.iliyan.com/publications/DitheredSampling
[stbn]: https://cseweb.ucsd.edu/~ravir/stbn.pdf
[hb19]: https://doi.org/10.1111/cgf.13778
[ahmed]: https://doi.org/10.1145/3414685.3417881
[pbrt4]: https://github.com/mmp/pbrt-v4/blob/master/src/pbrt/samplers.h
[heitz19]: https://doi.org/10.1145/3306307.3328191
[blender]: https://docs.blender.org/manual/en/latest/render/cycles/render_settings/sampling.html
[mesa]: https://docs.mesa3d.org/drivers/llvmpipe.html
[wgsl]: https://www.w3.org/TR/WGSL/
[joekuo]: https://web.maths.unsw.edu.au/~fkuo/sobol/
[cycles_pattern]: https://github.com/blender/cycles/blob/main/src/kernel/sample/pattern.h
[cycles_sobol]: https://github.com/blender/cycles/blob/main/src/kernel/sample/sobol_burley.h
[flip]: https://doi.org/10.1145/3406183
[ssim]: https://doi.org/10.1109/TIP.2003.819861
[sobol]: https://doi.org/10.1016/0041-5553(67)90144-9
[kensler]: http://eastfarthing.com/publications/cmj.pdf
[schretter]: https://doi.org/10.1080/2165347X.2012.679555
[mitsuba]: https://mitsuba.readthedocs.io/en/latest/src/generated/plugins_samplers.html
[falcor]: https://github.com/NVIDIAGameWorks/Falcor/tree/master/Source/Falcor/Utils/SampleGenerators
[jarosz]: https://doi.org/10.1111/cgf.13777
