# Residency is a complete 360° shell with an always-resident guard

The client keeps a complete shell of LOD rings around the camera, in every azimuth, rather than a frustum-shaped set in front of it. The view direction orders work and sets refinement priority; it does not decide what is resident. A coarse guard (levels 8–13, about 20 MB) is always resident, so a late fine brick becomes a coarser frame rather than a hole.

We chose this because turning, not flying, is the hard case. A 180°/s turn sweeps the horizon at about 800 m/s, sixteen times the 50 m/s flight speed. A frustum-shaped resident set would need a 104°-wide region in 0.58 s, which no prefetcher can guarantee; without a guard it shows holes, and with one it shows visible coarse pops. The shell is 27–105 MB, small against the ~2 GB budget, and the guard is 20 MB and generates from the seed in about 0.8 s on eight cores.

## Considered Options

- **Frustum-only residency with a prefetch cone:** saves 2–3× memory and generation, but only works for slow turns. Rejected for the shipped path; kept as a debug mode.
- **Frustum plus the guard, no shell:** hides holes but not coarse pops, and the pop is exactly what the fidelity bar forbids.
- **Pre-warming the whole Planet:** 327 MB at levels 6–13; a hundred times the live set and mostly below the horizon. Rejected.
- **A hard 2 GB cap that drops required bricks:** turns a budget into a hole. Exceed the budget by a bounded overflow, then coarsen the LOD.

## Consequences

- The resident set is view-independent; only refinement priority follows the view.
- A 1.25× radial margin and a 20 m vertical margin are residency, not prefetch.
- Eviction is distance-band first, then LRU inside the band, with 10–20% overflow and no eviction of the guard, the current frame's set, in-flight work or a live Edit chain.
- The shell is the precondition for the horizon cap and the LOD claims: culling without a complete shell is a hole, and LOD without prefetch is a pop.
- The price is paid in M1a-2, not M1a-1: the walk slice ships a simpler ring loader, and the shell lands with flight and orbit.
