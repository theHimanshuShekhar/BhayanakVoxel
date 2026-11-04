# Measuring convergence against a ground-truth render

Research for [#98](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/98), under the map
[#94](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/94) ("a clean, honest
reference path-traced view"). Answers: how should "converged" be measured, implemented and
enforced in **this** repo?

Status: research only. No code changed, no PR. Branch `research/reference-metric`.

---

## 1. The question, made concrete

The Reference view is `crates/render/src/shaders/pathtrace.wgsl`: one sample per dispatch, added
to a running mean in an f32 storage buffer, reset whenever the camera, sun or image size changes
(`crates/render/src/accumulation.rs:34`, key in `crates/render/src/tracer.rs:217`). The bench
draws it 60 frames × 4 spp along a **moving** camera path (`bins/client/src/bench/mod.rs:75`), so
every frame starts a new image and nothing in the repo today ever renders a converged frame or
measures distance to one.

"Converged" therefore has to be defined by us. Three things need answers:

1. **Which number** judges a pixel, and at what error does a still frame read as clean?
2. **What is it measured against** — what is the truth, how many samples, how is it kept
   reproducible?
3. **Where does it run** — the 4090 bench, CI's lavapipe, or both — without changing the
   per-sample budget the reports are built on.

## 2. What the code does today (the constraints the answer has to fit)

| Fact | Where |
|---|---|
| The image is written as `pow(linear, 1/2.2)` into `Rgba8Unorm`, so values are **gamma-encoded and clamped to 0..1** (not exactly sRGB, not HDR) | `crates/render/src/shaders/common.wgsl` (`encode`), `crates/render/src/tracer.rs:21` |
| The running mean is f32, one `vec4<f32>` per pixel, `mean + (x - mean)/(n+1)`; pathological in principle, exact enough to ~10⁶ spp in practice | `crates/render/src/shaders/pathtrace.wgsl:83-90` |
| The accumulation buffer is `STORAGE`-only — **it cannot be copied back to the CPU** today | `crates/render/src/tracer.rs:171-176` |
| Paths stop at `MAX_BOUNCES = 4`, no Russian roulette: a deliberate *bias*, not noise | `crates/render/src/shaders/pathtrace.wgsl:18` |
| RNG is integer PCG seeded by `(pixel, sample_index, RNG_SEED)`; same inputs ⇒ same bytes on the same device | `crates/render/src/shaders/sampling.wgsl:22`, `crates/render/src/tracer.rs:87` |
| Reference sample budget is **3.0 ms** at 1280×720 internal (measured: mean 1.10 ms, p95 2.69 ms) | `bins/client/bench-budgets.toml` |
| Reference goldens are 256×144, 64 spp, gated at MSE ≤ 1.0 (0..255 scale) and ≤ 0.2 % pixels off by > 40 levels | `crates/render/tests/golden.rs:100`, `crates/render/tests/common/mod.rs:38` |
| The repo already measured: a **different seed** at 64 spp scores **MSE 4.06**; no shadow rays 62; sun strength +10 % 5.1; one bounce fewer **0.02** | `crates/render/tests/golden.rs:116-120` |
| CI runs the goldens on lavapipe (`mesa-vulkan-drivers`); the bench only runs there as a `--short` smoke test whose timings are ignored | `.github/workflows/ci.yml:20-27` |
| Bench already reads the reference image back every frame (`render_reference` returns the 8-bit image) | `crates/render/src/offscreen.rs` (`render_reference`) |

Two consequences worth stating up front:

- The 64-spp golden gate (MSE ≤ 1.0) sits **below the noise that is actually in that image**
  (MSE 4.06 for independent samples). It is a *bit-reproducibility / correctness* gate, not a
  convergence gate, and it should not be repurposed as one.
- On that golden view, dropping a whole bounce changes MSE by only 0.02 — i.e. **the golden view
  cannot see the 4-bounce truncation bias at all**. A fidelity metric needs at least one view
  with real inter-reflection (a crevasse, a terrace shadow, an interior).

---

## 3. Findings

### 3.1 FLIP: what to use, in what language, at what cost

**The reference implementation.** FLIP is Andersson, Nilsson, Akenine-Möller, Oskarsson, Åström
and Fairchild, *FLIP: A Difference Evaluator for Alternating Images*, PACMCGIT 3(2), 2020
(HPG 2020), DOI [10.1145/3406183](https://doi.org/10.1145/3406183)
([paper page](https://research.nvidia.com/publication/2020-07_FLIP),
[PDF](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)). It is
per-pixel: linear RGB → Yyₓc_z opponent space, per-channel contrast-sensitivity filtering whose
kernel radius depends on pixels-per-degree, a colour difference, an edge/point feature
difference, combined as ΔE = (ΔE_c)^(1−ΔE_f), then pooled
([paper §4–5](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)). The
reference code is [NVlabs/flip](https://github.com/NVlabs/flip), **BSD-3-Clause**, v1.7, with
C++, CUDA, Python, PyTorch and MATLAB implementations.

Two upstream statements matter more than any blog:

- **Use the mean.** The paper page's *Hindsight* note: "in the paper we advocated for the
  weighted median … If a single number is to be used, then we recommend using the **mean** FLIP
  value instead" ([paper page](https://research.nvidia.com/publication/2020-07_FLIP)).
- **FLIP is not magic.** The paper reports its own failure mode (masking: it overestimates error
  in busy/compressed content) and notes it *detects fireflies* in very high-spp renders
  ([paper §6, Fig. 10](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)).
  A denoiser that smears fireflies will score better than one that keeps them; that is a
  property of the metric to know about, not a bug.

**Is there a Rust implementation?** Two, both usable:

- [`flip-rs`](https://crates.io/crates/flip-rs) 0.1.2 (Oct 2026), pure Rust, `no unsafe`, BSD-3,
  LDR-FLIP and HDR-FLIP, default `parallel` via Rayon, WASM-capable, MSRV 1.88
  ([README](https://raw.githubusercontent.com/Tavrin/flip-rs/main/README.md)). Its own parity
  harness (which it ships and documents how to reproduce) reports **pixel-exact equality with
  NVIDIA's C++ v1.7** (`b475eb4`) on 201 cases and on the 2,974 defined cases of a 3,000-case
  seeded sweep, and **2.0–2.6× faster than the C++ built with `g++ -O2`, single-threaded**.
- [`nv-flip`](https://crates.io/crates/nv-flip) 0.1.2 (Jul 2023), FFI bindings that compile
  NVIDIA's C++ at build time — authoritative by construction, but it bundles **pre-v1.3**
  sources, so **LDR-FLIP only**, and needs a C++ toolchain
  ([flip-rs's comparison](https://raw.githubusercontent.com/Tavrin/flip-rs/main/README.md)).

`flip-rs`'s "exact" claim is self-reported, but it is falsifiable and reproduced by a documented
harness, and it is plausible because the port preserves the reference's summation order. NVIDIA
itself warns that *independent* FLIP implementations differ slightly (f32 order of operations,
FMA, lower-precision `sqrt`) ([misc/precision.md](https://github.com/NVlabs/flip/blob/main/misc/precision.md)).
So treat "exact" as "exact on that corpus, with that toolchain", which is enough for us.

**A WGSL port?** None exists that I can find (GitHub code search for FLIP in `.wgsl` returns only
unrelated "flip the texture" shaders). The GPU implementations that do exist are NVIDIA's own
CUDA path (`FLIP_ENABLE_CUDA` in the C++ build) and third-party CUDA/VapourSynth ports. Porting
FLIP to WGSL is *feasible* — it is separable convolutions plus a colour transform — but it is
the wrong trade here: it is days of work to save a CPU cost we can afford (§3.5), and it moves
the yardstick onto the GPU, where WGSL explicitly allows reassociation and FMA fusion
([WGSL §15.7.5](https://www.w3.org/TR/WGSL/#reassociation)) and gives only bounded,
implementation-defined accuracy for built-ins (e.g. `cos` "absolute error at most 2⁻¹¹" inside
[−π, π], [WGSL §17.5](https://www.w3.org/TR/WGSL/#accuracy-of-built-in-functions)). A metric that
wobbles with the driver is a bad metric. **Reject.**

**Cost.** Published numbers (AMD Ryzen 9 7945HX, one thread / eight threads)
([flip-rs README](https://raw.githubusercontent.com/Tavrin/flip-rs/main/README.md)):

| Image | 1 thread | 8 threads |
|---|---:|---:|
| 1920×1080 | 0.347 s | 0.203 s |
| 3840×2160 | 1.452 s | 0.603 s |

Interpolating by pixel count (labelled estimate, not a measurement):

- **2560×1440**: ≈ **0.62 s** single-thread, ≈ **0.36 s** on 8 threads.
- **256×144** (the golden size): ≈ **6 ms**.

The FLIP paper's own timing, for scale: 180 ms for 1920×1080 on an RTX 2080 Ti with an
"unoptimized GPU implementation"
([paper §6.1](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)).

**Can it run inside `client bench` on Windows?** Yes. `flip-rs` is pure Rust with no C/C++
dependency, so it adds nothing to the Windows build beyond a crate; the bench is already a native
Windows binary (`.github/workflows/ci.yml:48-59`). 0.36–0.62 s per comparison is the same order
as the GPU work of one 1440p sample batch, and it happens on the CPU while the GPU is idle
between frames — the same place the existing image read-back already lives.

### 3.2 MSE / RMSE / PSNR on an 8-bit sRGB image: what the floor is

Definitions: MSE = mean over RGB of (reference − test)², RMSE = √MSE, PSNR = 10·log₁₀(MAX²/MSE).
MSE is the variance of an unbiased estimator's error, and against a reference it estimates
`E[(F − truth)²]`, which is why it is the natural *cheap* number for a Monte Carlo curve
([PBRT 4ed §2.1.4](https://pbr-book.org/4ed/Monte_Carlo_Integration/Monte_Carlo_Basics)).
PBRT ships exactly this workflow: `imgtool diff --metric MSE|MRSE|MAE|FLIP --reference …`
([`imgtool.cpp`](https://github.com/mmp/pbrt-v4/blob/master/src/pbrt/cmd/imgtool.cpp)).

**The floor of an 8-bit comparison.** The image under test is 8 bits per channel, gamma-encoded
and clamped ([webgpu: normalized formats clamp to the type's range](https://www.w3.org/TR/webgpu/#output-merging)).
Quantising a continuum with step Δ adds noise of variance Δ²/12; with Δ = 1 level,
**MSE_floor = 1/12 ≈ 0.083 LSB²**, i.e. **PSNR ≈ 58.9 dB** — the best two 8-bit images can ever
score. For calibration (own arithmetic, stated so it can be checked):

| Comparison | MSE (LSB²) | RMSE (levels) | PSNR |
|---|---:|---:|---:|
| identical bytes | 0 | 0 | ∞ |
| 8-bit quantisation floor | 0.083 | 0.29 | 58.9 dB |
| this repo's 64-spp seed noise | 4.06 | 2.02 | 42.0 dB |
| the reference golden's gate | 1.0 | 1.0 | 48.1 dB |

So: **any MSE ≤ ~0.08 or PSNR ≥ ~59 dB is unmeasurable on an 8-bit image**, and the existing
golden gate of 1.0 is 12× the floor — a determinism gate, as §2 argued.

Gamma encoding helps here: one 8-bit step is a roughly constant *relative* step (≈0.9 % of the
value, because d(x^2.2)/dx ∝ x^1.2 and x^1.2/x is flat-ish), which is why an sRGB-space MSE is a
tolerable proxy for relative error. In *linear* space the same one level is 2.4e-4 at x = 0.05
and 6.6e-3 at x = 0.8, so a linear MSE dominated by bright pixels says little about the darks
(own arithmetic from the sRGB transfer function,
[sRGB proposal eq. 1.2](https://www.w3.org/Graphics/Color/sRGB.html)). Report both.

Two caveats specific to this engine:

- `encode()` is `pow(x, 1/2.2)`, not the sRGB piecewise curve, which the sRGB specification
  describes as "closely fit[ting] a straightforward gamma 2.2 curve **with a slight offset**"
  ([sRGB proposal](https://www.w3.org/Graphics/Color/sRGB.html)). FLIP re-linearises assuming
  sRGB, so there is a small, systematic, dark-tone-biased error between what we store and what
  FLIP thinks it reads. It is far below any threshold we will set, but it must be documented, or
  `encode()` should become exact sRGB (a visible change ⇒ goldens regenerate).
- Values above 1.0 are clamped by the unorm format, so **the brightest pixels are invisible to
  any metric computed on the 8-bit image**, even when the estimator is still changing there.
  The honest array is the f32 accumulation buffer, which is currently not readable (§2).

### 3.3 Ground truth: spp, resolution, reproducibility

**A render, not an analytic scene — for the perceptual threshold.** FLIP is designed and
validated on rendered pairs, and the paper's own canonical demonstration is a *naive path
tracer's spp progression*: reference at 2²² spp, tests at 1, 4, 16, …, 65,536 spp, FLIP maps
alongside ([paper Fig. 10](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)).
That is exactly the artifact this ticket wants, and it also tells us the authors considered
~4 million spp a *paper-grade* reference.

**How many spp is enough here.** Error falls as O(N^-1/2), so quadruple the samples to halve the
RMSE ([PBRT §2.1.4](https://pbr-book.org/4ed/Monte_Carlo_Integration/Monte_Carlo_Basics)). Fitting
that law to the repo's measured 64-spp MSE of 4.06, and adding the 8-bit floor in quadrature:

| spp | predicted MSE (LSB²) | RMSE | PSNR |
|---:|---:|---:|---:|
| 64 | 4.1 | 2.0 | 42 dB |
| 256 | 1.1 | 1.0 | 48 dB |
| 1 024 | 0.34 | 0.58 | 53 dB |
| 4 096 | 0.15 | 0.38 | 56 dB |
| 8 192 | 0.11 | 0.33 | 57.5 dB |
| ∞ | 0.083 | 0.29 | 58.9 dB |

Two conclusions: **8-bit output stops being able to show improvement past roughly 4 000 spp**,
and an **8192-spp truth is enough for an 8-bit LDR truth** (its own uncertainty, ~0.11 LSB², is
within 30 % of the floor). To go further, the truth must be stored as f32 (the accum buffer) or
16-bit — a PNG truth cannot.

Cost on the dev's 4090, extrapolated from the repo's own measured 1.10 ms/sample at 1280×720
(×4 for the pixel count; **estimate**, to be confirmed by #99):

- 1 024 spp at 2560×1440 ≈ **4.5 s**
- 8 192 spp at 2560×1440 ≈ **36 s**

**Resolution.** The truth must be at the resolution the metric is taken at. #55 compares a
*denoised* image that is built at output resolution (the upscaler writes
`target.image()` at 2560×1440), so the comparison resolution is output resolution, matching the
map's decision that the Reference goes to output resolution. Note the consequence: the reference
traced at 1440p is 4× the work per sample of the 1280×720 one the 3.0 ms budget describes.

**Reproducibility.** Same binary + same driver ⇒ bit-identical, because the RNG is integer
arithmetic keyed on `(pixel, sample_index, seed)` and the running mean is deterministic; the
repo already asserts this ([`crates/render/tests/path_tracer.rs:115`](crates/render/tests/path_tracer.rs)).
Across devices/drivers it is **not** guaranteed: WGSL permits reassociation and FMA fusion and
bounds built-in accuracy rather than pinning it
([WGSL §15.7.4/§15.7.5](https://www.w3.org/TR/WGSL/#accuracy-of-built-in-functions)). That is
precisely why the goldens are compared with a tolerance and why Mesa-version drift is documented
in `crates/render/tests/golden.rs:13-15`. Therefore:

- Truths are **per-machine artifacts**. Never compare a 4090 truth to a lavapipe render.
- Store the truth's **inputs** (scene seed, camera, sun, view, resolution, `RNG_SEED`, spp) in a
  sidecar, and the truth as 8-bit PNG for humans plus the raw f32 accum for arithmetic.
- Sample *count* must be reproducible, not sample *timing*: the running mean after N samples is
  the mean of the first N, so a prefix is well-defined (the repo already relies on this,
  `render_reference` continuing across calls).

### 3.4 Threshold selection, and how it binds #55

There is no published just-noticeable-difference for FLIP, and inventing one would be exactly the
kind of unsourced claim this ticket forbids. What exists is (a) a published *starting range* and
(b) a way to calibrate against our own noise floor. Use both:

1. **Anchor from practice.** wgpu compares its own rendered examples with FLIP (via `nv-flip`) in
   CI on lavapipe, and its `ComparisonType` documentation says: "Error thresholds will be
   different for every test, but good initial values to look at are in the **[0.01, 0.1]** range.
   The larger the area that might have inherent variance, the larger this base value is. Using a
   high percentile comparison (e.g. **95 % or 99 %**) is good for images that are likely to have a
   lot of error in a small area"
   ([`wgpu/tests/src/image.rs`](https://github.com/gfx-rs/wgpu/blob/trunk/tests/src/image.rs),
   [docs/testing.md](https://github.com/gfx-rs/wgpu/blob/trunk/docs/testing.md)). This is the
   closest thing to a standard practice in our exact stack, and it is a *range*, not a number.
2. **Calibrate against the floor.** Render the same view twice with independent seeds at the
   truth's spp. The FLIP between them is the metric's own resolution on that view. **A threshold
   is only meaningful if it is several times that floor.** This is the same reasoning that makes
   the 8-bit MSE floor (0.083) the lower bound for any MSE threshold.

Proposed scale, to be confirmed by the #99 measurement (not asserted as fact):

| FLIP mean | reading |
|---|---|
| ≤ 0.02 | visually clean still frame; the target for "converged" |
| ≤ 0.05 | acceptable; small residual noise visible on inspection |
| > 0.10 | visibly not converged, or a real regression |

plus **FLIP p99 (weighted) ≤ 0.10**: a mean hides a handful of awful pixels, and FLIP is known to
light up fireflies; the high percentile is what catches them.

**Relation to #55.** #55 asks for "an image-difference metric (FLIP or MSE) against the converged
reference path tracer … with a threshold that CI enforces on lavapipe goldens". Two different
jobs are bundled there; keep them apart:

- *Denoiser quality* (4090 bench, real views): LDR-FLIP mean on the 8-bit sRGB image, because that
  is the image the player sees and it is the metric the paper validated.
- *Correctness in CI* (lavapipe, 256×144): the existing MSE gate on the 64-spp golden. It is
  cheap, already proven, and lavapipe can afford nothing else (§3.6).

For the denoiser's actual number, do not guess: **derive it from the measured convergence curve.**
Whatever FLIP(64-spp raw, truth) comes out as, the denoised 1–2 spp image must beat it — i.e. the
denoiser's job is "make 2 spp look at least as good as 64 spp of reference". That is a
self-calibrating criterion, it survives changes to the metric's parameters, and it means #55 can
be written against this ticket's curve instead of a magic constant.

One policy note: the metric must be taken on the **raw accumulation**, never on the displayed
image during the ~150 ms Primary→Reference crossfade the map specifies — otherwise the metric
measures a blend, not the estimator.

### 3.5 Bench integration without touching the per-sample budget

The budget unit is "one reference sample": each `encode_reference` dispatch is its own profiler
frame and `gpu_total.reference` budgets one sample
(`bins/client/bench-budgets.toml`, `bins/client/src/bench/report.rs:30-37`). Nothing here needs to
change that. What is missing is a phase where the camera **stands still** long enough to converge.

Proposal: a third bench phase, `convergence`, after `primary` and `reference`:

- **Still camera** at the path's last keyframe (or a named view), `--convergence-spp N`
  (default 1 024), optional `--convergence-truth-spp 8192` and `--seed`.
- Samples are drawn exactly as today — one dispatch per sample, each its own profiler frame, so
  the `reference` pass entries and `gpu_total.reference` keep their current meaning and the 3.0 ms
  budget stays comparable across PRs. The convergence phase may *also* report per-sample stats,
  but it is not a new budget unit and does not change the reported reference numbers.
- **Checkpoints** at spp = 1, 2, 4, …, N. Because the running mean after n samples *is* the mean of
  the first n, one run yields the whole curve: read the buffer back at each checkpoint.
- **Metric on the CPU, outside every timer.** Readback and FLIP are CPU work, reported in the
  phase's `frame_time`, never inside a pass. This is already how the bench treats read-back
  (`bins/client/src/bench/mod.rs:307-325`).
- **Truth**: a second still run with an independent seed at `--convergence-truth-spp`. Then
  MSE(n) = σ²(1/n + 1/N_truth) instead of σ²(1/n − 1/N) — no shared samples, no downward bias,
  and the independent truth doubles as the noise-floor measurement (`FLIP(truthA, truthB)`).
- **Report**, schema bump (`SCHEMA_VERSION = 3`), under the reference mode:

  ```jsonc
  "convergence": {
    "view": "last-keyframe", "width": 2560, "height": 1440,
    "ppd": 67.02, "seed": 3030886828, "truth_spp": 8192, "truth_seed": …,
    "noise_floor": { "mse": 0.11, "flip": 0.014 },
    "checkpoints": [
      {"spp": 16, "gpu_ms": 70.4, "wall_ms": 88.1,
       "mse_linear": 0.0104, "rmse_srgb": 2.02, "psnr_srgb": 42.0,
       "flip_mean": 0.131, "flip_p99": 0.402}
    ],
    "threshold": {"flip_mean": 0.02, "flip_p99": 0.10},
    "spp_to_threshold": 1418, "gpu_seconds_to_threshold": 6.2
  }
  ```

- **Storage and comparison.** `report.json` (compare two runs by diffing the block) plus a
  `convergence.csv` of `spp, metric` — the PBRT precedent, which writes exactly `spp, mse` per
  render wave to `--mse-reference-out`
  ([`pbrt/cpu/integrators.cpp`](https://github.com/mmp/pbrt-v4/blob/master/src/pbrt/cpu/integrators.cpp)).
  Attach both to the PR; PRs touching the tracer get `needs-benchmark` as usual
  (`docs/agents/benchmarking.md`).
- **Reported time**: prefer `gpu_seconds` (sum of the per-sample pass times) over wall time for
  "time to threshold", so batching (#101) does not change the number. Report wall as well.
- **x-axis is samples, not frames.** Batching changes frames-per-sample, not the curve; plotting
  against samples keeps curve and policy independent, which is what lets #97/#101 claim a win.

One API change is needed and is small: **the accumulation buffer must be readable** — add
`COPY_SRC` to its usage (`crates/render/src/tracer.rs:171`) and a read-back that returns
`Vec<[f32;4]>`. That single change gives linear HDR values for MSE, un-clipped highlights, and
HDR-FLIP if we ever want it, at zero cost to any pass.

### 3.6 CI: why lavapipe cannot do this, and what to keep

lavapipe is Mesa's software Vulkan driver built on llvmpipe: "a software rasterizer that uses
LLVM to do runtime code generation", multithreaded across CPU cores
([Mesa llvmpipe docs](https://docs.mesa3d.org/drivers/llvmpipe.html)). CI deliberately sets
`LVP_POISON_MEMORY=true` for it
([wgpu docs/testing.md](https://github.com/gfx-rs/wgpu/blob/trunk/docs/testing.md)), i.e. CI is
already running GPU tests on a CPU.

The arithmetic is decisive. Today's reference golden is 256×144×64 = **2.4 M samples**. A single
1440p truth at 8192 spp is 2560×1440×8192 = **3.0e10 samples** — **~12 800× more work**, on a CPU
instead of a 4090. Even the *evaluation* run (1440p, 1024 spp) is ~1600× the golden's work. There
is no configuration in which that lands in a PR check. And the metric itself is not the problem:
FLIP at 256×144 costs ~6 ms (§3.1), so lavapipe *could* run FLIP — it cannot produce an image
worth running it on.

So CI keeps, unchanged:

1. **The 64-spp 256×144 reference golden**, MSE ≤ 1.0 + far-off-fraction
   (`crates/render/tests/common/mod.rs:38-56`). This is the driver-agreement and
   "the estimator did not change" gate.
2. **The analytic checks** in `crates/render/tests/path_tracer.rs`: a lit floor's closed-form
   radiance within 0.02 at 64 spp, a shadowed floor darker than half, emissive contribution
   bounded, 2+3+1 == 6 samples, camera/sun change resets. This is the cheapest possible
   *convergence* test, because the answer is known in closed form.
3. **The `--short` bench smoke test** (schema only, timings ignored).

Optionally add **one** new cheap proxy, and only if we want a CI-visible convergence claim: on
the analytic floor scene at 32×32, render 64 and 256 spp and assert both (a) the mean-linear
radiance is within a fixed tolerance of the closed form at both counts, and (b) the residual at
256 spp is materially smaller than at 64 spp (a loose 1/√N sanity check). That is ~80 k samples
per count, well within what lavapipe already spends on the goldens, and it tests the accumulator
and the estimator rather than an image.

Do **not** use a golden *hash* for rendered images: WGSL does not guarantee bit-identical results
across implementations ([§15.7.5](https://www.w3.org/TR/WGSL/#reassociation)), which is why the
repo compares with tolerances. Hashes stay where they already are, on CPU-side data
(`.github/workflows/ci.yml:61-84`).

---

## 4. Recommendation

**R1 — Metric (adopt).** Primary: **LDR-FLIP mean**, computed by
[`flip-rs`](https://crates.io/crates/flip-rs) (pure Rust, BSD-3, pixel-exact with C++ v1.7 on its
harness, ~0.6 s at 1440p single-threaded), at a **fixed ppd = 67.02** (the reference tool's
default, a 0.7 m 4K monitor at 0.7 m
([FLIP paper eq. 1](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf))),
always on the same 8-bit sRGB image the player sees. Secondary, same run: **linear MSE/RMSE from
the f32 accum buffer** (cheap, monotone, quantifies the 1/√N law) and **FLIP p99** as the
firefly/localised-error guard. Do not build a WGSL port; do not use `nv-flip` unless HDR-FLIP
turns out to be needed (it only has pre-v1.3 LDR).

**R2 — Truth (adopt, with one small API change).** Truth = a Reference render at **output
resolution**, same view, **independent seed**, **8192 spp** on the 4090 (≈36 s, estimate), stored
as 8-bit PNG for humans **plus the raw f32 accumulation** for arithmetic. Its own uncertainty is
measured, not assumed: `FLIP(truthA, truthB)` and `MSE(truthA, truthB)` are the metric's floor
and go into the report. Analytic scenes stay the CI truth because their answer is closed-form.
Never compare truths across machines or drivers.

**R3 — Thresholds (adopt, then calibrate on the 4090).** FLIP mean ≤ **0.02** = clean,
≤ 0.05 = acceptable, > 0.10 = fail; FLIP p99 ≤ **0.10**. On the 8-bit image, MSE ≤ **0.25 LSB²**
(RMSE 0.5, PSNR ≈ 54 dB) is the equivalent "clean" line, and MSE ≈ **0.083** (58.9 dB) is the
hard floor below which nothing is measurable. Each threshold must be at least 4× the measured
noise floor, or it is noise. For #55: the gate is "≤ the FLIP the raw reference scores at 64
spp", read off this curve — a derived number, not a guessed one.

**R4 — Bench (adopt).** A `convergence` phase: still camera, samples drawn one per dispatch with
timing exactly as today so `reference`/`gpu_total.reference` and the 3.0 ms budget stay
comparable, checkpoints at powers of two, metrics on the CPU outside every timer, truth from a
second seed at 8192 spp, and a `convergence` block in `report.json` (schema 3) plus
`convergence.csv` (PBRT's precedent). Report samples on the x-axis and `gpu_seconds` for time to
threshold.

**R5 — CI (keep, plus one optional proxy).** Unchanged lavapipe goldens + analytic checks + bench
smoke test. Optionally the 32×32 analytic 1/√N check. No FLIP in CI, no image hashes.

### Alternative, if FLIP turns out to be too much work

Ranked fallback: **linear MSE from the accum buffer alone**, threshold MSE ≤ 4× the measured
noise floor of the truth, reported alongside PSNR. This costs one afternoon (the read-back plus
arithmetic) instead of a FLIP dependency, is sign-blind (a scene 2× too bright scores as badly as
noise, which is exactly what we want for regressions), but it will not match human judgement on
dark or textured regions — and the FLIP paper's user study is the evidence for that (42
participants; FLIP averaged 2.1/3 and corresponded "significantly better" than Butteraugli, CNN,
HDR-VDP-2, LPIPS, PieAPP, S-CIELAB, SMAPE, SSIM and Euclidean RGB
([paper §6.2](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf))).

Rejected: SSIM (worse in that study and needs structure to work), Butteraugli (not validated on
renders, no maintained Rust crate), HDR-VDP-2 and LPIPS (heavy ML dependencies, ruled out by ADR
0001 / #5's spirit), "eyeball it" (not a gate).

---

## 5. Expected gain

No pixel gets better. What improves is the ability to make and defend decisions:

- **#101 (still-frame policy)** and **#97 (batching)** get a measured objective: "spp to FLIP ≤
  0.02" and "gpu_seconds to threshold", with batching judged by whether it moves the wall-clock
  number while leaving the GPU number alone.
- **#55** stops needing a magic constant; its gate falls out of the curve.
- **Regressions in the estimator** (a changed sky, a wrong albedo, a lost bounce path) appear as a
  displaced curve rather than as "it looks noisier".
- **The map's own promise** ("clean in a fraction of a second") becomes checkable: at the
  measured 1.10 ms/sample × 4 for 1440p (≈4.4 ms/sample, estimate), 0.5 s buys ~110 spp. Whether
  110 spp clears FLIP ≤ 0.02 is the single most useful number #99 can produce; if it does not, the
  map's target needs the decisions in #101/#102 to change, not the metric.
- **Cost**: ~6 ms per comparison at golden size and ~0.4–0.6 s at 1440p, on the CPU, outside every
  pass timer; zero CI cost.

## 6. Risks for a graphics-beginner team

1. **FLIP's ppd is a free parameter you must freeze.** Every stored number is invalidated by
   changing it. Pin 67.02 in the report and in the code, next to `RNG_SEED`.
2. **FLIP is not a truth oracle.** It over-reports under masking (busy textures) and it reacts to
   fireflies ([paper §6](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)).
   Read the error map when a number surprises you; the map is the point of the tool.
3. **Never compare FLIP across resolutions or ppd.** An MSE/PSNR comparison across resolutions is
   merely wrong; a FLIP comparison is wrong in a way that looks plausible.
4. **8-bit truths are lossy and clip.** Until the accum buffer is readable, everything above 1.0
   is invisible and all thresholds must sit above 0.083. Prefer the f32 read-back (one-word
   change) from the start.
5. **`pow(1/2.2)` ≠ sRGB.** Document it, or fix `encode()` and regenerate goldens; do not silently
   compare a gamma-2.2 image with a metric that assumes sRGB and then chase the 1 % offset.
6. **Per-machine truths.** A 4090 truth compared against a lavapipe image is a category error;
   keep truths in the bench output folder, with their inputs, and never in `tests/golden/`.
7. **Do not let the metric become the product.** A convergence phase that runs 8192 spp on every
   PR will be turned off within a month. Default it to 1024 spp, gate the deep truth behind a flag,
   and keep the PR-facing gate where it already is.
8. **Percentiles need care in `flip-rs`.** Its unweighted percentile index is out of bounds for
   small images at high `p` (documented in its README); use the weighted form, as wgpu does.
9. **The truth is the truth of the 4-bounce estimator.** If `MAX_BOUNCES` ever changes, every
   stored truth and every threshold moves. Record it in the truth's sidecar.

## 7. What this contradicts, or qualifies, in the map (#94)

- **"Clean in a fraction of a second."** At output resolution, 0.5 s is ~110 spp (§5). That is
  plausibly *visually* close but probably not FLIP ≤ 0.02, and each halving of the error costs 4×
  the samples. The metric is likely to show that the target needs either a lower (looser)
  threshold, a smaller reference resolution, or batching plus a longer still budget. Do not
  discover this after building the scheduling.
- **"Keep the per-sample reference budget (3.0 ms)."** That budget is defined at 1280×720
  *internal*. A reference at 2560×1440 is 4× the pixels, so ~4.4 ms/sample (measured-mean
  extrapolation). Keeping `reference = 3.0` comparable therefore requires a *separate* reported
  entry (for example `reference_native`) rather than reusing the same name at a different
  resolution — otherwise the two runs are not comparable, which is the opposite of the intent.
- **"MSE where it is cheap and stable."** MSE on the 8-bit output is cheap but *not* stable in the
  range that matters (the floor is 0.083, and highlights are clamped). Cheap and stable requires
  the f32 accum read-back.
- **"MSE threshold that CI enforces on lavapipe goldens" (#55).** CI's meaningful metric is the
  existing 64-spp MSE gate, not FLIP, and it is a determinism gate rather than a convergence one.
  FLIP lives on the 4090 bench. Both can exist; conflating them will produce a lavapipe threshold
  that is either trivially loose or flaky.
- **"Bit-reproducible."** True per machine and per driver, not across drivers: WGSL permits
  reassociation and FMA fusion ([§15.7.5](https://www.w3.org/TR/WGSL/#reassociation)). Reproducibility
  should be stated as "same binary + same driver", which is what the goldens already rely on.

## 8. What still needs a GPU (for #99, on the 4090)

1. The FLIP-vs-spp curve for two views (the golden view and one view with real inter-reflection —
   a crevasse, a terrace shadow, an interior), at 1440p, checkpoints 1…1024, truth 8192 spp.
2. The metric's own floor: `FLIP(truthA, truthB)` and `MSE(truthA, truthB)` at 8192 spp.
3. `spp_to_threshold` and `gpu_seconds_to_threshold` for FLIP ≤ 0.02 — the number that decides
   whether the map's "fraction of a second" is reachable.
4. `FLIP(64-spp raw, truth)` for the golden view, which becomes #55's gate.
5. The 1440p per-sample cost, to replace the ×4 extrapolation used throughout this document.

## 9. Sources

- Andersson, Nilsson, Akenine-Möller, Oskarsson, Åström, Fairchild. *FLIP: A Difference Evaluator
  for Alternating Images.* PACMCGIT 3(2), 2020.
  <https://research.nvidia.com/publication/2020-07_FLIP> ·
  <https://doi.org/10.1145/3406183> ·
  <https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf>
- Andersson, Nilsson, Shirley, Akenine-Möller. *Visualizing Errors in Rendered High Dynamic Range
  Images.* Eurographics Short Papers 2021.
  <https://research.nvidia.com/publication/2021-05_HDR-FLIP>
- NVlabs/flip (reference implementation, v1.7, BSD-3). <https://github.com/NVlabs/flip> ·
  precision note <https://github.com/NVlabs/flip/blob/main/misc/precision.md> ·
  C++ README <https://github.com/NVlabs/flip/blob/main/src/cpp/README.md>
- flip-rs (pure-Rust port, parity harness, timings). <https://crates.io/crates/flip-rs> ·
  <https://raw.githubusercontent.com/Tavrin/flip-rs/main/README.md>
- nv-flip (FFI to the C++ implementation, LDR only). <https://crates.io/crates/nv-flip>
- wgpu's FLIP-based image comparison and threshold guidance.
  <https://github.com/gfx-rs/wgpu/blob/trunk/tests/src/image.rs> ·
  <https://github.com/gfx-rs/wgpu/blob/trunk/docs/testing.md>
- Pharr, Jakob, Humphreys. *Physically Based Rendering*, 4th ed., §2.1.4 (O(n^-1/2), MSE = variance,
  sample-variance estimates). <https://pbr-book.org/4ed/Monte_Carlo_Integration/Monte_Carlo_Basics>
- pbrt-v4 `imgtool diff --metric MSE|MRSE|MAE|FLIP` and the `spp, mse` convergence log.
  <https://github.com/mmp/pbrt-v4/blob/master/src/pbrt/cmd/imgtool.cpp> ·
  <https://github.com/mmp/pbrt-v4/blob/master/src/pbrt/cpu/integrators.cpp>
- W3C, *A Standard Default Color Space for the Internet — sRGB* (transfer function, 8-bit
  encoding; the standard is IEC 61966-2-1). <https://www.w3.org/Graphics/Color/sRGB.html>
- W3C, *WebGPU Specification* (normalized formats clamp to the type's range; output merging).
  <https://www.w3.org/TR/webgpu/#output-merging>
- W3C, *WebGPU Shading Language* — §15.7.4 accuracy of built-ins, §15.7.5 reassociation and
  fusion. <https://www.w3.org/TR/WGSL/#floating-point-evaluation> ·
  <https://www.w3.org/TR/WGSL/#accuracy-of-built-in-functions>
- Mesa, *LLVMpipe* (software rasteriser used by lavapipe).
  <https://docs.mesa3d.org/drivers/llvmpipe.html>
