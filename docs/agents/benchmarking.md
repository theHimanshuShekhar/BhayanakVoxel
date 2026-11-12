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
  time p99 (16.7 ms), frame-thread CPU p99 (8 ms), holes (none), soft misses at the speed-scaled
  level (none, ADR 0009) and memory. CPU-only PRs that touch streaming (the loader, the
  prefetcher, eviction, generation) get a `needs-stream-bench` label and the same run, since CI
  cannot see them. The part of this that CI can do, with counts and no times, is the streaming
  mode of #181 (`--streaming`, below); the full-size run in real time on the 4090 is #162.
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
| `--no-fail` | Exit with status 0 even if a pass is over budget, or a frame of the streaming mode has a hole. |

It runs unattended and takes a minute or so (generating the whole Planet is about 22 s of it).
The report is written to `DIR/report.json` and also printed on stdout; progress and a table go
to stderr.

**Exit status:** 0 if every budget is met, 2 if a pass is over its budget (or has a budget but
could not be timed) or, with `--streaming`, if any frame had a hole, 1 if the bench could not run (no Vulkan device, an unreadable budgets
file, a bad option).

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
   below.

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
whole of it (whose limit, 24.75, would be 1.88 times what is measured). Level 0 is what is not
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

## Reading the report

```
{
  "schema_version": 7,
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
                 "timing": { "deterministic": false, ... } } | null   // --streaming only
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

Anything else in the file, a pass or mode with a zero, negative or non-numeric budget, is an
error: a typo must not silently turn a budget off.

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
`client bench --short` (and `--short --streaming`) and checks the report's schema, the
screenshots and the exit codes: it runs in `cargo test` (and so in CI) on lavapipe. `--short` uses a patch of the Planet, the one the
golden image test renders, because the whole Planet takes about 22 s to generate in release.
