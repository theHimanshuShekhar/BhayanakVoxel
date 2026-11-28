# Benchmarking

`client bench` is the project's performance loop (#13): it flies a fixed camera path over the test Planet, times every GPU pass, runs the streaming and convergence checks, and writes `report.json` and screenshots. It answers "is this PR within the budgets?" with numbers measured on the dev's RTX 4090 at 1440p. CI runs it as a smoke test on lavapipe (software Vulkan): the report's shape is checked and its timings mean nothing.

Where to look:

- The numbers that gate: `bins/client/bench-budgets.toml`. The code that holds them: `bins/client/src/bench/budget.rs` and `realtime.rs`.
- The options: `client bench --help`.
- What a report field means: [`docs/perf/bench-report-reference.md`](../perf/bench-report-reference.md).
- Past runs, per-PR results and superseded decisions: [`docs/perf/bench-history.md`](../perf/bench-history.md).

## Which PRs are benchmarked

The 4090 is the gate, and it is slow (the real-time run takes tens of minutes). The cadence (owner, 2026-11-28):

| PR | What it gets |
| --- | --- |
| Shaders, passes, the tracer, the scene's GPU layout, GPU uploads | `needs-benchmark`: its own 4090 run of `client bench` before merge. |
| The loader, prefetch, eviction, the scene thread's streaming | `needs-stream-bench`: its own 4090 run of `client bench --streaming --real-time` before merge. |
| Both kinds of change | Both labels. One real-time run answers both. |
| A terrain or generator PR | Merges on green CI, plus the dev box's generation cost in µs per brick staying within the budget of #247. The measure arrives with the first terrain build ticket; this file names its command when it exists. |
| Any other PR, including CPU-only work that is performance-sensitive (brick packing, the sim hot path) | Merges on green CI. The bench measures GPU passes and cannot see CPU-only work: CI targets and tests guard its throughput. |

**A group run, in place of a run for each PR.** Once per group of 3 to 4 merged tickets, and at the end of a wave, the orchestrator runs `client bench --streaming --real-time` on the 4090 against `main`. It is not run for each PR.

- If a group run fails, the orchestrator bisects it with the merged commits' CI artifacts (`bench-pr.sh` with `SHA=<commit>`, a script on the orchestrator's machine and not in the repo) and fixes forward before the next group starts.
- One optimisation pass per group, at the group boundary.

**The PR body** says which label applies and why, which passes or gates it expects to change, and **what the 4090 must report** for it: the report fields and the limits they are held to (for example "`passes[shadows]` mean at most 0.4 ms; `streaming_real_time.gates.over` empty; `convergence.shadows.first_frame.flip.flip_mean`"). A ticket's acceptance names the fields in the same way. A PR that deliberately changes a budget in `bench-budgets.toml` says so and why.

## What the orchestrator does

1. Check the PR's comments first, so that a head is not benched twice.
2. Run on the 4090 (over SSH, from the PR head's `client-windows` CI artifact, with no `--no-fail`, so that the exit status shows the gates):
   - `needs-benchmark`: `client.exe bench --out bench-out`.
   - `needs-stream-bench`, and every group run: `client.exe bench --streaming --real-time --out bench-out`. Run it once per invocation: `--runs` defaults to 3 inside the process, and the report keeps each run with the least and the most of every number (`min`, `max`) and gates on the worst. A PR that changes the load or the loader also runs it with `--profile low-end` (below).
   - A PR whose acceptance asks for convergence curves: add `--truth-spp 8192` (ADR 0005's baseline, about 2.5 minutes for the phase).
3. Post `report.json` in a PR comment with a screenshot or two, and the fields the PR body asked for.
4. A pass or gate over its budget blocks the merge. Remove the label once the report is in and nothing is over; merge on green CI and review.

Run a release build (the artifact is). A debug build's CPU side distorts the frame times. Do not run a bench while the owner is gaming; the 4090 runs with both clocks locked and the live wallpaper stopped (`bench-pr.sh` does it).

Report headers that decide whether the numbers mean anything: `machine.device_type` is not `Cpu`, `machine.profile` says which profile ran, `config.bias` is 1, `config.diag` is empty, and the report's `rt.active` is `hw` on the 4090.

### Flags that matter

| Flag | Use |
| --- | --- |
| `--out DIR` | Where `report.json` and the screenshots go (default `bench-out`). |
| `--streaming` | Also run the stepped streaming mode on the test Planet: deterministic counts, a hash, a hole in any frame is status 2. CI runs it. |
| `--real-time` | With `--streaming`: the full-size 320 km Planet with real loader workers and scene thread on the machine's clock, along the paths, `--runs` times. Not deterministic; for the 4090. |
| `--runs N` | With `--real-time`, runs in this process (default 3). |
| `--no-fail` | Exit 0 over a budget or gate. For sweeps and diagnosis. An error still exits 1. |
| `--profile low-end` | Before the subcommand (`client --profile low-end bench ...`, or `BHAYANAK_PROFILE=low-end`): 4 threads, 5 GiB of free memory, 4 GB of video memory, no ray queries (the compute tracer), the process pinned to 4 logical processors. The project's low end (GTX 1650 / RX 570 class) on any machine. `machine.profile` in the report says what ran. A constrained report is not comparable with the budgets: it shows whether the low end streams, holds its memory and loads in reasonable time. `--threads N`, `--memory-mib N`, `--video-memory-mib N` and `--no-ray-queries` lower one thing at a time. |
| `--gen-extra-us N` | With `--real-time` (#242): N µs (0 to 1000) of artificial CPU for each brick generated, at every level. For the headroom curve of the streaming gates before a costlier terrain. The report's `gen_extra_us_measured` should be near `gen_extra_us_asked`; far from it, the machine was too busy for the point to mean what it says. |
| `--peak-m M`, `--trench-m M` | With `--real-time` (#242): today's world map with its heights stretched to peaks of M m (default 2000) and trenches of M m (default 1000); either alone changes only that side. A cost proxy for a taller ground column. `report.relief` says what ran. |
| `--rt-backend auto\|compute\|hw\|hw-nearest\|both` | How rays find the Planet. `auto` (default) is what the game does: hardware ray queries where the adapter has them, else compute with the reason in `rt.fallback`. `compute` measures the player's compute setting. `hw` must not fall back (an error in the real-time mode on an adapter without ray queries). `both` compares them. |
| `--no-convergence` | Leave out the convergence phase (it runs by default, about 45 s at `--truth-spp 1024`). |
| `--short` | A tiny run that checks the bench runs and the report's shape. CI uses it; its timings mean nothing. |
| `--diag LIST` | `no-follow`, `no-upload`, `no-flush`, `gap-ms=F`: finding out where time goes. The budgets are not checked, and it is an error with `--real-time`. |

A headroom sweep is one process per point, each `client.exe bench --streaming --real-time --no-fail --out sweep/NAME --gen-extra-us N --peak-m M --trench-m M`, then, per report, `streaming_real_time.gates` (`over` and each gate's `worst` against its limit) and the cold start, path and frame-thread figures.

Other flags (`--size`, `--bias`, `--budgets`, `--truth-spp`, `--shadow-rays`, `--shadow-weight`, `--converged-frames`, `--flip-gamma-2-2`, `--flat-ambient`, `--stream-slice-ms`, `--hw-group-regions`, `--hw-build-blas`, `--hw-wait`, `--time-of-day`, `--day-cycle`, `--day-seconds`) are in `--help`, and each is described in [the reference](../perf/bench-report-reference.md#running-it). **A report that changes `--size` or `--bias` is not comparable with the budgets.**

## Exit status

- **0**: every budget and gate is met (or `--no-fail`).
- **2**: a pass is over its budget, or has a budget and could not be timed (`no-data`: the GPU has no timestamp queries); with `--streaming`, a frame had a hole; with `--real-time`, a gate of `[streaming]` is over; the convergence phase's Primary-against-truth gates are over (`convergence.gi.gates.over`); or, with `--day-cycle`, a frame or the p99 is over 16.7 ms.
- **1**: the bench could not run (no Vulkan device, an unreadable budgets file, a bad option, `--real-time` with budgets that have no `[streaming]` table), or a real-time run stopped on an error. That is 1 even with `--no-fail`; `report.json` is still written with what was done and `streaming_real_time.error`.
- A panic in the normal bench before the real-time mode ends the process with status 101 and no report; a process abort leaves only stderr.

## The gates and where their limits are

A pass is `within` when its **mean** over the run is at most its budget; `p95_ms` and `max_ms` are for stutter. The pass and total budgets (`[passes]`, `[gpu_total]`) are GPU milliseconds on the 4090 at 1440p output (1280x720 traced): `primary`, `reference` (one sample, catches a gross regression), `upscale`, `shadows`, `atmosphere`, `gi`, `denoise`, the totals, and, as CPU time, `edits` (the Edit stress's mean per frame, hardware acceleration-structure update included) and `edit_frame_thread` (its longest Edit). A pass with no budget is `no-budget`; a stage's budget is uncommented when its pass exists, named as the pass is.

The `[streaming]` gates are held to the worst of the runs of `--streaming --real-time`, and `gates.over` names the ones that are over. All limits are in `bins/client/bench-budgets.toml`; do not copy them here.

| `gates.over` | Means |
| --- | --- |
| `frame_time`, `frame_cpu` | p99 of a path's frame time (CPU, then the wait for the GPU, with no presentation, so it is the pessimistic figure) and of the frame thread's CPU alone. |
| `holes` | A Split brick with its children not resident, found by the scene thread's looks (sampled every 5 s and forced at the end of each path, so a hole that lasts less than that is not seen). No look at all is over. |
| `memory` | The scene's buffers in use (`scene_used_peak_mib`), not VRAM, and the store's size against its own limit. |
| `soft_misses` | Frames in which the ground straight below the camera is drawn coarser than the speed-scaled bands ask, summed over the flights at 2 m and 50 m, the take-off and the turn (ADR 0009). |
| `scene_lag` | How far behind the camera the drawn scene is, in `d0`, at the most over those flights. Its limit is main's plus a margin, to tighten once the follow fixes land (comment in the budgets file). |
| `spawn_ring`, `time_to_control` | Seconds to make the 20 m ring round the Spawn, and to the window opening (the ring, the visible part of the Guard, the first scene). |
| `settle` | Seconds from the camera stopping after a flight to a scene that is at the camera with no soft miss. A run unsettled at the 120 s cap is over. |
| `gi` | Primary's `gi` pass mean and p99 on the full-size Planet. No timed path is over. |
| `starting_bias` | A full-size default-profile run whose client did not start at LOD bias 1.0 (the gates are budgeted for 1.0). A `--profile low-end` run is not held to it. |

Reported and not gated: the time to level 0 under the camera, the scene thread's own soft misses, the Guard's completion time and Edit latency (ADR 0006's bound is held by tests; the 4090 gives the number).

**The convergence phase** (ADR 0005) scores Primary and the Reference against 8192-sample truths with FLIP. Its gates are in `convergence.gi.gates`: Primary's settled FLIP mean at most 0.05 in every view (`primary-flip:VIEW`), the GI image no further from truth than the flat one (`no-further:VIEW`), the dig settling within 500 ms (`settle`), and the emissive room's Reference reaching FLIP 0.05 within 0.3 s of GPU time (`reference-room`; owner, 2026-11-20). The outdoor views' still-view acceptance (FLIP at most 0.05 at 0.25 s) is reported ("met" or "NOT met"), and a regression is a curve worse than main's by more than the floor, compared per checkpoint from two reports with the same `--truth-spp`. A truth is for the machine, driver and build that made it: never compare one between machines, and never put one in `tests/golden/`.

**A regression in a pass whose work did not change** (`upscale` does not read the Planet) points at how the frames are paced, not at the pass: the bench draws a frame at a time and the GPU idles while the CPU follows the camera. Run with `--diag` and compare: `no-follow` bringing `upscale` back to main's means following the camera; `no-follow,gap-ms=N` (N about `scene_update_ms`) staying high means the idle gap; `no-upload` staying high means the CPU's time, not the writes; `no-flush` bringing it back means the writes being sent ahead of the frame.

## The machine profile and the world map cache

The client and the bench size their threads and memory from the machine (`crates/machine`; `docs/perf/resource-audit.md`). Every run prints `machine: ...` first. The 320 km world map takes 11 s to build cold on the dev box's 8 threads and is cached in a file afterwards (`BHAYANAK_CACHE_DIR` sets the folder, `BHAYANAK_NO_MAP_CACHE=1` turns the cache off). A report that wants the cold figure runs `client map-build` first, or runs with `BHAYANAK_NO_MAP_CACHE=1`.
