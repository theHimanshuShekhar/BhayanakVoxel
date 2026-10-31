# Reference scheduling: batching, budgets and adaptivity for the still-frame path tracer

Research for [#97](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/97), under the map
[#94](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/94). Feeds the decision ticket
[#101](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/101).

**Method.** Primary sources only: the papers, manuals and reference implementations cited at the
end, plus this repository's own code and its recorded bench numbers. The agent machine has no GPU,
so nothing here was measured on a 4090 in this session. Numbers are labelled:

- **[measured]** — recorded by the dev's 4090 in this repo (`bins/client/bench-budgets.toml`).
- **[derived]** — arithmetic on measured numbers and on this repo's code. Safe, but only as good
  as the linearity assumption, which §3.1 tests.
- **[estimate]** — my judgement, for #99/#101 to confirm or kill.

---

## 1. The question

How should the **Reference** path tracer spend a still frame on a 4090 at 1440p so that the image
becomes clean in a fraction of a second, without breaking frame pacing? Specifically: k samples
per pixel per dispatch and whether that cost is linear; the budget split inside a 16.7 ms frame and
the presentation rate; tile work queues versus one dispatch; per-pixel adaptive sampling and
stopping rules; hysteresis for "still"; tile-level invalidation after an **Edit**; and the expected
time-to-clean with and without adaptivity.

Out of scope, per the map: denoising, reprojection or clamping **inside** the reference; hardware
RT ([#50](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/50)); path guiding, ReSTIR and
radiance caching inside the reference.

---

## 2. What this engine does today

Everything below is from this checkout (`docs/research` is the only file this branch adds).

| Fact | Where |
| --- | --- |
| The reference adds **exactly one sample per pixel per frame**, in the window, one sample per presented frame. | `crates/render/src/window.rs:333`, `encode_reference` |
| One **full-screen compute dispatch**, `workgroup_size(8, 8)`, `width/8 × height/8` workgroups, one new bind group built per dispatch. | `crates/render/src/tracer.rs:750`, `:716` |
| One sample = up to **10 voxel traversals**: `trace()` for the camera/continuation ray plus one shadow `trace()` per bounce, over `MAX_BOUNCES = 4` (loop runs 5 times). | `crates/render/src/shaders/pathtrace.wgsl:18`, `:47-67` |
| Sub-pixel jitter is **drawn inside the kernel** from the per-sample RNG (`camera_ray(..., offset)`), not passed in `frame.jitter`. | `crates/render/src/shaders/pathtrace.wgsl:80` |
| The running mean lives in a `vec4<f32>` per pixel; `w` is written as `1.0` and **never read** — a free slot for a per-pixel sample count. | `crates/render/src/shaders/pathtrace.wgsl:85-90` |
| The mean is read and written **every sample** (16 B read + 16 B write + 4 B image store per pixel per sample). | same |
| Accumulation restarts on any change to `View = { camera, sun, size }`, compared by value. | `crates/render/src/tracer.rs:218`, `crates/render/src/accumulation.rs:34` |
| Any scene update (an **Edit**) resets the **whole** image. | `crates/render/src/tracer.rs:574` |
| Offscreen/bench batching today is k **submits**, each its own profiler frame and a full wait between them — not a batch on the GPU. | `crates/render/src/offscreen.rs:309-328` |
| The profiler times passes; it allows **16 timed passes per frame** and has a 3-deep readback ring, so some frames are untimed. | `crates/render/src/profiler.rs` (`MAX_PASSES`, `RING`) |
| Bench: `reference = 3.0 ms` per sample and `gpu_total.reference = 3.0`; first 4090 run **mean 1.10 ms, p95 2.69 ms** at 1280×720 internal (commit `bdbba3d`). | `bins/client/bench-budgets.toml:22`, `:47` and its comment |
| Uniforms are written once per sample because "each sample must be submitted before the next is encoded". | `crates/render/src/tracer.rs:534` |
| The window blits **one** of two images (upscaled Primary, or the reference) — there is no blend path yet. | `crates/render/src/window.rs:352` |

Two derived cost figures that everything below uses:

- **1.10 ms / 921 600 px = 1.19 ns per pixel per sample** at 1280×720. **[derived]**
- At 2.52 GHz over 16 384 lanes that is **≈ 49 000 lane-cycles per pixel per sample** — an upper
  bound on the work a sample may cost, useful as a sanity check when reading the split in §3.7.
  **[derived]** (card figures: 16 384 CUDA cores, 2.52 GHz boost, 24 GB —
  [NVIDIA RTX 4090](https://www.nvidia.com/en-us/geforce/graphics-cards/40-series/rtx-4090/)).

---

## 3. Findings

### 3.1 k samples per pixel per dispatch: does the cost scale linearly?

**Yes in k, to within a few per cent, provided the k samples run as a loop inside one lane.** The
non-linearity is all in the fixed cost you *remove*, not in the work you add.

The GPU model pbrt states is a throughput model: performance is "not just a consequence of the
computation performed by an individual thread, but also how often that same computation is
performed at the same time with other threads within the same group", and kernel launching is cheap
— "launching vast numbers of threads on the GPU is extremely efficient, so there is no need to
amortize this cost using a thread pool" (Pharr, Jakob & Humphreys, *Physically Based Rendering*,
4th ed., §15.1.1 and §15.2.2,
[pbr-book.org](https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Mapping_Path_Tracing_to_the_GPU)).
Two consequences for us:

1. **Fill is not the reason to batch.** A 1280×720 dispatch is 921 600 threads; at 1440p it is
   3.7 M. pbrt notes a GPU "may require tens of thousands" of threads to run at peak
   (same §15.1.1), so one dispatch already oversubscribes the machine by roughly two orders of
   magnitude. There is no occupancy argument for splitting one sample into more workgroups, and no
   serious tail argument either (the tail is one wave of an already-huge grid).
2. **What batching actually buys** is the removal of per-dispatch fixed costs: command encoding, a
   `create_bind_group` per dispatch (`tracer.rs:716`), a uniform write and a submit per sample, and
   the read-modify-write of `accum` per sample. At 1.10 ms per sample the launch side is single-digit
   microseconds — of the order of 1% — so a batch of k should cost `k × cost(1) × (1 − ε)` with
   **ε ≈ 1–5%** and ε shrinking as k grows. **[derived/estimate]**

The theory side agrees that batching is the standard way to spend a fixed budget well: pbrt's
"**splitting**" reduces variance by taking several samples of an inner integral per outer sample so
that "the cost of tracing the initial ray from the camera" is amortised
(§2.2.5, [Improving Efficiency](https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency)).

**Reference implementations.**

- **Falcor** (the closest analogue: one thread per pixel, whole path in one lane) runs
  `for (uint i = 0; i < spp; i++)` per pixel inside **one** pass, with `samplesPerPixel` as a
  shader define, one thread group per 16×16 screen tile, and a special-case path for `spp == 1`
  ([`GeneratePaths.cs.slang`](https://github.com/nvidiagameworks/falcor/blob/master/Source/RenderPasses/PathTracer/GeneratePaths.cs.slang),
  [`PathTracer.cpp`](https://github.com/nvidiagameworks/falcor/blob/master/Source/RenderPasses/PathTracer/PathTracer.cpp)).
  It launches the whole screen every pass ("Full screen dispatch") and gets its k from that loop.
- **pbrt-v4** deliberately runs **one** sample index per wavefront pass and splits the *image* into
  scanline ranges instead, because its queue-based wavefront must size per-sample buffers, and
  because it launches every kernel for every depth whether or not there is work: "All the kernels
  would exit immediately, though there would be a cost from launching all of them"
  ([§15.3.4](https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Path_Tracer_Implementation)).
  That cost model is pbrt's, not ours: our tracer is a per-lane megakernel with no inter-pass
  queues, so k-in-lane costs us nothing extra per sample.
- **Mitsuba 3** exposes the same concept as a parameter: `samples_per_pass` "divides the workload in
  successive passes with `samples_per_pass` samples per pixel"
  ([integrator docs](https://mitsuba.readthedocs.io/en/latest/src/generated/plugins_integrators.html)).
- **Cycles** renders `samples_num` samples per `render_samples()` call and lets its tile scheduler
  feed work across them
  ([`path_trace_work_gpu.cpp`](https://github.com/blender/blender/blob/main/intern/cycles/integrator/path_trace_work_gpu.cpp)).

**The one non-obvious engine constraint.** k samples inside **one submit** cannot be k dispatches
with k different `sample_index` values written by `Queue::write_buffer`: wgpu stages those writes
and applies them "just before the explicitly submitted commands", i.e. once per submit, so every
pass in the submit would see the last value written
([`Queue::write_buffer`](https://docs.rs/wgpu/latest/wgpu/struct.Queue.html#method.write_buffer)).
The two clean ways to batch are therefore:

- **(a) an in-lane loop** — one dispatch, uniform carries the batch's base index, the shader loops
  `i in 0..k` seeding `seed_rng(pixel, base + i, seed)`, keeping the mean in a register and writing
  `accum` once. This is Falcor's shape and the cheapest.
- **(b) k dispatches with immediate/push-constant base indices** — works (immediate data is recorded
  per dispatch), but still pays k accum round-trips and k profiler passes, and the profiler has only
  16 pass slots (`MAX_PASSES`), so it also costs the timings.

Recommendation (a). It also preserves bit-exactness: the loop performs the same f32 operations in
the same order as today's one-sample-per-submit path, so goldens do not move.

### 3.2 Occupancy and divergence in a voxel DDA kernel

**Occupancy is not the problem; divergence is, and batching neither helps nor hurts it.**

- **Occupancy.** Our kernel keeps a small live set (`Ray`, `Walk`, `Hit`, one RNG word, the running
  mean). Register pressure, not sample count, sets occupancy: pbrt's worked example shows 64
  registers/thread allowing 1024 threads per processor and 128 registers/thread only 512, and "the
  fewer threads that are running, the more difficult it is to hide memory latency"
  ([§15.1.1](https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Mapping_Path_Tracing_to_the_GPU)).
  A k-loop adds no new live state, so occupancy is unchanged by batching.
- **Divergence.** A thread group "continues executing until all of its threads have finished", and
  in the worst case divergence serialises "all the way down to the level of individual threads,
  resulting in a 32 times loss of performance" (same §15.1.1). In *this* kernel the divergent
  control flow is: `if !hit.hit { break }` at the top of the bounce loop, the `for (var i = 0; i <
  BLOCK_STEPS)` / `MAX_REGION_STEPS` DDA loops (`trace.wgsl:206`, `:241`, `:321`), and
  `if facing > 0.0` around the shadow ray (`pathtrace.wgsl:60`). Neighbouring pixels share primary
  rays and mostly share path length, so a warp of adjacent pixels is reasonably coherent; a warp
  that straddles the horizon is not. **The cost of a sample is set by the slowest lane in the
  warp**, so the *spread* of path length and traversal depth matters more than the mean.
- **Batching and coherence.** With the in-lane loop, all lanes still hold 64 neighbouring pixels of
  the **same sample index** at any instant (they advance through `i` together), which is exactly
  today's coherence. Fanning k across lanes instead (one thread per (pixel,sample)) would put 32
  threads on one pixel — excellent traversal coherence, but 32 lanes writing the same `accum`
  address, which is a serialised store or an atomic. **Batching must keep one lane per pixel.**
- **Voxel-specific.** `trace()` walks levels 0..N and, per level, regions then bricks then voxels
  with an incremental DDA (`trace.wgsl:321`, `:241`, `:206`), each step a handful of ALU ops plus
  sparse `regions`/`bricks` loads. Rays that miss the Planet exit on the bound-sphere test
  (`trace.wgsl:359-374`, `h2 < 0` → immediate miss) and are *cheap*; rays that graze it pay a full
  traversal to `t_exit` and are *dear*. Cost is therefore strongly view-dependent — which is what
  the measured p95 of 2.69 ms versus a mean of 1.10 ms already says (2.4× spread).

**Practical consequence:** a k-batch's duration is ≈ `k × (p95 sample time for this view)`, not
`k × mean`. Batch size must be chosen from the **p95**, and it must be able to change per view.

### 3.3 Budget: how much of a 16.7 ms frame, and at what presentation rate

**Findings.**

- A 16.7 ms frame at 60 Hz is the presentation cadence, not a useful GPU budget by itself: with
  `VK_PRESENT_MODE_FIFO_KHR` the presentation engine holds a queue and hands out one image per
  vertical blank, so a frame that overruns does not get "caught up" — it either queues (latency
  grows) or, with mailbox, is replaced by the newer one
  ([`VkPresentModeKHR`](https://registry.khronos.org/vulkan/specs/latest/man/html/VkPresentModeKHR.html)).
  Either way, **spending 25 ms on a frame buys nothing at 60 Hz**; it costs responsiveness.
- The repo already has a budget to fit into: `primary = 8.6 ms` total for the whole real-time path
  (#10's "about 9 ms of GPU work in a 16.7 ms frame", `bench-budgets.toml:45`). In **Reference**
  mode the primary pass and the upscale pass do not run at all, so the reference's budget is the
  *whole* frame minus the two passes it cannot avoid: `blit` (one triangle, full screen) and
  `overlay` (egui). Both are sub-millisecond; call them **≈ 1 ms** together, plus one frame of
  slack so a spike does not cost two frames. **[estimate]**
- **The binding constraint is the tail.** `[measured]` mean 1.10 ms, p95 2.69 ms per sample at
  1280×720. At 1440p (4× the pixels) that is **4.40 ms mean, 10.76 ms p95** per sample
  **[derived]**. A single 1440p sample in a worst-case view already eats two thirds of a frame.

Hence the budget split I recommend while still (all **[derived]** from the measured numbers):

| Slot | ms | Note |
| --- | --- | --- |
| reference pass (k samples) | up to **12** at p95 | controlled, see §4 |
| blit + overlay | 1.0 | unchanged from today |
| slack / present | 3.7 | absorbs a spike without a second missed vsync |
| **total** | 16.7 | |

and the presentation policy: **keep presenting at 60 Hz and vary k instead.** The reference image is
monotone in quality, so a missed vsync shows up as a stutter in an image that was going to be
replaced anyway; changing the presentation rate changes the *feel* of the whole window (mouse look,
overlay). If a view is so expensive that even k = 1 cannot make 16.7 ms, drop to k = 1 and let
presentation fall to 30 Hz for that view rather than dropping frames at random — that is a
per-view, measurable fallback, not the default.

*Don't* try to "bank" time across frames by allowing a 30 ms frame; FIFO will queue it and the
overlay and mouse look will lag by a frame, which is exactly the "1 spp mush for seconds" feeling
the map wants to avoid.

### 3.4 Tile work queues versus one full-screen dispatch

**Findings.**

- **pbrt-v4** uses work queues *between kernels* and, because a kernel launch cannot read the
  queue's size back without a sync, launches "a thread for the maximum number of items that could be
  stored in the queue, rather than only for the number that are actually stored there"; the excess
  threads "exit immediately" (§15.2.4,
  [Implementation Foundations](https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Implementation_Foundations)).
  So even the queue-based design degenerates to "launch for the worst case and early-out" unless the
  dispatch itself is indirect.
- **Falcor**, whose structure matches ours (one lane per pixel, whole path in the lane), does
  **one full-screen dispatch** and gets adaptivity from a **per-pixel sample-count texture** and a
  per-tile prefix sum for output offsets — no work queue, no indirect dispatch
  ([`GeneratePaths.cs.slang`](https://github.com/nvidiagameworks/falcor/blob/master/Source/RenderPasses/PathTracer/GeneratePaths.cs.slang)).
- **Cycles** *does* queue tiles, and says why: it caps the number of path states per tile so that
  "tiles are smaller, so there is higher chance that more paths will become busy after adding new
  tiles"
  ([`path_trace_work_gpu.cpp`](https://github.com/blender/blender/blob/main/intern/cycles/integrator/path_trace_work_gpu.cpp)).
  That reason is a *path-state occupancy* problem in a GPU integrator that keeps a fixed pool of
  path states alive across bounces; our lane owns its path for its whole life, so the problem does
  not exist.
- Indirect dispatch is available if we ever want it: `dispatch_workgroups_indirect` in wgpu
  ([docs.rs](https://docs.rs/wgpu/latest/wgpu/struct.ComputePass.html#method.dispatch_workgroups_indirect))
  over `VkDispatchIndirectCommand`
  ([Vulkan](https://registry.khronos.org/vulkan/specs/latest/man/html/vkCmdDispatchIndirect.html)).

**What adaptive per-pixel counts need, concretely:**

1. A **count per pixel or per tile** that survives between frames (a small storage buffer; or
   `accum.w`, which is written every sample today and never read — see §2).
2. A **stop test** evaluated on the GPU (so the CPU never reads back mid-convergence).
3. A **presentable mean** that is valid for pixels with different counts — i.e. the mean formula
   `mean + (estimate − mean) / (n + 1)` must use each pixel's own `n`. Today `n` is a uniform.
4. A way to **invalidate a region** (an Edit) without resetting the rest — §3.6.

None of these require a work queue. A tile that has converged is discovered *by* the tile's own
threads, so the cheapest expression of "stop" is: the workgroup reads the tile's state, and if
converged, `return`s. The cost of the skipped tiles is one branch and one small buffer read per
workgroup (57 600 workgroups at 1440p with an 8×8 tile) — well under 1% of a batch. **So: one
full-screen dispatch, no work queue, until a measurement shows otherwise.**

The 8×8 `workgroup_size` is convenient here: **the workgroup is the tile**, so the tile's error can
be reduced in workgroup shared memory (64 f32 = 256 B) with no global atomics and no cross-group
sync, and the tile's state is read once per dispatch.

### 3.5 Adaptive sampling: error estimation, allocation, stopping, determinism

**Findings from production code.** Cycles is the best-documented case and its choices are directly
copyable:

- The convergence test compares the current pixel estimate against the *previous filtered* estimate
  and normalises by intensity: `error = |I − A| / (ε + normalize(I))`, with a threshold, and it
  records a per-pixel "converged" flag
  ([`kernel/film/adaptive_sampling.h`](https://github.com/blender/blender/blob/main/intern/cycles/kernel/film/adaptive_sampling.h)).
  The citation in that code is the hierarchical stopping-condition paper; the shape — *difference
  between successive batch estimates, intensity-normalised* — is the practical estimator.
- It checks **only every 16 samples** (`adaptive_step = 16`, asserted power of two) and never before
  `min_samples`
  ([`scene/integrator.cpp`](https://github.com/blender/blender/blob/main/intern/cycles/scene/integrator.cpp),
  [`integrator/adaptive_sampling.cpp`](https://github.com/blender/blender/blob/main/intern/cycles/integrator/adaptive_sampling.cpp)).
- Defaults: adaptive sampling on, noise threshold `0.01`, min samples `0` meaning *automatic* —
  "Threshold 0.1 → 32, 0.01 → 64, 0.001 → 128", floored at 4 (same file). **A production renderer
  with a good sampler does not start stopping before ~64 spp at its default threshold.**
- It **dilates the decision**: a two-pass box filter pulls neighbouring pixels that need more
  samples back into sampling, "when a pixel demands more adaptive samples, let its neighboring
  pixels draw more samples too" (same `adaptive_sampling.h`). That is exactly the mitigation for
  the warp-granular problem of §3.2.

**Allocation strategies.** Two families, and the difference matters for us
(Zwicker et al., "Recent Advances in Adaptive Sampling and Reconstruction for Monte Carlo
Rendering", *CGF* 34(2):667–681, 2015,
[PDF](https://www.cs.umd.edu/~zwicker/publications/RecentAdvancesSTAR-CGF15.pdf)): *a priori* (derive
rates from the light transport) and *a posteriori* (estimate error from samples taken). The big
measured gains in that survey come from **reconstruction** (filtering) on top of adaptive sampling —
which the map forbids inside the reference. Pure adaptive *sampling* keeps the estimator and just
redistributes work; its gain is bounded by the spread of per-pixel difficulty.

**Stopping rules and bias — the important caveat.** A stopping rule that looks at the samples it is
about to report makes the reported mean a *biased* estimator of the true value: this is the classic
optional-stopping result for sequentially stopped samples (Anscombe, "Sequential Estimation",
*JRSS-B* 15(1):1–21, 1953,
[doi](https://doi.org/10.1111/j.2517-6161.1953.tb00121.x)). The bias is not a bug in the
implementation and it does not go away with more care; it is inherent in "decide how many samples
to take by looking at the samples". It is small when the stopping threshold is small relative to the
noise at the point you stop, and it is unbounded in principle if a pixel is allowed to stop after
very few samples. **Two rules follow:**

- **never stop before a floor** (Cycles' `min_samples`; 16–32 for us, see §4);
- **the yardstick render is fixed-spp.** Adaptivity is a *display* policy; the golden image, the
  bench capture and any FLIP/MSE ground truth render at a fixed sample count. This resolves the
  map's "the Reference stays unbiased" with adaptivity in the room at all: the adaptive image is an
  interactive preview of the same estimator, not the estimator the metric is defined against.

**Determinism.** Kept, and cheaply:

- The RNG is already seeded from `(pixel, sample index, seed)` (`sampling.wgsl:15`), so as long as
  each pixel takes a **prefix** of sample indices 0,1,2,… the image is a pure function of the
  counts.
- The counts are a pure function of the samples taken *in index order*, so a given (view, seed,
  stopping rule) gives one image, whatever the scheduling. Cycles relies on the same property.
- The one place non-determinism can creep in is a **reduction order** in the tile error test
  (GPU `countOneBits`-style or wave sums whose order is unspecified). Fix it with a fixed
  shared-memory tree reduction in the workgroup; do not use atomics for the error.
- **Floating point:** the running mean in `f32` is fine to the thousands of samples we care about
  (`mean + (x − mean)/(n+1)` keeps relative precision; a running *sum* would not).

**What it does to a comparison metric.** With unequal per-pixel counts, "spp" stops being a single
number, so a convergence report must either (a) run at fixed spp, or (b) report the count
distribution (min/median/max or a histogram) next to the error. Metrics like FLIP are defined per
image, not per sample, so unequal counts are legal once the counts are reported — but the *error*
of a mixed image is dominated by its worst regions, which is exactly why adaptivity helps a
whole-image threshold and barely helps the eye (§3.8).

**Known failure mode.** Adaptive sampling can *miss* light it never samples: the multidimensional
adaptive sampler's published failure case is "all images are rendered using 64 samples per pixel …
Our method may miss illumination coming through one of the gaps" (Hachisuka, Jarosz et al.,
"Multidimensional Adaptive Sampling and Reconstruction for Ray Tracing", *TOG* 27(3), 2008,
[PDF](https://cs.dartmouth.edu/~wjarosz/publications/hachisuka08multidimensional.pdf)). In a voxel
world that is caves, crevices, and thin gaps between Debris — precisely the places a still frame is
looked at. A minimum-sample floor plus tile-granular (not per-pixel) stopping is the defence.

### 3.6 Defining "still": what counts, hysteresis, and the 150 ms crossfade

**What counts as a change.** Today's reset key is `{ camera, sun, size }` by exact value
(`tracer.rs:218`), which is the right *contents* but the wrong *policy*:

| Change | Should it reset accumulation? | Why |
| --- | --- | --- |
| Camera transform (position or orientation) | **Yes, immediately** | Every sample is of a different image. This is the dominant case; the Zoom means it happens for seconds at a time. |
| Sun direction or angular radius | **Yes** | Lighting changed; the mean is of a different integrand. The sun is currently only moved by arrow keys (`bins/client/src/main.rs`, `turn_sun`), so it is a deliberate act, not a stream. |
| Size / resolution / debug view | **Yes** | Different image / different estimator. |
| **Edit** (dig or place) | **Yes** for now (§3.6.2) | Geometry changed near the edit; the rest of the image is still valid in principle. |
| Fluid settling, Debris moving, player avatar | **Not in M1 (nothing animates yet); yes when they land** | Any moving geometry inside the view makes "converged" meaningless. Decide with #101 when M2/M3 arrive. |
| Overlay open/closed, window focus, mouse over the overlay | **No** | None of these change the rendered image (the overlay is drawn over it). |

**Hysteresis.** Two asymmetries, both deliberate:

- **Enter still slowly, leave instantly.** Accumulate only after the view has been unchanged for
  **2 consecutive frames (≈ 33 ms)**. Leave on the first changed frame, cut to Primary with **no
  blending** (the map's decision). The delay on entry costs one sample and prevents a one-frame
  hitch or a stray input event from starting a batch that is thrown away; the immediate exit keeps
  movement looking like Primary, always.
- **A jitter dead zone.** Reset on any change above a tiny epsilon (say 1 mm of position, 0.01° of
  rotation — below what a still hand produces, above float noise) so a resting mouse converges.
  Without one, a mouse at rest with any tremor never converges, and it is not obvious to a beginner
  why.

**The ~150 ms crossfade.** The map has already decided: cut to Primary on movement, reset to sample
0 when still, crossfade Primary → reference over ~150 ms, Primary display-only and never sample 0.
Under batching, three details need pinning:

- The first presentable reference image is the **first batch** (k samples), not sample 0. Start the
  fade at the frame the first batch lands, so it covers spp ≈ 2–10 at 60 Hz.
- The fade is a **lerp in the blit fragment shader** between the last Primary image (its upscaled
  output, already at output resolution) and the reference mean. It touches the display only; the
  accumulator is untouched, so the estimator stays unbiased and the fade can be interrupted at any
  point without corrupting anything. This is a design decision; there is no literature needed for
  a display-space lerp, and it must never be fed back into `accum`.
- It requires the reference to be at **output resolution** (otherwise the two images differ in size
  and the lerp needs an upscale — see §7).

### 3.7 Where a sample's cost goes, and how to measure it with the timers we have

**Structure of the cost, from the code** (`pathtrace.wgsl:47-67`): per bounce, one `trace()` for the
ray and one `trace()` for the shadow ray; loop runs at most 5 times, so **≤ 10 traversals per
sample**, and the path ends early when a ray escapes (sky) or the bounce budget runs out. A first-
order model of one sample:

```
cost ≈ N(camera ray) + Σ_{bounces hit} [ N(continuation ray) + N(shadow ray) ]
     + arrival/escape sky lookups  (cheap: a mix of two constants)
```

with `N(...)` dominated by the number of DDA steps, i.e. by distance and by how many levels the ray
crosses. Expected split for this scene **[estimate, to be measured]**: the camera ray is one of up
to ten traversals, so ≈ 10–35%; the **shadow rays are the largest single block** because they are
roughly one per bounce and are long (they go towards the sun, through the whole terrain above the
hit); escape/sky samples are bimodal — a ray that misses the Planet's bounding sphere exits on a
quadratic test and is nearly free (`trace.wgsl:359-374`), while a ray that grazes the limb walks to
the far side of the ball and is one of the most expensive rays in the frame.

**How to measure it without new instrumentation** — this is #99's step 3, and the pass timers
already give everything needed:

1. **Baseline.** `client bench` reports `reference` and `gpu_total.reference` per sample. That is
   the denominator. `[measured: 1.10 / 2.69 ms]`
2. **A temporary knob, not merged.** Add two uniforms to the reference pass — `max_bounces ∈
   0..=4` and `shadow_rays ∈ {0,1}` — read from an env var in the bench binary, then run the
   matrix. Each configuration is still one timed pass per sample, so the existing report and budgets
   work unchanged; the knob must not survive the measurement (this is #99's rule).
   - `bounces 0, shadows 0` = camera ray + sky only → **cost of one traversal + escape**.
   - `bounces 0, shadows 1` = camera ray + one shadow ray → difference is **NEE**.
   - `bounces 1..4` = slope per bounce → **continuation + shadow** per bounce.
3. **Compare against `primary`.** One ray per pixel, no NEE, no bounces: its measured pass time
   (`primary = 1.0` budget today) is a free cross-check of "one traversal" from a different code
   path. The `probe` kernel (`tracer.rs:754`) does single rays if you want a specific ray.
4. **Count, don't guess.** A temporary `atomicAdd` on a storage counter of `trace()` calls and of
   DDA steps, read back **once at the end of the run** (never per frame — that would stall), gives
   the mean steps per ray directly. Divide the pass time by it to get cycles per step.
5. **Report the view.** All of the above per camera view, not per run: the 2.4× mean→p95 spread is a
   *view* effect and is the number that sets k.

The profiler limits and their consequences, for whoever builds the controller in #101: 16 timed
passes per frame (`MAX_PASSES`), a 3-deep readback ring, and "the frame that finds the whole ring in
use is not timed" (`profiler.rs`). **A controller fed by these timers must tolerate missing samples
and must not react to a single frame.** Use the last N (=16) *timed* batches, take p95 of the
per-sample time, and hold k when no timing arrived.

### 3.8 Time to clean at 1440p, with and without adaptivity

All figures below are **[derived]** from 1.10 ms/sample at 921 600 px (`[measured]`) under the
linear-in-pixels assumption of §3.1, so **4.40 ms per sample at 2560×1440**.

**Without adaptivity, fixed k per frame while still, 60 Hz presentation:**

| k | GPU ms (mean / p95) | samples/s | 16 spp | 32 spp | 64 spp | 128 spp | 256 spp |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 4.40 / 10.76 | 60 | 0.27 s | 0.53 s | 1.07 s | 2.13 s | 4.27 s |
| **2** | 8.80 / 21.5 | **120** | **0.13 s** | **0.27 s** | **0.53 s** | 1.07 s | 2.13 s |
| 3 | 13.2 / 32.3 | 180 | 0.09 s | 0.18 s | 0.36 s | 0.71 s | 1.42 s |

Read the p95 column before the mean: at 1440p, **k ≥ 2 does not hold 60 Hz in the worst measured
view**, and k = 3 does not even hold it on the mean plus slack. This is the single most important
number in this document.

**For comparison, today's internal resolution (1280×720, 1.10 ms/sample):** k = 8 fits in 8.8 ms and
delivers **480 samples/s** — 16 spp in 33 ms, 64 spp in **0.13 s**, 256 spp in 0.53 s. That is 4×
better than 1440p for the same GPU time. The map's decision to move the reference to output
resolution therefore **costs exactly a factor of four in time-to-clean**, and it moves the per-sample
cost above the recorded `reference = 3.0 ms` budget (see §7).

**With adaptivity.** Let N be the sample count at which the last tile passes the threshold, and let
`n(p)` be the count a pixel (tile) actually takes. The saving is

```
G = N / mean(n)          cost_adaptive = cost_fixed / G
```

`G` is 1 exactly when every pixel is equally hard, and grows with the spread. Worked cases:

| Stop distribution | mean(n) | G |
| --- | --- | --- |
| all tiles reach 64 | 64 | 1.00 |
| 40% at 16, 60% at 64 | 44.8 | 1.43 |
| 30% at 16, 50% at 32, 20% at 128 | 46.4 | 1.38 (against N = 64) |
| 50% at 16, 30% at 32, 20% at 256 | 68.8 | 3.7 (against N = 256) |

Two things follow.

- **Adaptivity compresses the tail, not the head.** The time until *most* of the image is clean is set
  by the early samples, which every pixel takes — adaptivity cannot shorten that. What it shortens is
  the time until the **last** noisy tiles are done, i.e. the time to a *whole-image* threshold. If
  "clean" means "looks clean to me at 32–64 spp", expect **1.2–1.5×** **[estimate]**. If "clean"
  means "every tile below τ", expect **2–4×** **[estimate]**, because this scene's difficulty is
  very uneven (deterministic sky and flat lit terrain versus soft-shadow penumbrae, crevices, and
  the grazing-limb rays of §3.7).
- **Tile granularity eats part of it.** Stopping a whole 8×8 workgroup (the only granularity a warp
  can actually skip work at, §3.2) loses the saving for tiles that straddle a silhouette. Discount
  `G` by roughly 20–40% **[estimate]**: `G_effective ≈ 0.6–0.8 × G`.

So, honest headline for #101: **time-to-clean at 1440p is ≈ 0.5 s for a 64 spp target at k = 2, and
adaptivity takes perhaps 0.1–0.2 s off that** — it is the *third* most valuable item here, after
(a) batching, which is what turns the frame from 70% idle into 2–3× the samples, and (b) a better
sample sequence (#95), which changes the constant inside every one of these numbers.

**What "clean" is, is still unmeasured.** Nothing here says 32 or 64 spp is clean for this scene;
that is #99's job (convergence curve against a 1024-spp capture). This document's claim is only the
*shape*: samples/s = 60·k at 1440p, and every doubling of spp costs a doubling of time.

---

## 4. Recommendation, ranked

**R1. Batch k samples per pixel in an in-lane loop, one dispatch per frame, k chosen per frame from
a measured p95.** This is the whole still-frame policy in one sentence. Falcor's shape
(`GeneratePaths.cs.slang`) on our kernel: `frame.params.x` becomes the batch's **base** index, the
kernel loops `i in 0..k`, seeds `seed_rng(pixel, base + i, seed)`, keeps the running mean in a
register, writes `accum` once at the end of the loop. Consequence: one bind group, one uniform
write, one submit, one traversal setup per pixel per batch instead of per sample.

- **k = clamp(floor(budget_p95 / per_sample_p95), 1, 3)** with `budget_p95 = 12 ms`.
- Expect **k = 2 at 1440p** (8.8 ms mean, 21.5 ms p95 — over on the worst view, hence the feedback)
  and **k = 8 at 1280×720** if the reference stays at the internal resolution.
- Feedback: `per_sample_p95` from the last 16 *timed* batches, held when the ring gives no timing.
  Raise k by one at most every 4 clean frames; drop to 1 the moment a batch overruns. **Never exceed
  a hard cap** — a single dispatch must stay far under the Windows 2-second TDR timeout
  ([Microsoft TDR](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery)),
  and the first frame of a new view always uses k = 1 (unknown cost).

**R2. Keep 60 Hz presentation; never trade refresh rate for samples by default.** Present every
frame; the blit costs almost nothing. Use `AutoVsync`/mailbox where available so a long batch
replaces a stale image instead of queueing latency
([Vulkan present modes](https://registry.khronos.org/vulkan/specs/latest/man/html/VkPresentModeKHR.html)).

**R3. Keep the reference at 1280×720 internal until #99 has measured both.** The move to output
resolution costs 4× the time-to-clean and pushes one 1440p sample (4.40 ms mean, 10.76 ms p95) above
the recorded 3.0 ms per-sample budget. Do not spend that without a number that says it is worth it.
If output resolution is kept, re-express the budget as **ms per megapixel per sample** (3.0 ms /
0.9216 Mpx ≈ **3.26 ms/Mpx**) so reports stay comparable — see §7.

**R4. Define "still" as: camera, sun and size bit-identical, no Edit, and nothing animating, for 2
consecutive frames.** Cut to Primary on the first change with no blend; epsilon dead zone for
resting input; start the 150 ms display-only crossfade at the frame the first batch lands. Show
`min/median/max spp` in the overlay, not a single number, once counts can differ.

**R5. Add tile-granular adaptive stopping — but only after R1, and built on the tile, not the
pixel.** Tile = one 8×8 workgroup; state = per-tile `(u32 samples, f32 error)` in one small storage
buffer (57 600 tiles at 1440p ≈ 460 KB); test every **16** samples and never before **min 16–32**;
test = intensity-normalised difference between the batch mean and the previous batch mean (Cycles'
shape), reduced with a fixed shared-memory tree; stop the tile when its max error is under τ;
dilate the stop across neighbouring tiles so a noisy tile cannot starve. Keep adaptivity out of the
**yardstick** render (fixed spp) and out of the goldens.

**R6. No tile work queue, no indirect dispatch, at M1.** One full-screen dispatch; converged tiles
`return` immediately. Revisit only if a measurement shows the empty tiles costing more than a few
per cent of a batch.

**R7. Keep the whole-image reset on Edit for now, and do not build screen-space invalidation yet.**
Two reasons, both about value: an Edit happens while the player is playing — i.e. almost always while
the camera is moving and the reference is not accumulating at all; and the reset costs only the
time-to-clean from R1 (≈ 0.5 s), which is the same order as the time the player takes to look at the
crater. The state R5 needs (per-tile counts) is also the state invalidation needs, so **build R5,
then decide**: `n = 0` on the projected tiles is then a small addition. If it is built, the dirty
region is the edited bricks' screen-space AABB dilated by the sun-cone shadow of the edit (a
penumbra of 2·0.04 rad ≈ 4.6° at the surface, `sun.rs:7`) — the *indirect* light change is
unbounded in principle and small in practice, which is exactly why this is a "later, with a
measurement" item.

**R8. Measure before and after, in the bench, not by eye.** Keep the bench at k = 1 so the
per-sample budget stays the unit (#94's call); add a still-frame measurement that reports
spp → ms and spp → FLIP/MSE (#98/#102 own the metric). The still-frame policy is a window policy.

**The policy in pseudocode** (`Renderer::draw`, Reference mode):

```
on_change(camera, sun, size, scene, edits, animation):
    dirty = true

each frame in Reference mode:
    if dirty or settle < 2:      # 2 frames ≈ 33 ms
        present Primary; cut, no blend
        settle = changed ? 0 : settle + 1
        continue

    k = batch_size()              # R1, from the last timed batches
    dispatch_reference(base = tile_counts, k)     # R1 loop, R5 early-out
    present fade(primary_image, reference_mean, min(1, t_since_first_batch / 150 ms))
```

and the budget split as in §3.3: **≤ 12 ms reference (p95), 1 ms blit+overlay, 3.7 ms slack.**

---

## 5. Expected gain

| Change | Gain | Confidence |
| --- | --- | --- |
| In-lane k batching, k = 2 at 1440p | **2× samples/s** for the same frame rate; today the still frame idles ~70% of its 16.7 ms after one 4.4 ms sample | high [derived] |
| k = 8 at 1280×720 internal | **8×** samples/s over today's 1 sample/frame | high [derived] |
| Reference at 1280×720 instead of 1440p | **4×** time-to-clean improvement | high [derived] |
| Adaptive tile stopping | **1.2–1.5×** for an eye-clean image; **2–4×** for a whole-image threshold; 20–40% of that lost to tile granularity | medium [estimate] |
| Edit tile invalidation | **≈ 0** at M1 (Edits coincide with movement; a reset costs ~0.5 s) | medium [estimate] |
| Sample sequence (#95) | not this ticket, but it moves the constant in *every* row above | — |

---

## 6. Risks for a graphics-beginner team

1. **The p95, not the mean, is the budget.** The repo's own numbers are mean 1.10 / p95 2.69 ms. A
   batch is atomic: its duration is ≈ k × the *p95* sample for that view. Choosing k from the mean
   gives stutter that only shows up in some views — and that will read as "the tracer is slow"
   rather than "k is too big".
2. **A timer-driven controller oscillates, and the timer sometimes isn't there.** The profiler keeps
   16 pass slots, a 3-deep ring, and reports "the frame that finds the whole ring in use is not
   timed". A naive `k = budget / last_frame_time` will ping-pong. Use p95 over many timed batches,
   hold on missing data, rise slowly, fall instantly.
3. **Divergence makes "per-pixel adaptivity" a lie.** Savings only appear where a whole warp (and in
   practice a whole workgroup) is finished. Per-pixel stopping without dilation and without
   tile-granularity buys ~nothing and adds a buffer. Cycles' neighbour-dilation filter exists for
   exactly this reason.
4. **Adaptive stopping bends the estimator.** The optional-stopping bias is real, small if the floor
   is respected, and unacceptable in the yardstick. Keep fixed-spp for goldens/bench/ground truth and
   say so in the code.
5. **Adaptive sampling can miss light** (Hachisuka et al.'s own failure case, at 64 spp, through a
   gap). In a voxel world, caves and Debris gaps are the interesting places. Floor the samples; stop
   by tile.
6. **Off-by-one in the mean.** Today's formula treats `sample_index == 0` specially (replace, not
   blend). In the loop that becomes `if base == 0 { mean = est }` and `mean += (est − mean)/(base +
   i + 1)`; get it wrong and the first batch is wrong by 1/n — invisible in a still, invisible in
   nothing else, so it will survive review. Test it: 2 batches of 3 must equal 6 single samples
   bit-for-bit.
7. **One dispatch must stay under the TDR timeout.** Two seconds on Windows. A per-view k with a
   hard cap and k = 1 on the first frame of a new view keeps this a non-issue; a "just set k = 64"
   experiment on a slow driver does not.
8. **`accum.w` looks free but is part of the buffer's meaning.** It is written as 1.0 today and never
   read. Putting a per-pixel count there is a real change to a documented layout
   (`pathtrace.wgsl:83-90`); if a per-tile count is used instead (§4 R5), leave `accum.w` alone.
9. **Don't average images.** The crossfade is display-only. The moment a blended image is written
   into `accum`, the reference stops being ground truth and the map's whole purpose is lost.
10. **lavapipe/CI.** Goldens run at fixed spp on a software rasteriser. Any batching or adaptivity
    must be inert when `samples` is fixed, or CI time and golden hashes move for no reason.

---

## 7. Where this contradicts the map's assumptions

1. **"Output resolution for Reference" costs 4× and breaks the per-sample budget.** At 1440p one
   sample is **4.40 ms mean, 10.76 ms p95** [derived], against `reference = 3.0` ms
   [measured at 720p]. Either the reference stays at the internal resolution, or the budget unit
   becomes **ms per megapixel per sample** (≈ 3.26). The map's two decisions —
   "keep `reference = 3.0` so reports stay comparable" and "Reference at output resolution" — cannot
   both hold once the reference actually traces 1440p.
2. **"The Reference stays unbiased" and "per-pixel adaptive sampling" are in tension.** A
   data-dependent stopping rule biases the mean (Anscombe 1953). The resolution is that adaptivity
   is a display/preview policy and the yardstick render is fixed-spp; the map should say so
   explicitly rather than claiming one adaptive reference is both.
3. **"Reset to sample 0 and crossfade" needs a definition under batching.** The first presentable
   reference image is the first *batch* (2–3 spp), not sample 0; the fade's start and the
   accumulator's base index must be defined together, or the fade will blend Primary with a
   1-spp image.
4. **"Tile invalidation after Edits" is a low-value question at M1.** Edits are made while moving;
   the reference is not accumulating then; and a full reset costs ~0.5 s of a still frame. Defer it,
   and build the per-tile state for adaptivity instead — that is the same state.
5. **The still-frame policy is not "samples per frame" tuning; it is a controller plus a fallback.**
   The map's framing assumes a constant can be picked. The measured mean→p95 spread makes k
   view-dependent: at 1440p only **k = 1** holds the measured worst view's p95 (10.76 ms of 16.7,
   leaving 6 ms of slack), and every heavier view than the measured worst will overrun it. #101
   should decide the fallback for that case (k is already 1: lower the present rate, or lower the
   resolution) and record it, rather than leaving it to the code to discover at run time.

---

## 8. What is still unmeasured (#99's list, sharpened)

1. **Linearity in k.** Run a still view at k = 1, 2, 4, 8 in one dispatch (R1's loop) and check
   `time(k) / k`. Predicted ε = 1–5%. If ε is larger than ~10%, something else is wrong (register
   spills, clock throttling, or the bench's warmup being too short).
2. **The per-view p95.** The 2.69 ms p95 is across a flight, not within a view. A still-frame policy
   needs *within-view* p95, per view.
3. **The convergence curve** (spp → FLIP/MSE against 1024 spp) at 1440p, so the 16/32/64/128 table in
   §3.8 can be turned into a time-to-clean claim.
4. **The ray-type split** of §3.7 — camera / shadow / bounce / escape — and whether the grazing-limb
   rays really are the expensive ones.
5. **The stopped-count histogram** (`h(n)` in §3.8) for a candidate τ, which is the only way to turn
   `G` from an estimate into a number.

---

## 9. Sources

**Books and papers**

1. Pharr, M., Jakob, W., Humphreys, G. *Physically Based Rendering: From Theory to Implementation*,
   4th ed., MIT Press, 2023. §15.1.1 (thread groups, divergence, registers), §15.1.2 (megakernel vs
   wavefront), §15.2.2 (kernel launches), §15.2.4 (work queues, launch-for-worst-case):
   <https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Mapping_Path_Tracing_to_the_GPU>,
   <https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Implementation_Foundations>
2. Same, §2.2 "Improving Efficiency" (error vs samples), §2.2.4 (Russian roulette never reduces
   variance), §2.2.5 (splitting amortises the camera ray):
   <https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency>
3. Same, §15.3.4 (one sample index per pass; every kernel launched whether it has work):
   <https://pbr-book.org/4ed/Wavefront_Rendering_on_GPUs/Path_Tracer_Implementation>
4. Zwicker, M., Jarosz, W., Lehtinen, J., Moon, B., Ramamoorthi, R., Rousselle, F., Sen, P., Soler,
   C., Yoon, S.-E. "Recent Advances in Adaptive Sampling and Reconstruction for Monte Carlo
   Rendering." *Computer Graphics Forum* 34(2):667–681, 2015.
   <https://doi.org/10.1111/cgf.12592> · <https://www.cs.umd.edu/~zwicker/publications/RecentAdvancesSTAR-CGF15.pdf>
5. Hachisuka, T., Jarosz, W., Weistroffer, R. P., Dale, K., Humphreys, G., Zwicker, M., Jensen, H.
   W. "Multidimensional Adaptive Sampling and Reconstruction for Ray Tracing." *ACM TOG* 27(3),
   SIGGRAPH 2008 (includes the 64-spp failure case where adaptive sampling misses light through a
   gap). <https://cs.dartmouth.edu/~wjarosz/publications/hachisuka08multidimensional.pdf>
6. Anscombe, F. J. "Sequential Estimation." *Journal of the Royal Statistical Society B* 15(1):1–21,
   1953 (bias of a mean whose sample size was chosen from the data).
   <https://doi.org/10.1111/j.2517-6161.1953.tb00121.x>
7. Welford, B. P. "Note on a Method for Calculating Corrected Sums of Squares and Products."
   *Technometrics* 4(3):419–420, 1962 (numerically stable running variance, if a variance buffer is
   added). <https://doi.org/10.1080/00401706.1962.10490022>

**Reference implementations**

8. Blender/Cycles, adaptive sampling kernel (per-pixel convergence test, intensity normalisation,
   neighbour dilation filter):
   <https://github.com/blender/blender/blob/main/intern/cycles/kernel/film/adaptive_sampling.h>
9. Blender/Cycles, `AdaptiveSampling` (sample alignment; `adaptive_step`, `min_samples`):
   <https://github.com/blender/blender/blob/main/intern/cycles/integrator/adaptive_sampling.cpp>
10. Blender/Cycles, integrator settings: adaptive threshold default 0.01, automatic min samples
    (`0.1 → 32, 0.01 → 64, 0.001 → 128`), `adaptive_step = 16`:
    <https://github.com/blender/blender/blob/main/intern/cycles/scene/integrator.cpp>
11. Blender/Cycles, GPU work: `render_samples(samples_num)`, tile scheduler, path-state cap:
    <https://github.com/blender/blender/blob/main/intern/cycles/integrator/path_trace_work_gpu.cpp>
12. Blender manual, Cycles sampling: adaptive sampling, noise threshold range, min samples:
    <https://docs.blender.org/manual/en/latest/render/cycles/render_settings/sampling.html>
13. NVIDIA Falcor, `GeneratePaths.cs.slang`: one thread per pixel, `for i < spp` per thread, tile =
    one thread group, per-pixel sample count texture, prefix-sum offsets:
    <https://github.com/nvidiagameworks/falcor/blob/master/Source/RenderPasses/PathTracer/GeneratePaths.cs.slang>
14. NVIDIA Falcor, `PathTracer.cpp`: `samplesPerPixel` define, full-screen dispatch, `spp == 1`
    special case, resolve pass over samples:
    <https://github.com/nvidiagameworks/falcor/blob/master/Source/RenderPasses/PathTracer/PathTracer.cpp>
15. Mitsuba 3, integrator docs: `rr_depth` (default 5), `max_depth`, and `samples_per_pass`
    ("divides the workload in successive passes"):
    <https://mitsuba.readthedocs.io/en/latest/src/generated/plugins_integrators.html>

**Platform documentation**

16. wgpu, `Queue::write_buffer`: writes "begin GPU execution only on the next call to
    `Queue::submit()`, just before the explicitly submitted commands":
    <https://docs.rs/wgpu/latest/wgpu/struct.Queue.html#method.write_buffer>
17. wgpu, `ComputePass::dispatch_workgroups_indirect`:
    <https://docs.rs/wgpu/latest/wgpu/struct.ComputePass.html#method.dispatch_workgroups_indirect>
18. Vulkan, `vkCmdDispatchIndirect`: <https://registry.khronos.org/vulkan/specs/latest/man/html/vkCmdDispatchIndirect.html>
19. Vulkan, `VkPresentModeKHR` (FIFO queue vs mailbox replacement):
    <https://registry.khronos.org/vulkan/specs/latest/man/html/VkPresentModeKHR.html>
20. Microsoft, WDDM timeout detection and recovery — default timeout **2 seconds**:
    <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery>
21. NVIDIA, GeForce RTX 4090 specifications (16 384 CUDA cores, 2.52 GHz boost, 24 GB):
    <https://www.nvidia.com/en-us/geforce/graphics-cards/40-series/rtx-4090/>

**This repository** (primary for the engine's behaviour)

22. `crates/render/src/shaders/pathtrace.wgsl` — `MAX_BOUNCES`, the bounce loop, the running mean,
    `accum.w`, sub-pixel jitter, `workgroup_size(8, 8)`.
23. `crates/render/src/tracer.rs` — one full-screen dispatch, bind group per dispatch, the per-sample
    submit rule, `update_scene` resetting the whole image, `View`.
24. `crates/render/src/accumulation.rs` — the reset key and its tests.
25. `crates/render/src/window.rs`, `crates/render/src/offscreen.rs` — one sample per presented frame;
    the offscreen loop of submits.
26. `crates/render/src/profiler.rs` — `MAX_PASSES = 16`, 3-deep ring, untimed frames.
27. `crates/render/src/shaders/trace.wgsl` — level/region/brick/voxel DDA, bound-sphere early out.
28. `bins/client/bench-budgets.toml` — `reference = 3.0` ms per sample; measured 1.10 ms mean /
    2.69 ms p95 at 1280×720 (commit `bdbba3d`).
