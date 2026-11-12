# Measurements

Measured numbers that the ADRs' decisions rest on. The ADRs say what was decided and why; this file says what was measured, on what, and when. A ticket that measures something again adds a dated entry here and does not edit an ADR, unless the new number changes a decision.

Unless an entry says otherwise, the figures are from the 16 km Planet at 5 cm Voxels with the camera at rest near the ground, on the 8-core dev box. ADR 0008 (320 km) and ADR 0009 (a smaller `d0` at speed) change most of them; the streaming bench (#162) measures them again.

## Residency and the shell (ADR 0004)

### Shell and Guard sizes (#49, #142, #143)

- The Guard at 5 cm and a 16 km radius is levels 9–13 and 74 MiB, and generates from the seed in about 0.8 s on eight cores.
- The prefetch simulator sizes its shell at 227–259 MB per scene copy near the ground (about 1.7–1.9 million bricks at 136 bytes). The client measures 474 MiB per scene copy there (3.44 million drawn bricks at 144.5 bytes).
- Why they differ (#143): the simulator's shell stops at 1.25 times the horizon, and the client has no horizon cap yet, so it draws the whole Planet, every level to the far side, which is twice the bricks at the ground. The simulator's whole-Planet count was right (3.25 d0², the calibration test) and its bytes a brick a little low (136 against 144.5).
- `prefetch-sim` prints the client's whole-Planet size beside the shell (`built_size`). Its intake (35.2 K drawn bricks a second) and generation cost (62 µs a generated brick) are the #142 fill's: 3.44 million drawn and 9.41 million generated in 97.8 s on six workers. The model generates 1.7 bricks per drawn brick and the client 2.7, so its generation columns undercount the client's by 1.6×.
- The full-size test holds the scene to 512 MiB.
- The host store at the complete shell with the margins holds 20.9 million counted bricks (19.8 per d0²); 16.1 million without them (15.3 per d0²); 29.3 million before #142. `STREAMED_PER_D0_SQUARED` was raised from #141's 15.3 to 19.8 to fit the margins.
- Pre-warming the whole Planet at levels 6–13 would be 327 MB.

### What the margins cost (#143)

Full-size client, 1.25× radial and 20 m vertical margins against none:

| | with margins | without |
|---|---|---|
| Store, counted bricks | 20.90 million | 16.13 million |
| Units resident | 3.15 million | 2.18 million |
| Bricks generated in the fill | 13.67 million | 9.41 million |
| Fill time | 133–150 s | 98 s |
| Bricks drawn | 3.416 million | 3.440 million |

The margins cost 30% more of the store and 45% more bricks generated, and draw 0.7% fewer bricks (the bricks they make resident are known to be full beside the drawn ones, which buries a few more). The margin is 25% of each band's start, 26 m before level 1's. A vertical take-off, which the simulator needs 1.75× for against a horizon cap, reveals nothing in the client while it has no horizon cap.

### Soft misses in the client (#143)

Counted by the scene thread as the bricks drawn whole although the bands split them, at 60 frames a second in real time on the 8-core dev box, the shell complete and the camera still at the start of each path (`soft_misses_on_the_flight_take_off_and_standing_paths` in `streaming_tests.rs`). Frames with a soft miss, of the frames of the path, and the most bricks missing at once. The margins-only column is two runs and the margins-and-prefetch column three (the third, after the unlocked reorder of the loader's queue: 231 of 597, peak 69 K at 2 m; 166 of 597, peak 12.7 K at 50 m). The counts move by a factor of two or more between runs (the loader's rate is what they measure), so read them as orders of magnitude.

| Path | no margins, no prefetch | margins | margins and prefetch |
|---|---|---|---|
| 50 m/s flight at 2 m, 10 s | 585 of 598; peak 165 K | 472–532 of 596; peak 78–105 K | 215–311 of 596; peak 45–70 K |
| 50 m/s flight at 50 m, 10 s | 557 of 598; peak 148 K | 269–467 of 597; peak 12–54 K | 166–381 of 596; peak 2–13 K |
| 50 m/s vertical take-off, 6 s | 0 of 359 | 0 of 359 | 0 of 358 |
| standing still, 2 s | 0 of 120 | 0 of 120 | 0 of 120 |

- The flights are limited by generation, not by what is resident: the loader makes about 150 K bricks a second against the 1.1–1.3 million the simulator asks for (the first column's backlog is 435 K to 540 K bricks at the end of a flight). No residency margin prevents misses; it halves or thirds them, and prefetch cuts them to a tenth where the lead lands on ground that the loader can still reach.
- The take-off and standing still miss nothing in any column, because the client draws the whole Planet and nothing it holds depends on the view or on altitude.
- The 180° turn criterion does not apply to this client: the selection is view-independent and the whole Planet is drawn, so a turn changes nothing that is wanted or resident, and the 2 s of standing still is all that it can be measured as. The case returns when a horizon cap does.
- Margins and prefetch stayed within the budget (the complete set with the margins, 19.8 per d0²), and nothing took a step of coarsening.

### Eviction and coarsening (#141, #143)

- At full size, listing the units takes 12–15 s of the scene thread's time with the margins (the store passes the limit by 4% meanwhile in the 5 km flight), so the limit is tight.
- A later full-size run (after the loader's queue was reordered without its lock held) peaked at 22.22 million in the 5 km flight against a limit of 23.03 million, with 2.37 million bricks evicted in two passes, the scene 472 MiB after the fill and 220 MiB at the end of the flight, and the fill generating 13.67 million bricks in 154 s with the machine loaded: the reorder starves nobody.
- On the test Planet a budget at 90% of the complete set took five steps of coarsening and drew 9.4 thousand bricks instead of 21.0 thousand. The steps are coarse, so the budget should be sized so that the shell fits. The full-size client does not coarsen at rest or in a 5 km flight with the 19.8 per d0² budget.

### Turning against flying

A 180°/s turn sweeps the horizon at about 800 m/s, sixteen times the 50 m/s flight speed. A frustum-shaped resident set would need a 104°-wide region in 0.58 s.

## The scene thread (ADR 0006)

### Before the scene thread (#127)

- Selection and packing on the frame thread cost it from a millisecond to tens of them a frame: walking took 8.5 ms at the 99th percentile and flying 28 ms.
- Repacking is 18 µs a region, and flying at 48 m/s touches 330 regions a frame, before the selection's own time.

### Memory

- The scene thread's buffers are 542 MiB reserved at full size, about 425 MiB in use once the Planet is filled. The frame thread's copy is the same size.
- While the scene is packed again there is one more copy of all of it waiting in the outbox and the old one on its way back: up to about four times the scene's reserve for the moment of a jump or other bands.

### Edits and stalls (#156)

- Planning an Edit and writing its level-0 bricks on the frame thread is a few milliseconds for the largest Brush at 5 cm.
- The coarse levels are remade in steps of half a millisecond and are as they were for a few tens of milliseconds after an Edit.
- The frame thread waits for the level-0 pack for 4 ms at the most (`EDIT_WAIT`).
- The scene thread's read of the store lasts the whole of following the camera: tens of milliseconds at 48 m/s at the full tree and a few at walking speed.
- The ghost made again after a click costs 5 to 6 ms at the worst for the largest cube at 5 cm at the spawn, the plan's reads of the generator's bricks included.
- A click costs 2 to 4 ms for the largest cube, and 8 to 10 ms at a cold spot, where the plan reads bricks the generator has to make for the first time.

### Region layout

Packing a region's mixed bricks together writes about three times as much over the bench's flight (211 MB against 73 MB), for about a tenth more CPU. #163 is the ticket that removes the extra writes.

## 5 cm Voxels (ADR 0007)

### Generation and the fill (#147, #142)

| | 10 cm | 5 cm | 5 cm after #142 |
|---|---|---|---|
| 40 m spawn ring, eight cores | 751 000 bricks in 1.8 s | 5.6 million bricks in 7.6 s | |
| Full fill, camera still: generated | | 23.9 million in 158 s | 9.4 million in 118 s |
| Full fill: drawn | 3.04 d0² | 3.56 million (3.37 d0²) | 3.44 million (3.25 d0²) |

- The shell reaches the same metres (the horizon, 253 m) in twice as many Voxels, and `d0` halves to 51 m (1028 Voxels).
- Bricks wholly under the sea were the larger part of what #142 saved: 8 of the 14 million bricks, and 3.56 million drawn bricks became 3.44.

### The character's tick (#147, #154)

- The capsule touches about 7 times the Voxels, and a kilometre's run takes 10.6 to 12.8 ms a tick in the mean on the dev box (2.1 ms at 10 cm; 10.6 ms with the ground ball held at 4 m, 12.8 ms with it at 8 m), against the 16.7 ms there is. The 99th percentile is 24 ms and the worst tick 33 ms.
- The test `a_character_runs_a_kilometre_from_the_spawn_without_stopping_while_the_ground_keeps_up` keeps its half-tick limit and was `#[ignore]`d until #154.

### The ground ball

- `sim::Ground::REACH` is 8 m (160 Voxels), with a ring buffer of 64 bricks a side and 1024 new bricks a tick at the most (they were 80, 32 and 128 at 10 cm).
- `Ground::MAX_BYTES`, the bound, is 308 MB (294 MiB; 38 MB at 10 cm). The most a ground held in the kilometre walk, on the client and on the server alike, was 14.6 MB (2.4 MB at 10 cm).

### The Edit budget (#137, #156)

- 8 m³ a second is `VOXELS_PER_SECOND` 64 000 at 5 cm. The Voxel burst is 9 seconds' worth, 576 000. The work rate and burst are 4.8 million units a second and 2.88 million, 8 times the 10 cm figures. A full 2 m-radius dig takes about 4 s and not 33 s.
- The Brush limits are 2 m in radius (40 Voxels) and 1 m for a Smooth or a Flatten (20). `affordable_radius` gives 40, and 22 for a Smooth.
- Before #156, a biggest Brush asked the server for about 2.1 to 2.9 million units, 21 to 29 ms at 10 ns a unit, in one tick; `EDIT_WORK_PER_TICK` (twice the burst) is 5.76 million, which is 58 ms, three and a half ticks of server time, if enough players dig at once.
- Since #156: the frame thread spends 1.1 ms at the most on a 2 m Edit (it was about 40), the whole chain 6.2 ms at the most, and the server's largest Brush about 2 ms (it was 62).

### Coordinates

The 16 km Planet's extreme Voxel is 360 000 from the centre; the Zoom at 800 km is 16 million, below the 2^24 that the GPU holds exactly. At 320 km the Zoom at 20 R is 128 million Voxels (ADR 0008).

## Sizing constants in the client

`bins/client/src/main.rs` sizes the GPU buffers and the store's budget from these. Each was measured once, by hand.

| Constant | Value | From | Test Planet (#181), limit = value × `HEADROOM` |
|---|---|---|---|
| `BRICKS_PER_D0_SQUARED` | 3.25 | drawn bricks of the full fill after #142 | 3.51 (limit 4.06) |
| `REFS_PER_BRICK` | 1.0 | full fill | 1.01 (limit 1.25) |
| `WORDS_PER_BRICK` | 34.2 | full fill | 40.01 (limit 42.75) |
| `STREAMED_PER_D0_SQUARED` | 19.8 | store at the complete shell with the margins (#143) | 13.09 (limit 24.75) |
| `HEADROOM` | 1.25 | chosen | |
| `BUDGET_OVERFLOW` | 0.1 | ADR 0004's 10–20% | |

The streaming bench (`client bench --streaming`, #181) measures the four on the test Planet in CI and a stepped test fails when one is over its limit (see the entry below). The full-size, real-time run (#162, `client bench --streaming --real-time`; its section below is to be filled from the 4090 baseline) and the 320 km and speed-scaled LOD tickets (#166–#168) measure them again.

### The streaming bench on the test Planet (#181, 2026-11-12)

`client bench --streaming`: the world-map test Planet (512 texels a face), `d0` of 64 Voxels, the game's Guard (levels 9 to 13), the margins of ADR 0004, stepped (#180) on a fixed schedule: the fill until the resident set is complete (51 frames), 60 frames standing, 180 frames walking at 24 m/s, an Edit at frame 150 of the 240, and the store held to 102% of the complete set. Identical on every run: the debug and the release binary, and the unit tests, give the counts hash `b37e0fb7cd3a2c78` (committed in `bins/client/src/bench/streaming-counts.golden`). Measured on the 8-core dev box.

- **Sizing, with the resident set complete:** 3.51 bricks drawn per d0² below the Guard (the constant is 3.25), 1.000 refs and 39.78 words per Ring brick (1.0 and 34.2), 13.09 store bricks per d0² beyond the spawn ring (19.8, of which the ring is 5.33 and level 0 beyond the ring 1.29, so 13.19 is what the test Planet can show). The test Planet's `d0` is small against the Planet, so what does not scale with `d0` is left out of the measurement: the Guard's levels (6 627 bricks, 32% of the bricks drawn here, 5% at full size; the scene counts their words exactly: 53.8 a brick, 356 601 in all) and the 226 964 bricks of the spawn ring that the store starts with (all of them level 0, as the 5.63 million taken out of the constant is; level 0 does not grow in the fill, so there is none beyond the ring when the set is complete, which is where the check measures; the walk that follows does add level-0 bricks beyond the ring). Counting everything instead gives 5.12 and 68.5 for the bricks and the store, which are not per-`d0²` numbers. The constants had the Guard and the ring in, so **the limits are not 1.25 times what the Planet shows, like for like**: bricks 4.06 against the full-size Rings' 3.09 (1.32 times), words 42.75 against 33.1 (1.29 times, if the Guard weighs at full size what it does here: #162 measures that), refs 1.25 against 1.0, and the store 16.49 (the constant less the ring's and level 0's shares, times 1.25) against 13.19. The words (20% over what the full-size Rings took) and the bricks (8% over the constant, 14% over the full-size Rings') differ from the full-size values because `d0` is 1028 Voxels there, and the mix of bricks differs with the scale. Level 0 was not like for like until it was taken out of the constant too: its band lies inside the spawn ring when the set is complete, so there are no level-0 bricks in what is streamed at the point the check measures (before the walk), and at full size it held 1.36 million beyond the ring (1.29 per d0²); the part is 13.19 (limit 16.49) and 13.09 passes (99% of the part, 79% of the limit). The words are counted for the scene of every publish: an earlier version of the measurement took them from a snapshot of the round's first publish (14 540 references for 14 363 bricks, 40.08 words a brick), which the references-equal-bricks test now catches. All are inside the limits.
- **Upload:** 69.8 MB in 228 updates over 293 frames, one rebuild, 56.7 MB of it patches, at most 12.9 MB in a frame. The GPU's own count (lavapipe) is equal to the one worked out on the CPU, to the byte.
- **Soft misses:** 68 884 bricks in 38 of the 51 fill frames (3 407 at most in a frame), none while standing and none while walking at 24 m/s: the margins and the lead are ahead of the walk.
- **Scene work:** follow 179 steps and 53 335 bricks changed; pack 1 717 steps, 46 187 bricks changed from 158 488 made by the loader; upload 228 steps; eviction 52 steps, 33 641 units, 113 091 bricks.
- **Residency:** 280 570 counted bricks at the complete set, 288 776 at most, 287 041 at the end (budget 286 181 plus the 1% overflow); 67 Diverged bricks after the one Edit; the frame that made the Edit takes it (Edit latency 0: the Edit is made before the frame's look, as the game makes it, and a stepped scene thread finishes its round inside the look, so this field cannot show ADR 0006's bound; the stepped test of a busy scene thread and the real-thread tests hold that, and the real-time mode of #162 reports it: `streaming_real_time.runs[].edit_latency`, from the 4090), and results are taken in the frame they are published in (Outbox latency 0 in all 293 frames, until #145). **Holes:** none in 293 frames.

### The streaming bench in real time at full size (#162, baseline to be filled)

`client bench --streaming --real-time`: the full-size Planet with real loader workers and a real scene thread, on the machine's clock, three runs of the paths in `docs/agents/benchmarking.md` ("The real-time mode"), on the RTX 4090. **No number below has been measured yet: each `to be filled from the 4090 baseline` is for the dev to fill from the first report** (`streaming_real_time`, the least and the most over the three runs, with the commit and the date), and #168 sets the soft-miss, settle-time and Edit-latency gates from it (time to control as the spec means it needs a server, which the bench does not have). The gates (`bins/client/bench-budgets.toml`, `[streaming]`) are frame time p99 16.7 ms, frame-thread CPU p99 8 ms, no holes, and memory within the scene's 512 MiB and the store's limit.

| Measure | Path | Least over the runs | Most over the runs | Gate |
|---|---|---|---|---|
| Frame time p99 (ms) | cold start | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 16.7 |
| | flight at 2 m | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 16.7 |
| | flight at 50 m | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 16.7 |
| | vertical take-off | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 16.7 |
| | 180 degree turn | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 16.7 |
| | stop after a flight | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 16.7 |
| Frame time worst (ms) | each path | to be filled from the 4090 baseline | to be filled from the 4090 baseline | |
| Frame-thread CPU p99 (ms) | each path | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 8.0 |
| Frame-thread CPU worst (ms) | each path | to be filled from the 4090 baseline | to be filled from the 4090 baseline | |
| Soft misses per frame | each path | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated (#168) |
| Holes found by the scene thread's looks (sampled, see "What the holes gate can and cannot catch") | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 0 |
| Looks for holes | whole run, fewest in a path | to be filled from the 4090 baseline | to be filled from the 4090 baseline | at least 1 |
| Hole looks' share of the scene thread's time | each path | to be filled from the 4090 baseline | to be filled from the 4090 baseline | |
| Level 0 under the camera, from the start of the load (s; not the spec's time to control) | cold start | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated |
| Settle time after a flight (s) | stop after a flight | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated (#168) |
| Edit latency (frames; ADR 0006's bound is 1) | one dig at the Spawn | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated |
| Edit latency (ms), and the Edit's own call (ms) | one dig at the Spawn | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated |
| Scene thread, ms by category: follow, pack, upload, eviction | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | |
| Scene's buffers in use, peak (MiB; not VRAM) | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | 512 |
| Scene's buffers as the GPU reserves them (MiB; the VRAM) | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated |
| Store peak (bricks) against its limit | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | the store's limit |
| The run's resident memory, peak (MiB; sampled twice a second) | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated |
| The whole process's peak (MiB; includes the normal bench and earlier runs) | whole run | to be filled from the 4090 baseline | to be filled from the 4090 baseline | not gated |

The sizing constants at full size, measured with the resident set complete (`streaming_real_time.runs[].sizing`), which replace the hand measurements in the table above and settle what the test Planet could only bound (the Guard's words per brick at full size, `WORDS_PER_BRICK`):

| Constant | Value | Measured at full size (4090 baseline run) | Limit |
|---|---|---|---|
| `BRICKS_PER_D0_SQUARED` | 3.25 | to be filled from the 4090 baseline | 4.06 |
| `REFS_PER_BRICK` | 1.0 | to be filled from the 4090 baseline | 1.25 |
| `WORDS_PER_BRICK` | 34.2 | to be filled from the 4090 baseline | 42.75 |
| `STREAMED_PER_D0_SQUARED` | 19.8 | to be filled from the 4090 baseline | 24.75 (no share of level 0 taken out at full size) |
| Words per Guard brick | | to be filled from the 4090 baseline | |

Edit latency is measured in this mode, once a run: one dig at the Spawn after the stop path, made before the frame's look at the scene as the game does, in frames and in milliseconds (`runs[].edit_latency`, with `min` and `max`). It is reported, not gated; the table above has its place.

## The Reference view (ADR 0005)

Measured on the RTX 4090 on 2026-11-01 ([#99](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/99) has every checkpoint), at 2560×1440 output, with the estimator as it was then (white noise, 4 bounces, flat colours). FLIP is LDR-FLIP at ppd 67.02 on the 8-bit image; each truth is 8192 samples. #187 and #188 measure these again.

| view | per-sample ms, mean / p95 | samples to FLIP ≤ 0.02 | GPU s | samples to FLIP ≤ 0.05 | FLIP at 64 samples | floor, FLIP / linear MSE |
|---|---|---:|---:|---:|---:|---|
| open (golden), traced at 1440p | 2.58 / 2.61 | 18.6 | 0.05 | 1.2 | 0.0145 | 0.0034 / 3.6e-7 |
| open (golden), traced at 720p | 0.76 / 0.78 | 54.4 | 0.04 | 3.4 | 0.0191 | 0.0051 / 4.0e-7 |
| pit, traced at 1440p | 6.15 / 7.92 | 330 | 2.03 | 6.2 | 0.0267 | 0.0092 / 9.2e-7 |
| pit, traced at 720p | 1.59 / 1.61 | about 1048 (extrapolated) | about 1.66 | 16.9 | 0.0346 | 0.0139 / 9.5e-7 |

- **Batching:** k samples in one dispatch cost 1.00 to 1.06 times k single samples.
- **One sample by part**, open view at 1440p (2.59 ms): the primary ray 0.52 ms, the sun's shadow ray 0.34 ms, bounces 1 to 4 1.04, 0.37, 0.21 and 0.12 ms. In the pit the bounces are 85 % of 6.1 ms.
- **Stopping tiles early** would save 3.7 times the work at the 0.02 line on the open view and 1.5 times in the pit; 8 to 13 times at the 0.05 line. About 41 % of the open view's tiles, the sky, pass at 1 sample.

