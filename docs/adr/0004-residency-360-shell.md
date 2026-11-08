# Residency is a complete 360° shell with an always-resident guard

The client keeps a complete shell of LOD rings around the camera, in every azimuth, rather than a frustum-shaped set in front of it. The view direction orders work and sets refinement priority; it does not decide what is resident. A coarse guard (levels 9–13 at 5 cm Voxels, 74 MiB) is always resident, so a late fine brick becomes a coarser frame rather than a hole.

We chose this because turning, not flying, is the hard case. A 180°/s turn sweeps the horizon at about 800 m/s, sixteen times the 50 m/s flight speed. A frustum-shaped resident set would need a 104°-wide region in 0.58 s, which no prefetcher can guarantee; without a guard it shows holes, and with one it shows visible coarse pops. The shell is about 227–259 MB per scene copy near the ground (the prefetch simulator at 5 cm, #49), small against the ~2 GB budget, and the guard is 74 MiB and generates from the seed in about 0.8 s on eight cores.

## Considered Options

- **Frustum-only residency with a prefetch cone:** saves 2–3× memory and generation, but only works for slow turns. Rejected for the shipped path; kept as a debug mode.
- **Frustum plus the guard, no shell:** hides holes but not coarse pops, and the pop is exactly what the fidelity bar forbids.
- **Pre-warming the whole Planet:** 327 MB at levels 6–13; a hundred times the live set and mostly below the horizon. Rejected.
- **A hard 2 GB cap that drops required bricks:** turns a budget into a hole. Exceed the budget by a bounded overflow, then coarsen the LOD.

## Consequences

- The resident set is view-independent; only refinement priority follows the view.
- A 1.25× radial margin and a 20 m vertical margin are residency, not prefetch.
- Eviction is distance-band first, then LRU inside the band, with 10–20% overflow and no eviction of the guard, the current frame's set, in-flight work or a live Edit chain (#141). It runs on the scene thread (ADR 0006), a part of the units at a time.
- Two sizes are budgeted, and they are different quantities. The drawn scene is bounded by the bands. The prefetch simulator sizes its shell at 227–259 MB per scene copy near the ground (about 1.7–1.9 million bricks at 136 bytes); the client measures 474 MiB per scene copy there (3.44 million drawn bricks at about 144 bytes), so the simulator undercounts the drawn bricks by about 2×, and #143 re-checks it. The full-size test holds the scene to 512 MiB. The host store holds the bricks the scene splits, their ancestors and what the bands do not draw, except the air and the buried bricks, which the loader does not generate (#142): about 16.1 million counted bricks at the complete shell (15.3 per d0 squared; 29.3 million, 27.8 per d0 squared, before #142); the budget is that, with a 10% overflow before eviction starts.
- At the cap, when everything left is wanted, eviction backs off until the store has grown by a quarter. Coarsening the LOD to fit, as the option above says, is #143's; until then the store is bounded by the shell and its trail, not by a hard cap.
- The shell is the precondition for the horizon cap and the LOD claims: culling without a complete shell is a hole, and LOD without prefetch is a pop.
- Most of the price is paid in M1a-2, not M1a-1: the walk slice ships a simpler ring loader, and the shell lands with flight and orbit. The guard is the exception (2026-10-31): the walk slice's loader (#86) generates it right after the spawn ring, so nothing beyond the spawn ring is ever sky.
