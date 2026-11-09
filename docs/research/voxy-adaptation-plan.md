# Plan: adapting voxy's control plane to BhayanakVoxel

Feeds [#145](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/145), [#143](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/143), [#144](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/144), [#62](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/62), [#49](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/49), [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64), [#146](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/146). Read with [ADR 0002](../adr/0002-brick-store-all-voxel-lod.md), [ADR 0004](../adr/0004-residency-360-shell.md), [ADR 0006](../adr/0006-scene-thread.md), [ADR 0007](../adr/0007-5-cm-voxels.md), [`residency-prefetch-eviction.md`](residency-prefetch-eviction.md), [`measurement-and-gates.md`](measurement-and-gates.md) and [`visibility-and-culling.md`](visibility-and-culling.md).

Planned 2026-11-09, from a design review (grilling) of the external voxy techniques bundle — a report on MCRcortex/voxy (a client-side LOD renderer for Minecraft) plus a draft post-plan for this repo. The bundle and the voxy clone it cites live outside the repo on the dev machine; its section numbers appear below as *voxy §N*. **This document supersedes the bundle's Part II post-plan** wherever the two disagree, and they disagree in six places (§1).

Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). This is a plan, not an ADR: each phase that survives its gate earns an ADR when it lands. Every phase here that touches GPU passes carries `needs-benchmark` on its PRs; CPU-only work merges on green CI ([`docs/agents/benchmarking.md`](../agents/benchmarking.md)).

## TL;DR

- **Borrow voxy's control plane, but half of it is already here and a third of the rest is aimed at the wrong zoom mode.** Delta uploads with coalesced runs and outbox merging already ship (`SceneUpdate::Rings`, `RingPatch::merge`); CPU pass-stamped LRU **Unit** eviction already ships (#141) in the bounded, part-wise shape the draft post-plan asks for; #145 already covers the upload budget; #62 already is the generator-version plan. Orbit — the draft's favourite target — is the *cheap* mode: at orbit nothing but the **Guard** (levels 9–13, 74 MiB, ~0.8 s) is resident.
- **The bench cannot see any of this.** `client bench` floats the whole test Planet before frame 0, so the ring loader, uploads, residency churn, **Soft misses** and scene-thread cost are invisible to the one artifact that gates merges (#13). P0 is therefore a *streaming bench mode*, not "wire some counters".
- **The top-line metric is scene-thread + upload cost during flight and zoom transitions**, not orbit steady-state. That is where the measured pains live: pre-scene-thread flying p99 was 28 ms (ADR 0006), peak host memory reaches ~4× the scene reserve during a jump, and #49 budgets the jump *to* orbit at ≤ 5 s.
- **A GPU demand signal that reports what the DDA touched re-derives the **Shell** rule** — the CPU already knows the whole 360° shell is wanted (ADR 0004). The one thing only the GPU knows is which bricks were drawn *coarser than the **LOD band** rule demands*: a **Soft miss** detector. P2's mechanism is that, and nothing else.
- **Every phase gets an explicit kill criterion and dies at its gate unless its pain shows up in the P0 numbers.** Expected deaths: P3 (view-independent residency makes visibility stamps mostly redundant and view-shaped ordering explicitly rejected), plausibly P4 (a depth pyramid was already rejected for secondary rays; for primary rays it can only be temporal).

---

## 1. What changed vs the draft post-plan

| # | Draft post-plan said | This plan says | Why |
| --- | --- | --- | --- |
| 1 | Top-line metric: scene-thread cost at orbit | Same metric, but during **flight + zoom transitions** | Orbit steady-state needs only the pre-generated Guard (ADR 0004). |
| 2 | P0 = extend bench JSON with residency metrics | P0 = a **streaming bench mode** (loader, rings and uploads inside the timed loop) + the extended metrics | The bench never streams; the metrics are unproducible as-is. |
| 3 | P1 = voxy-style list-driven scatter/memcpy delta upload, incl. compute apply passes | P1 = **#145 + defer-never-stall cap + one repack-layout issue**; compute scatter dropped | Delta patching already ships; the real pain is the CPU-side 3× repack write amplification (211 MB vs 73 MB, ADR 0006), a packing *layout* problem; `queue.write_buffer` has none of the GL blocking cost voxy's compute scatter exists to avoid. |
| 4 | P3a "adopt" GPU `lastUseFrame` stamps; P3b optional eviction preselector | P3 **gated, expected to die** | CPU pass-stamped LRU shipped (#141); a 360° shell makes "wanted this frame" ≈ all resident bricks; true-visibility ordering re-introduces the view-shaped eviction ADR 0004 rejected at 800 m/s turn sweeps. |
| 5 | P4 = per-frame HiZ for "far rings and orbit" | P4 gate counter across the **altitude sweep**; far rings and self-occlusion coexist only at mid-altitude flight; temporal-only scheme | Ground horizon is 253 m; one ray per pixel means no other-geometry occlusion test — any pyramid is last-frame depth with TAA disocclusion hazards. |
| 6 | P5 sequenced unconditionally after P4 | P5 **contingent**: only if P2 *and* P4 survive and dispatch-size CPU round-trips show measurable cost | Zero data-dependent dispatches, zero atomics in WGSL, zero GPU-written counters exist today; the pain can only appear after P2/P4. |

Also settled: constants are **derived from our budgets, not lifted** (voxy's 50 req/frame, 2 MB/frame, 4096 scatters, 256-batch come from a 5-level, 8 km, meshing-bound world; ours is 14 levels, a 16 km-radius planet at 5 cm, no meshing) — voxy's numbers only show *which knobs exist*. And P6 is not a phase of this program at all: it is #62, landing with **Diverged brick** persistence.

## 2. Facts the plan rests on

All checked against `main` on 2026-11-09 (see the review's fact pass for file:line detail).

- **Scale**: 14 LOD levels (0–13), `d0` = 1028.2 Voxels = 51.4 m; Guard = levels 9–13 (73.9 MiB); walking **Shell** = 3.44 M drawn bricks (474 MiB client; simulator says 1.7–1.9 M — the ~2× undercount #143 re-checks); host store at complete shell ≈ 16.1 M bricks, budget = that + 10% overflow; ground **Horizon cap** 253 m; Zoom limit 800 km.
- **Shipped**: scene thread with outbox merge (ADR 0006, #127); ring loader (#86) with nearest-first priority, `MAX_AHEAD = 512`, cancellation; delta `Rings` patches with coalesced runs; part-wise bounded CPU eviction with pass-stamped LRU (#141) and scene-thread constants (`EVICT_HOLD` 500 µs, back-off 5/4); GPU pass timers with a 3-deep MAP_READ readback ring (`render/src/profiler.rs`).
- **Open and overlapping**: #145 (per-buffer growth, staging belt, per-frame upload budget — `ready-for-agent`), #143 (shell margins, velocity prefetch, loader priority classes, soft-miss measurement), #144 (edit epochs vs stale loader arrivals), #62 (SQLite saves with seed + `GENERATOR_VERSION` guard), #49 (jump-to-orbit ≤ 5 s), #64 (bench path), #146 (horizon-cap culling).
- **Absent**: no atomics in any WGSL shader, no indirect dispatch, no GPU-written counters, no GPU→CPU feedback except the profiler ring; a depth pyramid was researched and rejected for secondary rays (`visibility-and-culling.md` §238: build cost ~0.05–0.1 ms at 720p buys a low hit rate).
- **wgpu** `=30.0.1`, Vulkan-only; `dispatch_workgroups_indirect` is available unconditionally — feasibility for P5 is fine; value is what's missing.

## 3. Governance

- **Per-phase go/no-go.** No phase is built until its gate metric, produced by the P0 streaming bench, shows its pain is real. Issues are filed one phase ahead, never all at once.
- **Top-line metric**: scene-thread + upload cost per frame during streaming-heavy frames — the flyover path (4.8 m / 100 m / 500 m / 2 km altitude sweep) and zoom/jump transitions. Orbit steady-state (Guard only) is a sanity floor, not the target.
- **Kill criteria** (relative thresholds, so they don't depend on absolute baselines):
  - *Program-level*: if scene-thread residency cost during flight/zoom is already < 10% of frame budget in P0, the entire P2/P3 line dies; keep P1 (uploads) and #62.
  - *P1*: dies if repack write amplification is already < 1.5× or upload time is < 10% of frame budget.
  - *P2*: dies if steady-camera **Soft misses** are rare or already handled by CPU priority logic (#143).
  - *P3*: dies unless eviction choice causes *measured* soft misses and the predicted-vs-actual want gap (post-#143-fix) changes eviction outcomes.
  - *P4*: dies if no altitude band shows ≥ 20% of far-ring work skippable by an entry-depth test.
  - *P5*: dies if no dispatch-size CPU round-trip appears in the streaming bench as measurable cost.

## 4. P0 — streaming bench mode (M1a-1, unconditional)

The deliverable is a bench mode where the ring loader, ring recentring, scene packing and uploads run *inside* the timed loop, reporting the extended metric list. It directly serves #143's soft-miss acceptance criterion and #64's bench-path work, and gives every later gate its numbers.

- **Determinism**: the loader is driven to a fixed frame-index schedule (demand computed from the frame-indexed camera path, like `planet-flyover-v1`), so residency, soft misses and upload bytes are reproducible run-to-run — gate numbers must not jitter with thread timing. Real-threading behaviour (worker races, arrival jitter) stays in the `--ignored` full-size streaming tests with `HELD_FLIGHT` limits, which exist for exactly that.
- **Metrics** (beyond the existing per-pass timers, `frame_time`, `scene_update_ms`, `lod_at_keyframes`):
  1. upload bytes and GPU bytes changed per frame; rebuild count;
  2. soft misses per frame, counted *inside the resident set* (a bug class per CONTEXT.md), split steady-camera vs moving-camera;
  3. per-Ring fill vs its LOD band;
  4. scene-thread cost broken out by category (follow, pack, upload, eviction);
  5. **Diverged brick** count, store size, outbox latency;
  6. the far-ring occlusion-savings *estimate*: for each dying far-ring ray/tile, would an entry-depth test against the previous frame's depth have skipped it — reported per altitude band (P4's gate).
- **Ordering**: close the #143 simulator-vs-drawn ~2× undercount first; every gate reads those numbers.
- CPU-only for the harness parts; the soft-miss counter and occlusion estimate are additive GPU instrumentation → `needs-benchmark` on the PR, but expected timing-neutral.

## 5. P1 — uploads (M1a-2, unconditional): #145 + repack layout

- **Mechanism**: land #145 as written (per-buffer growth, one staging belt, per-frame upload budget), then add the cap semantics below. No new GPU passes.
- **Cap semantics — defer, never stall**: when the per-frame delta cap is hit, the remainder waits in a bounded outbox queue and lands next frame (drop-oldest on overflow). The cap is a queue-depth cap, never a blocking gate. Voxy can stall its worker because that thread is off the frame path; our scene thread publishes to the frame thread, and a stall is a hitch — the thing ADR 0006's outbox exists to prevent. The outbox merge rule already carries double-frames.
- **Repack layout** (separate issue, CPU-only): split each region's entry list from its brick payload so a change that adds or resizes one mixed brick rewrites only the entry list, not the region's bricks. ADR 0006 already records the split ("a change that only drops/adds solid rewrites only the entry list") and the pain (211 MB written vs 73 MB before, ~3×, for ~10% more CPU).
- **Dropped from the draft**: voxy's compute scatter/memcpy apply passes. `queue.write_buffer` is already an async staged copy in wgpu; a compute scatter adds dispatches and barriers for zero benefit.
- **Cap value**: derived from P0's measured upload headroom, not voxy's 2 MB.

## 6. P2 — GPU soft-miss detector (gated, M1a-2)

- **Mechanism**: while tracing, the shader compares each touched brick's actual level against the **LOD band** rule's expected level for its distance (both already known from the packed scene + camera) and emits a want-record only for coarse-where-fine-should-be. This is the one signal only the GPU has, it measures the CONTEXT.md soft-miss bug class directly (with truth, not CPU accounting), and it feeds the loader's priority order via the existing `take_wanted → request` path once #143's priority classes exist.
- **Scope**: steady-camera residency ordering only. wgpu has no persistent-mapped buffers, so the readback (copy-to-staging + `map_async`, the profiler ring's proven pattern) delivers records 1–2 frames late — free when demand is frame-stable, useless during motion, where #143's CPU velocity prefetch is the right tool.
- **Dependencies**: #144 (edit epochs — a stale want must be validated exactly like a stale CPU loader arrival) and #143 (priority classes). Records are hints ordering priority only; residency authority never moves off the scene thread (ADR 0006).
- **Hard cap** on records/frame derived from the loader's per-frame job capacity, drop-oldest on overflow.

## 7. P3 — visibility stamps / GPU eviction (gated, expected to die)

CPU pass-stamped LRU already shipped (#141) with exactly the bounded part-wise shape the draft wanted. What GPU stamps would add — true-visibility ordering — mostly doesn't apply here:

- The **Shell** is view-independent, so "wanted this frame" ≈ every resident brick; LRU barely differentiates and band order dominates eviction anyway.
- True-visibility tiebreaks would make back-of-planet units look least-recently-used every frame — view-shaped eviction, the exact behaviour ADR 0004 rejected because a 180°/s turn needs those units back within seconds.
- **Gate** (in addition to §3's): does eviction choice ever cause a *measured* soft miss, and does the predicted-vs-actual want gap change eviction outcomes once #143's undercount is fixed?

**If P3 survives**, the trust migration is: performance validations retire in order — (1) not-wanted-this-frame (the GPU want-stamp is definitionally fresher), (2) LRU-in-band ordering, (3) job-in-flight (only with a GPU-visible job ledger) — each retirement earned by bench evidence (e.g. 1M frames, zero evictions of wanted/in-flight units, golden hash green). **Never retired**: the Guard-membership and over/beside-**Diverged brick** checks — those are data-safety invariants (a shader bug evicting a Diverged brick destroys the only persisted copy), not performance decisions, and they stay CPU-side permanently.

## 8. P4 — temporal depth pyramid (gated)

- **Scheme**: necessarily temporal — one ray per pixel means there is no other geometry to occlusion-test against; the pyramid is last frame's reprojected depth, with TAA-style disocclusion hazards and camera-motion compensation. More invasive than voxy's scheme, not less.
- **Where it could pay**: mid-altitude flight (100 m–2 km), where the horizon opens to several km and far rings and terrain self-occlusion coexist. At 4.8 m the 253 m horizon cap keeps far-ring work to sky rays plus 2 km peaks piercing the cap — a small pixel fraction. Consistent with the existing rejection for secondary rays.
- **Gate**: the §4 occlusion-savings estimate, decided per altitude band; interplay with #146 (horizon-cap culling) read from the same counter. The Horizon cap stays the authoritative global occluder; the pyramid only saves per-frame work.

## 9. P5 — indirect self-measuring dispatches (contingent)

Build only if P2 *and* P4 survive their gates *and* the streaming bench shows dispatch-size CPU round-trips as measurable cost — today there are none, since every dispatch is fixed screen-size. Capability is not the question (`dispatch_workgroups_indirect` is unconditional in wgpu 30.0.1/Vulkan); value is. Port voxy's screen-space-error descent test only as the generalization of the band rule if a data-dependent pass exists to consume it. Gate features by compile-test into a capability bit with fixed-size dispatch fallback, and compare both paths in `bench-budgets.toml`.

## 10. P6 — generator versioning (= #62, unsequenced)

Not a phase of this program. #62's SQLite design already carries the guard (`meta` with seed + `GENERATOR_VERSION`, `edited_bricks` with per-brick version, regenerate-unedited/keep-edited on mismatch). It lands with Diverged-brick persistence, CPU-only, green CI. The golden-hash extension (dump → bump version → reload → hash) belongs to #62's acceptance, not this plan.

## 11. Sequencing and issue disposition

```
M1a-1:  P0  streaming bench + extended metrics (close #143 undercount first)
M1a-2:  P1  #145 + defer-never-stall + repack-layout issue
        P2  soft-miss detector      (gate: P0 steady-camera soft misses; deps #144, #143)
        P3  visibility stamps       (gate: expected to die)
        P4  temporal depth pyramid  (gate: altitude-sweep counter)
        P5  indirect dispatch       (contingent on P2 ∧ P4 ∧ measured round-trip cost)
any:    P6  = #62, with persistence
```

| Work | Disposition |
| --- | --- |
| Streaming bench mode + extended metrics | New issue, filed now |
| Entry-list/payload split (repack amplification) | New issue, filed now |
| #145 | Extended now with the defer-never-stall cap semantics |
| #143 | Extended now with the P2/P3 gate criteria and the steady-camera soft-miss split |
| P2, P3, P4, P5 | **No issues until the P0 gates read out** — filing speculative issues for phases expected to die creates noise and stale `ready-for-agent` labels |
| P6 | Nothing new; #62 covers it |
