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
  settle time are reported by that run and become gates in #168 (Edit latency is reported by it
  too, ADR 0006's bound). The part of this that CI can do,
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
| `--diag LIST` | Switches for finding out where time goes, separated by commas: `no-follow` (the Planet does not follow the camera), `no-upload` (it does, on the CPU, but the GPU is not told), `no-flush` (writes to the GPU go with the next frame's commands, not at once) and `gap-ms=F` (sleep F ms before each frame, as the CPU work of following would). The report lists them in `config.diag`, the summary warns, and the budgets are not checked (`budgets.skipped` says why; the exit status is 0). See "Reading a regression" below. **`--diag` is an error with `--real-time`**: the real-time gates are always checked, so a diagnostic run of it would contradict the first sentence. |
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
unreadable budgets file, a bad option, `--real-time` with budgets that have no `[streaming]`
table, or a run of the real-time mode that stopped on an error: that is 1 even with `--no-fail`,
and `report.json` is still written, with everything done before it and the error, see "When a run
stops").

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
  any) and the belt copies per frame (`writes`, the same, each a copy with a barrier of
  its own: neighbouring ranges of a patch are joined into one when they are within `COPY_GAP`
  words), `bytes_p99` and `writes_p99` (99th percentile, nearest rank, of the frames that had an
  update), `patch_bytes` (what patches wrote, a rebuild's all-buffers write excluded), `rebuilds`
  and `updates`. **Two counts are compared, and they are not independent of
  everything.** One is worked out from the patch before anything is written
  (`render::tracer::upload_cost`, which these fields are made of: `RingPatch::cost` sums the
  bytes and the copies of `RingPatch::copies()`, the ranges after joining, and the header). The
  other is counted by the tracer **where it writes to the staging belt**: the header's bytes and
  one copy, then the bytes and one copy of each range, the words between joined ranges included
  (`timing.gpu_uploaded_bytes` and `timing.gpu_uploaded_writes`, summed over the updates). **Both
  loop over the same `RingPatch::copies()` and use the same header size.** So the equality (the
  smoke test checks bytes and copies) shows that the tracer writes every range that `copies()`
  lists and the header, once each, and that `cost()`'s arithmetic matches the writes; if
  `copies()` missed or mangled a range, both counts would agree and be wrong. That the ranges
  cover the change, word for word, is checked by `GpuCopy::assert_is` in the stepped tests
  (`streaming_tests.rs`): a copy of the buffers written from `copies()` as the tracer writes them,
  compared with the frame thread's scene after every frame. None of it is a count of what a GPU
  executed (the belt and the driver are below it). A buffer that grew (#145) copies the old one on the GPU
  (`copy_buffer_to_buffer`): that is not an upload, is in neither count, and is counted apart
  (`timing.gpu_grown_bytes`). The upload budget (`scene_worker::UPLOAD_BUDGET`) adds `cut_frames`, the frames in
  which the scene thread held work back for it, and `longest_backlog_frames`, the most such frames
  in a row. Both are counts and are in the hash. **The budget does not engage on the test Planet**
  (the biggest frame is a fraction of it): the stepped tests run it with a small one. What a
  frame may take beyond the budget is in [What the upload budget bounds](#what-the-upload-budget-bounds).
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
    published in it. The upload budget (#145) does not change that: it holds work back in the
    scene thread, before it is published, so every patch the frame takes is whole and the field
    stays 0; it is there to show when a change makes the outbox hold results.
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
(`e621feef18ef1801` since #145, which added the `upload` writes and budget counts) still run first.

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

**The invocation is `client.exe bench --streaming --real-time`, once, with no `--no-fail`, so that
the exit status shows the gates (nothing else is needed: `--runs` defaults to 3). Pass it once,
not once for each of the script's N runs.** The runs are in
the process: `--runs N` (default 3) makes N complete runs, each on a fresh world and a fresh GPU
context, and the report keeps all of them in `streaming_real_time.runs`, and **the least and the
most of every number over them in `min` and `max`** (the same shape as a run: the number at its
least and its most, over the runs; a boolean is the all of the runs' in `min` and the any in `max`,
so a flag that says `over` says it in `max` if any run was; a word the runs agree on is kept and
one they do not is null). The gates use the worst, and the exit status is the gates' (2 if one is
over; 1 if the mode stopped on an error). So the script has nothing to merge:
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
| `cold start` | At the Spawn from the first frame until the resident set is complete. The first frame with level 0 under the camera is in it. |
| `flight at 2 m` | 50 m/s along the ground, 10 s, 2 m over it. |
| `flight at 50 m` | 50 m/s sideways, 10 s, 50 m over the ground. |
| `vertical take-off` | 50 m/s straight up, 6 s. |
| `180 degree turn` | A half turn in a second, then the view still for a second. |
| `stop after a flight` | 5 s of the flight at 2 m, then the camera stands where it was until the scene has no soft misses (up to 120 s). Settle time is in it. |

After the last path the camera is back at the Spawn and the loader has caught up, and **one dig**
is made there (below): the run's **Edit latency**. Its frames are not those of a path.

### The fields

`streaming_real_time.runs[]` (and `min` and `max` of the same shape), each made by this mode:

- `cold_start`: `world_map_s` (generating the map, which the client does before `load`), `load_s`
  (`load`: the spawn ring, the Guard, the first scene), **`time_to_level_0_under_camera_s`** (from
  the start of the load to the first frame in which one ray from the camera straight down hits the
  ground at level 0) and `fill_s` (from the start of the load to the resident set being complete).
  Not gated. **This is not the spec's "time to control"** (the 20 m ring round the Spawn at full
  detail, with the character standing in it: the bench has no server, so the character is not in
  it). The field is named for the ray that it uses: the spawn ring is made inside `load`, so the
  ring of 20 m is there before the first frame and the spec's condition would read the load time
  too; the time that streaming adds to it is `fill_s` minus this. The ray is cheap, and a ring of
  rays at 20 m could not be tried at full size without the 4090.
- `paths[]`: `name`, `frames`, `seconds` and, for each path:
  - `frame_time`: `samples`, `mean_ms`, **`p99_ms`** (nearest rank) and **`worst_ms`**. A frame is
    the frame thread's work in the game's order: `World::recentre`, the copy of what the scene
    thread made to the GPU, and the draw, until the GPU has finished (the offscreen draw waits).
  - `frame_cpu`: the same figures for the **frame thread's CPU time**: the recentring, the copy to
    the GPU's buffers (the writes queued and the submit that sends them), and the encoding and
    submitting of the draw, without the wait for the GPU (`Offscreen::draw_upscaled_timed`).
    **What each figure includes.** `frame_time` runs from the start of `World::recentre` to the GPU
    being done: recentre, update (write and flush), encode (with finishing the command buffer),
    submit and the GPU wait, and anything
    else the frame thread does between (`other_ms` below, a few microseconds). `frame_cpu` is the
    same without the GPU wait. **Outside both**: working out the camera of a flight before the frame
    starts (it reads the store for the ground, and takes its read lock, so it can wait for a lock
    that a look for holes or an eviction holds; it delays the frame's start and is not in its time),
    and the frame's look at the memory after its time was taken (the store's lock again, every 6
    frames). Both are reported for a slow frame as `camera_ms` and `after_ms`. The pacing sleep is
    in neither.
  - `frames_over_limit`, `frames_over_limit_by_update` and `slowest`: **where the time of a slow
    frame went**. `frames_over_limit` is the number of frames of the path whose time was over the
    frame-time limit (the gate's `frame_time_p99_ms`; the gate itself is unchanged and still on the
    p99), and `frames_over_limit_by_update` is those frames by what their update was: `none`,
    `patch` or `rebuild`, so that frames that were slow with no update to copy are counted apart
    from those that had a large one. The three add up to `frames_over_limit` in each run; in
    `min` and `max` each is taken on its own across the runs, so there they need not. `slowest` is
    the five
    longest frames of the path, worst first (a fixed list filled while the frames come: no
    allocation, and a path with fewer frames has fewer), each with:
    - `index` (the frame's place in the path, from 0) and `at_ms` (from the path's start to the
      frame's start), `time_ms` and `cpu_ms`;
    - the parts, in milliseconds, in the order the frame does them: `recentre_ms` (`World::recentre`:
      telling the scene thread the camera and taking what it published), `update_ms` (the copy of
      the update to the GPU: `update_write_ms`, the copies written to the staging belt (and, if a
      buffer grew, its new buffer and the copy into it), and `update_flush_ms`, the submit that
      sends them; both zero with no update) and `writes` (the number of belt copies: each is a
      copy on the GPU with a barrier of its own), `encode_ms` (the draw's encoding, the timer queries' resolving and **finishing the
      command buffer**) and `submit_ms` (`Queue::submit` alone), `wait_ms` (**the wait for the GPU,
      which comes after the CPU's clock has stopped**: the readback of the pass times, the device
      poll, what wgpu frees inside it (the belt's chunks that were used), and the GPU's own
      work: the update's copies, the draw, and whatever else the GPU was doing), and `other_ms`
      (`time_ms` less all of those: what none of them says);
    - **the GPU's own time for the frame's passes**, from the timestamps the passes wrote (the
      device's clock, not wall time; they are GPU timestamps, not what the CPU waited):
      `gpu_primary_ms`, `gpu_upscale_ms`, and `gpu_total_ms` (from the start of the frame's first
      pass to the end of its last, gaps between them included), and `wait_not_passes_ms`, which is
      `wait_ms` less `gpu_total_ms`: what the wait was that was not our passes (the update's copies,
      which are queued ahead of them, driver work, other GPU clients, wgpu's cleanup in the poll).
      It is negative if the passes were already running while the CPU was still in the submit. **Which
      frame the times belong to**: the profiler reads the times back after each frame, and the
      bench waits for them, so a frame's times are in when its draw returns; the profiler says
      which readback was this frame's (`Profiler::times_of_this_frame`), and the fields are **null,
      not zero and not an earlier frame's,** where the device has no timers, the frame was not timed
      (the readback ring was full) or its readback failed. A time between the submit and the start
      of the first pass (the copies' own time) is not reported: it needs a timestamp before the
      copies, in the earlier submit that sends them, and there is none there;
    - **what a growth cost** (#145), zero in a frame where no buffer grew: `lengthen_ms` (the frame
      thread making its own copy of the scene longer, `Scene::paste`: the `Vec`s are lengthened
      and the new words filled with zeros; it is **inside `recentre_ms`**), `grow_ms` (the GPU's
      new buffers and the copies old to new, recorded; **inside `update_write_ms`**) and
      `grown_bytes` (the old buffers' bytes that were copied). The frame-thread part is the one
      that was probed at 13.9 / 26.5 / 52.4 ms for 128 / 256 / 512 MiB (`docs/measurements.md`)
      and is not made smaller here: the 4090 run is what measures it at full size;
    - `over_budget`: why this frame's upload was over the plain budget, if it was (`jump`,
      `bands`, `edit`, `growth` or `operation`: see below); null if it was not;
    - `camera_ms` and `after_ms`, outside `time_ms` (see above);
    - what the update copied: `update` (`none`, `patch` or `rebuild`: a rebuild makes new buffers
      for the whole scene), `upload_bytes` (all the update writes, headers and tables included) and
      `patch_bytes` (the words that a patch lists);
    - `hole_look`: whether a look for holes **overlapped** the frame: the scene thread was in one at
      the start or at the end of it, from before it takes the store's read lock to after it lets go
      (so waiting for the lock is in it). It does not say that the look blocked the frame, and a
      look that began after the frame's start and was done before its end is missed.

    The measuring costs a few clock reads a frame. In `min` and `max` the i-th slowest frame of each
    run is taken with the i-th of the others, member by member (a `slowest` of runs that did not
    keep the same number of frames is null there): read the runs for a frame's own parts.
  - `largest_updates` and `over_limit_at` (schema 13; `over_limit_at` is of a run and is not in `min` and `max` from schema 14: lists of frame places do not compare across runs, so it is under each run in `runs` only): the five frames whose update copied the
    most bytes, the most first, in the form of `slowest` (with `index`, `at_ms`, the frame's time,
    its copies and its wait), and the places in the path of the first 200 frames over the limit.
    They are for reading the slow frames against the big updates: whether a wait of 30 ms with
    the GPU's passes at 1.5 ms is in the frame of a large update, or the frames after it (the 4090
    run of `9539899` could not say: the largest update was not among its five slowest frames).
  - `scene_lag_voxels_max` and `scene_lag_d0_max`, with `scene_lag_voxels_p99` and
    `scene_lag_d0_p99` (schema 15; 14 had the first two, measured at a publish, which understated
    it): how far behind the camera the scene **that is drawn** was, at each frame of the path: the
    distance between the frame thread's camera and the centre of the scene it draws, in Voxels and
    in the report's `d0` (a jump is 2), the most and the 99th percentile (nearest rank) over the
    path's frames. A lag the walk keeps up with stays near the distance a frame or two of the
    flight covers (50 Voxels at 1 000 a second); one that grows over a flight is the walk not
    keeping up (see "No ceiling on the lag"). A flight that ends with the camera 10 000 Voxels
    from where it began and a scene 8 000 behind has a maximum a little under the end's.
  - `upload` (#145): what the path's updates cost: `updates` (frames with one), `time` (the CPU's
    time of the copy: mean, p99 and worst, in milliseconds), `bytes_p99` and `bytes_max`,
    `writes_p99` and `writes_max` (the belt copies: a rebuild has none; **these are the largest
    frame's bytes and the largest frame's copies**, which can be two frames), `cut_frames` (the
    frames in which the scene thread held work back for the upload budget) and
    `longest_backlog_frames` and `longest_backlog_ms` (the longest run of those frames in a row, in
    frames and in time), `over_budget` (the frames whose upload was over the plain budget, and by
    cause: see below), and the growths: `growth_frames`, `grown_bytes`, `lengthen_ms_max` and
    `grow_ms_max` (the most one frame spent on each part of a growth; the slowest frames have
    the same per frame). Nothing here is gated.
  - `soft_misses` (a `Series` of what each frame reported: frames, total, max, frames with any) and
    `soft_misses_per_frame`. The scene thread counts them a few times a second, so a frame
    reports the last count, and the count stays within about 100 ms of the truth. Not gated yet
    (#168).
  - `hole_looks`, `holes` and `holes_per_frame`: **holes are sampled, not counted for every
    frame** (see "What the holes gate can and cannot catch"). Looking at every split brick takes
    0.3 s at full size with the store's read lock held, so the scene thread does it every 5 s
    (`SceneWorker::count_holes`), and **one more look is forced at the end of every path**
    (`SceneWorker::look_now`), so no path has 0 looks. The report has how many looks there were in
    the path and what they found.
  - `scene_thread`: milliseconds the scene thread spent in the path on `follow` (selecting again
    for a new camera and packing what changed), `pack` (asking the loader, taking in what it made
    and packing it), `upload` (publishing: copying what changed out for the frame thread, which
    copies it to the GPU), `evict` (eviction), `coarse` (the coarse levels over an Edit) and
    `hole_scan` (the looks for holes), and **`hole_scan_share`**: the looks' share of the scene
    thread's **busy** time, which is the sum of the parts above and not wall time (a thread that is
    idle most of the time can still read high). **A path's figures are those of its frames: the look
    that is forced after the last frame is not in them**, so a path in which no look came due reads
    0 and not 100%; its time is `end_look_ms`. **While a look runs the scene thread publishes
    nothing**, so the looks of a path are time in which the frame thread's updates were held back
    (a look at full size is 0.3 s with the store's read lock held), and a frame says whether one was
    running in `hole_look`. The run's `scene_thread` is over the whole run and has every look in it,
    the forced ones too.
  - `end_look_ms`: what the look that is forced at the end of the path took in the scene thread
    (the scan itself), in milliseconds. It is made after the frames, so it is in no frame's time.
  - `settle`, for `stop after a flight` only (null for the others): `seconds` from the frame the
    camera stopped in to the first frame with no soft misses, or the cap, and `settled`. Not gated.
    The scene thread's count of soft misses can be from before the stop (it counts a few times a
    second, and not at all while it sleeps), so a stale 0 would read as settled at once. So the
    stop is followed by **a count made after it** (`SceneWorker::look_now(false)`, the count and no
    look for holes, whose 0.3 s would be in the time), and the camera is settled when that count
    or a later one is 0 **and `waiting()` is 0**. The check is on `waiting` (the bricks the scene
    still waits for the loader's to split) and not on the loader's `pending`, because pending also
    holds jobs ahead of the view, which the view does not wait for; a count after the forced one is
    not stale, since the scene thread counts again at once when nothing waits any more, and every
    100 ms while it works. The time has the resolution of a frame, and of the count's 100 ms.
- `hole_looks` and `holes` of the run: the looks for holes over the whole run, **from the moment the
  measuring began** (the paths, the catching up between them, the sizing and the Edit), and what
  they found. **The holes gate is held to this**, not to the sum of the paths'. A look is forced at
  the end of each catch-up (once the loader has caught up, at the Spawn), after the sizing, and at
  the end of the run.
- `edit_latency`: the Edit that the run makes, **frames**, **ms** and `call_ms` (see below); null
  if there was no Edit to time, and then **`edit_not_made`** says why (`nothing in reach` or
  `refused`; null when the Edit was made).
- `memory`:
  - `resident_peak_mib`: **the largest resident set sampled during the run** (Windows' working set,
    Linux's `VmRSS`). A sample is taken every 30 frames of the cold start and the paths (every half
    second), and at the start and the end of the run; **not during the sizing, the looks for holes
    or the waits between paths, and not inside the generation of this run's world map and its
    `load`**, which come before the first sample. The samples are of the whole process, so **the
    allocator may still hold memory from the normal bench that runs first and from earlier runs**,
    and the figure can include it: the first run's is the cleanest reading of what a run needs.
  - `whole_process_peak_mib`: the OS's own peak for the **whole process** (Windows' peak working
    set, which cannot be reset; Linux's `VmHWM`) since it began. It includes the normal bench's
    generation of the full Planet and every earlier run, so it is an upper bound of the run's, and
    only grows over the runs. Null where the system does not say (as `resident_peak_mib`).
  - `scene_used_peak_mib`: the scene's buffers **in use** (`Scene::used_bytes`), at the most of
    what the scene thread said: it works the figure out every 250 ms (`TABLES_EVERY`), so this is
    the most of those, not of every frame. **The memory gate is on this**, against the full-size
    test's `SCENE_MIB_MOST` (512 MiB). It is **not VRAM**.
  - `scene_reserved_mib`: what the GPU reserves for the scene, used or not
    (`Scene::reserved_bytes`: the table, the regions, the brick references and the bricks, 4 bytes a
    word). The buffers are allocated whole and change only when one grows (#145), so this is the
    most over the run, after the growths. This is the VRAM the scene takes (at full size the
    bricks buffer alone is about 587 MB). Reported, not gated.
  - `scene_gpu_peak_growth_mib`, `scene_host_peak_growth_mib` and `scene_gpu_peak_rebuild_mib`:
    **the peak during a growth**, which `scene_reserved_mib` is not (it is the size after). While
    a buffer grows the old one and the new one both exist: the GPU's is the buffers' bytes before
    the growth plus the new buffers made (`Tracer::peak_growth_bytes`, the most over the run's
    growths; 0 if none grew). **A rebuild is apart**: while a rebuilt scene is made (a jump or
    other bands) the old scene's buffers and the new ones both exist, which is
    `scene_gpu_peak_rebuild_mib` (`Tracer::peak_rebuild_bytes`; 0 if there was none). The frame
    thread's is its copy's reserved bytes after the growth plus the old words of
    the buffers that grew in that frame, at the most over the frames, which is what the process
    holds if the allocator cannot extend the `Vec` in place and copies (an upper bound; the
    scene thread's own copy, which grew a round before, and the outbox's copies are not in it).
    Both are **reported and not gated**: `scene_used_peak_mib` and its limit are what the memory
    gate is on, unchanged, and it counts the words in use, not the room a growth reserves. ADR
    0004's accounting is of the sizes after a growth; these are what it has to add during a growth and during a rebuild.
  - `store_peak_bricks` (the store's size as its budget counts it, looked at every 6 frames) and
    `store_limit_bricks` (the most it may hold before eviction starts: the budget with its
    overflow).
- `sizing`: the client's sizing constants against what was measured **at full size** with the
  resident set complete, as `streaming.sizing` has them on the test Planet (same fields and the
  same checks, with `d0` of 1028.2, and no share of level 0 taken out of `STREAMED_PER_D0_SQUARED`:
  level 0 beyond the spawn ring is there at full size). The scene thread counts exactly once for
  it (`World::count_once_exactly`), since counting at every publish is a pass over 3.4 million
  bricks each. It is reported; `over` marks a constant that is too small, but this is not a gate.
  This is where `WORDS_PER_BRICK`'s Guard share and the other constants of
  `docs/measurements.md` are measured again.

`gates` holds, for each measure, the `worst` over every run (and path) and the `limit`, and `over`
lists the gates that are over: `frame_time`, `frame_cpu`, `holes`, `memory`. For the holes it also
has `hole_looks_fewest_in_a_run` and `hole_looks_fewest_in_a_path`.

**Edit latency is measured in this mode**, as ADR 0006's bound needs the real scene thread and the
stepped mode cannot show it (its `residency.edit_latency_frames` is 0 by construction, see above).
After the stop path the camera is back at the Spawn with the loader caught up, and one dig
(`world.edit(&aim, &Tool::default())`, `aim` the Spawn's view pulled to 40 Voxels from the ground,
as the stepped mode does it) is made **before the frame's look at the scene**, as the game does,
and frames at the Spawn follow until one has taken it in (`World::edit_taken`). `edit_latency` of
each run is **`frames`** (from the frame that made the Edit to the one whose look took it in: 1 is
the bound, an Edit made in frame N shows in N+1, 0 the frame that made it), **`ms`** (from before the
Edit's call to the end of that look) and `call_ms` (the Edit's own call on the frame thread:
planning and writing the bricks). `min` and `max` have the least and the most over the runs.
It is reported and not gated: the bound is held by the stepped and real-thread tests, and this
gives the number from the 4090 (#168 may gate it). **A dig that cannot be made is not an error:**
if the Spawn's view has nothing in reach (no Voxel within `AIM_REACH` along it, or the scene does not
show the ground there) `edit_latency` is null and `edit_not_made` is `nothing in reach`; if the client
knows the server would turn the Edit down (`Predictor::foresee`) it is `refused`. The run goes on and
finishes. An Edit that was made and is not taken in within 600 frames is the one that stops the run
with an error.

### What the upload budget bounds

The budget (`scene_worker::UPLOAD_BUDGET`: 8 MiB and 2 048 belt copies) limits what the scene thread
**adds** to the outbox and the frame thread has not taken, not what a frame takes. This is the one
place that says what a frame may take beyond it:

- A frame takes up to **the budget, plus the one operation that took what waits to it, plus the
  first batch of arrivals of the round, plus the gap words**. The check is between operations, and
  a patch is whole (a frame must not draw a brick whose words are not on the GPU), so the
  operation (a follow step or a batch of arrivals) that takes what waits to the budget is in the
  frame. **A follow step is itself cut** (#145, round 6): a camera further from the scene than a
  quarter of the budget's worth of its move is walked to in steps of that size, each noted in its
  turn. The length of a step is `FollowStep` (round 8; rounds 6 and 7 had a rate, below): it
  begins at half of its **limit**, and after each step that did not reach the camera it is that step's
  length scaled by `target / cost` (`target` a quarter of the budget, in bytes and copies, whichever
  binds; scaled by at most 2 up and down; a step that cost nothing doubles), kept between a quarter
  of the limit and the limit. The **limit** is a quarter of the budget over the least that a Voxel
  of the move is taken to cost, `FOLLOW_MIN_COST` (32 KiB and 2 copies, a little under the 4090's
  34 KB and 1.9 at 2 m), and at most half a jump: **64 Voxels at the default budget** (2 MiB /
  32 KiB; the copies come to 256, so the bytes bind). It does not depend on what the steps before
  it cost, so a run of steps that moved and changed nothing, or a rebuild (which has no patch's
  cost), cannot make the next one long (the 34 MB operation of the first 4090 run). Rounds 6 and 7
  sized a step by a *rate*, the cost a Voxel of the last step; that fell to steps of 4 Voxels
  and stayed there, because **a step's cost has a part that does not depend on its length** (the
  regions that it rewrites are written whole): in the local full-size flight a step of 64 Voxels
  cost 2.9 to 3.8 MB and 380 to 510 copies and a step of 4 cost 2.0 to 2.3 MB and 310 to 360, so a
  quarter of the budget (2 MiB) is under the cost of a step of any length there, and a length made
  to fit it shrank, for 240 steps and eight times the bytes of steps of 64 for the same walk. The
  least is a quarter of the limit (16 Voxels at the default budget) for that. **What that
  guarantees at full size:** a step is at most 64 Voxels at the default budget, so it costs at most
  `step * actual cost a Voxel` plus the part that is not in proportion: **the 4090's flights at
  c537d09 had steps of 64 Voxels that cost at most 3.68 MB and 516 copies** (their `bytes_max` and
  `writes_max`), and the first step of a walk is 32 Voxels. That is a bound by the limit and the
  measured cost, not by the budget: terrain that costs more a Voxel than the flights' gives a step
  that much over, down to the least, 16 Voxels. Before, a follow was one operation to wherever the
  camera was: the 4090's flights followed in one step of 17 to 34 MB. The first batch of arrivals of a round is taken whenever the outbox is empty, so that the
  follow steps do not starve the arrivals: **if a follow step crossed the budget, the frame carries
  two operations beyond it**, the follow step and that batch. The gap words are those between
  ranges joined into one copy (`COPY_GAP`). The copies of a frame are at most the sum of its
  operations' (joining only makes fewer).
- **Not budgeted at all:** an Edit and its coarse remakes (the Edit is packed ahead of the backlog,
  so ADR 0006's bound holds); a **teleport** (the camera put further than a jump, 2 `d0`, in one
  call of the frame thread, and ending further than a jump from the scene: the scene is made again,
  as that is cheaper than walking) and other bands, including the coarsening at the memory cap
  (the scene is packed again and uploaded whole: `Rebuilt`); and the copies of a growth, old buffer
  to new (they are on the GPU, not uploads, and are in no byte count). A scene that merely fell
  behind by more than a jump is **walked to**, not jumped to, and so is a teleport that ends within
  a jump of the scene (round 7: it was one operation of up to two `d0`, outside the budget:
  `a_teleport_that_ends_within_a_jump_of_the_scene_is_walked_to_within_the_budget`).
- **No ceiling on the lag, and the walk does not keep up with a flight (round 8).** A scene that
  is behind is always walked to, so a camera that moves faster than the walk keeps up with falls
  behind without limit, and walking many jumps costs more CPU than the one rebuild that the `JUMP`
  doc says it meets at about two `d0`. **The walk does not keep up with 1 000 Voxels a second,
  and neither did the baseline.** Measured (the 4090, c537d09; a scene that is behind is drawn as it
  is: the bands it has, no hole): the scene was 8 415 Voxels (8.2 `d0`) behind at the end of a 10 s
  flight at 2 m and 8 558 (8.3) at 50 m, with 12 and 10 updates and 9.0 s of the thread in
  `follow`, about 0.75 s for each step of 64 Voxels. The baseline (3934846) does it with one
  `recentre` to wherever the camera is when it is called, and a call takes as long as it takes
  (measured on the dev box, the same flight: 207 Voxels 1.3 s, 1 341 Voxels 4.5 s, 4 672 Voxels
  17.1 s, so 3.5 to 6 ms a Voxel); it ended its calls of 207 and 1 341 Voxels at about 1.5 s and
  6.7 s, was 6 300 Voxels behind just before the second ended and about 8 200 at 10 s (the drawn lag
  at each frame, printed by the ignored full-size test), with its last call, of 4 672 Voxels, still
  running 13 s later. The
  4090's baseline "2.7 to 4.0 s in `follow`" is the calls that ended within the path. So the
  walk of the scene takes about 4 ms of scene-thread time a Voxel, here and in the baseline (the
  selection looks at what the move made due, the nodes whose level changes are split or merged,
  and the regions they are in are packed), which is 250 Voxels a second of a thread that is also
  to take in the arrivals, against a flight's 1 000; **above that the scene falls behind by the
  difference, a second at a time.** Round 7's "keeps up with about 3 000 Voxels a second" was a
  guess from `follow`'s time over a flight, which counted the steps that ended and not what they
  covered; it is refuted by the run above. **What round 8 changed** is how much of a step's time was
  not the walk: each round told the selection the camera's move since the last round (about 1 300
  Voxels at 1 000 a second and 0.75 s a round) as the distance of the places it looks ahead to,
  and the selection looks at what that distance made due, so every call looked at 2 to 3 million
  nodes (80% of its 0.9 s) whatever the step; now the places are from where the scene is to be
  after the round's first step (`Engine::round`), so a call costs what it walks (a step of 64
  Voxels 0.27 s, 4 ms a Voxel, on the dev box) and the look at what is due is shared among
  threads (`look_at_due`, at most 4, only above 32 768 due). In the full-size flight on the dev box
  (a loaded box: 8 cores, load average above 10) the scene walked about 1 700 Voxels in 10 s against
  about 250 at c537d09 and 1 800 for the baseline, and the drawn lag at the end was 8 300 and 8 600
  at 2 m and 50 m (9 750 at c537d09, 8 200 for the baseline): **about what the baseline's is, and
  not under 3 `d0`.** Each real-time path reports how far behind the drawn scene was, at each frame, the most
  and the 99th percentile (schema 15). **What I would propose, not done here:** the walk is not
  made to keep up by any step policy, since its cost a Voxel is the same for steps of 16 and of
  200 (4 to 6 ms a Voxel, a little less for the long ones); to keep up it has to cost much less a
  Voxel (what is looked at, and the splits and merges, are the cost: ADR 0004's selection) or to be
  skipped: a fallback to the jump when the lag is over a number of `d0` (4, say) and the camera is
  still moving faster than the walk, as a one-time cost (a rebuild, 871 MB, 330 ms on the 4090)
  against a walk that does not end; before this PR the follow jumped past two `d0`. It is the
  owner's to decide because it spends the budget's bound once for a bounded lag.

**Whether "the budget plus the crossing operation plus the round's first batch" meets the issue's "a frame's upload is bounded by the
budget" is not settled here; it is the project owner's decision.** What is needed to judge it is
in the report, per path of the real-time mode: the largest frame's bytes and copies
(`upload.bytes_max`, `upload.writes_max`), and `upload.over_budget`, the frames whose bytes were
over `UPLOAD_BUDGET.bytes` or whose copies were over `UPLOAD_BUDGET.writes` (a rebuild of less
than the budget is not over it, whatever it is), each counted under one cause, the **first** that
applies, in this order: `jump` or `bands` (a rebuild, by the scene's rebuild reason), `edit` (the
frame took the newest Edit in), `growth` (a buffer grew in the frame), or `operation` (none of
those: the operations that took the budget, and the gap words). The causes are exclusive and add
up to `over_budget.frames`. The stepped tests measure the same on the test Planet: a fill with a
still camera and a small budget, one with a budget well over the largest operation (the plain
budget holds in every frame of it: this is the test that fails if the throttle does not hold
work back, with a still camera), and a flight from the spawn over ground that is still loading
(24 Voxels a frame, 120 frames), with a budget of one largest batch. The flight's control, with
nothing held back, has frames of 866 324 bytes in 69 copies and a largest follow step of 515 964
bytes. The throttled flight's follow steps are held to the budget (the test asserts it), and its
worst update (231 712 bytes, 23 copies, the budget being 203 724 / 23) is under a third of the
control's in bytes and half in copies; it is a little over the plain budget, by part of an
operation. The flight does not exercise the bound's terms for the crossing operation and the
round's first batch: built from the control's largest batch (203 724 / 23, twice, as the follow
steps are cut to the budget) the bound is 610 k bytes in 69 copies, looser than the control's own
worst, so it checks nothing there. (Those terms are what `with_a_small_budget…` exercises, against
the scene thread's own count of them, `Stats::excess_ops`, which holds whatever the throttle
decides.) The flight test fails, in a message that names the throttled flight, if the `break`
between batches in `stream()` is removed, and if the follow steps are not cut; it does not notice
the early return in `follow()`, which in the flight never has an outbox over the budget to hold
back for. A partly broken throttle that keeps every update under a third of the control's bytes
and half its copies is not caught by it. The follow throttle is tested by
`the_camera_is_not_followed_while_what_waits_is_over_the_budget_and_is_when_the_frame_thread_has_taken_it`:
the frame thread does not take what two rounds publish, with a budget of one byte, so the outbox
is over the budget when `follow` looks; one round follows (by a step) and the next two do not,
then the next follows again, with no hole in any frame, and the scene walks on to the camera and
draws what following in every round comes to (within 1% a level).

**What the 4090's first run of the budget (`9539899`) showed, and the tests that now cover it**
(`docs/measurements.md`, "The 4090's real-time run of 9539899"):

- *An operation larger than the budget is split:* `a_camera_that_the_scene_fell_far_behind_is_walked_to_in_steps_within_the_budget_and_one_that_was_put_there_is_jumped_to`
  (the camera four jumps away, the scene walked to it in 44 patches, the largest follow step within
  the budget, no rebuild; a camera put there in one move is still jumped to).
- *A flight and a stop do not end in a jump:* `a_scene_thread_that_is_busy_while_the_camera_flies_on_walks_to_it_after_and_does_not_end_in_a_rebuild`
  (the scene thread busy for 1.5 s of a 2 s flight, the scene 2 240 Voxels behind, a jump being 128
  here: 0 rebuilds).
- *The fill is not slowed by the default budget:* the constants test
  `the_default_budget_is_over_what_a_baseline_fill_frame_uploaded_so_it_is_not_what_bounds_a_fill`
  holds the copies to the 4090's 1 400 per fill frame (it fails with 128; the copy cap was what
  bound the 4090's fill). `with_the_default_budget_a_fill_takes_about_as_many_frames_as_one_with_no_budget`
  (43 against 43 frames, within 20%) **does not cover that**: the stepped test Planet's frames are
  too small to reach 128 copies, so that test still passes with the cap at 128. The stepped test
  Planet cannot show the regression; only the constants test, and the 4090, can.
- *A follow step is bounded whatever came before it:* `a_follow_step_is_sized_by_how_its_cost_compared_to_its_share_and_is_never_over_the_limit`
  (unit test of `FollowStep::after`: doubles at most, halves at most, a quiet step doubles to the
  limit and not over it, a run of costly ones stops at a quarter of the limit) and
  `the_least_a_voxel_costs_makes_a_step_at_most_64_voxels_at_the_default_budget` (the constants).
  `a_follow_step_is_within_the_budget_after_a_run_of_quiet_steps_and_after_teleports` (a 160 KiB
  budget; after 40 quiet steps and 20 teleports no step is over it) **does not show the limit**:
  it passes with the limit removed (checked), because a step's cost on the test Planet is the part
  that does not depend on its length, not Voxels of it. It guards the controller (a step that
  came to be over the budget after those). Round 7's claim that it "fails with the floor removed"
  was only of its teleport case (the quiet-steps case passed without the floor), and the floor is
  gone: the teleport case of round 7 is now `a_teleport_that_ends_within_a_jump_of_the_scene_is_walked_to_within_the_budget`.
- *The selection is not made to look for the camera's move:* `the_selection_travels_as_far_as_the_scene_walks_when_the_camera_goes_faster`
  (a camera that goes 40 Voxels a frame with a lead, the scene walks 960 of 2 360: the selection's
  travel is 2.0 times the walk; **3.5 times, and the test fails, with the places looked ahead to
  from the camera**, as at c537d09). On the test Planet the camera goes 2.4 times as far as the
  scene walks; at full size 8 times, and the factor, the 3 million nodes a call, is time that
  the stepped clock does not measure: the test is of the cause, not of the time (the time is the
  ignored full-size test `soft_misses_on_the_flight_take_off_and_standing_paths`, which prints
  the drawn lag of each path).
- *The thread goes on when the frame takes what waited:* `a_throttled_scene_thread_goes_on_when_the_frame_thread_takes_what_waited`
  (real threads, in `.github/real-thread-tests.txt`: it counts how the scene thread's throttled
  waits ended (`SceneWorker::throttled_waits`, test-only) and asserts that at most half end by the
  20 ms timeout, over 40 waits: 0 of 40 in every run of the job's command and of the whole binary;
  with `notify_one` removed 40 of 40. The share of frames with an update, which it counted before,
  depended on the load: 5 of 250 in the real-thread job).

### The gates

In `bins/client/bench-budgets.toml`, table `[streaming]`; every key is needed (a missing one would
turn its gate off, so it is an error, like a misspelt one), and `--real-time` with budgets that
have no such table does not run. Each gate is held to the worst of the runs, and a gate that is
over exits with status 2 (0 with `--no-fail`).

| Gate | Key | Budget | The worst over the runs of |
| --- | --- | --- | --- |
| Frame time | `frame_time_p99_ms` | 16.7 | the p99 of `frame_time`, of every path |
| Frame-thread CPU | `frame_cpu_p99_ms` | 8.0 | the p99 of `frame_cpu`, of every path |
| Holes | `holes` | 0 | the holes that the scene thread's looks found over each **whole run**, from the moment the measuring began, summed over the runs. It also fails if a run, or a path of a run, had **no look at all**: a gate that looked at nothing has not passed |
| Memory | `scene_mib` | 512 | the scene's buffers **in use** (`scene_used_peak_mib`; the full-size test's 474 MiB measured, #143). **It is not VRAM**: what the GPU reserves is `scene_reserved_mib`, reported and not gated |
| Memory | (the store's own budget) | | the store's size against its limit, `store_limit_bricks`, which the client sets from `STREAMED_PER_D0_SQUARED` and `BUDGET_OVERFLOW` |

**Frame time is the pessimistic figure.** Here a frame is the CPU's work and then the wait for the
GPU, one after the other, with no presentation; in a window the CPU records the next frame while the
GPU draws the last, so the two overlap. A run that meets 16.7 ms here meets it windowed, and one
that does not may.

Soft misses, the time to level 0 under the camera, settle time and Edit latency are reported and
not gated. The final stage budgets are not these: they are the stages of #10, per pass (#13's
`[passes]`).

#### What the holes gate can and cannot catch

It catches a brick that is split with the bricks under it not resident **at the moment of a look**:
the scene thread looks every 5 s (`HOLES_EVERY`) while it works, a look is forced at the end of
every path, of every catch-up, of the sizing and of the run, and the gate is on everything those
looks found in the run, between the paths too. It fails when a run or a path had no look at all.

It cannot catch a hole that is made and filled **between two looks**: one that lasted less than
the interval between looks is not seen, and that includes a hole that is in one frame of a flight
and gone by the next. A path's looks are at the interval and the end of the path, so the 2 s turn
and the 6 s take-off have the one at their end and, at most, one more. The stepped mode on the
test Planet counts holes in every frame, and so catches those on the test Planet, where the
scene thread splits only a brick whose children are resident (so a hole is a bug in that rule). **An
exact count at every publish at full size would be follow-up work in `render`**: `Scene::holes` is
a pass over every split brick, which at full size is about 0.3 s, and what would make it exact is
keeping the count of split bricks with a missing child as the scene changes (made and unmade by the
selection and by eviction), so that a publish reads it and no pass is needed. Until then the gate
is a sample.

### Reading it and tests

The gates, the least and the most over runs, the shape of the report, the report of a run that
stopped, and the budgets table are unit tests in `bench/realtime.rs` and `bench/budget.rs`, with no
GPU and no Planet: one test per gate builds runs that are within every gate but one and checks that
exactly that gate fires, with status 2 (the report is built from numbers, so they gate on Linux and
on Windows). For the holes there are three: a hole in a path, a hole **outside** any path (between
them, at the sizing, at the last look) and a gate that looked at nothing. A stepped test of the
scene thread (`streaming_tests`) checks that a look that is asked for counts the soft misses and
looks for holes whatever the clock says.
The slowest-frames list, the frames over the limit by update kind, GPU pass times that are null and
not zero, a dig with nothing in reach and a dig refused (spent by digging in a world with no scene
thread; the report has `edit_latency` null with its reason, and the run goes on), the envelope of a
run with no Edit, and the catching of a panic are
synthetic tests of the same kind, with no threads, and gate.
`the_real_time_run_on_the_test_planet_completes_with_every_path_and_field` runs the mode on the
test Planet with real threads and no GPU, and asserts what was run, no time (a look for holes in
every path, the Edit taken in); `a_run_that_stops_names_the_step_it_was_at` is its error case.
Both are in `.github/real-thread-tests.txt` and so do not gate. CI does not run the full-size mode.

#### When a run stops

A run that stops (the loader that does not catch up within its cap, a GPU error, the Edit not
taken in, a full scene) ends the mode, and the bench **still writes `report.json`**: the normal
bench's results, `streaming_real_time.runs` with the runs that finished (the `min`, `max` and
`gates` are of those; null `min` and `max` if none finished), and `streaming_real_time.error`:
`{ "run", "step", "message" }`, the run that was going and what it was doing (`building the
world`, `making the GPU context`, `cold start`, a path's name, `the loader's catch-up after ...`,
`sizing`, `edit latency`, `the last look`). The exit status is **1**, even with `--no-fail`: an
error says nothing about whether the gates would have held, and it is not 2. `error` is null when
the mode ran to its end.

**A panic** in the paths of a run is caught and reported as an error: the same errored report, with
the step the run was at and the message `panicked: ...` (the panic's own message and backtrace still
go to stderr). A panic in making the world or the GPU context, or in ending the run, is caught at
the step `building the world, making the GPU context, or ending the run`. A panic on the loader's
workers or the scene thread reaches the frame thread as one and is caught the same way. What is not
caught is the rest of `client bench`: a panic in the normal bench before the real-time mode starts
ends the process with **status 101 and no `report.json`**, as does any panic outside the run; and a
process abort (an out-of-memory kill, a driver crash) ends it by signal, also with no report. In
those cases the output on stderr is all there is.

## Reading the report

```
{
  "schema_version": 15,
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
                           "error": { "run", "step", "message" } | null } | null  // --real-time only
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
