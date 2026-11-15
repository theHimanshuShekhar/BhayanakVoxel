# LOD that coarsens with camera speed

The distance `d0` at which a level-0 Voxel covers one pixel is the LOD rule's only size (ADR 0002). It now shrinks as the camera moves faster: full detail up to 10 m/s, then a ramp to about one and a half levels coarser at 50 m/s. A fast camera draws coarser bricks on purpose. It was decided in the plan review of 2026-11-10 and amends ADR 0002's "one Voxel never covers more than about a pixel", which now holds for a slow camera.

We chose it because the strict rule cannot be met at 5 cm. Flying at 50 m/s at full detail needs 1.07–1.29 million generated bricks a second, and eight cores generate about 150 thousand: 7 to 8 times short. The acceptance "terrain keeps up with 50 m/s flight" was therefore failing by construction, as a stream of soft misses that no amount of prefetch fixes. Each level coarser needs about a quarter of the bricks for the same ground, so one and a half levels is about the 8× that is missing, and at speed the lost detail is under the motion on screen.

- **It is the drawn rule, not a fallback.** The band selects the coarser level, the shell holds it, and the prefetcher asks for it. A brick drawn at the speed-scaled level is not a soft miss (`CONTEXT.md`).
- **It coarsens at once and refines when the camera slows.** Full detail returns within a settle time after the camera slows or stops; the time is measured by the streaming bench, reported first, and gated once known.
- **The curve's numbers come from the streaming bench**, not from this ADR: the speeds, the depth of the ramp and any hysteresis are set so that the bench's paths show no soft miss at the speed-scaled level.
- **The Zoom uses the same rule**, with the camera's speed along the Zoom.
- **The spawn uses the same idea in distance:** play starts when a 20 m ring is at full detail (≤ ~2 s), and loading goes on nearest first, to 40 m and then the shell.

The acceptance becomes: no holes, ever; zero soft misses at the speed-scaled level on the bench's paths; frame time p99 ≤ 16.7 ms and frame-thread CPU p99 ≤ 8 ms, on the full-size Planet on the 4090.

## Considered Options

- **Keep the strict rule and make generation 8× faster:** GPU generation or a much cheaper generator. A research project with no known end, and bit-identical generation (ADR 0003) rules out the easy GPU path.
- **Keep the strict rule and accept soft misses at speed:** the same picture, but unplanned: the level drawn depends on what happened to arrive, so it pops, and "soft miss" stops being a bug that a gate can catch.
- **Lower the flight speed:** 50 m/s is already slow for a 320 km Planet (ADR 0008).
- **A coarser level only in the direction of travel:** the turn is the hard case (ADR 0004); a view-dependent rule brings the frustum problem back.

## Consequences

- `Bands::d0` becomes a function of camera speed, on the client only; the server and the generator do not know it.
- The shell at speed is smaller than the shell at rest, so memory is bounded by the camera at rest.
- Detail visibly sharpens after a stop. The settle time is the number the player feels, which is why it is gated.
- The streaming bench must exist before the curve can be set: it is the first M1a-2 ticket.

## The curve, as built (#168)

- **Depth** is 0 up to 10 m/s and a linear ramp to 1.5 levels at 50 m/s and above, taken in steps of half a level (0, 0.5, 1, 1.5): `d0` is scaled by 2^-depth (`render::lod::Bands::speed_scaled`, `SpeedScale`). A step is taken at once, rounded up, when the speed asks for it, and given back only when 1.1 times the speed no longer asks for it, so noise about a step's speed flips nothing; the selection's test holds a flight at a steady speed (with 4% noise on it) to the same drawn set, frame by frame, as the exact speed gives, and to one change of bands.
- **Both are in `Bands`, and they multiply with `coarsened`.** The memory cap's coarsening (ADR 0004) is applied to the speed-scaled bands (`Engine::bands_at`): where both apply the coarser wins, and the order of the bands holds. `Bands::d0` is the base `d0`, unscaled: the jump (`JUMP × d0`), the follow limit, the look ahead and the reserve are in the units of the scene at rest.
- **A change of depth is made in place**, not by making the scene again (which is 10 s at full size): every node is looked at again, 16 K of them in each `Selection::update`, finest first, and at most 32 K bricks change in one update, so the scene thread goes on following the camera. The scene is not settled (`Scene::waiting` is not 0) until all are looked at.
- **The numbers come from the full-size real-time bench with no GPU** on the 8-core dev box (`a_full_size_run_with_no_gpu_reports_what_the_scene_thread_did`; the scene thread's counts are as the 4090's, the times are not): `docs/measurements.md`, "Speed-scaled LOD". At 1.5 levels, and also at 1.0, the flights at 2 m and 50 m, the take-off and the turn have no soft miss in any of the 6 runs of this change (5 at 1.5, one at 1.0), where depth 0 has 14 K to 28 K a frame on a quiet box (and 0.2 K to 0.9 K on a box at a load of 20, so a run is not evidence of itself). **The soft misses are the scene's own**, counted against the scene's centre, which lags the camera by up to 9.5 `d0` (7.6 to 7.9 at depth 0): the lag is not better, and the owner is to say whether it is gated (#168's note from #145).
- **Settle after a stop from 50 m/s** takes 67 to 80 s on the dev box at 1.5 (97 s at 1.0 on a loaded box; 52 s with no speed scaling, one quiet run): the loader has the ground round the stop to generate at the finer level, which is what the settle time is.
- **The spawn ring is 20 m** (`SPAWN_REACH`): 0.83 to 0.88 s at full size on the dev box (40 m was 3.4 s). The Guard that follows it in the load takes 4 s, and the window waits for it, so that nothing past the ring is sky; the bench reports the two (`ring_s`, `guard_s`) and gates the ring.

