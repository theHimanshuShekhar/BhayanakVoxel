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
