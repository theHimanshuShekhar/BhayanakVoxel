# Benchmarking

`client bench` is the performance loop of the project (decision in #13). It flies a fixed camera
path over the test Planet, times every GPU pass, and writes a JSON report and screenshots. It
exists so that "is this PR within the frame budget?" has a number for an answer, measured on the
dev's RTX 4090.

## The `needs-benchmark` label

- **Agents** add `needs-benchmark` to every PR that can change GPU timings: shaders, passes, the
  tracer, the scene's GPU layout, or what is uploaded to the GPU and how much of it. They say in
  the PR body which passes are expected to change.
- **CPU-only PRs do not get the label**, even when they are performance-sensitive (planet
  generation, brick packing, the sim hot path): the bench measures GPU passes, so it cannot see
  them. Their throughput is guarded by CI targets and tests instead, and they merge on green CI.
- **The orchestrating agent** runs the bench on the RTX 4090 over SSH from the PR's
  `client-windows` artifact (below), and posts `report.json` in a PR comment, together with a
  screenshot or two. It checks the PR's comments first, so a report is not made twice. The dev can
  do the same by hand.
- **A pass over its budget blocks the merge** (#13). The agent removes the label once the report is
  in and nothing is over, and merges on green CI and review. Budgets are in `bins/client/bench-budgets.toml`; a PR that deliberately
  changes one says so and why.
- **Streaming gates** (decided 2026-11-10): on the full-size Planet the bench also gates frame
  time p99 (16.7 ms), frame-thread CPU p99 (8 ms), holes (none) and memory (#162, "The real-time
  mode" below); soft misses at the speed-scaled level (none, ADR 0009), time to control and
  settle time are reported by that run and become gates in #168. The part of this that CI can do,
  with counts and no times, is the stepped streaming mode of #181 (`--streaming`, below); the
  full-size run in real time is `--streaming --real-time`, on the 4090.
- **`needs-stream-bench`** goes on a CPU-only PR that touches **the loader, prefetch, eviction or
  generation** (`bins/client/src/loader.rs`, the scene thread's streaming, the store's
  eviction, `planet-gen`), since the `needs-benchmark` run does not stream and CI sees counts but
  no times. The orchestrating agent runs `client bench --streaming --real-time` on the PR's
  Windows artifact on the 4090, exactly as for `needs-benchmark`, posts the report's
  `streaming_real_time` (the gates, and the paths' figures) in a PR comment, checks the PR's
  comments first so it is not run twice, and removes the label once nothing is over. A PR that
  touches both GPU timings and streaming gets both labels, and one run answers both. A gate over
  its budget blocks the merge like a pass over its budget.
- CI runs the bench too, but only as a smoke test on lavapipe (software Vulkan): it checks that
  it runs and that the report has the right shape, and its timings mean nothing.

## Running it

From the Windows artifact, which needs no Rust toolchain and no other files:

```
client.exe bench --out bench-out
```

With a checkout, `cargo run --release -p client -- bench --out bench-out` does the same. Build in
release: a debug build's CPU side is slow enough to distort the frame times.

| Option | Meaning |
| --- | --- |
| `--out DIR` | Where `report.json` and the screenshots go. Default `bench-out`. |
| `--budgets FILE` | Use this budgets file instead of the one built into the executable. |
| `--size WxH` | Resolution of the traced (internal) image. Default `1280x720`, the internal resolution behind 1440p output that the budgets are for; primary mode upscales it to twice that in each direction (2560x1440). Do not change it for a report you paste. |
| `--bias F` | LOD bias: output pixels one Voxel may cover, default 1. Every switch distance between levels of detail scales with it (see `CONTEXT.md`, LOD band); 2 does about a quarter of the work for up to a pixel more of error. The report records it. A report with another bias is not comparable with the budgets. |
| `--diag LIST` | Switches for finding out where time goes, separated by commas: `no-follow` (the Planet does not follow the camera), `no-upload` (it does, on the CPU, but the GPU is not told), `no-flush` (writes to the GPU go with the next frame's commands, not at once) and `gap-ms=F` (sleep F ms before each frame, as the CPU work of following would). The report lists them in `config.diag`, the summary warns, and the budgets are not checked (`budgets.skipped` says why; the exit status is 0). See "Reading a regression" below. |
| `--short` | A tiny run (160x90, a handful of frames, a patch of the Planet): the CI smoke test. |
| `--streaming` | Also runs the streaming mode (below): the test Planet streamed in, stepped, on a fixed frame schedule, with counts that are the same on every run. The report's `streaming` is null without it. |
| `--real-time` | With `--streaming`, also runs the real-time mode (below): the full-size Planet with the real loader workers and scene thread, on the machine's clock, along paths of 50 m/s flights, a take-off, a turn, a stop and a cold start. It takes tens of minutes and is not deterministic. With `--short` it runs on the test Planet, as a smoke test. The report's `streaming_real_time` is null without it. |
| `--runs N` | With `--real-time`, how many times to run the paths in this process. Default 3. The report keeps each run and the least and the most of every number over them; the gates use the worst. |
| `--no-fail` | Exit with status 0 even if a pass is over budget, a frame of the streaming mode has a hole, or a gate of the real-time mode is over. |

It runs unattended and takes a minute or so (generating the whole Planet is about 22 s of it).
The report is written to `DIR/report.json` and also printed on stdout; progress and a table go
to stderr.

**Exit status:** 0 if every budget is met, 2 if a pass is over its budget (or has a budget but
could not be timed) or, with `--streaming`, if any frame had a hole, or, with `--real-time`, if a
gate of the real-time mode is over (below); 1 if the bench could not run (no Vulkan device, an
unreadable budgets file, a bad option, or `--real-time` with budgets that have no `[streaming]`
table).

### What it does

1. Generates the test Planet and uploads it to the GPU, each brick at the level of detail its
   distance from the camera says (the LOD bands, ADR 0002), and prints how much GPU memory they
   take and how many bricks each level has. The bands follow the camera along the path: selecting
   again and packing the bricks that change level, and writing them to the GPU, is done before a
   frame is timed, so it is in no pass and not in the frame's CPU time either.
2. Flies the camera path in **primary mode**, then in **reference mode** (the progressive path
   tracer, 4 samples per frame of the path). Each mode first draws a few warm-up frames, which are
   not measured: the first frames after a mode switch carry the other mode's state (see the notes
   on #28).
   Primary mode traces at `config.width` x `config.height` and upscales to `output_*` (passes
   `primary` and `upscale`); reference mode traces at the traced size, with no upscaling. Primary
   mode reads an image back only for a screenshot (a 1440p image is 15 MB), so that the frame is
   not mostly readback with the GPU idle in between; `frame_time` is then the CPU time of the
   frame as drawn, and a screenshot frame is slower.
3. Writes a screenshot at each keyframe of the path, per mode.
4. Then, after the flight, holds the camera still at four heights above the Planet's pole, over its mean radius
   (4.8 m, which is the ground, 100 m, 500 m and 2 km), looking at the ground that slopes away
   towards the horizon, and times both modes there for a few dozen frames each (24 primary frames
   and 6 reference frames of 2 samples). That is the report's `altitudes`. It takes a few seconds
   and leaves the flight alone, so the flight stays comparable with the reports of main. `--short`
   has no sweep.
5. Then, for the caves (#46), holds the camera still at a **cave mouth on a slope** and **inside a
   tunnel**, times both modes there like the sweep (24 primary frames, 6 reference frames of 2
   samples), and takes a screenshot of each mode (`primary-cave-mouth.png`,
   `reference-cave-tunnel.png`, ...). That is the report's `caves`. The caves are the real
   Planet's, and the flight is over the small test sphere, which has none: so the stills are of
   the real Planet on a coarse world map (512 texels a face, about a second to make), round the
   steepest mouth with a good deal of cave by it (`Planet::cave_mouths`), with a ring of level 0,
   the Guard from level 10 and the bands for a `d0` of 10 m, a tenth of the flight's. That keeps
   the view to seconds to make, and the numbers say what the cave costs, not the far ground. The
   flight and the sweep are left alone. `--short` has no stills.

6. With `--streaming`, after everything else (the cave stills included, so that the flag cannot
   move their timings), the streaming mode (#181), on a GPU of its own. See "The streaming mode"
   below. With `--real-time` as well, the real-time mode (#162) after it, on a world and a GPU of
   its own for each run. See "The real-time mode".

The flight above does not stream: it draws a Planet that is generated whole, so the ring loader
(#86) and its uploads are not in these numbers. The loader's own costs (the spawn ring, the Guard, how
long the bands take to fill, and the frame thread's scene work per frame while walking, worst and
p99) are read from the ignored full-size test:
`cargo test --release -p client -- --ignored --nocapture full_size`. The scene's selection and
packing run on a thread of their own there (ADR 0006), so those numbers are what the frame thread
pays for it: copying what the scene thread made. The bench's `scene_update_ms` is still the whole
of following the camera, done inline.

### The streaming mode

`--streaming` runs the loader, the rings' recentring, the scene thread's packing and the copy to
the GPU on the test Planet (the world map of 512 texels a face, the one the stepped tests use),
which the flight above leaves out. It runs **stepped** (#180, `docs/agents/ci.md`): no threads, no
sleeps, and a clock that only the bench moves, a frame (16.667 ms) at a time, so the work of each
frame does not depend on how long anything took. The code is the game's (`World`, `Loader`,
`SceneWorker`); the game starts threads and never steps, and its clock is the machine's (see
`bins/client/src/clock.rs`). The flight above does not use the world or the clock at all.

The schedule is fixed by frame index (`bins/client/src/bench/stream.rs`): the camera at the spawn
until the resident set is complete (**fill**); then 60 frames standing (**steady**) and 180 frames
walking at 24 m/s along three segments (**moving**), with an Edit at frame 150 of them and the
store held to 102% of the complete set so that the walk evicts. The bands have a `d0` of 64
Voxels, so that the fill is a few seconds, and the Guard is the game's (levels 9 to 13).
Everything in `streaming` is made by that mode and is the same on every run and every machine,
except `timing`.

`streaming` has `deterministic: true`, the `schedule`, and:

- `counts_hash`: FNV-1a of the counts of every frame and of every integer, boolean and name in
  the rest of `streaming` (all but `timing`, and itself): the report and each frame's counts are
  hashed as they are serialized (`hash_json` in `bench/stream.rs`), so a field added to either is
  in the hash without anyone listing it, and a test (`every_integer_of_the_report_and_of_a_frame_is_in_the_hash`)
  changes each integer in turn and fails if the hash does not move. **Floats are left out** (a
  platform's maths library may differ in the last bit); each is worked out from integers that are
  hashed (the sizing values from the `sizing.measured` counts, `band_start` and `d0` from the
  constants). It is the one number to compare: `bins/client/src/bench/streaming-counts.golden` has the committed value, a unit
  test (`the_counts_are_the_committed_ones`, which runs on Linux and on Windows, since it needs no
  GPU) and `tests/bench.rs` check it. When a change to the streaming code moves the counts on
  purpose, the test prints the new hash; put it in the file and say why in the PR.
- `upload`: bytes written to the GPU per frame (`bytes`: `total`, `max`, frames and frames with
  any), `patch_bytes` (what patches wrote, a rebuild's all-buffers write excluded), `rebuilds` and
  `updates`. They are worked out on the CPU (`render::tracer::upload_bytes`); `timing.gpu_uploaded_bytes`
  is what the GPU counted, and the smoke test checks that they are equal.
- `soft_misses`: bricks drawn whole although the bands split them, per frame, for `fill`, `steady`
  and `moving` (frames, total, max, frames with any), and the total by the level of the brick
  drawn whole. Counted for every frame, not a few times a second. The fill has them; with the
  camera steady or moving on the margins and the lead, there are none today.
- `rings`: for each level (a Ring), where its LOD band starts, and the bricks drawn at the first
  frame, when the resident set was complete, and at the end.
- `work`: what the scene thread did, in steps and bricks, not milliseconds: `follow` (selecting
  again for a new camera), `pack` (taking in what the loader made), `upload` (frames with an
  update, and bytes: patches are words, not bricks) and eviction.
- `residency`: the Diverged bricks (those an Edit changed), the store's size as its budget counts
  it (at the start, when complete, the most, at the end) and the budget, and two latencies, which
  are not the same thing:
  - `edit_latency_frames` (**Edit latency**): frames from the frame in which the Edit was made to
    the frame that took it in from the scene thread (the first in which `World::edit_taken`
    holds, which is not when the wait for it ended: a timeout ends that too). The Edit is made
    before the frame's look at the scene, where the game makes it (its input is handled before
    `recentre`). **A stepped scene thread finishes its round inside that look**, so the frame that
    made the Edit takes it: the field is **0**, and it cannot show ADR 0006's bound (an Edit made
    in frame N shows in frame N+1 if the real thread has packed it by then, which a thread that
    takes time can fail to do). The bound is held by the stepped test
    `an_edit_a_busy_scene_thread_has_not_packed_is_waited_for_edit_wait_and_shows_in_the_first_look_after_it_has`
    and the real-thread tests, and measured in real time by #162's run on the 4090. The field is
    there to show a change that makes an Edit later than the look that made it.
  - `outbox_latency_frames` (**Outbox latency**): frames that a result the scene thread published
    waited in its outbox (ADR 0006) before the frame thread took it, for each frame that took one
    (frames, total, max, frames with any). 0 in a stepped run, in which a frame takes what was
    published in it, until #145's cap on uploads holds results back; it is there to show when that
    changes.
- `holes`: Split bricks whose children are not resident in the store, per frame. **The gate:** any
  hole fails the run with status 2 (unless `--no-fail`).
- `sizing`: see below.
- `timing`: the one part that is not deterministic: CPU time of each copy to the GPU and of each
  draw (every frame in a measurement, every tenth with `--short`), and the bytes the GPU counted.

#### Sizing constants

`bins/client/src/main.rs` sizes its tables from `BRICKS_PER_D0_SQUARED`, `REFS_PER_BRICK`,
`WORDS_PER_BRICK` and `STREAMED_PER_D0_SQUARED`, each measured once at full size by hand (see
`docs/measurements.md`). `streaming.sizing` has the same four measured on the test Planet with the
resident set complete, and each beside its constant and `limit` (the constant times `HEADROOM`). A
unit test (`the_sizes_measured_on_the_test_planet_are_within_the_constants_with_headroom`) fails
when one is over, which means the constant in `main.rs` is too small for the Planet: raise it with
a measurement, not to get green. Each constant's doc comment names the field it is checked
against.

The constants are per `d0` squared, and the test Planet's `d0` is a sixteenth of the full-size
bench's in length, so what does not scale with `d0` is left out of the measurement: the bricks of
the Guard's levels (drawn, and their words, which the scene counts exactly by level, for the
scene of each publish: `Scene::words_by_level`; a test fails if the references are not the bricks
drawn, which a snapshot of an earlier state would not be), and the store as it was at the spawn
(the spawn ring: all of it level 0, 226 964 bricks, as the 5.63 million taken out of the constant
is). The constants had the Guard and the ring in, so the limits are not exactly 1.25 times what
they scale with. Like for like, with real figures (the doc comment of each constant in `main.rs`
has the working):

| Constant | Limit | What it is a limit of | Limit ÷ that | Measured |
|---|---|---|---|---|
| `BRICKS_PER_D0_SQUARED` 3.25 | 4.06 | 3.09: the full-size Rings, without the Guard's 178 000 bricks | 1.32 | 3.51 |
| `REFS_PER_BRICK` 1.0 | 1.25 | 1.0 (a Guard brick has one reference too) | 1.25 | 1.000 |
| `WORDS_PER_BRICK` 34.2 | 42.75 | 33.1: the Rings, if the Guard weighs 53.8 words a brick at full size as on the test Planet (#162 measures it there) | 1.29 | 39.78 |
| `STREAMED_PER_D0_SQUARED` 19.8 | 16.49 | 13.19: the constant less the spawn ring's 5.33 (`STREAMED_SPAWN_RING_PER_D0_SQUARED`) and level 0's 1.29 beyond the ring (`STREAMED_LEVEL_0_BEYOND_THE_RING_PER_D0_SQUARED`), which the measurement does not have | 1.25 | 13.09 |

The streamed value is held to the part of the constant that the test Planet can show, not to the
whole of it (whose limit, 24.75, would be 1.89 times what is measured). Level 0 is what is not
like for like: its band (128 Voxels, 160 with the radial margin) is the spawn ring (160) and no
more, so no level-0 bricks are added to what is measured (the report has level 0 at the start and
when complete: both 226 964, and a test holds them equal), while at full size level 0 held 1.36
million bricks beyond the ring (1.29 per `d0` squared). That share is taken out of the constant as
well, so the check is 19.8 − 5.33 − 1.29 = 13.19 times 1.25 = 16.49 against 13.09 measured (99%
of that part, 79% of the limit). It passes; nothing was tuned to make it.

The camera is a function of the frame index only (keyframes, interpolated in a straight line at
constant speed), never of the clock, so every run on every machine shows the GPU the same views.
The path has a version in its name (`planet-flyover-v1`); if the path ever changes, the version
changes, and reports of different versions are not comparable.

It draws **offscreen** (`render::Offscreen`), not in a window. The tracer and the pass timers are
the same as the window's; what is left out is the window's `blit` and `overlay` passes, and
what is gained is that nothing waits for the display and that a frame's GPU times are in when it
returns, so there is no lag to correct for. It asks Vulkan for the high-performance adapter, as
the window does, and the report says which GPU it got: check `machine.device_type` is not `Cpu`
before reading anything into the times.

## The real-time mode

`--streaming --real-time` (#162) runs what the stepped mode cannot show: the **full-size Planet**
(the world map of `WORLD_MAP_SIZE`, the client's own `load`, the standard bands, `d0` 1028.2
Voxels) with the **real loader workers and the real scene thread**, on the machine's clock, with a
frame at 60 a second, drawn on the GPU. What the threads have done by a frame depends on what the
operating system let them do, so **everything in `streaming_real_time` is not deterministic**, is
made by this mode only, and is not in the counts hash. The stepped test Planet mode and its hash
`b37e0fb7cd3a2c78` are unchanged and still run first.

It runs after the stepped mode, and takes tens of minutes at full size: each run generates the
world map (about 25 s), loads, fills (a few minutes) and flies the paths, with the loader caught
up between them. **Run it in release, on the 4090, from the Windows artifact:**

```
client.exe bench --streaming --real-time --out bench-out
```

The rest of `client bench` runs first, as for `--streaming`. `--short --streaming --real-time` runs
the same paths on the test Planet for a few seconds each, to see that it works: its times, on lavapipe,
mean nothing.

### What the 4090 script must pass

**Pass `--streaming --real-time` once, not once for each of the script's N runs.** The runs are in
the process: `--runs N` (default 3) makes N complete runs, each on a fresh world and a fresh GPU
context, and the report keeps all of them in `streaming_real_time.runs`, and **the least and the
most of every number over them in `min` and `max`** (the same shape as a run: the number at its
least and its most, over the runs; what is not a number is the first run's). The gates use the
worst, and the exit status is the gates' (2 if one is over). So the script has nothing to merge:
one `report.json` has the three runs, the envelope and the verdict. A script that runs
`client.exe bench <args>` N times to collect N reports should pass the real-time flags to one of
them (or pass `--runs 1` to each and take the worst itself, which the envelope in a single process
does for it).

### The paths

Each starts at the Spawn with the resident set complete and the camera still, and ends with the
camera back there and the loader caught up. The camera's speed and shape are those of #143's
`soft_misses_on_the_flight_take_off_and_standing_paths`, at full size.

| Path | The camera |
| --- | --- |
| `cold start` | At the Spawn from the first frame until the resident set is complete. Time to control is in it. |
| `flight at 2 m` | 50 m/s along the ground, 10 s, 2 m over it. |
| `flight at 50 m` | 50 m/s sideways, 10 s, 50 m over the ground. |
| `vertical take-off` | 50 m/s straight up, 6 s. |
| `180 degree turn` | A half turn in a second, then the view still for a second. |
| `stop after a flight` | 5 s of the flight at 2 m, then the camera stands where it was until the scene has no soft misses (up to 120 s). Settle time is in it. |

### The fields

`streaming_real_time.runs[]` (and `min` and `max` of the same shape), each made by this mode:

- `cold_start`: `world_map_s` (generating the map, which the client does before `load`), `load_s`
  (`load`: the spawn ring, the Guard, the first scene), **`time_to_control_s`** (from the start of
  the load to the first frame with level 0 under the camera, which is the ground the character
  stands on: the bench has no server, so the character itself is not in it) and `fill_s` (from the
  start of the load to the resident set being complete). Not gated.
- `paths[]`: `name`, `frames`, `seconds` and, for each path:
  - `frame_time`: `samples`, `mean_ms`, **`p99_ms`** (nearest rank) and **`worst_ms`**. A frame is
    the frame thread's work in the game's order: `World::recentre`, the copy of what the scene
    thread made to the GPU, and the draw, until the GPU has finished (the offscreen draw waits).
  - `frame_cpu`: the same figures for the **frame thread's CPU time**: the recentring, the copy to
    the GPU's buffers, and the encoding and submitting of the draw, without the wait for the GPU
    (`Offscreen::draw_upscaled_timed`).
  - `soft_misses` (a `Series` of what each frame reported: frames, total, max, frames with any) and
    `soft_misses_per_frame`. The scene thread counts them a few times a second, so a frame
    reports the last count, and the count stays within about 100 ms of the truth. Not gated yet
    (#168).
  - `hole_scans`, `holes` and `holes_per_frame`: **holes are sampled, not counted for every
    frame.** Looking at every split brick takes 0.3 s at full size with the store's read lock held,
    so the scene thread does it every 5 s (`SceneWorker::count_holes`), and the report has how many
    looks there were in the path and what they found. A hole that lasted less than the gap between
    looks is not seen; the stepped mode counts every frame on the test Planet, and the
    scene thread only splits a brick whose children are resident, so the look is a check of that.
  - `scene_thread`: milliseconds the scene thread spent in the path on `follow` (selecting again
    for a new camera and packing what changed), `pack` (asking the loader, taking in what it made
    and packing it), `upload` (publishing: copying what changed out for the frame thread, which
    copies it to the GPU), `evict` (eviction), `coarse` (the coarse levels over an Edit) and
    `hole_scan`. The same for the whole run is `scene_thread` of the run.
  - `settle`, for `stop after a flight` only (null for the others): `seconds` from the frame the
    camera stopped in to the first frame with no soft misses, or the cap, and `settled`. Not gated.
- `memory`: `process_peak_mib` (the process's peak resident memory since it began, so it only grows
  over the runs: Windows' peak working set, Linux's `VmHWM`; null where the system does not say),
  `scene_peak_mib` (the scene's buffers in use, at the most over the frames), `store_peak_bricks`
  (the store's size as its budget counts it, looked at every 6 frames) and `store_limit_bricks` (the
  most it may hold before eviction starts: the budget with its overflow).
- `sizing`: the client's sizing constants against what was measured **at full size** with the
  resident set complete, as `streaming.sizing` has them on the test Planet (same fields and the
  same checks, with `d0` of 1028.2, and no share of level 0 taken out of `STREAMED_PER_D0_SQUARED`:
  level 0 beyond the spawn ring is there at full size). The scene thread counts exactly once for
  it (`World::count_once_exactly`), since counting at every publish is a pass over 3.4 million
  bricks each. It is reported; `over` marks a constant that is too small, but this is not a gate.
  This is where `WORDS_PER_BRICK`'s Guard share and the other constants of
  `docs/measurements.md` are measured again.

`gates` holds, for each measure, the `worst` over every run (and path) and the `limit`, and `over`
lists the gates that are over: `frame_time`, `frame_cpu`, `holes`, `memory`.

**Edit latency is not measured in this mode**: no path makes an Edit (the versioned M1a path with
a dig and the Zoom is #168), and `edit_latency` in the report says so. The stepped mode's
`residency.edit_latency_frames` is 0 by construction (see above), so ADR 0006's bound is held by
the stepped and real-thread tests and not yet by a number from the 4090.

### The gates

In `bins/client/bench-budgets.toml`, table `[streaming]`; every key is needed (a missing one would
turn its gate off, so it is an error, like a misspelt one), and `--real-time` with budgets that
have no such table does not run. Each gate is held to the worst of the runs, and a gate that is
over exits with status 2 (0 with `--no-fail`).

| Gate | Key | Budget | The worst over the runs of |
| --- | --- | --- | --- |
| Frame time | `frame_time_p99_ms` | 16.7 | the p99 of `frame_time`, of every path |
| Frame-thread CPU | `frame_cpu_p99_ms` | 8.0 | the p99 of `frame_cpu`, of every path |
| Holes | `holes` | 0 | the holes the scene thread's looks found, summed over every path and run |
| Memory | `scene_mib` | 512 | the scene's buffers in use (the full-size test's 474 MiB measured, #143) |
| Memory | (the store's own budget) | | the store's size against its limit, `store_limit_bricks`, which the client sets from `STREAMED_PER_D0_SQUARED` and `BUDGET_OVERFLOW` |

Soft misses, time to control and settle time are reported and not gated. The final stage budgets
are not these: they are the stages of #10, per pass (#13's `[passes]`).

### Reading it and tests

The gates, the least and the most over runs, the shape of the report, and the budgets table are
unit tests in `bench/realtime.rs` and `bench/budget.rs`, with no GPU and no Planet: one test per
gate builds runs that are within every gate but one and checks that exactly that gate fires, with
status 2 (the report is built from numbers, so they gate on Linux and on Windows).
`the_real_time_run_on_the_test_planet_completes_with_every_path_and_field` runs the mode on the
test Planet with real threads and no GPU, and asserts what was run, no time; it is in
`.github/real-thread-tests.txt` and so does not gate. CI does not run the full-size mode.

## Reading the report

```
{
  "schema_version": 8,
  "build":   { "version", "commit" },          // commit: CI builds only, else null; on a PR
                                               // build it is GitHub's merge commit, not the head
  "machine": { "gpu", "device_type", "driver", "driver_info", "backend", "os", "arch",
               "timers_supported" },
  "config":  { "width", "height", "output_width", "output_height", "scene", "path",
               "keyframes", "bias", "diag" },          // width, height: traced; output_*: upscaled
  "modes": {
    "primary":   { "frames", "warmup_frames", "spp", "passes": [ENTRY...], "gpu_total": ENTRY,
                   "frame_time": STATS, "scene_update_ms": STATS,
                   "lod_at_keyframes": [ { "frame", "height_m", "bricks": [n...] }... ] },
    "reference": { ... same ... }
  },
  "altitudes": [ { "height_m", "bricks": [n...], "primary": STATS, "upscale": STATS,
                   "reference": STATS }... ],
  "caves":   [ { "name": "mouth" | "tunnel", "bricks": [n...], "primary": STATS,
                 "upscale": STATS, "reference": STATS }... ],
  "screenshots": [ "primary-frame0000.png", ..., "primary-cave-mouth.png", ... ],
  "budgets": { "source", "ok", "failed": ["primary/primary"], "unmatched": [] },
  "edits":   { ... } | null,                       // the Edit stress; null for --short
  "streaming": { "deterministic": true, "schedule", "counts_hash", "upload", "soft_misses",
                 "rings", "work", "residency", "holes", "sizing",
                 "timing": { "deterministic": false, ... } } | null,  // --streaming only
  "streaming_real_time": { "mode": "real-time", "deterministic": false, "planet", "d0",
                           "runs": [RUN...], "min": RUN, "max": RUN, "gates",
                           "edit_latency": "not measured: ..." } | null  // --real-time only
}
```

- `STATS` is `{ "samples", "mean_ms", "p95_ms", "max_ms" }`. The figures are `null` when there are
  no samples. `p95_ms` is the nearest rank. Times come from every sample the GPU timers recorded
  over the measured frames (`Timings::history()`), not from the overlay's rolling window.
- `ENTRY` is `STATS` plus `name`, `budget_ms` (null if the pass has none) and `status`:
  - `within`: the **mean** is at most the budget.
  - `over`: the mean is above it. This blocks the merge.
  - `no-budget`: nothing declares a budget for it.
  - `no-data`: it has a budget but no times, because the GPU has no timestamp queries
    (`machine.timers_supported` is `false`). It fails the run, since nothing was checked.
- `gpu_total` is the GPU time of one profiler frame, from the start of its first pass to the end
  of its last. In primary mode that is one frame of the path. In reference mode every sample is a
  profiler frame of its own, so `passes[]` and `gpu_total` are **per sample** there, with
  `frames * spp` samples, and `gpu_total.reference` budgets one sample.
- `frame_time` is CPU time to draw one frame of the path, and to read the image back where it is
  read (reference mode, and primary mode's screenshot frames). For reference mode a frame is all
  its samples. It is context, not a budget.
- `scene_update_ms` is the CPU time per frame that the Planet takes to follow the camera: the
  bands select the bricks again, those that changed level are packed, and the changes are written
  to the GPU, flushed at once. It is done before the frame is timed, so it is in no pass and not
  in `frame_time`, but a window pays it. Its mean is CPU time, not GPU time, and a CPU that is
  slow to follow leaves the GPU idle between frames (see below).
- `lod_at_keyframes` has, for each keyframe of the path, the frame, how high the camera is above
  the Planet's mean radius in metres, and how many bricks are drawn at each level of detail
  (level 0 first). Empty for `--short`.
- `edits` is the Edit stress (#147, #156; null for `--short`): the largest cube Brush dug and
  placed in turn at the metric rate for 30 s. `per_frame` is an `ENTRY` of CPU time over every
  frame, those with no Edit at 0, against the `edits` budget (a mean, so a hitch can hide in it).
  `frame_thread` is an `ENTRY` of the part of each Edit that the client's frame thread does (the
  Edit planned, level 0 of the Voxels changed, the writes sent to the GPU), held to the
  `edit_frame_thread` budget by its **longest** Edit, `max_ms`, so one hitch fails the run.
  `scene_thread` is `STATS` of the rest (the coarse levels remade, the bricks packed), which the
  frame thread does not wait for and which has no budget. `per_edit` is `STATS` of all of it as
  if on one thread.
- `altitudes` is the sweep: for each height, the bricks drawn at each level and the `primary` and
  `upscale` passes of primary mode and the `reference` pass of reference mode (one sample), with
  no budget held against them. It is how the cost of the LOD bands is read at the ground, 100 m,
  500 m and 2 km.
- `caves` is the same for the two cave stills (the camera at the mouth, and the one inside),
  with the bricks of the bands at a tenth of the flight's `d0`, so far fewer than the sweep's. Empty
  for `--short`. Their screenshots are in `screenshots`.
- `budgets.unmatched` lists budgets for passes that did not run (`passes.shadows` before there is
  a shadows pass): they are not an error.

The mean is what is held to the budget; look at `p95_ms` and `max_ms` for stutter.

### Reading a regression

A pass whose work did not change but whose time did (the `upscale` pass does not read the Planet)
points at how the frames are paced, not at the pass. The bench draws one frame at a time and
waits for it, so the GPU idles for as long as the CPU takes to follow the camera
(`scene_update_ms`) before the next frame, and a GPU that has idled may time a pass that is
limited by memory (`upscale` is) differently from one that has not. To tell the Planet's GPU work
from that gap, run the bench with `--diag` and compare `upscale` and `reference` against a plain
run (the camera path is the same):

| Run | If `upscale` falls back to main's | It is |
| --- | --- | --- |
| `--diag no-follow` | yes | something in following the camera: the next three say which |
| `--diag no-follow,gap-ms=N` (N about `scene_update_ms`) | no, it stays high | the idle gap: nothing the Planet sends to the GPU |
| `--diag no-upload` | no, it stays high | the CPU's time, not the writes |
| `--diag no-flush` | yes | the writes being sent at once, ahead of the frame |

## Budgets

`bins/client/bench-budgets.toml` is compiled into the executable, so the Windows artifact needs no
file beside it. To try other numbers, copy it and pass `--budgets copy.toml`. The report says
which budgets it used (`budgets.source`).

```toml
[passes]            # milliseconds of GPU time per frame, by the name of the pass
primary = 1.0

[gpu_total]         # GPU total of one profiler frame, by mode: primary, or one reference sample
primary = 8.6
```

The `[streaming]` table has the four gates of the real-time mode (see "The real-time mode"):
`frame_time_p99_ms`, `frame_cpu_p99_ms`, `holes` and `scene_mib`.

Anything else in the file, a pass or mode with a zero, negative or non-numeric budget, a
`[streaming]` table with a key missing or an unknown one, is an error: a typo must not silently turn
a budget off.

The numbers are the per-stage frame budget of the rendering decision (#10), on the RTX 4090 at 1440p
output. Only `primary` (Primary visibility, 1.0 ms), `upscale` (the TAAU pass, 0.5 ms) and the total
(8.6 ms) are live today, as those are the passes that exist. The later stages (sun shadows 0.4, GI
1.5, reflections 0.7, glass 0.7, volumetrics 1.0, denoise 1.0, atmosphere 0.3, edits 1.0, post 0.5)
are in the file as comments: **whoever adds a pass uncomments its budget and names it the same** (a
pass is named by the string given to `Profiler::compute_pass`). `reference` has no budget in #10;
its 3.0 ms (a sample, pass and total) catches a gross regression, set from the first real run on the
4090, which measured a mean of 1.10 ms.

## Tests

The statistics, budget evaluation and exit status, camera path, argument parsing and report shape
are unit tests in `bins/client/src/bench/`. So are the streaming mode's, in `bench/stream.rs`: two
runs give identical counts, the counts have the committed hash, every integer of the report is in
it, the Edit shows in the next frame, the holes gate gives status 2, and the sizing check passes
and fails when a constant is lowered below its measured value. A client test
(`a_round_that_changes_nothing_is_not_published_in_the_games_mode_but_is_when_counting_exactly`)
fails if the scene thread publishes for its counts of work in the game's mode. They are
stepped and need no GPU, so they gate on Linux and on Windows, which is how the two operating
systems are compared. `bins/client/tests/bench.rs` runs the real
`client bench --short` (and `--short --streaming`) and checks the report's schema (version 8, with
`streaming_real_time` null), the screenshots and the exit codes, and that `--real-time` needs
`--streaming` and budgets with a `[streaming]` table: it runs in `cargo test` (and so in CI) on
lavapipe. It does not run the real-time mode, which has real threads and real time (see "The
real-time mode" for the unit test that does). `--short` uses a patch of the Planet, the one the
golden image test renders, because the whole Planet takes about 22 s to generate in release.
