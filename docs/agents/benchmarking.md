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
  mode" below); soft misses at the speed-scaled level (none on the flights, ADR 0009), time to
  control (the 20 m ring) and settle time are gated by that run since #168, the settle limit
  **proposed** and not yet agreed (Edit latency is reported by it too, ADR 0006's bound). The part of this that CI can do,
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
| `--stream-slice-ms N` | With `--real-time`, how long the scene thread takes in what the loader made for in a round while the loader's workers are as far ahead as they may be (`scene_worker::BACKED_UP_SLICE`, default 32). For measuring what another slice does to the flights' camera soft misses, the settle and the frame times (#168); the report's `config.stream_slice_ms` says what it was. |
| `--runs N` | With `--real-time`, how many times to run the paths in this process. Default 3. The report keeps each run and the least and the most of every number over them; the gates use the worst. |
| `--flat-ambient` | With `--real-time`, runs it with the flat ambient light (the goldens' and tests' lighting, no radiance-cache GI) instead of the game's GI, to tell what the GI costs in the real-time frame: the same run, the same paths. The planet's label in the report says ", flat ambient". An error without `--real-time`. |
| `--flip-gamma-2-2` | Also score each checkpoint with the old gamma-2.2 curve (#188), from the same samples, to tell the sRGB move from the estimator's effect. Not with `--no-convergence` or `--short`. |
| `--no-convergence` | Leave out the convergence phase (below), which otherwise runs after the caves. |
| `--truth-spp N` | Samples of each of the convergence phase's two truths (the emissive room's have at least 8192 with FLIP, `ROOM_TRUTH_SPP`). Default 1024; ADR 0005's is 8192, which takes eight times as long (`client.exe bench --truth-spp 8192` is the 4090 baseline). At least 1024, the last checkpoint. |
| `--rt-backend B` | How rays find the Planet (#50, a spike): `compute` (the default: the compute-shader DDA, the reference), `hw` (ray queries over the bricks' tight boxes, shadow rays ending at the first hit, for the flight, the sweep and the Edit stress), `hw-nearest` (`hw` with shadow rays that look for the nearest hit) or `both` (the bench as for `compute`, then the same flight and Edit stress on `hw` and the flight on `hw-nearest`, each on a context of its own, and the three compared). An adapter without ray queries runs compute with a warning and the reason in `rt.fallback`, not an error. See "The hardware ray backend" below. |
| `--no-fail` | Exit with status 0 even if a pass is over budget, a frame of the streaming mode has a hole, or a gate of the real-time mode is over. |

It runs unattended. Without the convergence phase it takes a minute or so (generating the whole Planet is about 22 s of it); the convergence phase, on by default, adds about 45 s on the 4090 at the default `--truth-spp`, and more on a slower GPU or with a larger `--truth-spp`.
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

6. Then the convergence phase (#187), unless `--no-convergence`: how fast the Reference view gets
   clean, on two still views, scored against a truth. It has GPU contexts of its own and runs after
   everything that is timed against the budgets, so it cannot move them. See "The convergence
   phase" below.

7. With `--rt-backend both` (#50), after the cave stills: the same flight and Edit stress again on the
   hardware ray backend, and the flight on its nearest-hit variant, each on a context of its own. See "The hardware ray backend" below.

8. With `--streaming`, after everything else (the cave stills and the convergence phase included, so that the flag cannot
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
which the flight above leaves out, **with the horizon cap** (ADR 0004, #146: the test Planet's scene has the Planet's
horizon, as the game's does). It runs **stepped** (#180, `docs/agents/ci.md`): no threads, no
sleeps, and a clock that only the bench moves, a frame (16.667 ms) at a time, so the work of each
frame does not depend on how long anything took. The code is the game's (`World`, `Loader`,
`SceneWorker`); the game starts threads and never steps, and its clock is the machine's (see
`bins/client/src/clock.rs`). The flight above does not use the world or the clock at all.

The schedule is fixed by frame index (`bins/client/src/bench/stream.rs`): the camera at the spawn
until the resident set is complete (**fill**); then 60 frames standing (**steady**) and 180 frames
walking at 24 m/s along three segments (**moving**), with an Edit at frame 150 of them and the
store held to 102% of the complete set so that the walk evicts. The bands have a `d0` of 64
Voxels, so that the fill is a few seconds, and the Guard is the game's three levels lower (levels 10 to 17: the test Planet is an eighth of the radius, so level 10 holds about what level 13 does at 320 km, #167).
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
  purpose, the test prints the new hash; put it in the file and say why in the PR. It is `2466f0700e79713a` since #167 (18 levels and the test Planet's Guard from level 10: the pyramid is deeper, so the rings and the soft-miss levels are 18, not 14; `38b58d66b96791af` from #146 before it): the stepped scene has the horizon cap, so the drawn bricks of the Guard's levels went from 9 536 to 8 482, the rings' and a follow step's counts and the upload (64.30 to 64.80 MB, 6 439 to 6 986 copies) moved with them; what is streamed, arrived and evicted did not (`docs/measurements.md`). **The committed hash is the stepped run with the speed scale on** (#168), which is not that one: `26a1ce75…` at the first push of #168, `bd99020be09af4ba` at `99b08c4` (the scene thread takes in for longer while the loader is backed up and a margin node is not parked by a coarser change), `485be9b0eb879d35` once a coarser change parks only the nodes on the hot list (`docs/measurements.md`). The run with the scale **off** is still `2466f0700e79713a` (`with_the_speed_scale_off_the_counts_are_those_the_run_had_before_it`).
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
(`2466f0700e79713a` since #167, which made the pyramid 18 levels; `38b58d66b96791af` from #146, which put the horizon cap in it; `e621feef18ef1801` from #145, which added the `upload` writes and budget counts, until #166's terrain) still run first.

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
| `sea-level crossing` | (#213) The raised camera down to 30 m under the sea and up to 80 m over it along the Spawn's radial at 50 m/s, held at the bottom until the scene has come to the camera, and at the top until it has and the flip is absorbed (`World::caught_up`; up to 90 s each), so **the horizon cap goes off and comes back on** (`CAP_ON`, 50 m over) and the path ends with nothing left to absorb. The scene follows a camera through the ground at about 0.8 m an update, so a path that does not wait never gets to the flip. **The bottom does not wait for the flip to be absorbed:** before #226 a scene whose cap was off, once built, was the wide one (544 to 557 MiB in use at 30 m under the sea and 567 MiB at the top, the 4090 566 to 567 MiB, #213), so a path that lingered there failed `scene_mib`; a camera under the sea now has the cap of the sea's sphere (ADR 0004), and `held under the sea` is the path that stays. |
| `held under the sea` | (#226) Straight down the Spawn's radial at 8 m/s (under the 10 m/s at which the depth of the speed-scaled bands is 0) to 30 m under the sea, then held there until the scene has come to the camera, the flip is absorbed and the loader has nothing left (`World::caught_up`, up to 240 s). It is the memory of a camera that stays under the sea with the cap off: **774.4 MiB before #226** (the whole Planet drawn to its far side), **465.5 MiB after** (the cap of the sea's sphere over the camera; the dev box, full size), against the 512 MiB gate that `memory.scene_used_peak_mib` reads. The ignored test `a_camera_held_under_the_sea_at_full_size_keeps_the_scene_within_the_gate` (`cargo test --release -p client --bin client -- --ignored --nocapture held_under_the_sea`, about 5 minutes, no GPU needed) runs this path alone and asserts 0 holes and the gate. |
| `stop after a flight` | 5 s of the flight at 2 m, then the camera stands where it was until the scene has no soft misses (up to 120 s). Settle time is in it. |

After the last path the camera is back at the Spawn and the loader has caught up, and **one dig**
is made there (below): the run's **Edit latency**. Its frames are not those of a path.

**The still hold** (#217, the run's `still_after_flight`, described under "The fields"; not a path, so no
gate sees its frames): 5 s of the flight of `stop after a flight`, then the camera stands with the game's
policy for a Reference view, which is **catch up first, then hold** (`World::hold_still`, the call the
client makes): once the camera has stood for 2 frames the scene thread goes on loading and following the
camera, with Primary drawn, until the scene has reached the camera (`World::caught_up`: its lag under 1
Voxel, and no reband pending, #168: the nodes that a change of the bands listed are looked at, and the
speed scale has no step still to give back, `World::reband_pending`) or `CATCH_UP_CAP` (12 s, 5 s until 2026-11-17) has gone by. Then loading is paused and
the scene thread is told; **it holds back moving the scene to the camera, the steps of coarsening and, since #168, the
reband** (the speed scale is not looked at and what a change of the bands listed is not looked at;
leaving Still does both) (#217, ADR 0005, "Still frames") and takes in no arrivals. The camera stands for 3 s (1 s with `--short`)
from then. What the frame thread takes in meanwhile is the cuts of the Reference view, counted by cause
(`cuts`, as `still_cuts` below, under the paths) over the held frames. **The acceptance of #217 is at most 1 cut here from
scene patches** (the Planet has no Edit in this run). Then the view changes (loading goes on) and
`catch_up` is the time to the first frame with no soft misses, nothing waiting and the scene at the
camera, and `holes_after_catch_up` the holes the look after it found, which must be 0. Reported, not gated.

### The fields

`streaming_real_time.runs[]` (and `min` and `max` of the same shape), each made by this mode:

- `cold_start`: `world_map_s` (generating the map, which the client does before `load`), `load_s`
  (`load`: the spawn ring, the Guard, the first scene), **`time_to_level_0_under_camera_s`** (from
  the start of the load to the first frame in which one ray from the camera straight down hits the
  ground at level 0) and `fill_s` (from the start of the load to the resident set being complete).
  `time_to_level_0_under_camera_s` is not gated, and **is not the spec's "time to control"**: the
  field is named for the ray that it uses, and the window opens at the end of the load, so it reads
  the load time. **`spawn_ring_s`, `guard_s` and `time_to_control_s`** (#168, #220) are the parts of the
  load, from `crate::load`: the 20 m spawn ring (`SPAWN_REACH`), which is the player's control (ADR
  0009: the character stands in it, and the bench has no server), the part of the Guard after it that
  a camera within 2 km of the Spawn could see (`guard_s`: the cells that `plan_guard` says are
  visible, judged at the Planet's highest radius), and the real time from the world map being ready
  to the window opening (the ring, that part of the Guard and the first scene). The rest of the Guard
  is made by the loader's workers in the background (#220, schema 28): **`guard_cells`** is how many
  cells it has (bricks of the pyramid's top level; 0 on the test Planet, whose Guard is whole),
  **`guard_cells_at_control`** how many of them were made before the window opened, and
  **`guard_complete_s`** the time from the start of the load to the last of them being taken in
  (reported, not gated). Until then a flying camera is held to a ball of 2 km round the Spawn
  (`World::hold_to_guard`; the character is not held). **Two gates**, each the worst over the runs:
  `spawn_ring_s` (2 s) on the ring, and **`time_to_control_s` (2 s since #220)**, which is the
  acceptance of "control in 2 s with nothing visible from the Spawn sky". It was 10 s while the
  window waited for the whole Guard (4.3 s on the 4090, 7 s on the loaded dev box).
- `frame_parts` and `gpu_passes` (#54, schema 29; in each path), to find where a frame's
  time goes: `frame_parts` is `{recentre, update, encode, submit, wait, gpu_total, wait_not_passes}`,
  each a `Spread` (`samples, mean_ms, p99_ms, worst_ms`) of the frame thread's parts of one frame
  (`wait` is the wait for the GPU after submitting; `gpu_total` the GPU's own time of the passes;
  **`wait_not_passes` is `wait` less `gpu_total`: time the frame waited that no timed pass accounts
  for**, which is the driver or the GPU idling between frames when it is large). `gpu_passes` is a map
  from the name of each timed pass (`primary`, `normals`, `shadows`, `gi`, `shade`, `atmosphere`,
  `bloom_*`, `exposure*`, `tone`, `upscale`) to the `Spread` of its GPU time, so a pass that costs
  more in the game than in the GPU bench is visible. The summary prints both per path.
- `unpaced_control` (#54, schema 29; a run's, a path's form, not gated and not in `paths`): 180 frames with the
  camera standing at the Spawn **and no pacing sleep**, each frame following the one before as the
  GPU bench's do. Set its `frame_time` and `gpu_passes` beside a paced path's: a frame that is much
  longer paced than unpaced is the GPU idling between frames (clocks dropping, the driver), which no
  pass accounts for; the same length is the work itself.
- `still_after_flight` (#217, schema 22; a run's, not a path's): the view held Still after a flight, with
  the game's policy ("The still hold", above):
  - `before_hold`: the catch-up before the hold. `lag_when_still_voxels` is how far the scene was behind
    the camera when the view went Still (what a hold at once would have held it at), `lag_when_held_voxels`
    how far when the hold began (under 1 unless the cap ended the wait), `waited_s` from the view going
    Still to the hold beginning (how much later the Reference view started), and **`hit_cap`**, true if the
    wait ran to the cap and the Reference view started on a scene still behind the camera. Expected false. After #168 the wait also covers a reband (the depth of a flight is given back half a second after the stop, and what it lists is looked at: 50 s or more at full size on the dev box), and a release over `RELEASE_REBUILD_OVER` nodes is a rebuild of 8.5 to 9.0 s of the scene thread's time in the stop path and 10.0 to 10.4 s in this one on the 4090 (24 to 37 s on the dev box). **The cap is 12 s since 2026-11-17 (owner), so `hit_cap` is expected false on the 4090** (`a7c6b72`: the wait ended 10.87 to 11.24 s after the view went Still in 5 of 6 runs, and ran to the cap in 1; the figure of 9.3 to 9.7 s that the cap was set from was the stop path's `reband_s`, counted from the start of that path) **and true on the dev box**; `hit_cap` and `lag_when_held_voxels` say what the cap held.
  - `hold_s`: how long the camera stood after the hold began.
  - `cuts`: `still_frames`, `cuts` and `by_cause`, as `still_cuts` under the paths, over the held frames
    only. **At most 1 is the acceptance of #217.**
  - `catch_up`: `seconds` and `settled`, from the frame that ended the hold to the first frame with no soft
    misses, nothing waiting and the scene at the camera (or the cap).
  - `holes_after_catch_up`: the holes the look after it found. Must be 0.
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
    frames, and the harness's read of the process's resident memory, every 30). Both are reported
    for a slow frame as `camera_ms` and `after_ms`, and `after_lock_ms` says how much of `after_ms`
    was the wait for the store's lock (the rest is the resident read and bookkeeping, which the
    game does not do). The pacing sleep is in neither.
  - `frame_cycle` and `sleep_overshoot` (schema 19, #167): `samples`, `mean_ms`, `p99_ms`,
    `worst_ms` of the **pacing** that `frame_time` leaves out. `frame_cycle` is the time from the
    start of a frame to the start of the next (the path's last frame has none), **less what the
    harness itself does between two frames** (`harness_ms` below: the look at the stop of a flight):
    with the pacing sleep it is 16.7 ms when the frames are short, and a cycle that is longer than
    the frame and its sleep need is the frame thread waking late or the store's lock. Not gated:
    `slowest_cycles` (the five frames with the longest turn, in the form of `slowest`) says which
    part of a long cycle it was, and `frame_cycle.worst_ms` is the number to read against it. A
    gate on the worst cycle would be on one frame of a thousand, which a Windows scheduler tick
    can make 30 ms, and the p99 of a paced cycle is 17.3 ms whatever the work, so a gate on that
    would hold nothing either. **`harness_ms` is more than the wait for the scene thread:** the
    look at the stop also draws a frame on the GPU with the update that the look made, which the game
    would make in its next frame, so a stop's `harness_ms` is the wait plus one frame's copy and
    draw, and not all of it is work the game would not do. It is left out of the cycle as the bench's
    own call, not as time the game does not spend.
    `sleep_overshoot` is by how much each pacing sleep ran **over what it asked for** (the frames
    that slept; a frame that was already over 16.667 ms does not). On Windows a `thread::sleep` is
    rounded up to the timer's tick (up to 15.6 ms unless the process raised the resolution) and
    waits for the scheduler, so a sleep that is 10 to 30 ms over is the host's. Not gated.
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
    - **the frame's pacing** (schema 19, #167), outside `time_ms`: `before_ms` (from the end of the
      frame before, its sleep done, to this frame's start: working out the camera and the
      harness's own bookkeeping; 0 for a path's first frame, since the end look of the path before
      it, about 0.3 s at full size, is not a wait for a wake-up), `sleep_asked_ms` (the sleep that
      fills the frame to 16.667 ms; 0 if it was longer), `sleep_over_ms` (by how much the sleep ran
      over that), `between_ms` (the harness's bookkeeping between the look at the memory and the sleep; `time_ms`,
      `after_ms`, `between_ms`, `sleep_asked_ms` and `sleep_over_ms` add up to `cycle_ms`, which a
      test of the report holds to 0.05 ms) and `cycle_ms` (from the frame's start to the end of its sleep; the cycle to the
      next frame's start is `cycle_ms` plus the next frame's `before_ms`), and `harness_ms` (of
      `before_ms`, the look the harness makes at the stop of a flight: it waits for the scene
      thread to finish a round and counts the soft misses, then copies what that made to the GPU;
      a player's frame does not do it). **How to read a 30 ms frame** (the 180° turn's waits, #166 and #167): if `cpu_ms` is normal and `wait_ms` is long
      with `gpu_total_ms` short (`wait_not_passes_ms` large), the wait was **not our passes**: the
      frame thread woke late from the device poll or was not scheduled, or the GPU was busy with
      other queue work or the compositor. That is not work of ours, but this field alone does not
      say which of those it was (a late wake-up has a `sleep_overshoot` and a `before_ms` of the
      same kind elsewhere in the run; other GPU work does not). If `recentre_ms`, `update_ms` or
      `encode_ms` is long, it is our work. If the frame is normal and the `sleep_over_ms` of the
      frame before it (look at index − 1 in `slowest` of the same path only if that was slow too,
      else `sleep_overshoot` says how often it happens) is long, the harness's sleep woke late and
      the frame started late, which `time_ms` does not count;
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
      `gpu_primary_ms` (**from schema 20 it excludes `normals` and `shade`**, which have lines of
      their own: add them for what schema 19 called `primary`), `gpu_upscale_ms`, and
      `gpu_total_ms` (from the start of the frame's first
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
    reports the last count, and the count stays within about 100 ms of the truth. The scene thread's own soft misses are **reported and not gated**: it counts
    them against **its own** camera, which lags the frame thread's (see `scene_lag_*`), so 0 is the
    scene's, not the camera's (`gates.scene_soft_miss_frames` is the flights' frames with any).
  - `camera_soft_miss_frames`: the frames of the path in which **one ray straight down from the
    frame's camera** hits ground drawn coarser than the (speed-scaled) bands ask at its distance,
    less the bands' 5% hysteresis (scored on the flights only). **Gated** (#168): on the flights at
    2 m and 50 m, the take-off and the turn, summed, at the most of the runs, must be 0
    (`camera_soft_miss_frames`). It is the soft miss of what the player stands on, at the camera
    that is drawn from, which the scene's own count is not. The alternative, scoring every brick
    at the frame camera, was not taken: it is a pass over every node a frame, for the same answer
    where the player looks. The cold start and the stop are not flights: they fill and refine,
    and have many.
  - `scene_lag_d0_max` and the others: **the max over the four flights is gated**
    (`scene_lag_d0_max`, 8.0): main's 7.81 `d0` on the 4090 at 50 m plus a margin, for now. **The
    limit tightens to about 2 `d0` once the follow fixes land**; it is not a statement of what the
    lag should be.
  - `generated_bricks`, `arrived_bricks`, `loader_queue_max` and `loader_queue_mean` (#168): what
    the loader did in the path (and in the forced look after it): the bricks the generator was
    asked for, and the bricks of the arrivals that the scene thread took in; and the loader's
    queue, the bricks asked for and not yet taken in (`LoaderStats::pending`), as of each frame,
    its most and its mean. They are for reading a flight as limited by the generator (a long
    queue, few arrived) or not; reported, not gated.
  - `loader_generating_s`, `loader_held_back_s`, `stream_calls` and `stream_slices_used` (#168):
    what the loader's workers did with their time in the path, summed over them (the paths'
    seconds times the workers is what there is): generating, and waiting **with jobs queued**
    because they were `MAX_AHEAD` jobs ahead of what the scene thread had taken in. Held back
    most of the time with the queue long means the scene thread's taking in limits the loader, not
    the generator. And how many times the scene thread took in (its time is `scene_thread.pack_ms`)
    and in how many of them it stopped because its slice was used up. Reported, not gated.
  - `scene_lag_d0_mean`, `scene_follow_steps`, `scene_budget_cuts` and `camera_soft_miss_per_second` (#168; the last two
    lines of the 4090 sweep of the slice ask for them): the
    mean of the drawn scene's lag over the path's frames, in `d0` (the maximum and the 99th percentile
    do not say how long the scene is behind), the rounds in which the scene moved to the camera and
    those in which it held work back for the upload budget, the frames with a camera soft miss in
    each second of the path (one entry for every second the path covers, so a trailing 0 is a second without a miss; a flight only: at the start of it or all along it). Reported, not gated. (The most that the
    scene's buffers in use were in a path is `scene_used_peak_mib`, #213's, below.)
  - `camera_soft_miss_lag_side`, `camera_soft_miss_loader_side` and `lag_over_floor_frames` (#168,
    the flights): the camera misses split by what can have caused them. The score asks for level 0
    within `floor(1) * (1 - HYSTERESIS)` (about 0.67 `d0`) of the frame's camera, so a frame whose
    drawn scene is further behind the camera than that is a miss by construction, whatever the loader
    has made. `lag_over_floor_frames` counts those frames (misses or not); a miss in one is **lag
    side** (the scene thread's following: the long slice's giving way to a camera that moves is
    for it), a miss in any other is **loader side** (a brick that the bands ask for is not there:
    the generator; decision A's curve, 3 levels at 50 m/s on levels 0 to 4, is in). The two add up to `camera_soft_miss_frames`.
  - `scene_release_rebuilds`, `release_frame_ms_max` and `release_update_ms_max` (#168): how many
    times a finer depth made the scene again (`Rebuild::Release`, `scene_worker.rs` `RELEASE_REBUILD_OVER`,
    500 thousand nodes) instead of looking at its nodes again, and the frame thread's cost of the
    one it took: the longest frame that had one and that frame's update (the copy to the GPU, which
    is over the budget like a jump's and is counted under `over_budget.bands`). The scene thread's
    time in it is `scene_thread.release_ms`, to be read against #217's `CATCH_UP_CAP` (12 s since
    2026-11-17; it was 5 s, which the 4090's 8.8 to 9.2 s hit with `before_hold.hit_cap` true).
    The release frame is the one frame over the limit that the owner accepted (about 230 ms); the
    frames around it are not: **while the scene thread makes the scene again the threads that read
    and pack its bricks run at the lowest priority** (`render::scene::select::set_worker_start`,
    `priority.rs`), because all the cores at the frame thread's priority kept it from waking for
    30 ms at a time (26 to 43 frames over the limit, update `none`, on the 4090), and **the
    frame thread writes only the used words of the rebuilt buffers** (`Scene::filled_words`: the
    rest of the roomy buffers is zero, which the GPU makes itself): a rebuild writes about half of
    what `upload.bytes_max` says (940 MB), which still counts the buffers made, the used words and
    the zeros, so that the count is the same on every machine. Reported, not gated.
  - `hole_looks`, `holes` and `holes_per_frame`: **holes are sampled, not counted for every
    frame** (see "What the holes gate can and cannot catch"). Looking at every split brick takes
    0.3 s at full size with the store's read lock held, so the scene thread does it every 5 s
    (`SceneWorker::count_holes`; **in threads on every core, and a look that is only due waits while
    the bands are speed-scaled**, #168: on the 4090 it held the lock for about 90 ms in a flight and the
    camera had misses in the second after it), and **one more look is forced at the end of every path**
    (`SceneWorker::look_now`), so no path has 0 looks. The report has how many looks there were in
    the path and what they found.
  - `scene_used_peak_mib` (#213): the most MiB of the scene in use at the end of one of the path's frames (the run's `memory.scene_used_peak_mib`, which is the gate, is the most of these and of the looks between frames; this says which path holds it).
  - `scene_used_end_mib` and `scene_level_bricks_end` (#168, schema 27): the scene's buffers in use (MiB) and the bricks drawn at each level when the path ended, to be read against `scene_used_peak_mib` and the bricks at each level at that peak (`memory.scene_used_peak_level_bricks`): a peak far over the end is a transient, and the levels say what it was made of (the crossing's 552 to 572 MiB was 251 K bricks at level 13). Reported, not gated.
  - `scene_round_max_ms`, `scene_update_max_ms`, `cap_flips`, `cap_flip_max_ms`, `backlog_peak`,
    `absorb_updates`, `absorb_ms` (#213, schema 26): the longest round of the scene thread in the
    path's frames (without the look for holes), the longest single update of the scene to a camera in
    them (one step of `follow`, the selection's update: the time the scene stood still for the frame
    thread), the number of times the horizon cap went off or on, and the longest update that was a
    flip's or part of absorbing one (0 if none). A flip of the cap makes every node be looked at
    again, and **the selection spreads that**: a sweep stamps 8 192 nodes an update, and the nodes
    that it finds changing when it first looks at them (with the children that revisiting them
    makes, and the nodes still queued under the old cap that change when their turn comes) are
    revisited at most 800 an update (and about 8 000 bricks changed), nearest to the camera *as it is
    then* first, the rest waiting in a backlog; what changes because the camera moved is not held
    back by it. Then the queue is tidied (65 536
    entries an update) of the entries that the sweep left stale. `backlog_peak` is the most nodes
    waiting at once, `absorb_updates` how many updates were absorbing (the sweep, the backlog, the
    tidying), and `absorb_ms` the time from the update that found a flip to the one that ended its
    absorbing, summed over the flips that ended in the path. **Absorbing goes on after the camera
    has stopped, so it can spill over into the next path** (the crossing waits at the top for it, so it
    does not; a flip absorbed at the end of another path would): these fields are in the path where it
    ends, and `cap_flips` is in the path where the flip was. **The acceptance of #213: the longest
    `cap_flip_max_ms` of any path (not only the crossing's) is no longer than the longest
    `scene_update_max_ms` of the paths with no flip** (those with `cap_flip_max_ms` 0), which the
    ignored test `the_sea_level_crossing_round_at_full_size_is_no_longer_than_the_other_paths_worst`
    prints and asserts (`cargo test --release -p client --bin client -- --ignored --nocapture
    sea_level_crossing`, about 8 minutes at full size, no GPU needed). The crossing's own longest
    update is an ordinary follow step and is within a few percent of the others' from run to run.
    A Still view waits for the absorbing to end (`World::caught_up`).
  - `scene_thread`: milliseconds the scene thread spent in the path on `follow` (selecting again
    for a new camera and packing what changed), `pack` (asking the loader, taking in what it made
    and packing it), `upload` (publishing: copying what changed out for the frame thread, which
    copies it to the GPU), `evict` (eviction), `coarse` (the coarse levels over an Edit) and
    `hole_scan` (the looks for holes), `release` (making the scene again for a depth that was
    given back, and the listing that was not looked at: `Rebuild::Release`), `reband` (looking again at the nodes that a change of the
    bands for the camera's speed listed, and packing what changed: **not in `follow`**; ADR 0009),
    and **`hole_scan_share`**: the looks' share of the scene
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
  - `still_cuts`, for `stop after a flight` only (null for the others; #217, schema 22):
    `still_frames`, `cuts` and `by_cause` (`edit`, `bands`, `coarse`, `follow`, `arrival`, `coarsen`, `reband`).
    A **cut** is an update that the frame thread took in while the view was Still and the Reference view
    was being shown: the camera had stood for 2 frames (`STILL_FRAMES`) and the frame before had no
    update either, as `render::still::Still::plan` counts (an update in the 2 frames after another is
    in Primary frames, and is no cut). The update goes to the GPU, which restarts the Reference view (a
    cut to Primary for 2 frames and a fade, from sample 0). The count matches the renderer's. Its cause is what the
    scene thread was doing when it made the patch: `edit` (an Edit's bricks), `bands` (a new LOD
    bias), `coarse` (the coarse levels remade after an Edit), `follow` (the scene moving to the
    camera), `arrival` (bricks the loader made, taken in) and `coarsen` (a step of the memory cap's
    coarsening, or one taken back) and `reband` (#168: nodes looked at again for a change of the bands for
    the camera's speed). A cut that was made of several kinds of work counts under each,
    so the causes add up to at least `cuts`. In **this** path nothing is held back (it measures the
    settle time, which needs the loader to go on), so this is what a Still view would get if the
    scene thread worked as it does for the Primary view. The held view is `still_after_flight` of the
    run. A stepped test (`a_still_view_held_after_a_flight_gets_at_most_one_cut_from_scene_patches`)
    holds the number on the test Planet: 118 cuts in 120 held frames before #217, all of them
    `follow`, and at most 1 after.
  - `settle`, for `stop after a flight` only (null for the others): `seconds` from the frame the
    camera stopped in to the first frame in which the scene is at the camera (the drawn scene's camera is
    the frame's), has no soft misses and nothing left to look at again for the bands
    (`Scene::waiting` counts both), or the cap, and `settled`; `walk_s` is the first part (the
    scene walking to where the camera stopped), `reband_s` the second (until the scene thread has
    looked at every node that the change of the bands for the stop listed: 0 if it had by then)
    and `fill_s` the rest (waiting for the loader's bricks, and making them into the scene); the
    three add up to `seconds`. **Gated** (#168), `settle_s`, 55 s:
    main's 47.8 s on the 4090 plus 15%. Over if it is over the limit or did not settle.
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
  - `scene_used_peak_in` and `scene_used_peak_level_bricks` (#168): what the run was doing when
    `scene_used_peak_mib` was reached (a path's name, or "the loader's catch-up after ...", which
    is time that is in no path), and the bricks drawn at each level then. The run's peak is of
    every frame, those between paths included; a path's is `scene_used_peak_mib` under it.
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
    growths; 0 if none grew: before round 11 it was set by every patch and so equalled the scene's
    size, e.g. the 4090's 1 000.3 MiB of `188ebe8`, which was not a growth). **A rebuild is apart**: while a rebuilt scene is made (a jump or
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
lists the gates that are over: `frame_time`, `frame_cpu`, `holes`, `memory`, `soft_misses`
(under the camera), `scene_lag`, `spawn_ring`, `time_to_control`, `settle`, `gi`. For the holes it also
has `hole_looks_fewest_in_a_run` and `hole_looks_fewest_in_a_path`. **`gi`** (#54, schema 29) holds Primary's
`gi` pass in this run, on the full-size Planet, which the stepped bench's small flight understated by
about 15 times (0.3 ms there, 4 to 5 ms here in the first run): the worst path's mean (`gi_mean_ms`, the
pass's own 1.5 ms) and 99th percentile (`gi_p99_ms`, 3 ms), from `paths[].gpu_passes.gi`; with no
timed path (`gi_paths_timed` 0) the gate is over, since a gate that looked at nothing has not passed.

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
  `writes_max`; **5489545's flights: largest update 4.5 MB and 700 copies at 2 m, 4.0 MB and 562 at 50 m**, a step or a batch), and the first step of a walk is 32 Voxels (at least a Voxel). That is a bound by the limit and the
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
- **No ceiling on the lag, and the walk does not keep up with a flight (rounds 8 and 9).** A scene
  that is behind is walked to (round 6), so a camera that moves faster than the walk keeps up with
  falls behind without limit; before round 6, as on main, `Scene::recentre` rebuilt the scene
  (`Rebuild::Jump`) on any move over `JUMP * d0` (2 `d0`, 2 058 Voxels at full size), which bounds
  the lag by a jump plus what the camera flies during a call. **Measured on the 4090 (a scene that
  is behind is drawn as it is: the bands it has, no hole):**

  | 10 s flight at 1 000 Voxels a second | c537d09 | 5489545 |
  | --- | --- | --- |
  | updates, flight at 2 m / 50 m | 12 / 10 | 116 / 93 |
  | thread's time in `follow`, 2 m / 50 m | 9.0 s / 8.9 s | 7.3 s / 7.5 s |
  | largest update, 2 m / 50 m | 3.7 MB / 3.5 MB | 4.5 MB / 4.0 MB |
  | drawn lag at the most, 2 m | 8 415 Voxels (8.18 `d0`) | 8 004 (7.78), p99 7.73 |
  | drawn lag at the most, 50 m | 8 558 (8.32) | 8 321 (8.09) |
  | take-off, lag at the most | 3.87 `d0` | 3.69 |
  | stop after the flight: starts, follow time | 4.66 `d0`, 28.1 s | 4.12 `d0`, 15.3 s |

  (c537d09's lag is the round-7 measure, the camera the thread was told against the scene's
  centre; 5489545's is the drawn lag, which is not under it for a scene that is behind.) A step
  went from about 0.75 s to 65 to 80 ms, there are ten times as many, and the scene still advances
  only 1 700 to 2 000 Voxels in a 10 s flight (steps average 15 to 20 Voxels, at the limit of 64
  or under): **what is left is the cost per Voxel walked, about 4 ms a Voxel on the 4090's CPU
  (the selection looking at what the move made due, the splits and merges, the packing), which is
  250 Voxels a second against a flight's 1 000. The walk does not keep up, and no step policy
  changes that: its cost a Voxel is the same for steps of 16 and of 200.** Round 7's "keeps up with
  about 3 000 Voxels a second" was a guess from `follow`'s time over a flight, which counted the
  steps that ended and not what they covered; it is refuted. **What the baseline (main) does, measured on the 4090** by PR #197 (main `7706578` plus
  measurement only, never merged; one run, GPU at 0% before it): over the bench's 10 s flights
  **the drawn lag is comparable with main's, not smaller everywhere**: better at 2 m (7.73 against
  8.84 `d0`), worse at 50 m (8.05 against 7.41), and take-off (3.55 against 3.59) and stop (4.03
  against 3.84) within 0.2, so within about 0.7 `d0` either way. Both are far behind, and neither keeps
  up.

  | 10 s flight at 1 000 Voxels a second | main `7706578` (#197) | `188ebe8` (this head) |
  | --- | --- | --- |
  | drawn lag at the most, 2 m | 9 088 Voxels (8.84 `d0`) | 7 945 (7.73) |
  | drawn lag p99, 2 m | 9 003 (8.76) | 7 876 (7.66) |
  | drawn lag at the most, 50 m | 7 622 (7.41) | 8 275 (8.05) |
  | drawn lag p99, 50 m | 7 537 (7.33) | 8 222 (8.00) |
  | take-off, lag at the most | 3.59 `d0` | 3.55 |
  | stop after the flight, lag at the most | 3.84 `d0` | 4.03 |
  | updates, 2 m / 50 m | 3 / 3 | 119 / 96 |
  | largest update, 2 m / 50 m | 24.7 MB / 36.9 MB (patches) | 4.6 MB, 689 copies / 4.0 MB, 548 |
  | frame time p99, 2 m / 50 m | 30.4 / 32.1 ms (gate fails) | 3.8 / 3.5 ms |
  | `recentre` calls that began in the path, 2 m / 50 m | 5 / 4 | (a step of at most 64 Voxels each) |

  The baseline's calls in the 2 m flight (Voxels, ms): 40 in 12, 17 in 146, 140 in 758, 804 in
  2 046, then 2 094 in 10 745, flagged `rebuilt` and still running when the path ended; at 50 m: 920
  in 552, 562 in 1 805, 1 862 in 3 773, then 4 231 in 10 133, `rebuilt`, running at the end. The
  larger walks main makes under a jump run at 300 to 500 Voxels a second (562 in 1.8 s, 804 in 2.0 s,
  1 862 in 3.8 s; the first 920 at 50 m, 1 700), against this head's 170 to 200; what main cannot do is walk past a jump: the call over
  2 058 Voxels is a repack that takes about 10 s at full size and during which the drawn scene does
  not move (the 4090's frame time p99 of 30 ms on main is the GPU waiting in frames with no update,
  as in the #162 section, not the repack). **What a `rebuilt` call is, and what the report's rebuild
  fields are.** A call's `rebuilt` flag says that `Scene::recentre` on the scene thread returned
  `SceneUpdate::Rebuilt`: the scene was made again at the camera the call was given (the camera when
  the call began: after the 2 094 Voxel call the scene was where the camera had been 3 s into the
  flight; in the stop path the next call found it 955 Voxels from the camera, which stopped 0.9 s
  after the call began). The next publish copies the whole scene (`Scene::mirror`) and the frame
  thread takes it as a `Rebuilt` update, one upload of the whole scene. The path fields
  `rebuilds_jump`, `rebuilds_bands`, `rebuilds_out_of_room` and `largest_update_bytes` count only
  the updates that reach a **frame of the path**, and they are 0 and a patch (34 MB) in every path
  for a reason that is the measurement's, not "no rebuild": the bench's `look` at the end of a path
  (and at the stop) waits for the scene thread to publish, so in the stop path (a flight of 5 s,
  then `look`, 64.2 s in all against a settle of 59.2 s) it waited about 10 s for the call that began
  at 4 083 ms and ended at 15 058 ms and copied its `Rebuilt` to the GPU **outside any frame**
  (`Session::look`), and in the flights the call was still running when the path's frames ended. So
  "a jump's rebuild" is a call and an update that no path's frames contain, and what main's rebuild
  costs the frame on the 4090 (about 330 ms for 871 MB) is not in these reports. It also means main's
  stop-path settle (59.2 s) has about 10 s of that wait in it and is not like for like with this
  head's (54.4 s).

  **What differs beyond 10 s (inferred from the figures above, not measured).** This head walks
  about 170 to 200 Voxels a second, so in a longer flight at 1 000 its lag grows by about 800 a second
  and has no ceiling, and after the camera stops the walk back takes about 5 s for every 1 000
  Voxels of lag. Main's scene stays where it was for the roughly 10 s a far `recentre` takes and
  lands where the camera was when the call began (checked in the stop path, above), so its lag has a
  ceiling of roughly the call's flight time, about 10 `d0` at this speed, and a camera that stops
  gets a scene that is at most that far off and walks the rest at a few hundred Voxels a second.
  By that reading this head's lag passes main's after about 12 s of a flight at 1 000 Voxels a second.
  **Neither keeps up with 50 m/s; keeping up is #168 (the speed-scaled LOD), which must gate the drawn
  lag** (`scene_lag_d0_max` and its p99), and the fallback below is the other way to give the lag a
  ceiling.

  **What round 8 changed** is how much of a step's time was not the walk: each round told the
  selection the camera's move since the last round (about 1 300 Voxels at 1 000 a second and 0.75 s
  a round) as the distance of the places it looks ahead to, and the selection looks at what that
  distance made due, so every call looked at 2 to 3 million nodes (80% of its 0.9 s on the dev box)
  whatever the step; now the places are from where the scene is to be after the round's first step
  (`Engine::round`), so a call costs what it walks, and the look at what is due is shared among
  threads (`look_at_due`: at most 4 and at most half the machine's, scoped to the call, order-preserving,
  only above 32 768 entries). **Round 9 measured it** after the look-ahead fix (loaded 8-core dev box, full-size
  flight at 2 m, two runs each): 53 and 58 calls, 52 and 57 of them over 32 768 due entries (about
  300 000 to 400 000 on average), the look at them 108 and 92 ms each serially and 24 ms with the
  threads, and the scene walked about 940 and 1 020 Voxels serially against 1 690 and 1 710 with them
  (drawn lag at the end 9 060 and 8 979 against 8 311 and 8 286). 5489545, which has it, passed every
  gate on the 4090 (frame CPU p99 2.0 ms), so it did not make the frames wait there. The unit test
  `looking_at_more_entries_than_it_shares_among_threads_comes_to_what_one_thread_finds` compares 1, 2,
  3, 4 and 8 threads on 5 000 entries (stale, changing and later), and under test the threshold is 64
  so that the render library's own unit tests reach the threads (559 calls in `cargo test -p render --lib`
  on a machine with 4 or more cores; `cfg(test)` is not set for the client's stepped runs, which made 0
  threaded calls, or for `render/tests`: they use the 32 768).
  Each real-time path reports how far behind the
  drawn scene was, at each frame, the most and the 99th percentile (schema 15). **What a fallback
  would be, not done here** (the owner's to decide; the baseline's numbers are above): the
  walk-instead-of-jump decision of round 6 is `Engine::follow`: a camera that was put further than
  a jump (`Engine::teleported`) and ends further than a jump from the scene is jumped to, and any
  other is walked to by `next_place`. Restoring main's jump for a scene that has fallen more than
  a jump behind is to send `target = camera` when the scene's distance to the camera is over
  `JUMP * d0`; `Scene::recentre` then rebuilds. On the frame thread that is one `Rebuilt` update:
  the whole scene is packed again on the scene thread (`Scene::mirror`), taken whole by the frame
  thread, and written in one frame (871 MB and 330 ms on the 4090). Growth being per buffer and the
  upload budgeted does not change that: a rebuild is unbudgeted by design (the table "Not
  budgeted" above) and is one frame. It could be delivered in budgeted parts only by making the
  new scene beside the old one (the old is drawn until the new is complete, since a part of a
  scene has holes) and uploading the new one's buffers over several frames before switching: twice
  the scene's GPU memory while it lasts, a staging path in the tracer, and an exception to ADR
  0006's whole patches; it is not a small change.
  **Two things about the drawn lag and the look-ahead (round 9).** (i) The drawn lag reads the
  centre of the scene that the last taken publish had (`World::lag`), and a round that moved the
  scene and changed nothing publishes nothing, so after such rounds it is more than it was by at most
  the Voxels they moved (a step is at most 64 at the default budget), until the next publish; a
  flight's rounds all change something. A stats-only publish for it was not made: it would put an
  untaken outbox in front of the budget's check. (ii) The places looked ahead to are the scene's
  next place plus the camera's velocity times the lead, while a scene that lags walks along the line
  to the camera, which on a turn is not the velocity's way. Taking the way from the line was tried
  and is worse: the places are 2 400 and 4 800 Voxels on in the test, a line that turns a little
  moves them by hundreds of Voxels every round, and the selection's travel went from 2.0 times the
  scene's walk to 6.7 (the test below fails). The velocity is steady, the effect is on prefetch only
  (the places make the children of the bricks near them wanted, at the lowest priority) and ends when
  the scene is at the camera, so it is documented (`Engine::round`) and left.

**Whether "the budget plus the crossing operation plus the round's first batch" meets the issue's "a frame's upload is bounded by the
budget" was accepted by the project owner on 2026-11-13 (the last comment on #145).** What is needed to judge it is
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
  `a_follow_step_is_within_the_budget_after_a_run_of_quiet_steps_and_after_teleports` (a 192 KiB
  budget; after 40 quiet steps and 20 teleports no step is over it) **does not show the limit**:
  it passes with the limit removed (checked), because a step's cost on the test Planet is the part
  that does not depend on its length, not Voxels of it. It guards the controller (a step that
  came to be over the budget after those). Round 7's claim that it "fails with the floor removed"
  was only of its teleport case (the quiet-steps case passed without the floor), and the floor is
  gone: the teleport case of round 7 is now `a_teleport_that_ends_within_a_jump_of_the_scene_is_walked_to_within_the_budget`.
- *The first step of a walk is at least a Voxel:* `the_first_follow_step_is_half_the_limit_and_at_least_a_voxel`
  (round 9: with a budget under 256 KiB the limit is a few Voxels and half of it was under one:
  0.75 at the tests' 192 KiB).
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
| Memory | `scene_mib` | 512 | the scene's buffers **in use** (`scene_used_peak_mib`; at 320 km the full-size runs measure **792 MiB without the horizon cap and 510 to 512 MiB with it** on the dev box (#146), and the 4090's real-time run **514.8 MiB at its peak, 2.8 over the gate**, which is left to #167, by the owner's decision). **It is not VRAM**: what the GPU reserves is `scene_reserved_mib`, reported and not gated |
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
the scene thread looks every 5 s (`HOLES_EVERY`) while it works (not while the camera's speed has the bands scaled: the look is made after the release), a look is forced at the end of
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

## The convergence phase

ADR 0005 judges the Reference view, and Primary against it, by a convergence curve: how far the
image is from a truth after 1, 4, 16, 64, 256 and 1024 samples. `client bench` makes that curve
(#187), so that the estimator (#188) and the still-frame policy (#189) are measured before and
after. It runs on every `client bench` unless `--no-convergence`, after the caves.

**On the 4090**, from the Windows artifact, for ADR 0005's baseline (8192-sample truths):

```
client.exe bench --truth-spp 8192 --out bench-out
```

That is the whole bench with the phase. At `--truth-spp 8192` the phase took 146 s a run on the 4090
with #188's estimator (about 5 minutes with the 4-bounce one, the baseline of #187; both in
`docs/measurements.md`). With the default `--truth-spp 1024` it takes about 45 s (44.0 s for both
views, measured on the 4090 before #188 and not measured again); the `wall_s` of the report's `convergence`
block is the phase's wall-clock time. If the phase fails (a file it cannot write, say, or a panic),
the bench warns on stderr, leaves the block null and still writes the rest of the report: the phase
has no budget to fail.
It writes `bench-out/report.json`
(the `convergence` block), `bench-out/convergence.csv` and `bench-out/convergence/golden/` and
`convergence/pit/` (about 0.2 GB of truths at 8192 or 1024 alike: they are the image, not the samples).

**The views.** Both are on the patch of the *test* Planet (the small sphere of 1500 Voxels that the
golden image tests use: `planet_gen::generate_region`), not on the 320 km Planet that the flight, the
sweep and the cave stills draw. The patch takes a fraction of a second to make, and the views are
fixed Voxel for Voxel. `golden` is the reference golden's camera and sun
(`render_reference` in `crates/render/tests/golden.rs`); `pit` is a pit dug into the same hillside
(41 by 61 Voxels, 40 deep: 2 by 3 m and 2 m deep at 5 cm Voxels, which #99 measured as 4 by 6 m and
4 m at 10 cm), looked into from its rim with the sun behind the far wall, so that its far wall and
the far part of its floor are lit by bounce light. The pit is a fixed part of the bench scene
(`bench/convergence/views.rs`), made by setting its Voxels to air in the patch's store, and the
`golden` scene has none of it, so the golden's picture is the one of #99 and of the golden test.
Each view is its own scene on its own GPU context.

**What is drawn.** The Reference view traces at the output resolution (twice `--size` in each
direction: 2560x1440 for the default), one sample a dispatch, each submitted and waited for and timed
like the flight's reference mode times its samples (`reference` and `gpu_total`: the same pass and
timers, so they can be set beside `modes.reference`, which is at 1280x720; the view's
`ms_per_mpix_per_sample` is the mean of `reference` over the image's megapixels). The order, per view:
truth A (seed 1) and truth B (seed 2), `truth_spp` samples each, which warm the GPU and are not
timed; then the run (seed `0xB4A7A9AC`, the reference's own), timed, with the image and the `f32`
accumulation read back at each checkpoint; then the GPU context is dropped and everything is scored
on the CPU, **outside every timer**. The truth is the same view with another seed, never the run's
own: with the same seed the first samples would be the truth's and every error would read low.

**The scores** (`report.convergence.views[].curve[]`, each against truth A):

- `flip_mean` and `flip_p99`: LDR-FLIP of `flip-rs` (`=0.1.2`, pinned in `bins/client/Cargo.toml`
  and as `FLIP_VERSION` next to the seeds, and a test holds the two together), ppd 67.02, on the 8-bit
  image that the Reference view shows (what `render_reference` reads back: the mean encoded with the
  true sRGB curve, since #188; before it the curve was a gamma of 2.2, which `--flip-gamma-2-2` scores
  again, see below). `flip_p99` is the
  error-weighted 99th percentile, a guard against speckles.
- `mse_linear`: the mean squared difference per channel of the `f32` accumulations, in linear light.
  `mse_linear_run_only` is that less half the floor's, which is the truth's own share, so it is the
  run's noise alone and falls as 1/N.
- `gpu_s`: the GPU seconds of the run's first N samples, as timed (null without timers).

- `flip_mean_gamma22` (only with `--flip-gamma-2-2`; null otherwise): the same FLIP mean on the same
  samples, with the mean encoded by the old gamma-2.2 curve on the CPU. #188 moved the Reference and
  Primary to the true sRGB curve, which shifts FLIP a little against #187's baseline whatever the
  estimator does; this column is the new estimator on the old curve, so the two effects separate:
  `flip_mean_gamma22` against #187's `flip_mean` is the estimator, `flip_mean` against
  `flip_mean_gamma22` is the curve.

`converged` is how bright the view's truth A is (`mean_linear`: the mean of each channel, linear;
`mean_luminance`: Rec. 709 luminance of that mean; `mean_luminance_b`: the same for truth B). #188
reads the pit's converged brightness off it: the estimator's bounce light shows as a brighter pit,
and a truth from before #188 is compared by the same mean of its `truth-a.f32`.

`floor` is truth B against truth A in the same three: the metric's noise in this view. `lines` has,
for FLIP 0.05 and 0.02, the samples at which the curve gets there (`samples`; null if it never does),
how that was found (`how`: `at-first-checkpoint`; `interpolated` on a log-log line between two
checkpoints; `extrapolated` along the last two, which is an estimate, not a measurement;
`not-reached`), the GPU seconds that takes at the mean and at the p95 of one sample's time (`gpu_s`,
`gpu_s_p95`), and `counts`: **a line counts in a view when it is at least 4 times the view's floor
FLIP mean**. Where it does not, the view cannot tell that line from noise (on #99, FLIP 0.02 in the
pit at 720p). Read a line that does not count as nothing, and `extrapolated` as a sign that the curve
had not reached the line by the last checkpoint (1024 samples, fixed; `--truth-spp` does not move
the checkpoints), so the number is an estimate to look into, not one to quote.

**The files.** `convergence.csv` has the curve for plotting: `view`, `samples`, `gpu_s`,
`flip_mean`, `flip_p99`, `mse_linear`, `mse_linear_run_only`, and the view's floor
(`floor_flip_mean`, `floor_mse_linear`) on every row, then `flip_mean_gamma22` and
`floor_flip_mean_gamma22` (empty without `--flip-gamma-2-2`). `convergence/VIEW/` has `sppNNNN.png` for each
checkpoint and, for each truth, `truth-a|b.png`, `.f32` (little-endian RGB floats, linear, rows from
the top) and `.json`, its inputs (the view, camera, sun, seed, samples, size, generator version, GPU,
driver and build). **A truth is kept for looking at and for working out what a number was made of. It
is for the machine, the driver and the build that made it: never compare one between machines or
drivers, and never put one in `tests/golden/`.** Two machines' curves are compared as curves, each
against its own truths.

**A regression** is a PR whose curve on the 4090 is worse than main's by more than the floor (ADR
0005); nothing gates on it yet, and the phase has no budget in `bench-budgets.toml`. Compare
`curve[].flip_mean` per checkpoint, view by view, from two reports made with the same `--truth-spp`.
Truth sample counts matter: the error against a truth of M samples carries the truth's own `1/M`.

**In CI** (`--short`): the phase runs at half of the traced size with three checkpoints (1, 4, 16),
truths of 16 samples and **no FLIP** (`convergence.flip` is null, and so are every FLIP number and
`lines`), to see that it runs and that the report and the files have the right shape: the bench smoke
test checks them. FLIP is never run in CI: `flip-rs` is compiled in (the Windows artifact needs it)
and nothing calls it. Two ignored tests do, by hand: `cargo test -p client --bin client -- --ignored
flip_of` (the wrapper) and `-- --ignored the_phase_with_flip` (the whole phase at 96x54, or at
`CONVERGENCE_TEST_SIZE=WxH`). `crates/render/tests/convergence.rs` holds, on lavapipe at 32x32, that the
squared error against a scene with a known answer (a flat sunlit floor: `albedo * (sun * cos + the sky
averaged with the cosine weight)`) falls as 1/N from 1 to 256 samples, with N times the error equal to
the variance of one sample worked out from the sky's gradient, that two seeds differ by the sum of
their errors (the floor), and that a seed gives the same image every time.

**The still view** (#189, `report.convergence.views[].still`). After the single-sample run (which is
unchanged, as are the truths and every timed pass) each view runs the **still-frame policy** from the
stop: frames at 60 Hz drawn by `Offscreen::draw_frame`, which is the window's Reference mode without
the window (`Tracer::encode_frame`: the same state machine, the same budget, the same fade pass).
The first two frames after the stop are Primary, then each frame takes a batch of the Reference view
(1 on the first, then as many as fit in 12 ms, rising by one after 4 frames that fit) and fades it in over the last
Primary image. A frame that takes longer than 16.7 ms waits for the next vsync, as a window does. The
clock is the wall clock from the stop. It is the only part of the phase that is timed by wall clock
and the only part that batches; the curve above is single samples, always (batching is inert when the
sample count is fixed: the goldens, the curve, and every number `reference` has had).

- `frame`: the work of a frame that took a batch, in ms (encoding until the GPU is done, the copy to the
  window included), with `mean_ms`, `p95_ms`, **`p99_ms`** and `max_ms`; `frames_over` counts the ones
  over 16.7 ms and `frame_p99_within_target` is #189's frame-time acceptance (p99 at most 16.7 ms).
- `gpu_batch`: the GPU's time for a batch; `batch_first`, `batch_last`, `batch_max` the sizes;
  `samples_per_s` the samples a second over the frames that took one.
- `marks[]`: at 0.1, 0.25, 0.3, 0.5, 1 and 2 s after the stop (the 0.3 s mark is the room's: ADR 0005; every view has it), the samples the run had, and the FLIP (mean,
  p99) and linear MSE of the image of that many samples against **the same truth A as the curve**. The
  image is drawn after the run from single samples of the same seed, which is bit for bit what the
  batches added up to (`batching_bit_exact` says it held on this GPU, and a test holds it on
  lavapipe), so the run is never paused to read 59 MB back. `flip_within_target` is the FLIP
  acceptance: at most 0.05 at the 0.25 s mark (0.3 s in the room, whose wall clock is reported, not gated: `still.flip_within_s` says which).
- `cuts` (#217, schema 22): `total`, `view` and `scene`: the **cuts** the view had while it was Still
  (`Tracer::cuts`: a frame that showed the Reference view and was followed by a Primary one), by what
  changed, the camera, sun or window (`view`) or the Planet (`scene`). A cut is 2 Primary frames, a
  fade and a restart from sample 0. The run holds the camera and the Planet still, so this is **0 in
  both views**; it says the renderer's own count works and is where a phase that patches the Planet
  would show them. The scene thread's patches are measured where there is a scene thread: the
  streaming mode's `still_after_flight` (below).
- `to_line[]`: for FLIP 0.05 and 0.02, the samples the curve needs (as `lines`) and the seconds from the
  stop until the run had that many, with `counts` as in `lines`. A line that does not count in the view
  is shown and not used. It is null where the run ended first.
- The run's images at the marks are in `convergence/VIEW/still-NNNNms.png`.
- `--short` runs 0.3 s, no FLIP; the smoke test only checks the shape.

**For the 4090** (#189's acceptance, which is the orchestrator's to run, at 1440p in both views; the same
command as the baseline above, because the still view is part of the phase):

```
client.exe bench --truth-spp 8192 --out bench-out
```

and the numbers are `report.convergence.views[].still` (`frame.p99_ms`, `marks[]` at 0.25 s,
`to_line[]` for 0.02), with the printed lines `still view, golden|pit, ...` and `acceptance: ...` on
stderr. Run it with both clocks locked as for the other 4090 numbers, 3 times; the policy's batch sizes
come from the GPU's own timers, so they vary a little from run to run. The phase's wall-clock time rose
by 2 s a view plus the single-sample images of the marks (a few seconds), so it is about 20 s longer.
Nothing in `bench-budgets.toml` moved: `reference` is still one sample.

**The policy's constants** are all in `crates/render/src/batch.rs` (`BATCH_BUDGET_MS` 12,
`MAX_BATCH_SAMPLES` 16, `RECENT_BATCHES` 16, `RISE_AFTER_FRAMES` 4) and `crates/render/src/still.rs`
(`STILL_FRAMES` 2, `FADE` 150 ms). The cap is on the work in a
dispatch: 16 samples at 1440p, and the same work at any size (`max_samples`: 7 at 4K, 1 at 8K). The worst
sample known at 1440p is 10.98 ms (#99's pit, the maximum of 1 024 samples, before #188; the pit's mean is
4.38 ms now), so a capped batch is 16 x 10.98 = 176 ms, 9 % of Windows' default 2 s timeout (TDR), and 0.53 s
(26 %) with ADR 0005's factor of 3 for a snow cave. A GPU would have to be 11 times slower than the 4090 on
that sample (3.8 times with the snow cave) to reach the timeout with a capped batch. The budget, not the
cap, sets the size: the 4090 takes 4 samples at most at 1440p (the runs on #216); the cap is for what the
budget cannot see, timers that lie and views that cost under 0.75 ms a sample. A time of 0 or under
`MIN_SAMPLE_MS_PER_MPIX` (0.05 ms a Mpix a sample) is no data: it is left out, and the size is held.

**Times arrive late, and the flight's `reference` is not the player's number.** The GPU's times come in
one to three frames after the batches they are of (the profiler's ring), so the size can rise sooner than 4
frames at the new size: the rule counts timed batches that fit, and some of them were drawn at the size
before. A drop is still at once, on the first time that says so. And the flight's `reference` pass (the
budget in `bench-budgets.toml`, one sample) times the *internal* size, a quarter of the pixels at the
default 0.5 render scale, while the window traces the Reference view at the full output size. The
still-view phase, at full size with batching, is the player's number.

**The sun-and-sky view** (#52, `report.convergence.shadows`, `convergence/sun/`, **schema 24**). The
golden view's camera with **the hero sun's geometry (8 degrees up, 0.12 radians wide) and the sun
and the sky alone in both views**: no GI (Primary has no
ambient term; the Reference follows no path past its first surface) and no emitted light
(`Offscreen::set_sun_and_sky_only`). What is left to compare is the sun's disc and Primary's two
shadow rays a pixel (`--shadow-rays N` for another count; the block's `shadow_rays` says which)
against the Reference's. It is scored at the output size (1440p), with Primary traced at half of
it as a window does. `--shadow-rays` changes this view alone: the `shadows` line of `passes[]` is
timed at the default count (2), so to put 1 ray's FLIP beside its cost, read that cost from the
run without it (0.178 ms at 1 ray on the 4090, 1440p) or from `--diag` runs of a build with
`DEFAULT_SHADOW_RAYS` changed:

- **Truth A** is the Reference's direct-sun image at `--truth-spp` samples (1024 by default, so it
  meets the ticket's "at least 1024"; pass 8192 for ADR 0005's baseline), seed 1; truth B, seed 2,
  gives the **floor**. FLIP is LDR at the convergence views' 67.02 ppd, on the 8-bit image the
  player sees (the old light with every stage off, as the other views have).
- `first_frame` is Primary's first frame with no history, which is what the player has the frame
  after a sun change; `settled` is Primary after `settle_frames` (32) frames of a still camera. Each
  has FLIP's `flip.flip_mean` and `flip.flip_p99`, and `edge`. **`meets_target`** is the **first
  frame's** FLIP mean at or under `target_flip` (0.05), as the ticket's acceptance has it (a
  few samples of the disc a pixel and no upscaler history is the frame after a sun change, and the
  worst Primary frame); **`target_counts`** is whether 0.05 is at least 4 times the floor's mean (ADR
  0005's rule, as the views' `lines[].counts`). `settled` is reported beside it and is not held to
  the target. **Where it stands (#52, 4090, 1440p, hero sun, 8192-sample truth, floor 0.0008):** at
  the default 2 rays the first frame is **0.062** and the settled image **0.059**, so
  `meets_target` is false; 1 ray was 0.074 and 0.072 (`--shadow-rays 1`). The owner shipped 2 rays
  with the centre-cosine weight; its bias is the cause and #55 re-judges the weight (ADR 0005's
  "Sun and shadows" row has the numbers). `meets_target` is reported and printed only: it is not a
  gate and does not change the exit status, and the target is not loosened.
- **The shadow-edge error** (`edge`). FLIP's mean over a whole image says little about the few
  pixels of a shadow's edge, where Primary weighs its rays by the cosine at the disc's centre
  and the Reference weighs each of its samples by its own. The edge is found from the Reference: a
  second image of the same view with the shadow rays off (`ReferenceOptions::no_shadow_rays`, 256
  samples) gives how much of the sun each pixel has (truth A over it), and a pixel is **on an edge**
  when it has some of the sun and not all (5 to 95 %), or has a pixel within 2 pixels on each side of
  the half. `edge.edge_pixels`, `edge_mean` and `edge_p99` are the mean and 99th percentile of the
  absolute difference of the 8-bit luma (Rec. 709 on the encoded values), in levels of 255, over those
  pixels; `other_mean` is the mean over the rest of the pixels the sun reaches (the sky and the
  faces turned from the sun say nothing of shadows, and are left out). `edge_share` is the share of
  the image on an edge. `convergence/sun/edges.png` shows the mask, `truth-a.png`,
  `unshadowed.png`, `primary-first.png` and `primary-settled.png` the images.
- **CI** (`--short`) runs the view at a few dozen pixels with no FLIP, to see that the report has the
  right shape; the edge error is made without FLIP. FLIP runs on a real GPU only, and in the ignored
  test `the_phase_with_flip_scores_both_views_and_writes_the_files`.
- **For the 4090** (the ticket's acceptance, which is the orchestrator's to run): the same command
  as the baseline above. The numbers are `report.convergence.shadows` (`first_frame.flip.flip_mean` at
  or under 0.05, `meets_target`, `target_counts`; `settled`, `edge`; `shadow_rays`) and the printed lines
  `convergence, sun (...)` on stderr, and `passes[shadows]` in each mode (its 0.4 ms budget, held by
  the run's exit status). The phase's wall-clock time rose by the view's two truths and the
  unshadowed image at 1440p, which have no bounce rays and cost a fraction of the pit's.

**Primary's GI against truth, the dig and the gates** (#54, `report.convergence.views[].gi`,
`report.convergence.gi`, **schema 29**). The phase has a third view, **`room`**, the emissive
room (a closed box of rock, 40 by 16 by 40 Voxels inside, with one block of warm light on the floor,
seen from the opposite corner: `views::room`), which converges like the other two, with truths,
the curve and the still run. In **each** view Primary is also drawn as the window draws it (traced at
half the size, upscaled) and scored by FLIP (LDR, 67.02 ppd, mean and 99th percentile) against that
view's **truth A**, which the view's own run made, at the output size:

- **`gi.flat`** is Primary with **the flat ambient of 0.25** that main had (`set_flat_ambient(true)`):
  the baseline, **before**. It is not a mode of the game, only here so that "no view moves further from
  the Reference than main's Primary" has a number on the same build.
- **`gi.gi`** is Primary with the radiance cache's GI: **after**.
- Each has `first_frame` (the frame after the view is made, with a cache that has nothing in it) and
  `settled` (after `gi.settle_frames` = 64 frames of a still camera, about a second at 60 Hz), each with `flip`
  (`flip_mean`, `flip_p99`; null without FLIP) and `luminance_ratio` (the image's mean luminance in
  linear light over truth A's image's, both 8-bit as the player sees them, 1 = as bright; it needs
  no FLIP, so a short run has it).
  `gi_not_further` is whether the settled GI image's FLIP mean is at most the flat one's.
  The images are in `convergence/VIEW/primary-{flat,gi}-{first-frame,settled}.png`.
- **`convergence.gi.edit`**, **the dig**: the room with a wall across it that the lamp's light does not
  get through, seen from the dark side, warmed up for 90 frames; a gap of 160 Voxels is dug in
  the wall and frames are drawn at 60 Hz (a frame that takes longer than 1/60 s takes its own time). The image's mean
  luminance after each frame is `luminance[]`, at `frame_ms[]` from the start of the update.
  **`settle_ms`** is the time of the first frame after which the luminance stays within a tenth of the
  change's size of where it ends up (null if the dig changed it by under 5 %); `before`, `after`,
  `update_ms` (the Edit's CPU time) and `slow_frames` (frames over 60 Hz) say the rest.
  **`edit.game_path`** says which path made the Edit. True (the full run): **the game's own**: a ring
  scene of the test Planet (`stream::test_planet()`), a `World` with its loader threads, the Edit made
  by `World::edit`, the scene worker's patch arriving in `World::recentre` a frame or two later, and
  the cache's boost taken, on the frame thread at `World::edit` time, from the Edit's changed bricks
  (`Offscreen::edit_made`): the settle time includes that latency, which is what a player waits.
  False (`--short`): the flat scene, `Scene::update` and `Offscreen::update_scene` at once.
  What the settle time is made of: the cache follows an Edit within a few frames (its cells forget faster for
  30 frames, `GI_BOOST_FRAMES`), and the rest is the upscaler's history: 16 frames of it take a new
  light at a sixteenth a frame, which on lavapipe was 40 frames to settle (650 ms on the 4090 at
  `2eac792`, 400 ms before the upscaler's blends moved to linear light and its clamp got slack).
  So for the 30 frames after an Edit (`Tracer::edit_made`) the upscaler keeps no more than 4 frames of
  history (`SETTLING_HISTORY`; a sun change starts the history over anyway): on lavapipe the dig
  settles after 15 frames (flat) or 18 (the game's path, with the patch's two-frame latency).
  `light_let_in_by_an_edit_shows_through_the_upscaler_within_half_a_second` holds it.
- **The gates** (`convergence.gi.gates`, held with FLIP only, **exit status 2** if any is over, `--no-fail`
  silences it): `primary-flip:VIEW` (**#55, schema 32:** the settled FLIP mean of Primary as the window draws it, with
  the denoiser on, is 0.05 or under against the truth, in every view: the golden, the pit, the room and the
  sun-and-sky view `sun`; it was the room's 0.10 before the denoiser), `no-further:VIEW` (a view's settled GI image no further from truth than its flat
  one, for each of the three), `settle` (the dig settles within 500 ms), and `reference-room` (the
  room's Reference view reaches FLIP 0.05 within **0.3 s of GPU time**: the line's `gpu_s`
  (`views[].lines[]`, `flip` 0.05: its samples times the Reference pass's mean GPU time), against
  `still::ROOM_WITHIN_S`: the owner's exception of 2026-11-20 for the closed room, whose long light
  paths cost about three times a sample as much as the outdoor views', and **his decision the same
  day that the gate is on GPU time**; the 4090 has 0.292 and 0.289 s; no timers (a `gpu_s` of null) is
  over) **where that line counts in the room** (ADR 0005: at least 4 times the room's floor, so a room
  whose truths are noisier than 0.0125 is not held to it; **the room's truths have at least 8192
  samples** when FLIP is run, `ROOM_TRUTH_SPP`: the floor was 0.0211 at 1024 and 0.0137 at 4096 on the
  4090, and about 0.0120 is what the two fit for 8192).
  **Two clocks, one gated:** the room is gated on the line's `gpu_s`. The 60 Hz **wall clock** of the
  still run (the image at the 0.3 s mark, `still.flip_within_target` with `still.flip_within_s`, and the
  line's seconds from the stop, `still.to_line[].seconds`, which have the first still frames, the batch
  budget of 12 ms a frame and the vsyncs in them: 0.674 s and FLIP 0.066 at the mark on the 4090) is
  **reported, not gated**; the summary prints both as "room Reference gate". **The outdoor views are
  unchanged**: their acceptance is still the still run's FLIP at 0.25 s on the wall clock
  (`flip_within_target`, "met" or "NOT met" in the summary; they have no entry in `gates.over`).
  Each view's `truth_spp` (`views[].truth_spp`, and the summary's "truths of N samples") says how many
  samples its truths had; the room's is `ROOM_TRUTH_SPP` when that is more than `--truth-spp`.
  `gates.over` names those that are over, `gates.not_counted` those that were not held for being under
  4 times the floor.
- **Why gate the settled frame**: the first frame of a cold cache is the noisiest GI is, and the player
  has it once, in the frame the scene is made; it is reported beside the settled one, not held.
- **CI** (`--short`) draws all of it at a few dozen pixels with no FLIP: the report has the shape, the
  dig's frames are there, no gate is held. The ignored test
  `the_phase_with_flip_scores_both_views_and_writes_the_files` runs it with FLIP.
- **For the 4090** (the ticket's acceptance, which is the orchestrator's to run): the baseline command
  above. The numbers are `report.convergence.views[].gi`, `report.convergence.gi`, `passes[gi]` (its
  1.5 ms budget, held by the run's exit status) and the printed `convergence, Primary against truth`
  lines. The phase's wall-clock time rose by the room's truths and still run, and twelve FLIPs of
  1440p images.

The flight, the cave stills, the streaming mode, the real-time mode and the look-dev sheet draw Primary
with the radiance cache (`set_flat_ambient(false)`), since that is the player's frame; the convergence
views' Reference images do not depend on it.

### The denoiser (#55)

Primary is drawn **denoised** in every view of the phase and in the flight: `Offscreen` starts with
`Denoise::Off` (the tests with answers worked out from the raw samples want it), so a caller that draws
Primary as the game does calls `set_denoise(Denoise::Full)` next to `set_flat_ambient(false)` (the bench,
the look-dev sheet, the flight). The `denoise` pass (budget 1.0 ms in `bench-budgets.toml`) is in
`passes[]`; it holds the frame's samples from the lattice, the temporal accumulation and the filter, and
sits before the upscaler (`crates/render/src/denoise.rs` has the account).

What the 4090 runs for #55 (the orchestrator's, with the PR's Windows artifact):

```
client bench --truth-spp 8192                              # the gate: centre-cosine weight, every view
client bench --truth-spp 8192 --shadow-weight per-sample   # the same, with the Reference's weight
```

Each run reports **both weights** and the denoiser off, so one run has the comparison:
`convergence.shadows.{first_frame,settled}` are the chosen weight denoised, `other_weight` the other,
`raw` the chosen weight with no denoiser, `reference_64` the Reference at 64 samples (reported, not
gated); and each view's `gi.{gi,other_weight,raw,flat}` the same for the GI views, with `gi.reference_64`.
The gate is on the weight that `--shadow-weight` chose; the run that gets the sun view under FLIP 0.05
settled, in the other views too, is the weight to keep. The summary prints every line.
The convergence views' pictures are the denoised ones (`primary-gi-*.png`, and for the other weight
`primary-gi-per-sample-*.png` or `primary-gi-centre-cosine-*.png`), and the look-dev sheet's
`primary.png` and `primary-settled.png` are drawn as a window draws a first frame and a settled one.

## The hardware ray backend (#50)

A spike: are wgpu ray queries over procedural AABBs faster than the compute DDA on the rays that
`trace()` casts (the primary rays, the sun shadows and the GI), at 5 cm on the 4090? The code is
`crates/render/src/hw.rs` (the acceleration structures), `shaders/trace_hw.wgsl` (the shader) and
`bins/client/src/bench/rt.rs` (the report). Compute is and stays the default and the reference; the
hardware backend is chosen only by `--rt-backend hw|hw-nearest|both` (the player's game does not use
it), and only on an adapter with `EXPERIMENTAL_RAY_QUERY` (`render::choose_backend`: no ray queries,
or limits of 0, is the compute backend with a reason, never an error). Whether lavapipe has ray
queries depends on its Mesa (the dev box's, 25.0.7, has them); the tests that compare the backends
skip themselves, printing why, where it has none, and the fallback is tested through a device made
without the feature (`Offscreen::without_ray_queries`).

**What is built.** One AABB for each non-empty brick of each ring level, as tight as the Voxels that
the brick's occupancy mask has set (a solid or uniform region is one box of its whole 64-Voxel
cube), in a **BLAS per group**: the groups are regular, 4 x 4 x 4 regions of a level's grid
whatever is in them (never content-driven), so an Edit rebuilds the groups it touched and nothing
else. One TLAS instance per group, scaled by 2^level, with the TLAS in a frame anchored at the
camera (snapped to 4096 Voxels) so that its floats are fine near the camera. On a candidate hit the
shader runs the compute path's in-brick DDA (`hit_brick`, `hit_region`) and commits its hit with
`rayQueryGenerateIntersection`; every pass is the same shader with either backend.

**The two kinds of shadow ray.** The sun's shadow rays (`shade.wgsl`) and the GI's (`gi.wgsl`) ask
`occluded(ray, reach)`, which only needs to know that something is there: on compute it is the same
walk as any ray's, which goes front to back and stops at the first Voxel; on **`hw`** it is a ray
query with `RAY_FLAG_TERMINATE_ON_FIRST_HIT` that ends at the first box that has a Voxel in the way,
whichever the hardware reaches first; on **`hw-nearest`** it looks for the nearest hit like a
primary ray does (what `hw` was before the early-out, kept to tell what the early-out is worth). The
primary rays, the GI's bounces and the path tracer's rays are the nearest-hit query on both.

**Running the comparison** (on the 4090, from a release build or the Windows artifact):

```
client.exe bench --rt-backend both --no-convergence --no-fail --out bench-out-rt
client.exe bench --rt-backend hw --no-convergence --no-fail --out bench-out-hw   # the sweep too
```

`both` is one process: the compute run is the ordinary bench; the hardware run is a second context
that flies the same path and makes the same Edits; and a third, `hw-nearest`, flies the path (no
Edits). All are on the same GPU, driver and clocks. Read `rt.comparison`:

- `speedup`: for `primary`, `shadows`, `gi` and `gpu_total`, the compute mean, the hardware mean
  and `compute_over_hw` (above 1, hardware is faster; null if a pass was not timed), and the same
  with `hw-nearest` (`hw_nearest_mean_ms`, `compute_over_hw_nearest`).
- **`speedup.gpu_total` is the GPU time of the frame's passes and leaves out the hardware's
  acceleration structure updates**, which happen between frames and wait for the GPU. For the frame
  with them in, `hw_gpu_total_plus_accel_ms` is the hardware's mean `gpu_total` plus the flight's
  updates' total cost (`hw_accel.flight.total_sum_ms`) spread over the measured frames, and
  `compute_over_hw_with_accel` is compute's `gpu_total` over it. (The updates' time is mostly CPU
  time with the GPU idle, so adding it to the GPU time is the frame's length if the update is on
  the frame thread, as here, and not the GPU's work.)
- `compute`, `hw` and `hw_nearest`: the flight's primary mode on each backend: every pass's STATS,
  `gpu_total`, `frame_time` and `scene_update_ms`. **`scene_update_ms` (its `max_ms`) is the stutter
  that following the camera makes**, the hardware's acceleration structure updates included: it is
  timed in the flight apart from the passes, and there is no p99 (`p95_ms` and `max_ms` are what
  `STATS` has).
- `hw_accel`: the cost of the acceleration structures. `initial` is the first build. `flight` is a
  summary of the updates of the **steady flight**: the measured frames of the mode, less the first
  (`flight_frames` of them). The jumps are apart, since a jump rebuilds everything and is not
  what following the camera costs: **`warmup`** (the move to the end of the path where the mode
  warms up, and the warm-up frames), **`first_frame`** (the first measured frame, which follows
  the camera back from there to the start of the path; `scene_update_ms` has it in its `max_ms`,
  which is a jump and not a stutter of the flight) and **`settle`** (the move to where the Edit
  stress starts). `edits` is the Edit stress's. A summary has `updates`, `noop_updates` (scene
  updates after which no box was stale), `blas_rebuilt`, `prims_rebuilt`, `tlas_rebuilds`, the
  BLASes rebuilt per update `_mean`/`_max`, `prepare_ms`, `build_ms` and `total_ms` as STATS of the
  updates that rebuilt something, `total_sum_ms` (their total and the search of the no-op updates)
  and `noop_search_ms` (the no-op updates' part of it). Each update has a `kind`: `build` (all),
  `patch` (the groups a move or an Edit touched, and the TLAS again) or `anchor` (the camera left
  the TLAS's frame: instances only).
  - **`prepare_ms`** is CPU time: the search for the groups the patch made stale (the scene's
    patch mapped to groups; it sorts every region's first entry when entries of `brick_refs` were
    written, so it is not free, and a no-op update has it alone), then the boxes worked out and a buffer and a BLAS made for each group.
  - **`build_ms`** is everything from the first build command to the GPU being idle: it records the
    BLAS and TLAS builds, writes the boxes' words, the instance table and the bind group, submits,
    and **waits for the GPU to finish everything queued** (`device.poll(wait)`), the scene's own
    uploads (`update_scene`'s writes) included, which compute does not wait for. So it is the time
    the frame thread stands still for an update, and is an upper bound on what the builds alone
    cost.
- `hw_edits`: the Edit stress on the hardware backend, in the report's `edits` shape, to put by
  `edits` (compute). **Both stress runs start from the same camera and rings**: the sweep's last
  height (20 000 Voxels over the pole), where the compute run ends the sweep, and the hardware run
  is put there before it starts (`settle_for_edits`; on the compute run it is one `recentre` to the
  camera the sweep already sits at, which does nothing unless the selection still owed work, so the
  compute `edits` are as they were, if not provably to the last digit), so the Edits land on the same
  levels (a stress that started from
  the flight's low end would make its Edits in level 0, which are cheaper to repair). The two sides'
  buffers can still differ in size: the compute run flew the Reference and the sweep first. Its budgets
  are not gated. The acceleration structures' update is inside the frame thread's time
  (`update_scene` waits for the build), so it is in `frame_thread`.

**Where the updates run, if the player's client used this.** All on the **frame thread**, and the
frame waits: `Renderer::update_scene` calls `Tracer::update_scene`
(`crates/render/src/tracer.rs`, `update_scene`), which calls `HwScene::update` right after writing
the scene's buffers, from the client's `upload` (`bins/client/src/main.rs`, `upload`, called by
`stream` and `show` on the frame thread). `HwScene::update` makes the groups' boxes and BLASes
(`prepare`, CPU) and builds them with a `device.poll(wait_indefinitely)` (`build`), so a frame that
makes a patch is that long late. The move of the TLAS's frame when the camera leaves its snap
(`HwScene::follow`) is in `Tracer`'s draw, also on the frame thread, with its own wait. The scene thread
(ADR 0006) packs the bricks and does none of this. The bench measures that: the flight's updates are
in `scene_update_ms` and `hw_accel.flight`, and the worst of them is a stutter of that size. Moving
them to another thread is not done here.

`--rt-backend hw` alone runs the whole bench (the flight, the sweep and the Edit stress) on the
hardware backend: `rt.active` is `hw`, `rt.accel` has the same `initial`/`flight`/`edits` as
`hw_accel`, and `modes`, `altitudes` and `edits` are the hardware ones, to compare with a
`compute` report's by hand (`hw-nearest` is the same with nearest-hit shadows). The budgets are
the compute backend's: a `hw` report is held to them like any other, so use `--no-fail` to read
it. The cave stills, the convergence phase and the streaming modes always use compute: they are
not what is compared.

**Tests.** `cargo test -p render --lib hw::` (CPU only: the tight boxes, the regular grouping, and
the groups an Edit makes stale, in level 0 and in a coarse level where a brick is written in place
and nothing but `RingPatch::rewritten_coarse` says where); `cargo test -p render --test
hw_agreement -- --nocapture` (on a device: the choice of backend, the fallback through a device made
without ray queries, an Edit's patch and agreement with compute at level 0 and at a coarse level,
the shadows of both variants against compute's, and the hits of both backends on 6000 rays; those
that compare skip themselves, printing why, on an adapter without ray queries).

## Reading the report

```
{
  "schema_version": 31,
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
  "convergence": { "width", "height", "scene", "checkpoints", "truth_spp", "seeds", "flip",
                   "flip_gamma22",
                   "lines", "min_line_over_floor", "folder", "csv", "wall_s",
                   "views": [ { "name", ...the view's camera and sun, "reference": STATS,
                                "gpu_total": STATS, "ms_per_mpix_per_sample", "converged", "floor",
                                "curve": [...], "lines": [...], "files": [...],
                                "still": STILL }... ],
                   "shadows": { "name": "sun", ...the view's camera and sun, "width", "height",
                                "traced_width", "traced_height", "sun_and_sky_only", "truth_spp",
                                "unshadowed_spp", "seeds", "flip", "floor": SCORES | null,
                                "target_flip", "min_line_over_floor", "target_counts",
                                "first_frame": PRIMARY, "settled": PRIMARY, "settle_frames",
                                "meets_target", "edge_share", "files" } } | null,
                                      // null with --no-convergence or if the phase failed;
                                      // see its section
  "streaming": { "deterministic": true, "schedule", "counts_hash", "upload", "soft_misses",
                 "rings", "work", "residency", "holes", "sizing",
                 "timing": { "deterministic": false, ... } } | null,  // --streaming only
  "streaming_real_time": { "mode": "real-time", "deterministic": false, "planet", "d0",
                           "runs": [RUN...], "min": RUN, "max": RUN, "gates",
                           "error": { "run", "step", "message" } | null } | null,  // --real-time only
  "rt": { "requested": "compute" | "hw" | "both", "active": "compute" | "hw", "fallback": STRING | null,
          "accel": { "initial", "flight", "edits" } | null,     // active hw only
          "comparison": { "compute", "hw", "hw_nearest", "speedup", "hw_gpu_total_plus_accel_ms",
                         "compute_over_hw_with_accel", "hw_accel", "hw_edits" } | null }  // both only
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
- `budgets.unmatched` lists budgets for passes that did not run (a budget for a stage whose pass
  does not exist yet): they are not an error.

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
output. Only `primary` (Primary visibility, 1.0 ms), `shadows` (the sun's shadow rays, two a pixel, 0.4 ms, #52),
`atmosphere` (the atmosphere's tables, 0.3 ms, #53), `upscale` (the TAAU pass, 0.5 ms) and the total (8.6 ms) are live today, as those are the passes that
exist, `gi` (the radiance-cache GI, 1.5 ms, #54) and `denoise` (1.0 ms, #55). The later stages (reflections 0.7, glass 0.7, volumetrics 1.0, edits 1.0, post 0.5)
are in the file as comments: **whoever adds a pass uncomments its budget and names it the same** (a
pass is named by the string given to `Profiler::compute_pass`). `reference` has no budget in #10;
its 3.0 ms (a sample, pass and total) catches a gross regression, set from the first real run on the
4090, which measured a mean of 1.10 ms.

## The art direction's passes (#202)

The exposure, bloom, tone curve and smooth normals are passes of their own, so each has a line in
`passes[]` with its own timing (the bench reports every pass generically, so the new lines need no
field), and `primary` no longer includes the lighting, which is **schema 20**. Names, in the order
they run in a Primary frame:

| Pass | What it is | Budget |
| --- | --- | --- |
| `primary` | primary visibility and the G-buffer (it no longer lights) | 1.0 (existing) |
| `normals` | the smooth normal of every hit (the cost of the smooth normals, alone) | none yet |
| `atmosphere` | the tables of the atmosphere (#53): the sky-view table and the aerial-perspective volumes every frame, and the transmittance, multiple-scattering and sun-free tables only when the air or the sun's height changes. It runs first, in both views, and is a pass of its own, so it adds a line to `passes[]` and no field (no schema bump). The 0.3 ms budget is the issue's, **for the 4090**: the orchestrator measures it | 0.3 |
| `shadows` | the sun's shadow: two shadow rays a pixel from the G-buffer (#52, **schema 24**; `--shadow-rays N` in the bench) | 0.4 |
| `gi` | Primary's global illumination (#54, **schema 29**): one bounce and an emitter sample for **one pixel of every 2 x 2 block** (a different one each frame; the rest of the block is reconstructed in `shade_pass`) from the G-buffer, the world-space radiance cache that supplies the bounces after it, and the cache's per-frame resolve | 1.5 |
| `denoise` | Primary's denoiser (#55, **schema 32**): the frame's samples of the sun's weight and the bounce light from the lattice (`gi_samples`, which `shade_pass` read from `gi`'s output before), the temporal accumulation with clamping, and four iterations of the variance-guided a-trous filter (A-SVGF), at the traced size and before the upscaler. **Always timed**, with the denoiser off too (then only `gi_samples` runs) | 1.0 |
| `shade` | the lit image, as linear light, from the G-buffer, the shadows and the denoised light | none yet |
| `exposure`, `exposure_adapt` | the log-average luminance and the adapted exposure | none yet |
| `bloom_down`, `bloom_h`, `bloom_v` | the bloom, at a quarter of the size | none yet |
| `tone` | exposure, bloom and tone curve into the encoded image | none yet |
| `upscale` | the TAAU pass | 0.5 (existing) |

The Reference view runs the same passes after `reference` (not `normals` or `shade`: it smooths and
lights inside its own pass). **`primary` now excludes `normals` and `shade`** (the lighting and the
smooth normals that it used to include), so `passes[primary]`, the cave-still `primary` and the
real-time `gpu_primary_ms` are not comparable with a report of schema 19 or before, nor is the
budget on them: add `normals` and `shade` to it (schema 20). #10 gave the whole of tone,
exposure and bloom ("post") 0.5 ms and `gpu_total.primary` 8.6 ms; the new passes have no budget of their own yet, because the first
number comes from the 4090: set them from that run (uncomment or add them in
`bins/client/bench-budgets.toml`, named as above). The convergence phase runs on the old light with
every stage off (`Offscreen::use_legacy_light`), so its numbers are comparable with reports before
#202.

## The look-dev shot list

`bins/client/src/bench/lookdev.rs` (#148) is a recorded camera path for judging the art direction
(#135) on the real 320 km Planet: eight shots modelled on Crystal Islands' (a forest floor, a canyon
with haze, a dark descent, two crystal caves of different dominant hue, a sunlit cliff, a meadow and
a pull-out to the Planet). It is not a `client bench` mode and no pass is timed on it. Each shot is
its own short `CameraPath` (the places are kilometres apart: they are cuts), with the roll of the
debug free camera where it tilts (`Q` and `E` roll it, `X` levels it), and names the places where
hand-placed stamps go for what M1b has not built yet (crystals and vegetation). The shot list
holds no stamp code.

The places are found on the seeded Planet and recorded as numbers in `lookdev/shots.rs`. They are
for one `GENERATOR_VERSION`, seed and radius, and a test fails when any of them moves on. To find
them again:

```
RECORD_TO=$PWD/bins/client/src/bench/lookdev/shots.rs \
  cargo test --release -p client --bin client -- --ignored record_the_look_dev_sites
cargo fmt -p client
```

Every build runs the fast tests in `lookdev.rs`: the recorded `GENERATOR_VERSION`, seed and radius
are the current ones, the shots are the issue's list in order, every keyframe falls on a whole frame
and the roll blends the short way round. They read the record only and never the Planet.

Three tests are `#[ignore]`d because they make the 320 km world map (25 s to a minute), and are run
by hand after the places are recorded or the generator changes:
`record_the_look_dev_sites`, `the_recorded_places_are_the_ones_the_finder_picks` (the record is what
the finder finds) and `the_recorded_shots_are_where_they_say` (every camera is in the open, every
stamp place is air against rock, each cave shot is 20 m or more under the ground at every keyframe
with rock straight overhead, and the dark descent ends 15 m or more under, roofed). Rock overhead
says the chamber is roofed; it does not prove there is no sky from the side.

### The GI sheet (#54)

`client lookdev --out DIR --gi` renders every shot, and the bench's emissive room, three times at the
game's settings and writes them side by side to `DIR/index.html` and
`DIR/<shot>/{reference,primary-flat,primary-gi}.png` (`DIR/room/` for the room): the Reference
(the truth), Primary as it was before the radiance cache (the flat ambient of 0.25: today's look), and
Primary with the cache's GI, both settled over 32 frames of a still camera. The crystal caves and the
forest floor are shots of the list; the room is the bench's. It is for the owner's eye: the
cache's image is held to the Reference, and the art direction (hazy, soft, pastel, a time-of-day tint,
a warm hero light, #53) must be where it was. On lavapipe:
`client lookdev --out DIR --gi --size 480x270 --spp 16`; `--shot NAME` narrows it.

### The atmosphere sheet (#53)

`client lookdev --out DIR --atmosphere` renders every shot in both views under **three looks** and
writes them side by side to `DIR/index.html` and
`DIR/<shot>/{primary,reference}-{gradient,untinted,tinted}.png`: hero light B's **gradient and haze as
they were before #53**, **the physical atmosphere with no tint**, and **the atmosphere with the hero tint**
(the game's look, `Look::HERO_B`; the owner's decision of 2026-11-18: see ADR 0005's sky row). It is the
sheet for the owner to judge by eye that the atmosphere keeps the hero look (#148, #202). It needs the
real Planet, so it makes the world map (25 s) per shot. On lavapipe (the dev box) a sheet at
480x270 with 16 samples takes about 10 minutes:

```
cargo run --release -p client -- lookdev --out /tmp/lookdev-atmo --atmosphere --size 480x270 --spp 16
```

On the 4090 the same command at the default size and samples is the sheet. Each shot's camera sets
the Planet's ground level (`set_ground_level`), which the atmosphere counts the camera's height from.
`--untinted` makes the other sheets (the combinations, the stills) use the physical atmosphere with no
tint (`Look::HERO_B_UNTINTED`), so that the tint can be compared on any of them.

The sheet ends with **the tint by the time of day**: the forest floor and the canyon with haze, in the
Reference view, at sun elevations of -12 (night), -4 (dusk), 0, 8 (the hero sun), 25 and 60 degrees (midday),
the untinted atmosphere over the tinted one, in `DIR/<shot>/strip-<elevation>-{untinted,tinted}.png`. The
tint is violet, mauve and purple when the sun is low or under the horizon and hazier and more orange when it
is up, blended by the sun's elevation (ADR 0005's sky row). The display's auto-exposure settles on each
image, so the night's brightness is evened out and what the strip shows is the colour. The sun is the same
70 degrees from each shot's forward; only its height changes.

The tint's five settings (`HERO_TINT` in `atmosphere.rs`) were set by eye on the strip: change them, rerun
`client lookdev --out DIR --atmosphere --shot canyon --shot forest` (about 3 minutes at 480x270 and 8 samples on
lavapipe: `--shot` picks the shots whose names contain it), and look at the strip. `cargo test -p render
--test atmosphere sky_by_sun_elevation -- --ignored --nocapture` prints the sky's linear colour at the horizon,
half way up and overhead for sun elevations from -12 to 60 degrees, untinted and tinted, and `horizon_profile`
prints the rows of a level view through the horizon.

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
