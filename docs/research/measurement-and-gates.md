# Research: measurement and fidelity gates

Resolves [#77](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/77). Feeds the catalogue [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71), and the bench path [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64).
Checked on 2026-10-31 against `main` at `41f27f4`.

Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). The binary is `client`; older notes call it `bhayanak`, so `bhayanak bench` is `client bench` today. The bench itself is documented in [`docs/agents/benchmarking.md`](../agents/benchmarking.md), and that document stays the reference for how a report is read.

**Confidence** is marked on every estimate: **high** means read in this repo's code or docs and arithmetic from it; **medium** means derived from a number the repo states; **low** means literature or first principles that only the 4090 can settle.

## TL;DR

- **The project already has a good GPU stopwatch and a poor hitch detector.** `Profiler`/`Timings` time every pass by name, keep full history, and survive a device without timestamp queries. `client bench` writes a versioned report and enforces per-pass mean budgets. But every offscreen frame waits for the GPU (`poll(wait_indefinitely)` plus a full image readback), so the reported `frame_time` is a draw-and-readback cost, not frame pacing. A 1.6 s hitch is invisible to a mean over 240 frames: it adds 6.7 ms to the mean, and a p95 over 240 samples is the 228th sample, so one hitch lands in `max_ms` only.
- **The bench currently measures none of the things the catalogue needs to rank.** It never digs, never streams, builds every brick before frame 0, and has no LOD rings, culling or prefetch to measure. There is no GPU-memory figure, no resident-brick count, no generation throughput, no prefetch statistic, no image difference against the reference tracer, and no p99/p99.9.
- **Rank the gates by what can silently break "invisible".** Exact culling-on/off equality of a brick-id buffer catches a removed occluder; a hole check against the CPU tracer catches missing residency; a 1-output-pixel LOD bound plus FLIP catches geometry popping; the path-traced golden is the only check that sees shading. Timing budgets cannot catch any of these.
- **The current tracer changes the cost model.** A first-hit DDA walk stops at the first solid Voxel, so frustum/horizon culling has little to remove for primary rays on today's compute path. Culling pays through (a) smaller hardware-RT acceleration structures, (b) secondary shadow/GI rays, (c) not generating or uploading what cannot be seen, and (d) LOD. The experiment below is designed to find out whether that is true here rather than assuming it.
- **At 50 m/s the generation rate is probably not the wall; the orbit jump and the scene rebuild are.** The repo generates 2.6 million bricks in about 20 s on eight cores, roughly **0.9–1.5 × 10⁵ bricks/s** (medium). At 500 Voxels/s through a ~120 m ring only about **1.5–5 × 10³ bricks/s** enter the view (medium-low), so flight has an order of magnitude of headroom. Reaching orbit needs the coarse levels in ≤ 5 s, about **3 × 10⁴ bricks/s** (medium) — comfortably under the measured rate, but the ~2 s spawn ring and the coarse pyramid are the tight cases. The 1.6 s full repack after enough Edits is the known hitch.
- **VRAM is unlikely to bind in M1a.** Packing arithmetic puts the test Planet's GPU scene at roughly **40–100 MB** and the CPU store at **0.15–0.5 GB** (low-medium), against the 2 GB cache. Measure it anyway, but do not let it outrank frame time or hitches.
- **What can be gated in CI:** CPU determinism and generation throughput on 4 vCPU, golden images on lavapipe, culling-on/off equality at small resolution, the analytic LOD bound, hole checks through the CPU tracer, and the report schema. **What cannot:** absolute GPU times, VRAM, real hitches, streaming at 50 m/s, hardware ray-query performance, and TAAU quality. Those run on the 4090 from the `client-windows` artifact, three runs, clocks recorded.

## 1. What the repo measures today

| Instrument | What it gives | What is missing for this catalogue |
| --- | --- | --- |
| `render::Profiler` | Start/end timestamps for up to 16 passes per frame, ring-buffered readback, `supported()` false when the device has no timestamp queries. Every pass begun through `compute_pass`/`render_pass` is timed by name with no extra wiring. | Only time. No counters, no memory, no per-pass inputs (rays, bricks, pixels). Passes beyond 16 are silently untimed (`MAX_PASSES`). |
| `render::Timings` | Rolling 120-frame average/max, plus full history when `set_keep_history(true)`; `wall` records the CPU interval between `begin_frame` calls. | No percentiles, no histogram, no threshold counts. The overlay's 120-frame window hides any hitch older than two seconds at 60 fps. |
| `client bench` | A camera path that is a function of frame index only; primary and reference modes; warm-up frames; a JSON report (`schema_version` 1) with `mean/p95/max`, per-pass `status`, machine, config, screenshots; exit 2 on an over-budget pass. Versioned path name. | It draws every frame offscreen and reads the image back, so it synchronises per frame. It floats the whole Planet before frame 0, never Edits, never changes rings, and reports `frame_time` including the stall. `--short` is 6 primary frames, so no percentile means anything. |
| `bins/client/bench-budgets.toml` | Per-pass mean budgets and a `gpu_total` budget per mode; compiled into the executable; `--budgets` for experiments; unmatched budgets reported. | The gate is the **mean**. It cannot express "no frame over 33 ms" or "p99.9 under 2× median". Only `primary`, `reference` and the two totals are live; later stage budgets are comments. |
| Golden images (`crates/render/tests/golden.rs`) | Two fixed views, 256×144, lavapipe, tolerant comparison (MSE ≤ 4 and ≤ 0.5% far-off pixels for primary; MSE ≤ 1, ≤ 0.2% for reference). Failures upload actual/expected/diff from CI. | Only two views, one scene state, one driver, no LOD or culling states. No perceptual metric, no brick-id comparison, and no hardware golden. |
| Determinism matrix (`.github/workflows/ci.yml`) | `planet-gen` and `sim` golden hashes on Ubuntu and Windows, debug and release. Rendering tests run only in the Ubuntu `cargo test --workspace` leg. | Rendering is not compared across Windows and Linux, or across Mesa versions. The runner is `ubuntu-latest`, whose Mesa version moves without a lock. |
| `trace_agreement` | GPU `trace()` vs the CPU `reference::trace` on hit, position, normal, Material, and `t` within 1e-3 relative. The CPU mirror is line-for-line. | It is the strongest correctness tool in the repo and is only used on hand-made scenes and patches, not as a per-frame oracle on a bench path. |
| `scene::Scene::update` | Reports `Patched` or `Rebuilt`, and the comments state the rebuild cost (~1.6 s for the test Planet). | The bench never exercises it, so no budget or report line exists for repack. |

Two consequences drive the rest of this note.

1. **A mean-over-a-run gate and a hitch gate are different instruments.** The frame budget (#10) is 8.6 ms of GPU work in a 16.7 ms frame, so there is 8.1 ms of slack. A single 1.6 s repack spread over a 240-frame run adds 6.7 ms to the mean and would show; the same hitch once in a 60 s path adds 0.44 ms to the mean and is one sample out of 3,600, so it appears only in `max_ms`. The catalogue ranks hitches second, so the report needs p99.9, counts over thresholds, and a path long enough to contain the event.
2. **Offscreen readback makes the CPU number unusable for pacing.** `Offscreen::render` submits, copies the texture to a staging buffer, calls `device.poll(PollType::wait_indefinitely())`, then maps and copies. That is deliberate and right for reproducible per-pass GPU times, but it means the next frame cannot overlap the previous one. Any hitch caused by a worker pool, a repack, a `write_buffer` burst or a mutex will be charged to the frame it lands in, but the *baseline* is also inflated and the GPU is never more than one frame deep. A separate pacing mode (submit without readback, log `Instant` deltas) is needed to see the client's real cadence.

## 2. What to measure

Each metric gets: a definition precise enough to implement, the cheapest place to collect it, and the gate it feeds. Units are milliseconds unless stated.

### 2.1 Resident bricks and bytes

- **Resident** means held in the client's active working set for a ring level: on the CPU in the `BrickStore`/ring cache, and on the GPU in the scene buffers. Count `bricks[level]`, `voxels[level]`, and bytes separately for CPU and GPU; "resident" without a side is ambiguous.
- **CPU bytes** are harder than they look because `Brick` is `Air | Solid(u16) | Mixed(Box<Mixed>)`, and a `HashMap` entry carries its own overhead. Do not guess with `size_of`; count exact heap allocations at insert/remove in a debug build, or report `bricks` only and mark bytes as approximate.
- **GPU bytes** are exact for our own allocations: sum the `size` of every buffer and texture the renderer creates, by owner (scene, rings, acceleration structures, targets, staging). `wgpu::Adapter` has no memory query — the adapter API is `request_device`, `features`, `limits`, `get_info`, and format queries only ([wgpu 30.0.1 `Adapter`](https://docs.rs/wgpu/30.0.1/wgpu/struct.Adapter.html)) — so this accounting is the primary number. Cross-check with `VK_EXT_memory_budget`'s `heapUsage` through `as_hal` on Vulkan: it is an estimate of the process's heap usage and is explicitly "not invariant" ([`VkPhysicalDeviceMemoryBudgetPropertiesEXT`](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryBudgetPropertiesEXT.html)). Report both, and never compare the extension's number across drivers as if it were exact.
- Gate: peak resident GPU bytes ≤ 2 GB on every bench path; no unbounded growth while flying (same figure at the start and end of a 60 s flight within a stated margin).
- Expected today: **40–100 MB GPU, 0.15–0.5 GB CPU** for the 2.6 M-brick test Planet (low-medium). A mixed brick is 16 mask words + header + palette + indices, roughly 70–160 B depending on the palette width; a uniform brick is a 4 B `brick_refs` entry; the rest of the shell is uniform rock. This is arithmetic from `scene.rs`, not a measurement.

### 2.2 Generation throughput

Report, per level and in total:

- **bricks/s** and **Voxels/s** generated, measured from the first worker start to the last brick handed to the uploader, not including upload.
- **readiness**: time from a request (spawn, ring re-centre, orbit jump, teleport) to the last *needed* brick being resident and drawn. This is the number #49's acceptance criteria are written in ("~120 m spawn ring in ~2 s", "whole-Planet coarse LOD in ~5 s").
- **queue depth** and **starvation**: requests outstanding, and frames where the front of the queue was empty while a needed brick was missing.

The generator is deterministic per `(seed, GENERATOR_VERSION, brick coordinate)` and threads over x-slabs, so throughput is the only machine-dependent part. Guard the output with the existing brick content hash so a "faster" change cannot silently change terrain.

Measured anchor: `generate_planet` holds about 2.6 M bricks and takes about 20 s on eight cores (repo comment; bench docs say 22 s for the bench's full run). That is **0.9–1.5 × 10⁵ bricks/s** (medium). Anything the catalogue proposes must be compared with that, not with an assumed infinite worker pool.

### 2.3 Upload bytes and upload time

Two different quantities, often confused:

- **Steady-state upload**: bytes written per second while flying. `Queue::write_buffer` copies into staging immediately and begins GPU execution only on the next `submit`; on native it allocates a fresh staging buffer each call unless a `StagingBelt` or explicit mapped buffers are used ([wgpu `Queue::write_buffer`](https://docs.rs/wgpu/30.0.1/wgpu/struct.Queue.html#method.write_buffer)). Measure bytes and calls per frame, plus the wall time of the write and the GPU time of the copy, separately.
- **Event upload**: bytes and wall time for one Edit or one ring re-centre. After enough Edits, `Scene::update` returns `Rebuilt` and repacks the whole scene; the comment states ~1.6 s for the test Planet. That is the hitch users feel, and it is a **CPU** cost followed by a scene-wide upload, not a steady-state byte rate.

For reference, one mixed brick is ~70–160 B; 1,000 bricks/s is ~0.07–0.16 MB/s. **Steady-state bytes are not the bottleneck; rebuilds and upload bursts are** (medium). The report should say `bytes` but gate on event time.

### 2.4 Culling pass time, false positives and false negatives

Define the terms so the gate is testable:

- **False negative (fatal):** a Voxel that a ray hits with culling off is missed with culling on. One is a hole. The fidelity bar ("never removes a surface that could contribute a pixel") is exactly "false negatives = 0" over every ray the frame casts, including shadow and GI rays.
- **False positive (waste):** a brick kept by the culling test that no ray of the frame hits. It costs traversal and residency but cannot change the image.
- **Culling efficiency:** `1 − hit_bricks / kept_bricks` over all rays.

The measurement that makes this exact is a **brick-id buffer**: a debug view that writes the hit brick's index to a `R32Uint` texture instead of shading. Render it twice, culling off and on, and compare. Any differing pixel is a false negative or a LOD change; there is no tolerance to argue about. This is only usable while `trace()` still returns a Voxel hit (it does, `Hit.voxel` and `Hit.material`), and it is cheap: one integer per pixel, no shading.

For false positives, add optional atomic counters in the traversal: bricks tested, bricks kept, bricks hit. Counters are per-frame and reset by the frame uniform. They are diagnostic, not gated across vendors, because the order of atomics is not defined.

Pass time comes free from `Profiler` unless the culling runs on the CPU; then record a `std::time::Instant` span and add it to the report as a CPU stage. The CPU side matters because #48/#49 leave the ring update and the scene rebuild on the CPU by construction.

### 2.5 Prefetch hit rate

Three numbers, because "hit rate" alone can be gamed by prefetching everything:

- **Hit rate** = prefetches that found the brick already resident ÷ prefetches issued. A high hit rate means the prefetcher is late or redundant.
- **Coverage** = needed bricks that were resident on the frame they were needed ÷ needed bricks. This is the one that maps to holes.
- **Usefulness** = prefetched bricks that were drawn within *k* frames (say 60) ÷ prefetched bricks. Low usefulness means wasted generation and VRAM.

A prefetch that arrives after use is a miss for coverage even if it eventually completes. Record the frame index of request and of first use so latency, not just counts, is visible.

### 2.6 Hitches: p99, p99.9, and a threshold count

Frame pacing has to be measured on the CPU clock, not from GPU timestamps: a frame can miss its deadline waiting on a worker, a lock, or an upload. Record one interval per presented frame (or per `render` call in pacing mode), then report:

- `p50`, `p95`, `p99`, `p99.9`, `max`;
- **slow frames**: count and fraction over 16.7 ms;
- **hitches**: count and fraction over `max(2 × p50, 33.3 ms)`, plus the largest hitch and its frame index;
- the frame index is essential: a hitch that coincides with an Edit, a `Rebuilt`, or a ring boundary names its own cause.

Percentiles must be taken in frame-time space, not FPS; mixing them up inverts the meaning, which is a common report bug ([PresentMon issue #219](https://github.com/GameTechDev/PresentMon/issues/219)). A p99.9 from fewer than 1,000 samples is just the maximum under another name, and a stable estimate wants about 10,000 (arithmetic, not a citation). So a 240-frame path can report p99 and `max`, and only a 3–5 minute path can report p99.9 honestly. This is why the hitch paths are longer than `planet-flyover-v1`.

`Timings::record_wall` already records the CPU interval between `begin_frame` calls. In the offscreen bench that interval includes the readback wait, so it must not be presented as pacing. Two honest options: (a) label it "draw + readback" and keep it as context, as the docs already do; (b) add a pacing mode that submits without reading back and records the interval. The catalogue needs (b).

### 2.7 Image difference

Three layers, each answering a different question:

1. **Exact structural diff** — the brick-id buffer (2.4). Catches wrong geometry with zero tolerance.
2. **Statistical diff against the same renderer, technique off** — mean squared error per channel, fraction of pixels differing by more than a stated level, and a perceptual score. This isolates the technique from path-tracer noise. Use FLIP, which is built for comparing a rendered image to a ground truth and whose error map tracks what a human sees when alternating images ([Andersson et al., HPG 2020](https://research.nvidia.com/sites/default/files/node/3260/FLIP_Paper.pdf)). FLIP is preferable to SSIM here because the failure mode is local geometry popping, not global blur.
3. **Diff against the reference path tracer** — the referee the fidelity bar names. This is noisy by construction: the reference is progressive, so compare against a high-sample still or use the existing tolerant comparison. It catches shading errors that (1) and (2) cannot, because both sides share the same shading code.

Thresholds for FLIP must be calibrated once on the dev's display rather than copied from a paper. A workable default is mean FLIP ≤ 0.05 and 99.9th percentile ≤ 0.2 for a "no visible change" claim, with the dev lowering it if a side-by-side flip shows anything; label it a hypothesis until then (low confidence). The repo's current golden tolerances (MSE 4/1, far-off fractions) are the precedent for stating a number and its reason.

### 2.8 LOD error in output pixels

The fidelity bar is "LOD error stays under ~1 output pixel". Make that geometric and checkable before rendering:

```
footprint(d) = 2 · tan(fov_y / 2) · d / H_output      (Voxels per output pixel, d in Voxels)
```

With `fov_y = 70°` (the camera's default) and 1440 output rows, `footprint(d) ≈ d / 1028` Voxels. A level-`L` Voxel is `2^L` Voxels wide, so level `L` becomes admissible — no coarser than one output pixel — at

| Level | Voxel size | Becomes admissible at 1440p | at 720p internal |
| --- | --- | --- | --- |
| 0 | 0.1 m | 102.8 m | 51.4 m |
| 1 | 0.2 m | 205.6 m | 102.8 m |
| 2 | 0.4 m | 411 m | 205.6 m |
| 3 | 0.8 m | 823 m | 411 m |
| 4 | 1.6 m | 1646 m | 823 m |
| 5 | 3.2 m | 3291 m | 1646 m |
| 6 | 6.4 m | 6582 m | 3291 m |

So a ring scheme that switches at half those distances has margin. At 3 km the footprint is about 29 Voxels, so level 4 is admissible; at 7.5 km it is about 73, so level 6 is. The ground-to-orbit range therefore needs levels 0–6, consistent with "each ring twice as coarse" and a small number of rings. Gate at **output** resolution, because "invisible" is judged on the display; tracing at 1280×720 does not lower the bar. This bound is per-Voxel geometry, not per-pixel shading: a coarse Voxel that covers one output pixel can still change that pixel's colour, which is what FLIP and the path-traced reference are for.

This is the formula Nanite uses in spirit — LOD is chosen to keep error at or below one pixel, and switching below that threshold is considered perceptually free ([Karis et al., SIGGRAPH 2021](https://advances.realtimerendering.com/s2021/Karis_Nanite_SIGGRAPH_Advances_2021_final.pdf), "LOD matching < 1 pixel of error"). It is a principle, not a measurement of this engine.

### 2.9 One diagnostic the current shader makes necessary

`primary.wgsl` blends the hit normal towards the radial direction from 100 to 1500 Voxels because there is no LOD yet (`FAR_BLEND_START/END`). The reference path tracer does not do this. Until rings replace that blend, a primary-vs-reference image diff at distance measures the placeholder, not the LOD. The experiment that introduces rings should delete the blend in the same change, or the gate compares two different shading models.

## 3. Bench paths that force the worst case

The current `CameraPath` stores keyframes and interpolates position and target linearly by frame index. That is deterministic and sufficient for the existing flyover, but three of the required paths cannot be expressed with it. The path type needs three small additions: a per-keyframe interpolation mode (linear or geometric), a teleport (a frame index where the previous segment is abandoned), and named events (dig at frame *n*, Edit storm over frames *a..b*) that the bench driver executes. All remain pure functions of the frame index, which is the property that makes runs comparable.

| Path (proposed name) | What it stresses | Parameters | Needs today |
| --- | --- | --- | --- |
| `planet-flyover-v2` | The M1a acceptance path #64 asks for: orbit → ground, 50 m/s, a dig, Zoom up and back. | 50 m/s = 500 Voxels/s; 3,600 frames at 60 fps is 60 s. | Extend v1 with the dig event and the zoom; keep v1 for comparison. |
| `fast-turns-v1` | Culling and TAA under rotation; the worst case for temporal history and for frustum churn. | 300°/s yaw (5°/frame at 60 fps) for 2 s, then reverse; 60° pitch sweep. | Linear keyframes already support it; add enough keyframes that the turn is smooth. |
| `zoom-v1` | Ring re-centring, orbit generation, LOD switches. | Continuous ground ↔ orbit over 8 s each way, **geometric** in altitude (equal ratio per frame), not linear. | A geometric interpolation mode; linear interpolation spends most frames near the ground. |
| `cave-dive-v1` | Occlusion culling, near geometry in every direction, LOD under a ceiling. | Enter a carved tunnel, 3 turns, 20 m/s, 10 s. | The test Planet has **no caves** — `planet-gen` is a noisy sphere with a soil layer. The bench needs a deterministic carved cave (a scripted sequence of `EditOp::dig_sphere` before frame 0) or a `--scene cave` test scene. |
| `teleport-v1` | Residency spike: everything becomes needed on one frame. | One step at frame 60 from the surface to 500 m up and 2 km sideways; 300 frames. | A teleport event in the path. |
| `edit-storm-v1` | Edit → prediction → repack → upload, plus the `Rebuilt` path. | 10 digs/s for 10 s, then 100 digs/s for 10 s (1,100 digs, the scale of the known 1.6 s repack); the dig centre advances 1 m per dig so each removes new Voxels. | Event support and a scene-update call in the bench driver; the bench currently has no `World`. |
| `still-v1` | Convergence of the reference and of TAAU; the noise floor for image diff. | Camera fixed; reference 1, 4, 16, 64, 256 spp; TAAU 1, 8, 32, 128 frames. | Reference mode already accumulates; add a `--samples` sweep and a fixed-history reset. |

The 50 m/s figure is worth checking against the client: `fly.rs` moves at 60 Voxels/s and multiplies by 8 while the fast key is held, which is 480 Voxels/s ≈ **48 m/s**. So "50 m/s" is essentially the existing fast-fly speed, and the bench path should use 500 Voxels/s to be slightly conservative.

Traffic estimate for the flight path: on a sphere of radius *R* Voxels moving at *v*, the swept silhouette area is about `2 R v` Voxels²/s; divided by a brick face (64 Voxels²) that is `2 R v / 64` bricks/s if the whole sphere is in view. At *R* = 1200 and *v* = 500 that is ~18,750 bricks/s; a 70° frustum sees roughly a quarter to an eighth of the sphere, so **1.5–5 × 10³ bricks/s** (medium-low). Against ~10⁵ bricks/s of generation, flight has headroom. The orbit jump is the tight one: the coarse pyramid is about a third of the surface brick count (a geometric series of levels, each a quarter of the last), so ~1.5 × 10⁵ bricks in 5 s needs ~3 × 10⁴ bricks/s if generated directly at each level — comfortable — but if it is built by downsampling the fine level it is a much larger job, which is exactly why #47 exists.

## 4. Fidelity gates

Ordered by strength, cheapest first. A technique must pass every gate that applies before its timing counts.

### 4.1 Culling on vs off: exact equality

- Render the brick-id buffer with culling off and on at the same camera, same sun, same resolution.
- **Gate:** zero differing pixels. If a brick is culled that a ray hit, its pixel changes id; there is no noise because the id is integer scene data, not shading.
- Do it at 320×180 in CI, and at 1280×720 on the 4090; run it on every keyframe of `planet-flyover-v2` and every frame of `cave-dive-v1`.
- Include secondary rays in the definition: a culled brick that is invisible to primary rays but visible to a shadow ray changes the image later. If the implementation culls only for primary visibility, say so and run a separate shadow-ray gate; do not claim "zero false negatives" from a primary-only buffer.

### 4.2 LOD against brute force

- **Reference:** the same renderer at level 0 for every brick in view, which today means the existing whole-Planet scene. It fits: ~2.6 M bricks at 40–100 MB.
- **Test:** rings on, same camera, same shading (with the `FAR_BLEND` removed).
- **Metrics:** fraction of pixels whose brick id or Voxel id differs; mean and 99.9th-percentile FLIP; and the analytic footprint table in 2.8.
- **Gate:** no pixel differs by more than one output pixel of geometric error (checked analytically per ring), and FLIP stays under the calibrated threshold. "One pixel" is a bound on the *LOD switch*, not on every pixel: a coarse Voxel can legitimately change a pixel's colour, which is why the perceptual metric is the second half of the gate.
- Also gate **monotonicity**: along a slow zoom, the chosen level must never go finer then coarser then finer at the same distance (hysteresis), because that is visible as popping even under the error bound. Record the level per frame and assert it is monotone within a margin.

### 4.3 No-hole paths

- On each frame, take a fixed pseudo-random sample of pixels (say 1 in 16) and trace the same rays on the CPU with `reference::trace` against the **complete** store. If the CPU hits and the GPU misses, that is a hole. This is the per-frame oracle the repo is already built for, and it costs a fraction of a millisecond on the sampled set.
- **Gate:** zero holes on every frame of `planet-flyover-v2`, `teleport-v1` and `cave-dive-v1`. Report the first frame index and the camera position of the first hole.
- A cheaper screen-space proxy — "every ray that hits in the previous frame still hits" — is not enough: a newly visible brick appears exactly where the previous frame had none.

### 4.4 Golden images, and keeping them deterministic on lavapipe

- **Inputs are functions of constants only.** Seed, camera, sun, resolution, spp and frame index. No clock, no frame-rate-dependent step (`MAX_FRAME_TIME` clamps the client's camera step — the bench's path already avoids it), no HashMap iteration order (the store docs already forbid exposing it), no thread-count-dependent packing.
- **One golden per scene state, not per frame.** A still camera after a fixed number of samples, and a fixed frame of a pan. For TAAU, reset history at frame 0 and compare at frame *n*; the jitter sequence and history are deterministic if the sample index is.
- **Pin what can move.** `ubuntu-latest` currently picks an Ubuntu image whose Mesa version changes; record `machine.driver_info` in the report (it already does), and prefer `ubuntu-24.04` plus a Mesa version check, or a container pinned by digest. A golden that fails because Mesa improved is a false alarm that costs the dev a morning.
- **Tolerances have reasons.** Keep the existing primary/reference tolerances and state why; add a brick-id golden only where exactness is expected (it is integer data, so it should be exact, and a mismatch is a real regression).
- **Hardware goldens are separate.** A 4090 image and a lavapipe image will not agree bit-for-bit, because floating-point results are not guaranteed identical across implementations and Vulkan's float controls are opt-in ([`VK_KHR_shader_float_controls`](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_shader_float_controls.html)). Do not try to make one image serve both. Keep lavapipe goldens as the CI gate; compare 4090 runs to a committed 4090 golden with a wider tolerance, and only when a change is expected to alter shading.

### 4.5 The reference path tracer as referee

- Use it for shading changes, not for geometry-only changes: it shares `trace()` but not the primary shader, so it catches a wrong normal or a missing Material and is blind to a culled brick that both paths cull.
- Compare at high sample count on a still (`still-v1`), because the progressive image's noise is deterministic but large at low spp. The existing reference tolerance (MSE ≤ 1 at 256×144) is a reasonable starting point.

### 4.6 What each gate can and cannot catch

| Gate | Catches | Blind to |
| --- | --- | --- |
| Brick-id equality | Culled visible bricks, wrong LOD cut, wrong ring assignment | Shading, floating-point drift, hitches |
| Hole check (CPU oracle) | Missing residency, a ring with no bricks, a bad prefetch | Wrong LOD (a coarse brick is still a hit), shading |
| Analytic LOD bound | A ring that is too coarse for the distance | Whether the coarse Voxel's colour is acceptable |
| FLIP vs technique-off | Visible popping, edge crawl, TAA ghosting | Anything both renders get wrong |
| Path-traced golden | Shading, normals, Materials, far blend | Geometry that both paths agree on |
| Timing budget | Cost | Any fidelity failure |
| Hitch count | Cadence | A steady 20 ms frame (slow but smooth) |

## 5. Experiments

Five experiments for the techniques the map is most likely to shortlist. Each is a `client bench` run on the 4090 with the `client-windows` artifact, three repetitions, median reported. The current flags are `--out`, `--budgets`, `--size`, `--short` and `--no-fail`; every other flag below is proposed here and would land with #64.

### E1 — Conservative visibility and culling

- **Hypothesis:** on the current first-hit compute tracer, frustum plus horizon culling saves less than the literature suggests for primary rays, but saves 15–40% of the *primary pass* at ground level once secondary rays exist, and cuts resident bricks by ≥ 30% at altitude.
- **Setup:** `planet-flyover-v2` and `cave-dive-v1`, culling off then on, brick-id buffer on, counters on. Same seed, same keyframes.
- **Metrics:** primary pass mean/p95; culling pass time; kept/hit bricks; false negatives (must be 0); FLIP vs culling off; resident bricks and bytes.
- **Pass/fail:** **fail if any false negative**; pass the timing gate at ≥ 15% primary reduction at p95 with FLIP ≤ 0.02. If the win is under 5%, record it as conditional and wait for secondary rays, rather than forcing it into the shortlist.
- **Runtime:** 3 × 2 paths × 2 states × ~60 s ≈ 12–20 min.
- **Command:**
  ```
  client.exe bench --path planet-flyover-v2 --culling off --brick-ids --out bench-out\flyover-cull-off
  client.exe bench --path planet-flyover-v2 --culling on  --brick-ids --out bench-out\flyover-cull-on
  client.exe diff bench-out\flyover-cull-off bench-out\flyover-cull-on --gate culling
  ```

### E2 — LOD rings and ring boundaries

- **Hypothesis:** rings that switch at the 1-output-pixel distances (table in 2.8) cut primary time by ≥ 40% beyond 400 m and by ≥ 2× at orbit, with FLIP under threshold and no level oscillation.
- **Setup:** `zoom-v1` (geometric) plus `planet-flyover-v2`, rings off then on. Rings off is the level-0 whole-Planet scene; rings on uses the proposed radii. Remove `FAR_BLEND` in the rings-on build, or run both builds with it removed.
- **Metrics:** primary mean/p95 at five altitude bands (ground, 100 m, 500 m, 2 km, 30 km); bricks per level; pixels with a different brick id; FLIP; level-per-frame trace.
- **Pass/fail:** ≥ 30% primary reduction at 500 m and above; zero differing pixels beyond the analytic bound; no finer→coarser→finer transition at the same distance; FLIP ≤ calibrated threshold.
- **Runtime:** 3 × 2 × ~90 s ≈ 12–18 min.
- **Command:**
  ```
  client.exe bench --path zoom-v1 --lod-rings off --out bench-out\zoom-lod-off
  client.exe bench --path zoom-v1 --lod-rings on  --out bench-out\zoom-lod-on
  client.exe diff bench-out\zoom-lod-off bench-out\zoom-lod-on --gate lod
  ```

### E3 — Streaming, prefetch and the Edit repack

- **Hypothesis:** nearest-and-in-view-first generation plus prefetch keeps holes at zero, and moving the scene rebuild off the UI thread keeps the p99.9 hitch at or below 2× the median, including the 1,000-dig case; the orbit jump is ready in ≤ 5 s.
- **Setup:** `planet-flyover-v2` with the dig event, `teleport-v1`, and `edit-storm-v1`; worker pool enabled; prefetch on; repack on the UI thread first as a baseline, then off it. Record generation readiness, coverage, usefulness, resident bytes, and holes per frame.
- **Metrics:** spawn-ring and orbit readiness; bricks/s; coverage; hit rate; usefulness; hole count; hitch p99/p99.9/max and their frame indices; peak resident bytes; repack wall time per `Rebuilt`.
- **Pass/fail:** zero holes; orbit ≤ 5 s; spawn ring ≤ 2 s on the dev's CPU; p99.9 ≤ 33 ms with no hitch over 100 ms outside the scripted teleport; resident ≤ 2 GB. The baseline edit-storm is expected to show the 1.6 s hitch — that is the number the change must remove, not a reason to relax the gate.
- **Runtime:** 3 × 3 paths × ~90 s, plus generation ≈ 20–30 min.
- **Command:**
  ```
  client.exe bench --path planet-flyover-v2 --stream on --prefetch on --assert-no-holes --out bench-out\stream
  client.exe bench --path teleport-v1 --stream on --prefetch on --assert-no-holes --out bench-out\teleport
  client.exe bench --path edit-storm-v1 --stream on --repack ui-thread  --out bench-out\edits-before
  client.exe bench --path edit-storm-v1 --stream on --repack off-thread --out bench-out\edits-after
  ```

### E4 — Hardware ray queries vs compute

- **Hypothesis:** one AABB per non-empty brick in a BLAS cuts primary time by ≥ 1.5× at 720p internal on the 4090, with hit-for-hit agreement.
- **Setup:** same paths, `--tracer compute` then `--tracer raytrace`; BVH rebuilt after the scripted dig; the compute build is the reference.
- **Metrics:** primary mean/p95; BVH build and refit time after the Edit (the #50 budget is 1 ms for Edit + BVH); hit agreement against `reference::trace` on a sampled ray set; brick-id equality.
- **Pass/fail:** ≥ 1.5× primary speedup at p95; zero brick-id differences; Edit + BVH ≤ 1 ms at p95. If ray queries are unavailable on the adapter, the run must say so and fall back, not fail silently.
- **Runtime:** 3 × 2 × ~60 s, plus BVH build per run ≈ 10–15 min.
- **Command:**
  ```
  client.exe bench --path planet-flyover-v2 --tracer compute  --out bench-out\trace-compute
  client.exe bench --path planet-flyover-v2 --tracer raytrace --out bench-out\trace-hw
  client.exe diff bench-out\trace-compute bench-out\trace-hw --gate hits
  ```

### E5 — Half-resolution tracing and TAAU

- **Hypothesis:** 1280×720 internal reconstructed to 1440p stays under the calibrated FLIP threshold on a still and a pan, while cutting primary time by ≥ 1.6×, with the upscale pass ≤ 0.5 ms.
- **Setup:** `still-v1` at 1, 8, 32 and 128 frames; `fast-turns-v1` for ghosting; internal 1280×720, output 2560×1440; jitter and history reset at frame 0.
- **Metrics:** primary and upscale pass times; FLIP against a 256-spp reference still; FLIP per frame of the pan; history length debug view; convergence frame (first frame under threshold).
- **Pass/fail:** upscale ≤ 0.5 ms; still FLIP ≤ threshold by frame 32; pan ghosting under threshold after history clamping; ≥ 1.6× primary speedup relative to tracing at output resolution.
- **Runtime:** 3 × ~2 min for the still sweep and pan ≈ 10–15 min.
- **Command:**
  ```
  client.exe bench --path still-v1 --size 1280x720 --output-size 2560x1440 --upscale taa --frames 128 --out bench-out\taau-still
  client.exe bench --path fast-turns-v1 --size 1280x720 --output-size 2560x1440 --upscale taa --out bench-out\taau-pan
  client.exe bench --path still-v1 --size 2560x1440 --upscale off --out bench-out\taau-off
  ```

An optional sixth, if the first five leave room: a **prefetch simulator** on the CPU (no GPU) replaying the flight path through a synthetic brick cache. It can size the cache and the prefetch window in minutes, before any streaming code exists, and it runs in CI. The map raises exactly this as a possible prototype, and it is cheap enough to be worth it.

## 6. CI versus the dev's 4090

### 6.1 What runs where

| Check | CI (lavapipe, 4 vCPU, 16 GB) | 4090 (Windows artifact) |
| --- | --- | --- |
| Build, fmt, clippy, unit tests, CPU determinism matrix | ✅ already | — |
| Generation throughput lower bound (bricks/s on 4 vCPU, generous margin) | ✅ | ✅ reference number |
| Golden images at 256×144 (and a couple more views) | ✅ tolerant, lavapipe | ✅ separate hardware golden |
| Brick-id equality, culling on/off at 320×180 | ✅ exact | ✅ at 1280×720 |
| Analytic LOD bound and level monotonicity | ✅ CPU, no GPU | ✅ |
| Hole check via `reference::trace`, 1-in-16 pixels | ✅ | ✅ |
| Report schema and exit codes (`--short`) | ✅ already | ✅ |
| `client bench --short` smoke | ✅ timings ignored | — |
| Absolute pass times and budgets | ❌ meaningless on a CPU driver | ✅ the gate |
| Hitches and p99.9 | ❌ | ✅ |
| Resident GPU bytes / VRAM | ⚠️ account only; the extension's estimate is a software number | ✅ plus `VK_EXT_memory_budget` cross-check |
| 50 m/s streaming with real workers | ❌ (could run small, but throughput is not representative) | ✅ |
| Hardware ray queries | ⚠️ lavapipe advertises `VK_KHR_ray_query` ([Igalia, Vulkanised 2025](https://vulkan.org/user/pages/09.events/vulkanised-2025/T5-Lucas-Fryzek-Igalia.pdf)), so hits and correctness can be smoked; times are not representative | ✅ the performance gate |
| TAAU quality | ⚠️ deterministic goldens only | ✅ FLIP and ghosting |
| TAAU/denoise convergence at speed | ❌ | ✅ |

CI's standard Linux runner is 4 vCPU / 16 GB / 14 GB SSD for a public repository ([GitHub-hosted runners reference](https://docs.github.com/actions/reference/runners/github-hosted-runners)). That is enough for the fidelity gates and the CPU simulator, and not enough for any timing claim. The existing `docs/agents/benchmarking.md` already says the same about `--short`: it checks that the bench runs and the report has the right shape, and its timings mean nothing.

### 6.2 The 4090 protocol

Make it a checklist in `docs/agents/benchmarking.md` so the dev does not have to invent it each time:

1. Same driver, same Windows build, same power plan; record them (`machine` already records GPU, driver, driver_info, OS, arch).
2. Close other GPU work; do not run from a debug build.
3. **Record the clocks, and ideally lock them.** NVIDIA's own guidance is that the clock is floating by default and that changing clocks make short measurements unstable; `nvidia-smi -lgc` locks it and `-rgc` releases it, and requires root/administrator ([nvidia-smi docs](https://docs.nvidia.com/deploy/nvidia-smi/index.html); [TensorRT performance guide](https://docs.nvidia.com/deeplearning/tensorrt/10.x.x/performance/hw-sw-environment.html)). Locking is optional for a merge gate but should be recorded so two reports are comparable.
4. Run each path three times; report the median of the three run means, and the worst p99.9 across them. Do not cherry-pick the best run.
5. Attach `report.json` and two screenshots, as today. Say which passes were expected to change (the `needs-benchmark` label's contract).
6. Compare only reports of the same path name and version. A path change invalidates the comparison, as the docs already say.

### 6.3 Report additions, mapped to #64

#64 already asks for generation readiness and the worst hitch. The schema needs these blocks; bump `schema_version` to 2.

```json
"config":  { "...", "lod": "rings|off", "culling": "on|off", "tracer": "compute|raytrace", "internal": [1280,720], "output": [2560,1440] },
"generation": { "spawn_ready_ms": 0, "orbit_ready_ms": 0, "bricks_per_s": 0, "voxels_per_s": 0, "worker_threads": 0 },
"memory":  { "gpu_bytes_by_owner": {}, "cpu_bricks": 0, "gpu_bricks_by_level": [], "peak_gpu_bytes": 0 },
"streaming": { "coverage": 0.0, "prefetch_hit_rate": 0.0, "prefetch_usefulness": 0.0, "holes": 0, "first_hole_frame": null },
"culling": { "kept_bricks": 0, "hit_bricks": 0, "false_negatives": 0, "pass_ms": {} },
"hitches": { "p99_ms": 0, "p99_9_ms": 0, "slow_frames": 0, "hitch_frames": 0, "largest_hitch": { "ms": 0, "frame": 0 } },
"image_diff": { "reference": "culling-off", "mse": 0, "flip_mean": 0, "flip_p99_9": 0, "different_pixels": 0 }
```

`passes` and `gpu_total` keep their meaning; `hitches` is explicitly CPU pacing, not GPU time. Keep the existing `budgets` block and add a `gates` block for the non-timing checks, so CI and the 4090 produce the same shape and a single command can print "fidelity ok, timing over".

## 7. What this means for the catalogue's ranking

- **Frame time first, and the biggest lever is LOD, not culling.** On a first-hit DDA tracer, a shorter walk at distance (coarser Voxels) reduces steps for every ray; culling only removes work that the walk would not have done. E2 should be the first experiment run, E1 second.
- **Hitches are a streaming and repack problem.** The known 1.6 s repack dominates any per-frame cache miss. E3 should measure it explicitly and the catalogue should prefer an incremental repack (or off-thread repack) over a prefetch policy that only improves the average.
- **VRAM is not the constraint in M1a.** At 40–100 MB against 2 GB, a technique that trades memory for time is attractive, not suspicious. Revisit at M5 (content), where vegetation and Fluids change the arithmetic.
- **Hardware RT changes what culling is worth.** With one AABB per brick, a smaller BVH helps traversal and build time; the same culling that is nearly free today could pay 10–20% there. E4 should record BVH size and build time, not just frame time.
- **The fidelity gates are the ranking's real filter.** A technique that wins 30% and fails one brick-id pixel is not conditional, it is rejected. The catalogue's "rejected/watchlist" section should cite the gate that rejected it, not an opinion.

## 8. Open questions

- **The FLIP threshold is uncalibrated.** The 0.05 mean / 0.2 p99.9 starting point is a hypothesis. One sitting with the dev, flipping between two LOD levels on the 4090, would settle it; until then the gate should be "no difference in a blind flip test", with FLIP as the regression tripwire.
- **There is no cave scene, and the map's fidelity bar names cave dives.** Either a scripted carved cave becomes a bench scene (deterministic, from Edits) or the cave path is dropped from M1a and the catalogue says so. This is a content decision, not a measurement one.
- **Does the client ever measure pacing in a window?** The offscreen bench cannot. A `--pacing` mode (no readback) is the cheapest honest fix; a separate windowed recorder is more faithful and more work.
- **Should CI pin Mesa?** A golden that moves with `ubuntu-latest` will eventually fail for a reason unrelated to the PR. Pinning `ubuntu-24.04` and recording `driver_info` is cheap; pinning a Mesa version is a container or an apt pin.
- **Is lavapipe's ray-query support usable in CI?** It advertises the extension, but #50 plans to fall back if not. A tiny smoke test (one AABB, one hit) settles it; do not assume either way.
- **Clock locking on the dev's machine is a policy choice.** It makes reports comparable and makes the numbers slightly worse than a floating clock. Record the state either way.
- **The 1-output-pixel gate at 1440p is stricter than at 720p internal.** This note recommends output resolution because "invisible" is judged there; if the dev disagrees, the ring table in 2.8 gives the 720p numbers and the catalogue can use them.
- **Unverified here:** no GPU was available, so every timing expectation is literature or arithmetic. The VRAM figures are packing arithmetic, not a reading; the generation rate is a repo comment, not a new measurement.
