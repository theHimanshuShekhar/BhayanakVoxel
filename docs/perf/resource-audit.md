# Resource audit: does the game use the whole machine?

Opened by #264 (the owner's rule, 2026-11-27: the game and its tools use the CPU, GPU and memory
of the machine they run on to the full, and the low end runs as well as it can). The rule and what
it asks of a change are in `AGENTS.md`, "Use the whole machine". This file is the audit it points
to: every place where a serial path, a fixed thread count or a fixed budget was found, what it cost
**measured**, what it does at the low end and at the high end, and what was done or is left.

Add to it when you find another or change one.

## How it was measured

- **Machine:** the dev box, a Ryzen 7 5700G (its affinity gives `available_parallelism` 8), 16 GB,
  **no GPU** (lavapipe only), Linux. So **every number here is a CPU number on 8 threads**, and none
  is a GPU or a 4090 number. The 4090 PC's figures are what the PR for #264 asks the orchestrating
  agent to report (below).
- **Per-thread CPU:** `/proc/PID/task/*/stat` sampled every 250 ms (the process's CPU use and how
  many of its threads were running). A stage on "1.0 cores" is serial.
- **Low end:** `--profile low-end` (or `BHAYANAK_PROFILE=low-end`): 4 threads, 5 GiB of free memory (the
  8 GB machine's: Windows at idle leaves 4.5 to 5 GiB), 4 GB of video memory and no ray queries.
  The process is **pinned to 4 logical processors** (`sched_setaffinity` on Linux,
  `SetProcessAffinityMask` on Windows), so that the threads of it (the frame thread, the scene
  thread, the simulation, the driver's) compete for them as on a 4-thread machine. On Windows the
  process's mask binds every thread; **on Linux a thread can set its own mask, and Mesa's shader
  disk-cache thread (`client:disk$0`) does, so it runs on any processor** (a thread of little
  work; the game's threads stay pinned). The processors
  are the first 4 the process was allowed (here 2 to 5) and are reported in
  `machine.profile.pinned_cpus`. It is not a slower CPU, so the low-end times are what 4 threads of
  this CPU do. If the system refuses, the run goes on unpinned and says so.
- **Before** is `main` at 89273fd, **after** is this branch, the same box and seed (`0x5e17b93e79b28f9e`
  is the hash of the 2048-texel map at both, and at 1, 4 and 8 threads).

## The machine crate

Everything below that says "from the machine" reads it in `crates/machine` and nowhere else
(`available_parallelism` is called in one place). What it gives:

| | Default | `--profile low-end` | Floor |
| --- | --- | --- | --- |
| `threads()` | the machine's | 4 (or fewer if the machine has fewer) | 1 |
| `bulk_threads()` (work the frame thread must not wait behind) | threads − 1 | 3 | 1 |
| `loader_workers()` | threads − 2 | 2 | 1 |
| memory | the least of `MemAvailable` and the cgroup's room (v2 `memory.max` less `memory.current` less `inactive_file`; v1 `memory.limit_in_bytes` less `memory.usage_in_bytes` less `total_inactive_file`; the least over the process's own cgroup, from `/proc/self/cgroup`, and each ancestor up to the root); Windows `GlobalMemoryStatusEx` | 5 GiB | 1 GiB, in `bias_for` |
| processors | all the process may use | pinned to 4 of them | |
| video memory | not read (see below) | 4 GiB | none read |
| ray queries | the adapter's | off (the compute tracer) | |

Overrides, for a machine that has more than the profile says: `--profile NAME`, `--threads N`,
`--memory-mib N`, `--video-memory-mib N`, `--no-ray-queries` on the command line (before or after
the subcommand), or `BHAYANAK_PROFILE`, `BHAYANAK_THREADS`, `BHAYANAK_MEMORY_MIB`,
`BHAYANAK_VIDEO_MEMORY_MIB`, `BHAYANAK_RAY_QUERIES` in the environment. They only **lower** what the
machine has (`Profile::limited_to`); a profile never asks for threads that are not there. The client
prints the profile it runs with on its first line (`machine: ...`).

## Fixed here, and the numbers

| # | Where | Before | After | Low end | High end |
| --- | --- | --- | --- | --- | --- |
| 1 | `WorldMap` build, `crates/planet-gen/src/world_map.rs` (`erode`, `drainage`, `normalise`, `area_weights`) | **23.7 s** at 2048 on 8 threads: raw 4.7 s, normalise 0.15 s, **erode 14.6 s, drainage 1.4 s**, texels 2.8 s. On `main` the erosion already ran on all threads (`par_fill`: one contiguous share a thread, threads made again for each of its 128 passes) and was the slow stage because each texel found its 4 neighbours through `split`, `step` and `join`; `normalise`, `area_weights` and the drainage (a serial highest-first walk) were serial | **11.0 s** (12.6 s with the box busy): raw 4.95 s, normalise 37 ms, **erode 2.96 s, drainage 0.41 s**, texels 2.64 s. The same hash. **The erosion's gain is from reading the interior texels' neighbours by index (the seams still use `step`) and from fixed pieces, and I did not separate the two**; the drainage's is from the leaf-first rounds on all threads, `normalise` and `area_weights` from pieces | 4 threads pinned to 4 processors: 17.8 to 19.5 s, the same hash (4 unpinned, earlier: 17.1 s) | pieces are fixed: 8 192 texels (`PIECE`), 8 rows for the erosion, so more threads are used up to the pieces |
| 2 | The map's disk cache, `world_map/cache.rs` | every launch built the map: 24 s before the first frame | **hit 157 ms** (a 201 MB file, read and checked on all threads; 80 times faster than the build), written in 236 ms, the same hash | the same file. The key is the generator version, the seed, size and radius, and the hash of a 16-texel map that the build makes at the radius (a few ms) with a hash of its main constants: any input of the build that reaches a texel is another key, version bump or not | a corrupt, short or other-key file is rebuilt and written over; temporary files of killed launches older than 5 minutes are deleted on the next write; the folder keeps the 3 maps used last (a hit is a use); `BHAYANAK_NO_MAP_CACHE=1` turns it off |
| 3 | `caves::fill`, `bins/client/src/bench/caves.rs`, which the look-dev's shot build runs | **362.75 s on exactly one core** at the first shot (no worker alive): one thread made every child of every brick in turn | **92 to 131 s**, 3.4 to 3.7 cores. Children are made on `bulk_threads()` in rounds of 400 and held in the order the 400 were taken in. What is left is row 7 | 3 generating threads, the same order and bricks | bounded by row 7, not by generation (48 to 62 s of it on 7 threads) |
| 4 | `look_at_due`, `holes`, `par_map` and `map_shared`, `crates/render/src/scene/select.rs` | `MOST_THREADS = 4` (and half the machine's) for the look at due entries; `PARALLEL_FROM` 32 768; the holes: every thread walked the whole table to take every n-th node (more threads were no faster); a share a thread, so a share with a slow node left the rest idle | all `bulk_threads()`; pieces of a fixed 2 048 entries (`LOOK_PIECE`), the next free thread takes the next, results in piece order; `PARALLEL_FROM` is two pieces; the holes list the split keys in one pass and share them in pieces | 3 threads | 31 on a 32-thread machine, not 4. **Not measured on the 4090** |
| 5 | `par_bricks` (the spawn ring, `generate_region`, `generate_planet`, `Planet::guard`) and `Planet::guard_in` (the Guard's cells), `planet-gen` | slabs of constant x (about a hundred for the spawn ring, so that a slab that held the surface left the others idle at the end) | columns of constant x and y, the next free thread takes the next; a cell a piece in `guard_in`. Output in the order of x, y, z whatever the threads (it was in no order) | `threads()` = 4 | all `threads()` |
| 6 | `loader::workers_for`, `GUARD_WORKERS` | `cores − 2`; the Guard's background share 2 workers whatever the workers | `workers_for` is `machine::loader_workers_for`, so a constrained profile gets 2 workers on 4 threads; the Guard's share is `max(2, workers / 3)` (`guard_share`) | 2 workers; the Guard has all but one when the queue is empty, one beside it | 30 workers on a 32-thread machine, 10 on the Guard beside the queue (was 2) |
| 7 | **Not fixed:** `Selection::arrived` and `Scene::arrive`, the scene thread's tree update (`select.rs:1911`, `rings.rs`) | 43 to 54 s of the shot fill's 92 to 131 s, on one core | the same: 40 s of it is `revisit_whole_as` (about 17 ms per round of 400 parents), 11.7 s the recheck of the neighbours; `show_diff` (region repacking, `rings.rs:1316`) another 10 to 11 s | | follow-up 1 below |
| 8 | The LOD bias the client starts at, `bias_for` in `bins/client/src/main.rs` | 1.0 whatever the memory | 1.0 where the machine has the room (the peak is estimated as `MEMORY_FIXED` 830 MiB + `MEMORY_SCALED` 8030 MiB / bias², fitted to the 4090's measurements, and may be 90% of the memory: **9.7 GiB free**; the 4090's 18.7 GiB has the room); coarser in the square root of the shortfall, up to 3.0, below that. The tables (`room_for`, `budget_for`, `expected_words`) scale with `d0²`, so a larger bias is a smaller resident set. A multiple of 0.05. **The report says it** (`machine.profile.starting_bias`) and a full-size real-time run of the default profile that did not start at 1.0 has `starting_bias` in `gates.over`: the gates are budgeted for 1.0 | the low-end reference (5 GiB) gets **1.5** (estimated peak 4.3 GiB, 86% of it; it was 1.35 and peaked at 5228 MiB, over the 5 GiB, before the 4090 run recalibrated the estimate); never fails, never below the Guard and the ring | the ceiling is the default 1.0: spending spare memory and GPU on quality is #230's |
| 9 | The constrained profile's ray queries, `request_gpu_for` in `crates/render/src/lib.rs` | the adapter's | `ray_queries = false` removes `EXPERIMENTAL_RAY_QUERY` from the requested features and the experimental opt-in from the device, as the compute setting does (#260), so the compute tracer runs on any machine | the compute tracer | the adapter's |

Rows 1 to 3 are what the owner saw: the load, and the look-dev's shot build.

The 4090 PC (16 threads) measured this branch (56a26fb) against `main` (89273fd), back to back, default profile, all gates passing, two runs: Guard 16.0 s to 8.4 s; the map at launch 15.5 to 16.0 s to 6.8 s cold and 0.09 to 0.12 s from the cache; time to control 1.10 s to 1.10 and 1.14 s; scene lag d0 max 1.49 to 1.46 to 1.52; frame time p99 9.78 ms to 9.62 to 10.4 ms; fill 119 s to 120 s; `resident_peak_mib` 8508 to 8607 to 8377 to 8850. `map-build`: 6.50 s cold on 16 threads, 23.1 s cold on 4 pinned threads, 81 ms from the cache, the same hash. During the load 15 threads ran at about 40% and one, the scene thread (follow-up 1 below), at 100%. The low-end profile (pinned to CPUs 0 to 3, bias 1.35) completed with no holes, over its soft-miss and time-to-control gates (108 frames, 2.25 s; #230 sets those), and its `resident_peak_mib` of 4941 to 5228 was over the 5 GiB: the memory model of item 8 was recalibrated from it (see the checklist below).

## Not fixed: what is not cheap, and what it measured

For the owner. None is filed (the ticket said to list them).

1. **`Selection::arrived` and `Scene::arrive` are serial on the scene thread** (row 7). 54.5 s of
   the 131 s of the shot fill in the run that measured the split, of which 40.3 s is
   `revisit_whole_as` and 11.7 s the recheck of the neighbours, and `show_diff` another 11 s. A round of
   400 parents costs about 17 ms there and about 2 700 rounds fill the Planet around the shot. A
   parallel plan like `new_reached`'s (plan each parent's update on the threads, put them in the
   tree in order) is the way, but it touches the scene thread's design (ADR 0006), so it is a
   ticket of its own. A round of 4 000 instead of 400 took the fill from 110 s to 92.65 s (the
   part of `arrive` that grows with the number of rounds falls) but it changes the order the bricks
   are loaded in, which the look-dev's stills depend on, so it is not done here.
2. **Video memory is not read.** `wgpu` does not expose it. `Profile::video_memory` is only what
   `--video-memory-mib` or the low-end profile's 4 GiB says, and **no budget reads it**: the scene's buffers and the
   resident set are sized from the system memory and `d0²`. A read through the adapter's raw handle
   (Vulkan's memory heaps, DXGI's `QueryVideoMemoryInfo`) is a ticket; the 4 GB low-end figure is
   what the compute tracer's budgets should then be checked against on a GTX 1650.
3. **The frame-time budgets are fixed and were measured on the 4090 only:** `UPLOAD_BUDGET` (8 MiB
   and 2 048 writes a frame, `scene_worker.rs:113`), `hw::BUILD_BUDGET` (1 << 18 boxes of BLASes a
   frame, `hw.rs:102`) and `batch::BATCH_BUDGET_MS` (12 ms of the 16.7, `batch.rs:39`). They bound a
   frame's CPU or GPU time and not a resource, so a faster machine can only go under them, but a
   much slower one has no smaller figure. Scaling them needs measurements on the low-end GPU and
   on the 4090, and #230 owns the quality a faster GPU buys. They are not touched, which is why the
   PR has no `needs-benchmark` label: no GPU timing changes.
4. **`MAX_AHEAD = 512`** (`loader.rs:60`): the jobs the workers may run ahead of the frame thread's
   taking in. It is 256 a worker on the low end's 2 workers and fewer a worker the more there are. It is
   not known to bind on either; it is a bound on the queue's age (ADR 0009), not on the CPU.
5. **`PLAN_CHUNK = 1 << 16`** (`select.rs:472`) is the size of a chunk of the scene's first plan,
   which runs a chunk ahead of putting the last in the tree: a fixed piece, as the rule asks, with
   a pipeline two deep whatever the machine.
6. **The simulation and the server** are one thread each: by design (ADR 0003: the tick is
   deterministic and in order). Not a defect under the rule, but a tick that grows past its time
   would be the first thing it finds.
7. **`prefetch-sim`** runs a thread per path or variant (`std::thread::scope`, `report.rs:260, 275,
   525`): bounded by the handful of paths, so a 32-thread machine uses a fraction of itself and a
   4-thread one oversubscribes a little. It is a tool for the developer's box and not on the path of
   anything slow measured here; a `machine::par_map` would do and is cheap.

## What the 4090 run has to report

For the `needs-stream-bench` run, on **the default profile and on `--profile low-end`** (the same
Windows artifact: `client.exe --profile low-end bench --streaming --real-time`):

- **The map build, cold and cached.** `client.exe map-build` builds cold, writes the cache, reads it
  back, checks that the hashes agree and prints the times and the stages; then the real-time run
  once with the cache warm (the cache is in `%LOCALAPPDATA%\bhayanakvoxel`, or `BHAYANAK_CACHE_DIR`).
- **The Guard time** (the cold start's Guard) and the **time to control** (the 20 m ring).
- **The CPU utilisation during the load:** per-thread CPU of the client process from the start to
  the first frame (how many threads were at 100%, not Task Manager's total). Before this change the
  map's erosion and drainage ran at about one core for 16 s of the 24 on the dev box.
- **`resident_peak_mib` on the low-end profile:** it must be at most 90% of the profile's 5 GiB
  (4608 MiB). The 4090's first run of this PR (56a26fb) measured the memory model wrong: the
  estimate was 7 GiB at bias 1.0 (the 6.66 GiB after the fill, #167, which is not the run's peak),
  but the 4090 peaks at 8377 to 8850 MiB at 1.0 and the low-end run, at the 1.35 that the estimate
  chose, peaked at 4941 to 5228 MiB, over its 5 GiB (the estimate said 3.9 GiB). The model is now
  `MEMORY_FIXED` 830 MiB + `MEMORY_SCALED` 8030 MiB / bias², fitted to those two peaks (8850 at 1.0,
  5228 at 1.35), and the starting bias is the least multiple of 0.05 that puts the estimate under
  90% of the memory: **1.5 at 5 GiB, an estimated 4399 MiB (86%)**; 1.0 from about 9.7 GiB free (the
  4090's 18.7 GiB). This run, at 1.5, checks it again. The report's `machine.profile` says the profile, the
  processors the run was pinned to, the memory found and the starting bias; the Windows memory read
  (`GlobalMemoryStatusEx`) is run by CI's `windows-tests` (the `machine` crate's, since #268's fix
  added `-p machine` to them) and by this run, and `machine: ...` on its first line says what it
  found. The pinning (`SetProcessAffinityMask`) is run by a test of the `machine` crate on the
  Windows runners, which pins that test's process to one processor and puts the mask back.
- **The scene's lag and the scene thread's latency**, against `main`: `gates.scene_lag_d0_max` (and
  every latency of the scene thread the report has: the paths' `scene_lag_d0_max`, the settle times,
  the soft misses of the scene thread). The look at due entries went from at most 4 threads at normal
  priority, starting at 32 768 entries, to `bulk_threads()` at the lowered priority of the client's
  workers, starting at 4 096. It should be faster on the 4090 (more threads) and is not measured
  there: the run compares it with `main`'s, and a rise is this change's.

## Rules this file keeps

- Every budget derived from the machine has a floor (the low end degrades and never fails) and a
  ceiling (a big machine uses the work it has), and a test at both (`bias_for`, `workers_for` and
  `guard_share`, and the `machine` crate's).
- Parallel code here is deterministic: pieces of a fixed size, reduction in piece order, no shared
  RNG or atomic id counters or `HashMap` order in output. The tests that hold it: the map is the
  same at 1, 4 and all threads and as it was before the build was parallel (`world_map`), the
  drainage of rounds is that of the texels highest first (`world_map`), the cache reads back to the
  same hash (`world_map::cache`), and the bricks of a box and the Guard's cells are the same at 1,
  4 and all threads (`planet-gen`).
