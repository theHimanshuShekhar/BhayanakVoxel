# Benchmark history

The log of how `client bench` and its gates came to be what they are: the runs on the 4090, what each PR measured, decisions that were later superseded, and how the gates moved. It is in date order. Each entry has the date of the decision or of the first measurement, and the issue or PR number.

This file and [`bench-report-reference.md`](bench-report-reference.md) are the old `docs/agents/benchmarking.md` (2,338 lines), split on 2026-11-28 by the docs review. What agents need to run a bench now (which PRs get a label, what the orchestrator runs, what each gate means, what to post) is [`docs/agents/benchmarking.md`](../agents/benchmarking.md). The numbers that gate today are in `bins/client/bench-budgets.toml`.

How to read it:

- An entry that holds a quoted block is the old text, moved without change. It says what was true on its date, and some of it is superseded: the notes at the end list what the review found stale. Quoted section names inside a block ("What the upload budget bounds", "No ceiling on the lag", ...) are headings of the reference or of this file.
- An entry that starts "Pointer" records a dated decision or measurement whose text stays in the reference, because the reference's field or section is also where the number is read. It gives the numbers so that the log is complete, and says where the full text is.
- An entry that grew over several days is dated by its first day and names the range.

## The log

### 2026-10-28, #13: the bench is the performance loop, and a pass over budget blocks the merge

> The opening of the old document and its rule on budgets. The label rules are now in `docs/agents/benchmarking.md`.

`client bench` is the performance loop of the project (decision in #13). It flies a fixed camera
path over the test Planet, times every GPU pass, and writes a JSON report and screenshots. It
exists so that "is this PR within the frame budget?" has a number for an answer, measured on the
dev's RTX 4090.

- **A pass over its budget blocks the merge** (#13). The agent removes the label once the report is
  in and nothing is over, and merges on green CI and review. Budgets are in `bins/client/bench-budgets.toml`; a PR that deliberately
  changes one says so and why.


### 2026-10-31, #99: the pit view

Pointer. The convergence phase's `pit` view is #99's `interreflect` view at 5 cm Voxels (2 m by 3 m and 2 m deep, where #99 measured 4 m by 6 m and 4 m deep at 10 cm). Text: [reference](bench-report-reference.md#the-convergence-phase), "The views".


### 2026-11-08, #147 and #156: the Edit stress and its budgets

Pointer. `edits` (1 ms, the Edit+BVH mean) and `edit_frame_thread` (4 ms, the longest Edit on the frame thread) are held by the Edit stress. Text: [reference](bench-report-reference.md#reading-the-report), `edits`; the numbers and their reasons are comments in `bins/client/bench-budgets.toml`.


### 2026-11-10, #143, #162 and #168: the streaming gates and the `needs-stream-bench` label are decided

> Decided 2026-11-10. "Proposed" below is of that date; `settle_s` is a gate in the budgets file now (notes at the end).

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


### 2026-11-12, #181 and #162: the stepped mode's sizing constants, and the counts hash up to 2026-11-16

> The first measurement of the stepped streaming mode's constants on the test Planet, and the hash text as it stood. The committed hash today is in `bins/client/src/bench/streaming-counts.golden`; the values it has had are in the notes at the end of this file.

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



The old `counts_hash` entry, last sentences:



>   purpose, the test prints the new hash; put it in the file and say why in the PR. It is `2466f0700e79713a` since #167 (18 levels and the test Planet's Guard from level 10: the pyramid is deeper, so the rings and the soft-miss levels are 18, not 14; `38b58d66b96791af` from #146 before it): the stepped scene has the horizon cap, so the drawn bricks of the Guard's levels went from 9 536 to 8 482, the rings' and a follow step's counts and the upload (64.30 to 64.80 MB, 6 439 to 6 986 copies) moved with them; what is streamed, arrived and evicted did not (`docs/measurements.md`). **The committed hash is the stepped run with the speed scale on** (#168), which is not that one: `26a1ce75…` at the first push of #168, `bd99020be09af4ba` at `99b08c4` (the scene thread takes in for longer while the loader is backed up and a margin node is not parked by a coarser change), `485be9b0eb879d35` once a coarser change parks only the nodes on the hot list (`docs/measurements.md`). The run with the scale **off** is still `2466f0700e79713a` (`with_the_speed_scale_off_the_counts_are_those_the_run_had_before_it`).



The old opening of "The real-time mode":



> (`2466f0700e79713a` since #167, which made the pyramid 18 levels; `38b58d66b96791af` from #146, which put the horizon cap in it; `e621feef18ef1801` from #145, which added the `upload` writes and budget counts, until #166's terrain) still run first.


### 2026-11-12 to 2026-11-16, #145 and #197: the upload budget's rounds, the walk that does not keep up, and main's baseline

> The owner accepted "the budget plus the crossing operation plus the round's first batch" as the issue's "bounded by the budget" on 2026-11-13 (the last comment on #145). Rounds 6 to 9, the 4090's runs of `9539899`, `c537d09`, `188ebe8` and `5489545`, and #197's measurement of main (`7706578`, 2026-11-13, second table) are below. What the bound is today: [reference](bench-report-reference.md#what-the-upload-budget-bounds).

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



### 2026-11-14, #146, #166 and #167: the horizon cap, the 320 km Planet and 18 levels

Pointer. The scene's memory with the cap is the `memory` gate's figure: at 320 km the full-size runs measured 792 MiB without the horizon cap and 510 to 512 MiB with it on the dev box (#146), and the 4090's real-time run 514.8 MiB at its peak, 2.8 over the gate, which the owner left to #167. The real-time mode's `d0` of 1028.2 Voxels and the pacing fields (`frame_cycle`, `sleep_overshoot`, schema 19) are #167's. Text: [reference](bench-report-reference.md#the-gates).


### 2026-11-14, #148: the look-dev shot list and the roll of the free camera

Pointer. Eight shots on the 320 km Planet, recorded as numbers for one `GENERATOR_VERSION`, seed and radius. Text: [reference](bench-report-reference.md#the-look-dev-shot-list).


### 2026-11-14 to 2026-11-15, #187, #188 and #189: the convergence phase, the true sRGB curve and the still-frame policy

Pointer. The phase took 146 s a run on the 4090 at `--truth-spp 8192` with #188's estimator (about 5 minutes with #187's 4-bounce one) and about 45 s at the default 1024 (44.0 s for both views, measured before #188). The still view's constants (`BATCH_BUDGET_MS` 12, `MAX_BATCH_SAMPLES` 16, `RECENT_BATCHES` 16, `RISE_AFTER_FRAMES` 4, `STILL_FRAMES` 2, `FADE` 150 ms) and the worst sample known at 1440p (10.98 ms, #99's pit before #188) are #189's. Text: [reference](bench-report-reference.md#the-convergence-phase).


### 2026-11-15, #202: the art direction's passes (schema 20)

Pointer. `normals`, `shade`, `exposure`, `bloom_*` and `tone` became passes of their own and `primary` no longer lights, so `primary` of schema 19 and before is not comparable. Text: [reference](bench-report-reference.md#the-art-directions-passes-202).


### 2026-11-16, #168 and #217: the speed-scaled bands' gates, and the still hold

Pointer. #168 added the gates of the camera's soft misses, the drawn scene's lag (`scene_lag_d0_max` 8.0: main's 7.81 `d0` at 50 m plus a margin, "for now", to tighten to about 2 `d0` once the follow fixes land), the spawn ring and the settle time (`settle_s` 55 s: main's 47.8 s on the 4090 plus 15%). #217 added the still hold and counts the cuts a Still view gets (acceptance: at most 1 from scene patches; 118 cuts in 120 held frames before it, all `follow`). Text: [reference](bench-report-reference.md#the-fields).


### 2026-11-17, owner: the catch-up cap is 12 s

Pointer. `CATCH_UP_CAP` was 5 s, which the 4090's rebuild of 8.8 to 9.2 s hit with `before_hold.hit_cap` true. The owner made it 12 s on 2026-11-17 (`a7c6b72`: the wait ended 10.87 to 11.24 s after the view went Still in 5 of 6 runs, and ran to the cap in 1). Text: [reference](bench-report-reference.md#the-fields), `still_after_flight`.


### 2026-11-17, #213: the sea-level crossing

Pointer. Before #226 a scene whose cap was off was the wide one: 544 to 557 MiB in use at 30 m under the sea and 567 MiB at the top (the 4090 566 to 567 MiB). Text: [reference](bench-report-reference.md#the-paths).


### 2026-11-17, #52: the sun-and-sky view and the shadows pass

Pointer. On the 4090 at 1440p with the hero sun and an 8192-sample truth (floor 0.0008): 2 rays gave 0.062 on the first frame and 0.059 settled, 1 ray 0.074 and 0.072. The owner shipped 2 rays with the centre-cosine weight (2026-11-17); #55 made the per-sample weight the default (0.021 settled with the denoiser at `4c02da7`; 0.026 before the filter's last change). The `shadows` budget is 0.4 ms. Text: [reference](bench-report-reference.md#the-convergence-phase).


### 2026-11-18, #53: the atmosphere's tint

Pointer. The owner's decision of 2026-11-18 is the tinted atmosphere (`Look::HERO_B`), judged on the atmosphere sheet; the `atmosphere` budget is 0.3 ms. Text: [reference](bench-report-reference.md#the-look-dev-shot-list).


### 2026-11-20, #220 and #54: control within 2 s, and the room's gate on GPU time

Pointer. #220: time to control was 10 s while the window waited for the whole Guard (4.3 s on the 4090, 7 s on the loaded dev box); the Guard is built in the background and the gate is 2 s. #54, owner, 2026-11-20: the closed room's Reference view reaches FLIP 0.05 within 0.3 s of GPU time (the 4090 has 0.292 and 0.289 s), and the gate is on GPU time. Text: [reference](bench-report-reference.md#the-fields) and [the convergence phase](bench-report-reference.md#the-convergence-phase).


### 2026-11-21, #50, #226, #236, #55 and #203: hardware ray queries by default, and what followed

Pointer. Hardware is the default where the adapter has ray queries (owner, 2026-11-21, #50 Part 2). #226 took a camera held under the sea from 774.4 MiB to 465.5 MiB (dev box, full size). #236's sweeps made `16:32@2` the default BLAS grouping (worst path's update `p95` 0.55 ms; 16 everywhere 0.91; 32 everywhere 0.57 with a soft-miss gate missed). #55's denoiser costs 0.227 ms mean (0.255 p95) on the 4090 at `4c02da7`, budget 1.0 ms. #203's day cycle is `--day-cycle`, with a frame-time gate of 16.7 ms. Text: [reference](bench-report-reference.md#the-hardware-ray-backend-50).


### 2026-11-21, #236: the first Edit by the ground, and what the author could not verify

  The stress does **not** wait for the GPU before its first Edit: the first hardware Edit by the
  ground on the 4090 was 5.7 ms (the others 1.4 to 2.0), 3.2 of them in `scene_write`, and compute's
  1.9 with 0.4, and a wait would have hidden what a player pays the first time they dig. The cause
  found (on lavapipe, with the device's allocator report) is the scene upload's staging belt: it
  made a 4 MiB chunk, a device allocation, when it had none free, and a context whose belt was cold,
  or whose chunks the GPU still held, did that in its first Edit's frame (compute's first write does
  it too on a fresh context: 2.9 ms there). The belt now has three chunks before the first update
  (`BELT_WARM_CHUNKS`), and `edits_by_the_ground_fit_in_the_scenes_buffers_on_either_backend` asserts
  that no Edit makes a device allocation of a MiB or more (the small ones an Edit makes are wgpu's
  own staging and build scratch, sub-allocated, which a first Edit and a later one both make). If the first hardware Edit is still over on the 4090, its
  `scene_write`, `scene_submit` and `scene_grow` say which part it is.
  **The first Edit's submit** (on the 4090 at fbc133e: 5.8 ms on compute and 7.0 on hardware by the
  ground, against 0.06 on the Edits after it and on the altitude stress's) follows the jump to the
  ground, whose upload, freed buffers and builds the GPU and driver are still on: `settle_for_edits`
  now draws six frames after the jump, as the game does before a dig (each waits for the GPU), and
  prints their times (`settle:` on stderr). `scene_queue_submit` says whether the time is in
  `Queue::submit` itself.



**Not verified on a real GPU by the author of #236**: the dev box's lavapipe has ray queries, so
every test runs for real, but it cannot show a GPU race or a time. The order of the box pass, the
builds and the TLAS build rests on queue order and wgpu-hal's barriers, and what the 4090 does
with it is for the orchestrator's runs to show (`needs-benchmark`, `needs-stream-bench`).


### 2026-11-21, #55: what the settled residual is

**What the settled residual is (#55, measured on the dev box at 640x360 against 1024-sample truths).**
A settled frame's distance from the denoised mean is mostly the radiance cache's error, not the
frame's noise: a cell's light is the mean of all its samples so far, so it is the same from frame to
frame and the history cannot average it (a history cap of 64 instead of 32, or no rejection of
history, changed the settled FLIP by under 0.0003, and a cache that forgets slower changed nothing in
the golden and the pit). Only the filter, over the cells, can, so it runs on every pixel with a tight
luminance stop (`PHI_LIGHT` 1.5): the first version left a pixel whose history was long alone, and the
pit and the room paid for it (settled FLIP, dev box: pit 0.081 to 0.071, room 0.090 to 0.075, golden
0.062 to 0.059, raw 0.091, 0.098 and 0.059). What remains beyond that is the cache's own noise and
bias, which more GI samples a cell would take.

The FLIP of a mean of a noisy image is not the mean of the FLIPs, so `bias` and `residual` do not add
up to the settled FLIP; they say which of the two is the larger. The run adds `--converged-frames`
frames a view, so the default 1024 makes the phase longer (it was not timed on the 4090). To measure
it alone with fewer: `client bench --truth-spp 8192 --converged-frames 256`.


### 2026-11-27, #242 and #251: headroom hooks and the loader's latency

Pointer. `--gen-extra-us`, `--peak-m` and `--trench-m` (#242) give the headroom curve of the streaming gates; #251's `loader_latency`, `loader_ahead_*`, `camera_soft_miss_by_level` and `by_second` say why a flight has camera misses. Text: [reference](bench-report-reference.md#headroom-hooks-242) and [the fields](bench-report-reference.md#the-fields).


### 2026-11-28, #264: the machine profile and the world map's build time

Pointer. The 320 km world map took 24 s to build before #264 and takes 11 s on 8 threads of the dev box. The low-end profile (4 threads, 5 GiB free, 4 GB of video memory, no ray queries) starts at LOD bias 1.5; the default 1.0 needs 9.7 GiB free. Text: [reference](bench-report-reference.md#the-machine-profile-and-the-world-map-cache-264).


### Gate and budget timeline, compiled 2026-11-28

From the entries above, the commits of `bins/client/bench-budgets.toml` and `streaming-counts.golden`, and the issues. Dates are of the commit or the decision.

| Date | Change | Where |
| --- | --- | --- |
| 2026-10-27 | The per-stage frame budget of the rendering decision: `primary` 1.0 ms, `upscale`, `gpu_total.primary` 8.6 ms; `reference` 3.0 ms set from the first 4090 run (1.10 ms mean, `bdbba3d`) | #10 |
| 2026-10-28 | A pass over its budget blocks the merge | #13 |
| 2026-11-08 | `edits` 1.0 ms (the metric Edit rate) and `edit_frame_thread` 4.0 ms (the longest Edit) | #147, #156 |
| 2026-11-10 | The streaming gates decided: frame time p99 16.7 ms, frame-thread CPU p99 8 ms, holes none, scene memory 512 MiB; the label `needs-stream-bench` | #143, #162 |
| 2026-11-12 | The real-time mode runs the gates at full size; the stepped mode gates holes in every frame and keeps a counts hash | #162, #181 |
| 2026-11-16 | Gates added: soft misses under the camera (none), `scene_lag_d0_max` 8.0, `spawn_ring_s` 2.0, `time_to_control_s` (10 s while the window waited for the whole Guard), `settle_s` 55.0 | #168 |
| 2026-11-17 | `shadows` 0.4 ms, `atmosphere` 0.3 ms | #52, #53 |
| 2026-11-19 | `gi` 1.5 ms; the real-time gates `gi_mean_ms` 1.5 and `gi_p99_ms` 3.0 (the full-size Planet cost 4 to 5 ms in the first run) | #54 |
| 2026-11-20 | `time_to_control_s` 2.0 (the Guard is built in the background); the room's Reference gate on GPU time (0.3 s) | #220, #54 |
| 2026-11-21 | `denoise` 1.0 ms; Primary's FLIP at most 0.05 against the truth in every view; the budgets now hold the hardware backend (same numbers) | #55, #50 |
| 2026-11-21 | The day cycle's frame gate (no frame and no p99 over 16.7 ms) | #203 |
| 2026-11-28 | `starting_bias` in `gates.over` for a full-size default-profile run that did not start at LOD bias 1.0 | #264 |

## Notes of the 2026-11-28 docs review

Statements in the text above that no longer match `main`, found when `benchmarking.md` was split. The text above is left as it was written.

- **The settle limit is a gate, and was "proposed".** The text of 2026-11-10 and "The gates" say the settle limit is proposed and not yet agreed. `settle_s = 55.0` is in `[streaming]` of `bins/client/bench-budgets.toml` and is held by every real-time run. No comment in the repo records the owner agreeing to 55 s (main's 47.8 s on the 4090 plus 15%); see the review's report.
- **"The gates" is older than the budgets file.** The text says the `[streaming]` table has four gates and that soft misses, time to level 0, settle time and Edit latency are reported and not gated. Since #168, #220 and #54 the table has eleven keys: `frame_time_p99_ms`, `frame_cpu_p99_ms`, `holes`, `scene_mib`, `camera_soft_miss_frames`, `scene_lag_d0_max`, `spawn_ring_s`, `time_to_control_s`, `settle_s`, `gi_mean_ms` and `gi_p99_ms`. Still reported and not gated: the time to level 0 under the camera, the scene thread's own soft misses and Edit latency. A full-size run of the default profile that did not start at LOD bias 1.0 has `starting_bias` in `gates.over` (#264).
- **"Budgets" is older than the budgets file.** The text says only `primary`, `shadows`, `atmosphere`, `upscale` and the total are live. The file has `[passes]` `primary` 1.0, `reference` 3.0, `upscale` 0.5, `edits` 1.0, `edit_frame_thread` 4.0, `shadows` 0.4, `atmosphere` 0.3, `gi` 1.5 and `denoise` 1.0, and `[gpu_total]` `primary` 8.6 and `reference` 3.0.
- **The exit status paragraph left out three gates.** Status 2 is also the Primary-against-truth gates of the convergence phase (`convergence.gi.gates.over`, #54, #55) and the day cycle's frame gate (`day_cycle.gates.over`, #203); `--no-fail` silences both. See `bins/client/src/bench/mod.rs`.
- **The stepped run's committed hash.** The text lists the hashes up to `485be9b0eb879d35` ("once a coarser change parks only the nodes on the hot list"). The hash committed in `bins/client/src/bench/streaming-counts.golden` has been `7933c0c471f096e6` since `1ca1276` (2026-11-19, "The split lead is 1%"). The committed values, from `git log` of that file: `38b58d66b96791af` (#146, 2026-11-14), `2466f0700e79713a` (#167, 2026-11-15), `26a1ce7521482646` (`011e547`, 2026-11-16, #168), `d3a349f7c274126e` (`47322ae`, 2026-11-16), `bd99020be09af4ba` (`99b08c4`, 2026-11-16), `aab6939c9cbb3060` (`8f3b259`, 2026-11-17), `5aef330642fdf928` (`0c15489`, 2026-11-18), `7933c0c471f096e6` (`1ca1276`, 2026-11-19). `485be9b0eb879d35` is not in the file's history: it was an intermediate value of #168's branch.
- **"Tests" names schema version 8.** The report's schema is 38 (`SCHEMA_VERSION` in `bench/report.rs`; `bins/client/tests/bench.rs` checks it).
- **Times that #264 changed or did not re-measure.** "Generating the whole Planet is about 22 s" (the old run-time estimate) and the real-time mode's "world map (about 25 s)" are from before the world map cache and the parallel build: the 320 km world map takes 11 s cold on the dev box's 8 threads and is read from a file after the first build (#264). The bench's own run time was not re-measured.
- **The memory gate's figure.** "514.8 MiB at its peak, 2.8 over the gate, left to #167" is the 4090's run of 2026-11-13. #146's horizon cap, #213 and #226 changed the scene's memory (a camera held under the sea: 774.4 MiB before #226, 465.5 after, on the dev box); the latest 4090 peak is in `docs/measurements.md`, not here.
