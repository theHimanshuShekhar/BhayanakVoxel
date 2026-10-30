# Research: residency, prefetch and eviction

Resolves [#74](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/74). Feeds the catalogue in [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71) and reshapes [#49](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/49). Read with [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48), [#47](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/47), [#62](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/62), [ADR 0002](../../docs/adr/0002-brick-store-all-voxel-lod.md) and [ADR 0003](../../docs/adr/0003-client-generated-terrain-edit-ops.md).

Researched 2026-10-31. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). Lines marked **[estimate]** are my arithmetic, with the working shown. **[judgement]** is engineering opinion, not measured fact. **[unverified]** could not be checked against a primary source. Numbers are for the wayfinder's 16 km-radius Planet at 10 cm **Voxels**, 1440p output, 70° vertical field of view. The M1a test Planet is 150 m across, so its horizons and working sets are about 100× smaller; it exercises the mechanism, not the numbers.

## TL;DR

- **The guarantee is architectural, not predictive: hold a complete 360° shell.** Turning at 180°/s sweeps the horizon at about 800 m/s, sixteen times the 50 m/s flight speed. No prefetch can outrun that, so a frustum-shaped resident set will show a hole without a guard, or a visible coarse pop with one; neither meets the bar. Residency must cover every azimuth; the view direction may only *order* work. This is the single most important finding in this note.
- **The working set is small; generation is the binding constraint.** At one Voxel per output pixel the visible shell is 27–52 MB of bricks (k = 1) and at most about 105 MB with rough terrain (k = 2), peaking at 50–100 m altitude. The always-resident coarse shell is 20 MB. A 2 GB disk cache holds roughly 10 million bricks, about forty times the live set. The GPU is not the problem; the CPU has to generate 37 thousand bricks a second at 50 m/s, and up to about 490 thousand a second for a fraction of a second on a vertical take-off.
- **Sizing comes from the ring rule, not a guess.** With `r0 = 0.1 m / (70°/1440) = 118 m`, level `L` has Voxels `0.1·2^L m` and bricks `0.8·2^L m`, and each octave ring holds about the same number of bricks (~40–50 thousand). The surface area in a distance band is `2πR²[cos θ(d₁) − cos θ(d₂)]`; divide by the brick footprint. Section 3 has the table.
- **Prefetch 1–2 s ahead by velocity, and add a vertical margin near the ground.** Horizontal flight at 50 m/s churns 37 thousand bricks a second. A vertical take-off at 50 m/s reveals the horizon at about 4.5 km/s while still 2 m up: 55 thousand new bricks in the first 0.16 s, a burst of 350–490 thousand bricks/s against a steady 37 thousand. A 20 m vertical margin, generated while the player stands still, removes it for 19 MB.
- **Evict by distance band, then LRU inside it.** Distance decides what *should* be resident; LRU decides what to drop first when over budget, but never the bricks the current frame needs. This is exactly Cesium's tile cache ([source](https://github.com/CesiumGS/cesium/blob/main/packages/engine/Source/Scene/Cesium3DTilesetCache.js)). Add hysteresis (admit at `r`, retire at `1.25r` after 0.5 s) and an overflow allowance so the budget never forces a hole ([`maximumCacheOverflowBytes`](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html)).
- **An Edit is journaled first, stamped with an epoch, and patched into every resident level before the frame is recorded.** The chain is 1–8 coarse bricks per level, about 370 bricks and 75 KB for a 2 m brush. Nothing that can be edited may live only in RAM: the journal is the source of truth (ADR 0003), unedited bricks are regenerable (ADR 0002), and the disk cache stores **only** unedited bricks.
- **The always-resident coarse LOD is cheap enough not to cache.** Levels 8–13 of the 16 km Planet are 102 thousand bricks, 20 MB, and generate directly from the seed in about 0.8 s on eight cores. The ≤ 5 s orbit target is met without a disk cache. Cache it only if the measured single-core time exceeds ~3 s, and only as pure seed data, which can never be stale.

**Plain-language recommendation.** Keep a complete ring of bricks around the camera in every direction, like a shell, rather than only in front of the camera. Make the shell big enough for the horizon plus a margin, and let its outer edge be one LOD level coarser than it needs to be. Prefetch in the direction you are moving, a second or two ahead, but never rely on that for correctness. Keep a very coarse copy of the whole Planet that is always there, so a late fine brick means a coarser frame rather than missing geometry; the prefetch margin is what keeps even that off screen, and a frame that needs the guard inside the resident radius counts as a bug. Journal every **Edit** before it is drawn, and rebuild the handful of coarse bricks above it in the same frame. Evict what is far away, least recently used, and not needed this frame; never evict the coarse copy or anything the frame is about to draw.

---

## 1. What the policy has to guarantee

The fidelity bar (map [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71)) says culling must never remove a pixel, LOD error stays under ~1 output pixel, and there are no popping, holes or stale **Edits**. For residency that splits into four distinct failures, which need different defences:

| Failure | What it looks like | Defence | Where it is enforced |
|---|---|---|---|
| **Hole** | A ray finds no brick, so sky or the Planet's core shows through the terrain. | A complete shell plus an always-resident coarse level below it. Never depend on a request finishing in time. | `trace()` fallback + a frame counter; image diff. |
| **Pop** | The right brick exists at a coarser level, then is replaced by a finer one already on screen. | Prefetch ahead of the LOD boundary; hysteresis at the boundary; coarse-to-fine fill so a pop is a refinement, not a hole. | LOD-difference counter; image diff between LODs. |
| **Stale Edit** | The frame draws the pre-Edit version of a brick the player just changed. | Journal first, epoch-stamp, patch every resident level before submit; discard prefetched bricks built under an older epoch. | Per-brick epoch counter; an edit-storm bench path. |
| **Hitch** | A frame stalls while a brick is generated, packed, uploaded or the scene is rebuilt. | Background generation, per-level patch instead of rebuild, async upload, a per-frame budget for the main thread. | `frame_time` p99.9, generation queue latency, upload bytes. |

The four motions the player can make stress these differently:

- **Turning** does not change the camera position, so no new distance band opens. It changes *which directions* are in view. At 180°/s, a point at the 253 m ground horizon moves at 795 m/s across the view; at the 316 m resident radius, 993 m/s. A frustum-only resident set needs a new 104°-wide region in 0.58 s, which is 16× the flight churn. **This is why residency must be view-independent.** Cesium has the same problem and its answer is `loadSiblings`, described as ensuring "tiles are already available when the viewer turns left/right" ([docs](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html)).
- **Flying** translates at up to 50 m/s (`fly.rs`: 60 Voxels/s base × 8 with the speed modifier = 480 Voxels/s). The shell translates; the leading edge gains bricks and the trailing edge loses them. At 50 m/s the churn is 37 thousand bricks/s (Section 4.2).
- **Climbing/descending** moves the horizon. On a sphere the horizon distance is `d = √(h(2R+h))`, so at low altitude it grows as `√h` and very fast: `dd/dt ≈ v·√(R/2h)`, about 4.5 km/s for a 50 m/s climb at 2 m. The *set* of visible bricks also changes LOD as it grows: from ground to 50 m, 100 thousand new bricks, most of them in the first second. A vertical margin is the cheap fix.
- **Zooming** is a continuous radial move plus an LOD change. Zoom-out is the easy direction: the coarse levels are already resident, so the image can only get coarser, never hollow, and the fine rings are evicted as they leave the radius. Zoom-in is the hard direction: the destination's fine set (~226 thousand bricks at ground, 1.7 s on eight cores) has to be ready before arrival. A 5 s zoom has 3× headroom; a 1 s zoom does not.

There is also a **soft miss**: the ray hits a coarser level than the LOD rule asks for, inside the resident radius. It is not a hole, but it is a visible pop and a radiance-cache invalidation. The policy's target is zero holes, zero stale frames and zero soft misses inside the resident radius; soft misses are allowed only outside it, where the coarse level *is* the correct LOD.

---

## 2. Prior art: what shipped systems actually do

The pattern below is not novel; it is the consensus of virtual geometry, clipmaps and 3D Tiles.

- **Geometry clipmaps** cache terrain in "a set of nested regular grids centered about the viewer", "incrementally refilled as the viewpoint moves", which "allows interactive flight at 60 frames/sec" ([Hoppe](https://hhoppe.com/proj/geomclipmap/), [Losasso & Hoppe 2004](https://hhoppe.com/geomclipmap.pdf)). Two things matter here: the grid is **centred on the viewer and complete in azimuth** (a square, not a frustum), and the update is **incremental** at the leading edges. That is the ancestor of the shell policy.
- **Cesium 3D Tiles** has the most directly applicable policy. It separates *selection* (screen-space error, i.e. distance and pixel size) from *eviction* (LRU in a byte-budgeted cache). `cacheBytes` is the GPU budget and "tiles not in view are unloaded to enforce this"; `maximumCacheOverflowBytes` lets the loaded set exceed it "if tiles sized more than `cacheBytes` are needed to meet the desired screen space error", then unloads them when out of view. The cache source is a doubly linked list ordered LRU→MRU with a sentinel: tiles selected this frame are never trimmed, and only unselected tiles are unloaded while over budget ([source](https://github.com/CesiumGS/cesium/blob/main/packages/engine/Source/Scene/Cesium3DTilesetCache.js)). This is precisely "distance band + LRU within it, budget may overflow rather than show a hole".
- **Cesium's prefetch knobs** are the vocabulary the ticket asks for: `preloadFlightDestinations` "fetch tiles at the camera's flight destination while the camera is in flight"; `cullRequestsWhileMoving` avoids requesting tiles "that will likely be unused when they come back because of the camera's movement" with a multiplier of 60; `foveatedScreenSpaceError` prioritises the centre of the screen and defers the edges until the camera stops; `loadSiblings` covers turning ([docs](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html)).
- **Cesium's refinement** is a coarse fallback by construction: `skipLevelOfDetail` lets "children … be rendered alongside their parents" and "requires significantly less memory", instead of waiting for every child before the parent can refine ([docs](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html)). That is the same "parent is the hole guard" idea.
- **Horizon culling** is a per-frame test over terrain tiles, and in a typical view "over half of them … are below the horizon and do not need to be rendered" ([Cesium](https://cesium.com/blog/2013/04/25/horizon-culling/)). Residency should be horizon-limited for the same reason, with a margin (Section 3).
- **Aokana** (open-world voxel, 2025) streams LOD chunks and keeps "only about 5% of the complete voxel scene data … loaded into VRAM". Its loading order is explicit: "prioritize loading the chunks with higher LOD levels, allowing us to directly remove all child blocks associated with that chunk to free up VRAM. When there is sufficient space in the VRAM, chunks that are closer to the player are prioritized" ([arXiv 2505.02017](https://arxiv.org/abs/2505.02017), §3.4). Coarse-first, distance-second is the order this note recommends.
- **Nanite** streams geometry "in on demand so that only visible detail needs to reside in memory" and relies on fast disk, recommending SSDs ([Epic docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/nanite-virtualized-geometry-in-unreal-engine)). The transferable point is the coarse resident root plus on-demand refinement, not the mesh format.

Nothing in these sources gives a hard number for a 10 cm Voxel Planet; the numbers below are arithmetic from the repo's geometry and measured generator throughput.

---

## 3. Sizing the working set from ring geometry

### 3.1 The LOD rule and the ring ladder

Take the output pixel angle at 1440p with a 70° vertical field of view:

```
pixel = 70° / 1440 = 8.49e-4 rad
r0 = 0.1 m / pixel = 117.9 m
```

A level-`L` Voxel has edge `0.1·2^L m`, so it is one output pixel wide at distance `r0·2^L`. A level-`L` brick is 8 Voxels = `0.8·2^L m`. Ring `L` spans `[r0·2^(L−1), r0·2^L]`, so the LOD error is at most one output pixel everywhere in it. (Choosing one Voxel per *internal* 720p pixel instead would put `r0` at 59 m and make the LOD error two output pixels; that fails the bar. The `#51` upscaler reconstructs 1440p from jittered 720p samples; it does not turn a two-pixel Voxel into a one-pixel one.)

| Level | Voxel | Brick | Distance where the Voxel is 1 output pixel |
|---|---|---|---|
| 0 | 0.1 m | 0.8 m | 118 m |
| 1 | 0.2 m | 1.6 m | 236 m |
| 2 | 0.4 m | 3.2 m | 472 m |
| 3 | 0.8 m | 6.4 m | 944 m |
| 4 | 1.6 m | 12.8 m | 1.9 km |
| 5 | 3.2 m | 25.6 m | 3.8 km |
| 6 | 6.4 m | 51 m | 7.5 km |
| 7 | 12.8 m | 102 m | 15.1 km |
| 8 | 25.6 m | 205 m | 30.2 km |
| 9 | 51 m | 410 m | 60 km |
| 10 | 102 m | 819 m | 121 km |
| 11 | 205 m | 1.6 km | 241 km |
| 12 | 410 m | 3.3 km | 483 km |
| 13 | 819 m | 6.6 km | 966 km |

Bricks per ring are nearly constant. For a flat approximation, ring `L` spans `[r0·2^(L−1), r0·2^L]`, so its area is `3π(r0·2^(L−1))² = 3π r0² 4^(L−1)` and its brick footprint is `(0.8·2^L)²`, so

```
N_ring = 3π r0² / (4 · 0.8²) ≈ 3π · 117.9² / 2.56 ≈ 51,200 bricks
```

**[estimate]** For the spherical Planet the exact area between two camera distances is

```
A(d1, d2) = 2πR² [cos θ(d1) − cos θ(d2)],
cos θ(d) = (R² + (R+h)² − d²) / (2R(R+h))
```

At the ground this gives 51–68 thousand bricks per ring, close to the flat estimate. The horizon clips the outer rings, which is what makes the table below smaller than `rings × 51k`.

### 3.2 Bytes per brick

The packed GPU brick in `crates/render/src/scene.rs` is:

| Palette | Words | Bytes |
|---|---|---|
| 1 bit, 2 Materials | 16 occupancy + 1 header + 1 palette + 16 indices = 34 | 136 B |
| 2 bit, 3–4 Materials | 16 + 1 + 2 + 32 = 51 | 204 B |
| 4 bit, 5–16 Materials | 16 + 1 + 8 + 64 = 89 | 356 B |
| 8 bit, 17+ Materials | 16 + 1 + 128 + 128 = 273 | 1,092 B |

Plus 4 B in `brick_refs` and ~32 B per region amortised. Terrain is mostly 1–2 bits (rock, soil, a few Materials), so **200 B per mixed brick [estimate]** is the number used below. Most far bricks are uniform and cost only the 4 B reference; that is a bonus, not the budget. In RAM the `Brick` struct is 64 B of occupancy plus 64–512 B of indices plus palette and `Vec` overhead: about 190–380 B, so **256 B per brick [estimate]** for RAM sizing.

### 3.3 Working set versus altitude

`R_res = 1.25 × horizon(h)` (the 25% margin is Section 4.1), clipped to the visible cap. Bricks are counted at one Voxel per output pixel (r0 = 118 m). "MB" uses 200 B/brick; k = 2 doubles it for rough terrain (slopes and overhangs have about twice the flat surface, as in the [voxel-structures note](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/voxel-data-structures/docs/research/voxel-data-structures.md), §3.1).

| Altitude h | Horizon | Resident radius | Levels | Bricks (k = 1) | MB (k = 1) | MB (k = 2) |
|---|---|---|---|---|---|---|
| 2 m | 253 m | 316 m | 0–2 | 133,000 | 27 | 53 |
| 10 m | 566 m | 707 m | 0–3 | 191,000 | 38 | 76 |
| 50 m | 1.27 km | 1.58 km | 0–4 | 240,000 | 48 | 96 |
| 100 m | 1.79 km | 2.24 km | 0–5 | 229,000 | 46 | 92 |
| 200 m | 2.54 km | 3.17 km | 1–5 | 201,000 | 40 | 80 |
| 500 m | 4.03 km | 5.04 km | 3–6 | 160,000 | 32 | 64 |
| 2 km | 8.25 km | 10.3 km | 5–7 | 102,000 | 20 | 41 |
| 5 km | 13.6 km | 17.0 km | 6–8 | 72,000 | 14 | 29 |
| 10 km | 20.5 km | 25.6 km | 7–8 | 43,000 | 9 | 17 |
| 20 km | 32.2 km | 36.0 km | 8–9 | 20,000 | 4 | 8 |
| 50 km | 64.0 km | 66.0 km | 9–10 | 6,000 | 1.2 | 2.4 |
| 100 km | 115 km | 116 km | 10 | 2,200 | 0.45 | 0.9 |
| 304 km (20 R) | 320 km | 320 km | 12 | 146 | 0.03 | 0.06 |
| 784 km (50 R) | 800 km | 800 km | 13 | 37 | 0.01 | 0.01 |

**[estimate]** The peak is not at the ground and not at orbit: it is at 50–100 m, where the horizon is already 1–2 km and the LOD is still fine. Above about 20 km the visible set is a subset of the always-resident guard (levels 8–13), so the *local* rings cost nothing.

These are point values from the formula; treat them as **±30%** until the generator throughput and the real relief are measured. The count is of *surface* bricks in every azimuth, not a solid ball: the Planet's interior is the implied `CORE` (a uniform ball in `planet-gen`) and costs no resident bricks. Caves and overhangs add interior surface that the surface-area formula does not see; the k = 2 roughness factor is the allowance for them, and a cave-heavy Planet would push it toward k = 3.

At two Voxels per output pixel (r0 = 236 m) the table is 2.5–3.3× larger: about 65 MB at ground and 160 MB at the 50 m peak (k = 1), or 130–320 MB with k = 2. That is the "quality headroom" case, and it still fits a 2 GB voxel budget.

### 3.4 The always-resident coarse shell

The whole Planet at a level costs `4πR² / (0.8·2^L)²` bricks:

| Levels always resident | Bricks | MB at 200 B |
|---|---|---|
| 13 | 75 | 0.02 |
| 12–13 | 375 | 0.08 |
| 10–13 | 6,400 | 1.3 |
| **8–13** | **102,000** | **20** |
| 6–13 | 1,640,000 | 327 |

**Recommendation: levels 8–13, 102 thousand bricks, 20 MB.** It covers the whole Planet at 20 km altitude and above, including the 20–50 R orbital views, with a level to spare at each end (at 20 R the correct level is 11–12; at 50 R it is 12–13). Below 20 km the local rings cover the visible cap; the guard is the fallback behind them. Levels 6–7 (327 MB) are too big to keep always and are not needed: they are local rings when the camera is at 2–10 km.

This is the "never a hole" floor. Every ray has *some* brick to hit at every Zoom level, even on the first frame after a teleport or a cold start.

### 3.5 Generation and upload cost

`planet-gen` generates the 150 m test Planet's 2.6 million bricks in "about 20 seconds on eight cores in a release build" (`generate_planet` docstring), which is **~16,000 bricks/s/core (call it 8,000–24,000), ~130,000/s on eight cores [estimate, ±50%]**. That is the number to plan against until it is measured on the dev's CPU.

| Job | Bricks | Time on 8 cores |
|---|---|---|
| Guard, levels 8–13 (whole Planet) | 102,000 | 0.8 s |
| Spawn shell at 120 m, full resolution (k = 1) | 71,000 | 0.55 s |
| Spawn shell at 120 m (k = 2) | 141,000 | 1.1 s |
| Full ground shell, 2 m altitude, with 20 m vertical margin (k = 2) | 451,000 | 3.5 s |
| Full 50 m shell (k = 2) | 519,000 | 4.0 s |
| Zoom-in destination, ground shell (k = 2) | 451,000 | 3.5 s |

The 2 s spawn target is comfortable. The 5 s orbit target is comfortable (0.8 s from cold, no disk). A cold *zoom-in* to the ground inside 5 s is comfortable; a 1 s zoom-in is not, which is a design constraint on the Zoom controller (#58), not on residency.

Upload is not a constraint: 37 thousand bricks/s × 200 B = **7.4 MB/s** at 50 m/s, and even 4× that is small. The per-frame number at 60 Hz is ~600 bricks, 120 KB. The hard part is CPU generation and packing.

### 3.6 The tiers

| Tier | What lives there | Size (16 km Planet) | Eviction |
|---|---|---|---|
| **VRAM** | Packed `Scene` buffers per resident level: guard, local rings, prefetch margin, overflow; plus the renderer's own targets, radiance cache, G-buffer | 65–150 MB at 1 Voxel/pixel (k = 1–2); 300–600 MB at 2 Voxels/pixel; hard target ≤ 2 GB including lighting | LRU inside the distance band; never the guard, never the current frame's set |
| **RAM** | The `BrickStore` for the resident set (unpacked), per-level staging, the edit journal, the request queue, the disk page cache | 100–600 MB (2–4× the packed bytes); journal bounded by edits, not by the Planet | Same policy; the journal is never evicted, only snapshotted/pruned (#62) |
| **Disk (2 GB)** | Region-granular cache of **unedited** packed bricks; the world DB (`edited_bricks` snapshots + versions); the world map | 2 GB / 200 B = 10 M bricks = ~20,000 regions of 512 bricks (~102 KB each). The guard is 200 regions (20 MB). The live shell is ~500 regions (50 MB). So the cache holds the whole Planet at levels 6–13 (320 MB) **and** ~40 shell snapshots, or just ~40 shells | LRU by region; never a region the frame needs; edited regions are not cached at all |

The 2 GB is a *disk* budget; it is not the VRAM budget. The two should not be conflated in the catalogue.

---

## 4. The policy

### 4.1 Residency: a complete shell, not a frustum

**Admit** every brick whose level's distance band is inside

```
R_res = min(1.25 × horizon(h + h_margin + max(0, v_r)·T), visible cap, far plane)
```

with `h_margin = 20 m` near the ground (0 at altitude) and `T` the prefetch lead (§4.2). The 1.25 factor and the 20 m margin are not prefetch: they are *residency*, held at all times. They buy two things:

- **Turning**: the shell is complete in azimuth, so a 180° turn finds every brick already resident. No angular prefetch is needed for correctness.
- **Discontinuities**: a ridge that hides a valley, a low pass over a crest, a climb. The 25% radial margin means the newly revealed band is already at a coarser level, and the 20 m vertical margin pre-generates the take-off spike (Section 1).

**Retire** a brick only when it has been outside `R_res` by another 25% (i.e. beyond `1.25·R_res`) for at least 0.5 s. That is the hysteresis that stops a camera hovering on a boundary from thrashing. Because the shell is symmetric, there is no *directional* residency: the trailing edge is as far away as the leading edge, and both are retired by distance. Direction belongs to prefetch and to the victim order inside the overflow, not to what is resident.

**Cost of the margin [estimate]:** at ground, the 20 m vertical margin takes the set from 133k to 226k bricks (+19 MB at 200 B, k = 1); at 50 m it adds 19k (+4 MB). The 25% radial margin is already included in the table in §3.3.

**What may never be evicted:**

1. The guard, levels 8–13 (20 MB).
2. Any brick in the current frame's required set: the shell's bricks at their LOD, the brick under the crosshair for the edit ray, and the 3×3×3 bricks around it (so a dig never starts in air).
3. In-flight generation or upload entries; evicting a loading brick just makes the next frame request it again.
4. The chain of coarse bricks over a live Edit until it has been rebuilt and uploaded (§4.4).
5. The journal entries themselves: they are the source of truth, not a cache (ADR 0003, #62).

**When the required set exceeds the budget**, exceed the budget, up to a bounded overflow (10–20%), and only then coarsen the LOD. This is Cesium's `maximumCacheOverflowBytes` rule: a budget is a target, the fidelity bar is the hard constraint ([docs](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html)). Dropping a required brick to meet a byte target turns a budget problem into a hole.

**Per-level stores, not one `Scene`.** Today `Scene` packs every brick in the `BrickStore` into one set of buffers, and `BrickStore`'s pyramid only holds a coarse brick where a level-0 brick under it is held (`build_lod` / `remake`). Rings need the opposite: a sparse level-8 brick with no level-0 bricks under it. #48 must give each level its own sparse `Scene` (its own table, regions, refs, bricks), and #49 must fill them independently. The store's `refresh_lod` already returns the exact dirty list, `LodRefresh.changed: Vec<(level, BrickCoord)>`, which is the per-level patch input. That is the seam to build on.

### 4.2 Prefetch: a leading crescent plus a margin

Because residency is already complete, prefetch is **not** what makes turning safe. It is what makes *translation, climb and zoom* safe, and it is allowed to be late as long as the resident margin is not exhausted.

**What to request.** Predict the camera position at `t + T`:

```
p̂ = p + v·T + ½a·T²,  T = clamp(2·horizon / v, 1.0 s, 2.0 s)
```

and request the bricks in `[R_res(p), R_res(p̂)]`: a crescent ahead of the camera, not a cone. A cone/FOV restriction is only an *ordering* device (centre of screen first, via `foveatedScreenSpaceError`-style priority). If the design ever falls back to frustum-only residency for memory, the cone must be widened by `ω·T` (at 180°/s, 0.3 s of latency gives 54°), and even then it cannot be guaranteed: label that mode rejected (§7).

**Cone width, for ordering.** Use the camera frustum (a 51° horizontal half-angle at 16:9 with a 70° vertical field of view) widened by 10–15° and by `ω·T`. That covers a normal turn's next second while still refining the centre first. A narrow 20–30° cone is for a static camera; during a turn it starves the edges, which is the failure mode that pushes a team into frustum-only residency and then into holes.

**Lead time.** At 50 m/s, `T = 1 s` is 50 m, 16% of the 316 m ground radius. The crescent is ~37k bricks (7 MB), ~0.3 s of generation on eight cores. That is the buffer that absorbs a generation hiccup. `T = 2 s` doubles it to 14 MB and 0.6 s; use 2 s when the measured p99 generation latency exceeds 0.5 s.

The 37k/s is the sum over levels, and it is dominated by the fine ones:

| Level | Bricks entering per second at 50 m/s | Why |
|---|---|---|
| 0 | 18,400 | d = 118 m, brick 0.8 m |
| 1 | 9,200 | d = 236 m, brick 1.6 m |
| 2 | 4,600 | d = 472 m, brick 3.2 m |
| 3 | 2,300 | d = 944 m, brick 6.4 m |
| 4 | 1,150 | d = 1.9 km, brick 12.8 m |
| 5–13 | ~1,200 total | each level halves |
| **all** | **~37,000** | |

**[estimate]** This is `Σ_L 2·d_L·v / b_L²` with `d_L = r0·2^L` and `b_L = 0.8·2^L`, which is why the total is finite and why the fine levels set the generation budget. The k = 2 roughness case doubles it to ~74k/s; the 1.25 radius margin does not change the rate, it only changes the standing set.

**Velocity extrapolation.** Use a smoothed velocity (EMA over ~100 ms) so a jittery input does not reorder the queue every frame. Clamp `p̂` to the Planet's surface: do not predict through the ground. Include the radial component: climbing at 50 m/s from 2 m moves the horizon at 4.5 km/s (Section 1), so the *predicted altitude* matters far more than the horizontal position. That is why the 20 m vertical margin is residency and not prefetch.

**Angular velocity.** Since the shell is complete, `ω` does not create new residency. It changes the *priority* of refinement: while the view is moving, do not spend the pool refining off-centre rings; refine the centre and the leading edge, and let the margin cover the rest (`foveatedTimeDelay` in Cesium is the same idea: defer edge tiles until the camera stops, [docs](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html)). If the player spins in place at 180°/s, the shell is already there; nothing to load.

**Zoom-out.** Free. The guard is resident, the LOD gets coarser as the camera recedes, so the image can only lose detail, never gain a hole. Retire the fine rings as they leave `R_res` immediately: they are not coming back in this direction, and the budget is better spent on the next zoom-in. (Keep them in the overflow if the zoom might reverse.)

**Zoom-in.** The destination is under the crosshair. Start generating the destination shell as soon as the zoom begins, in coarse-to-fine order (Aokana's order, [§3.4](https://arxiv.org/abs/2505.02017)): guard first, then the destination's outer rings, then the near disc. The destination's full ground shell is ~226k bricks, 1.7 s on eight cores (k = 1), 3.5 s with k = 2. A 5 s zoom has room; a 1 s zoom does not, so the Zoom controller must either slow down or accept a coarse final second — a #58/#73 decision, not a residency one. Cancel the destination prefetch the moment the zoom reverses or retargets.

**Cancellation.** Every request carries `(epoch, level, coord, predicted_distance, score)`. On each frame, recompute `score`; a request whose predicted distance is now outside `1.25·R_res` is dropped lazily (drop on pop if stale), not eagerly scanned. A request that is already generating is allowed to finish if it is still within the *resident* radius; otherwise it is discarded on completion. Do not cancel an in-flight GPU upload: the cost is already paid, and the brick may be needed again after a reversal.

**Yielding to visible work.** The queue has three priorities: *needed* (missing inside the current `R_res`), *margin* (predicted to enter within `T`), *warm* (disk/return visits). In steady state *needed* is empty by construction. Give the pool a reserved worker for *needed* (or preempt the lowest worker), cap prefetch at ~50% of the cores, and never let a prefetch request delay an Edit upload. #75 owns the pool and the upload path; this note only fixes the keys and the ordering.

**Prefetch hit rate** is the metric that proves it: the fraction of bricks first used in a frame that were already resident at the start of that frame. Target ≥ 99% with the rest arriving within one frame; any *soft miss* inside `R_res` is a bug, not a tuning knob.

### 4.3 Eviction: distance picks the set, LRU picks the victim

Pure distance and pure LRU both fail:

- **Pure LRU** keeps a brick you looked at once and never need again, and evicts the near ring after a long look at the sky. It has no notion of "this brick is about to be visible".
- **Pure distance** evicts exactly the trailing ring that a reversal needs, and thrashes when the camera hovers on a boundary.

The working policy, after Cesium:

1. **Selection by distance and LOD**: the required set is everything inside `R_res` at its level.
2. **Retention margin**: keep it until `1.25·R_res` for 0.5 s (hysteresis).
3. **Victim order**: among bricks outside the required set, evict LRU first; among equals, evict the trailing side (dot product with velocity < 0) before the leading side. This is only a tie-break; it never evicts a required brick.
4. **Overflow**: allow 10–20% over budget for recently used, just-retired bricks, so a reversal hits RAM instead of regenerating. This is Cesium's `maximumCacheOverflowBytes` in spirit, and it is what makes a "fly out and come back" cheap.
5. **Pinning**: required set, guard, in-flight, live-Edit chain (all above). Edited bricks are *prioritised* for ~30 s or until the autosave (#62) and the coarse rebuild have happened, but they are not pinned forever: the journal is the source of truth, and a player can edit millions of bricks.

**Persistence before eviction.** Nothing evictable is the only copy:

- An **unedited** brick is a pure function of `(seed, GENERATOR_VERSION, level, coord)` and can be regenerated (ADR 0002: "Unedited bricks are a cache that can be regenerated from the seed").
- An **edited** brick is reconstructible from the seed plus the journal, and the journal is append-only and Planet-wide (ADR 0003: "Sending Edits Planet-wide keeps far LOD and the orbital Zoom accurate"). #62 persists `edited_bricks` as zstd snapshots with a brick version.
- The disk cache therefore stores **only unedited bricks**. An edited brick is never read from the cache; it is rebuilt from the journal, or read from the world DB. This removes the stale-entry problem at the root instead of papering over it with an epoch check.

The only ordering rule that matters: the **Edit is journaled before the brick is shown**. A crash may lose the last autosave interval (#62 accepts up to ~1 min), but it cannot leave a *stale* brick, because the journal and the brick are updated in the same frame (§4.4).

### 4.4 Edit invalidation: journal first, epoch-stamped, patch before submit

The current code already has the right shape for level 0: `sim::apply` applies the Edit and calls `store.refresh_lod`, returning `Undo::lod().changed` — the coarse bricks that read differently at each level; `World::refresh` calls `Scene::update` synchronously; `App::show` uploads before the next frame. The ring version extends this to every level and closes the worker race.

1. **Journal first.** Append the `EditOp` (24 B) to the in-memory journal and bump the global `edit_epoch`. The journal is authoritative even before the SQLite autosave.
2. **Apply at level 0** exactly as today (`sim::apply`), which also validates the prediction. Collect `changed0 = Undo::bricks()`. For a 2 m Brush this is up to 343 level-0 bricks (a 41-Voxel cube spans at most 7 bricks per edge).
3. **Walk the chain.** For each resident level `L = 1..N`, compute the parents of `changed0` (as `refresh_lod` does) and mark the 1–8 coarse bricks per level dirty. `LodRefresh.changed` already gives the exact set. For a 2 m Brush the whole chain is ~370 bricks, ~75 KB.
4. **Rebuild bottom-up.** If the level-`L−1` bricks under a dirty level-`L` brick are resident (the normal case near the camera), downsample them exactly as `refresh_lod` does. If they are not resident (a far or guard brick), rebuild from the seed at level `L` plus the journal delta. The journal delta is bounded by the Brush's footprint at that level, not by the level's Voxel volume; a Brush that is smaller than one coarse Voxel can only flip that Voxel if its solid fraction is within the Brush's Voxel count of the 50% threshold, so the guard can carry a 64 B/brick near-threshold mask and defer the rare ambiguous voxel to a background re-read. **[judgement]** The simpler alternative — refresh the affected guard bricks from the journal in the background and assert zero image difference — is acceptable while the Brush is sub-voxel at that level, which it is for every plausible Edit above level 8 on a 16 km Planet.
5. **Upload before submit.** Batch the dirty bricks into one upload and record the frame only after it is submitted. The frame that shows the Edit is the first frame after the upload; there is no intermediate frame with the old brick. This is the same ordering that already makes `World::dig` show the hole immediately.
6. **Discard stale prefetch.** A generation result carries the `edit_epoch` at which it started. On completion, if the epoch is older **and** the brick is in the dirty set for that Edit, discard and regenerate. Do not discard every in-flight brick on every Edit: Edits are rare and the dirty set is tiny; a global epoch check would throw away a frame of unrelated work for no reason.
7. **Roll back predictions the same way.** A rejected Edit calls `Undo::revert`, which returns the same per-level dirty list; steps 4–6 repeat with the inverse delta. A revert must never be a "special case" that skips the coarse levels.
8. **Count stale frames.** A frame is stale if any drawn brick's epoch is older than the global epoch *and* its content differs. By steps 5–6 that count is zero; the bench asserts it rather than trusting the argument.

**What "no visible stale frame" costs.** The synchronous part is the level-0 patch plus the 1–8 coarse bricks per level, 50–75 KB and well under 0.1 ms of GPU time; the 1.0 ms Edit budget in `bench-budgets.toml` (commented out until #64) is the ceiling. Nothing here requires a rebuild. The 1.6 s `Scene::update` rebuild is a *bug to remove* in #49/#75, not a cost to schedule: it is triggered by a changed brick outside every known region, or by garbage exceeding `spare_words(live)`. Per-level ring scenes must grow their region table instead of rebuilding, and an Edit outside the resident shell must update the journal without touching the scene at all.

**Radiance-cache interaction.** A resident-level swap (coarse → fine, or an Edit) changes the geometry the path tracer sees, so the radiance cache cells over that brick are no longer valid. The cost is cache re-warm, not a hole, but it is why hysteresis matters: every boundary oscillation re-warms cells. This belongs to #54/#55; residency should hand them the changed brick set (it already has it) and avoid repeated swaps.

---

## 5. Ranked shortlist

Ranked by the catalogue's order: frame time first, then hitches, then VRAM. **This whole family has almost no frame-time win** — it does not remove rays or passes — so it sits below culling (#72) and LOD selection (#73) in the catalogue. Its value is that it is the *precondition* for those techniques' fidelity claims: culling without a complete shell is a hole; LOD without prefetch is a pop. It ranks as a must-have for the fidelity gate even though its frame-time delta is ~0.

Confidence is my confidence in the range, not a measurement.

### R1. Complete-shell residency with per-level stores — **must-have**

- **What it buys:** zero holes at any turn rate; zero turn-triggered stalls. A frustum-only design needs a 104° region in 0.58 s at 180°/s (16× flight churn), so it shows a hole or a coarse pop; this design does neither. **[estimate, high confidence]**
- **Bytes:** 27–52 MB (k = 1) / 53–105 MB (k = 2) at 1 Voxel/pixel, peak at 50–100 m; 2.5–3.3× at 2 Voxels/pixel; guard +20 MB. **[estimate]**
- **Hitches:** removes the 0.5–4 s fill stall a frustum turn would cause; in the shell design there is none. **[estimate, high]**
- **Cost:** 2–3 agent-weeks, mostly inside #48/#49 (per-level `Scene`, sparse levels, region grow-instead-of-rebuild).
- **Risk:** VRAM at 2 Voxels/pixel (still ≤ 0.6 GB); per-level tables multiply region overhead (small); the current `Scene::update` rebuild must be replaced, not wrapped.
- **Fidelity:** none by construction. The image is identical to an infinite cache; only *when* a brick is present changes.
- **Dependencies:** #48 (rings), #43 (pyramid, per-level `LodRefresh`), #47 (for sparse coarse levels), #75 (upload/patch path).
- **Experiment:** `planet-turn-v1` (Section 8).

### R2. Always-resident guard, levels 8–13 — **must-have**

- **What it buys:** no hole at orbit, teleport, cold start, or any zoom-out; the coarse fallback that makes every other technique safe. 20 MB, 0.8 s from cold on eight cores. **[estimate, high]**
- **Bytes:** 20 MB (levels 8–13); 1.3 MB if only 10–13 are kept, which is enough for 20–50 R but leaves no margin below 20 km orbital heights.
- **Hitches:** removes the ~0.8 s cold orbit fill and the 1.7–3.5 s zoom-out fill; makes zoom-out instant.
- **Cost:** 1–2 agent-weeks, mostly #47 (direct generation at a level) plus the guard upload.
- **Risk:** a coarse guard used inside `R_res` is a visible pop; the guard must be a fallback only, never the target LOD. Editing the guard is the subtle part (§4.4 step 4).
- **Fidelity:** coarser than one pixel below ~20 km if it is ever the visible level; invisible at orbit where it is the correct level.
- **Dependencies:** #47 (hard), #48, #62 (journal).
- **Experiment:** `planet-zoom-v1` (Section 8).

### R3. Velocity and altitude prefetch with cancellation — **must-have**

- **What it buys:** no holes and no soft misses at 50 m/s horizontal; no take-off spike. 37k bricks/s horizontal, up to ~490k/s at lift-off (55k bricks in the first 0.16 s); a 1–2 s lead and a 20 m vertical margin absorb both. **[estimate, medium-high]**
- **Bytes:** the crescent is ~37k bricks (7 MB) at 1 s, 74k (15 MB) at 2 s; the vertical margin +19 MB at ground; the overflow adds 10–20%.
- **Hitches:** turns a 0.3–3.5 s fill stall into background work; keeps the main thread at ~0 ms of generation.
- **Cost:** 1–1.5 agent-weeks.
- **Risk:** prediction error on a sudden reversal (the overflow covers it); the take-off spike still needs the 20 m margin or a faster generator; the queue can starve visible work if not prioritised (#75).
- **Fidelity:** none if the hit rate is ≥ 99%; a miss inside `R_res` is a soft miss.
- **Dependencies:** R1, R2, #75.
- **Experiment:** `planet-flyover-v2` (Section 8).

### R4. Distance band + LRU + hysteresis + reuse overflow — **high-value**

- **What it buys:** fewer reloads on a back-and-forth path; the working set stays in budget without ever dropping a required brick. Cesium's exact pattern: LRU only among unselected tiles, overflow allowed to meet the fidelity target. **[judgement, medium-high]**
- **Bytes:** the overflow is the cost, 10–20% (5–20 MB).
- **Hitches:** avoids the regenerate-on-reversal hitch; a 20–50% reduction in reloaded bytes on a there-and-back path is the guess. **[estimate, low confidence — measure]**
- **Cost:** 0.5–1 agent-week.
- **Risk:** a margin that is too small thrashes; a margin that is too large wastes budget. The 1.25 factor and 0.5 s are starting points, not constants.
- **Fidelity:** none; eviction never changes a frame that has its required set.
- **Dependencies:** R1, #75.
- **Experiment:** `planet-reverse-v1` (Section 8).

### R5. Journal-first, epoch-stamped Edit invalidation — **must-have**

- **What it buys:** zero stale frames; zero 1.6 s rebuilds; ~75 KB and < 0.1 ms per Edit. **[estimate, high]**
- **Bytes:** none (the journal is bounded by Edits, not by the Planet).
- **Hitches:** removes the known M0 1.6 s scene rebuild after ~1,000 digs; removes the edge-of-shell rebuild.
- **Cost:** 0.5–1 agent-week (the level-0 path exists; the per-level chain and the epoch check are new).
- **Risk:** worker races; an Edit at the shell edge hitting an unknown region; the guard's sub-voxel case. Fuzz it with a deterministic Edit/prefetch interleaving.
- **Fidelity:** exact if the chain rule holds; this is the technique that *defines* "no stale Edit".
- **Dependencies:** R1, #62, #75.
- **Experiment:** `planet-edit-storm-v1` (Section 8).

### R6. Region-granular disk cache of unedited bricks — **conditional**

- **What it buys:** cold-start and return-visit hitches; 2 GB holds 10 M bricks / 20k regions / ~40 shells. Not needed for the core guarantee; generation is fast enough that M1a can ship without it. **[judgement]**
- **Bytes:** 2 GB disk; no VRAM.
- **Hitches:** removes 0.5–3.5 s of regeneration on a revisit; a disk read of 100 KB per region at 7–30 MB/s of demand is trivial, but the read must be async or it becomes the hitch it removes. LZ4 decompresses at ~5 GB/s on a desktop core (README benchmark: 4,970 MB/s on a 9700K), so compression is safe if it is off the critical path ([LZ4](https://github.com/lz4/lz4)).
- **Cost:** 1.5–2.5 agent-weeks.
- **Risk:** stale entries (removed by never caching edited bricks); I/O jitter; a second source of truth if the journal is not authoritative.
- **Fidelity:** none if the unedited-only rule holds; a stale cache is a violated invariant, not a tuning issue.
- **Dependencies:** R5, #62, #45 (the 16 km Planet is big enough for this to matter).
- **Experiment:** `planet-reverse-v1` (Section 8), with a restart between the two passes.

### R7. Horizon prediction from the guard — **watchlist**

- **What it buys:** removes the ridge/crest pop without the 1.25 radial margin, by reading the coarse height field ahead and prefetching the valleys about to be revealed. Saves maybe 20% of the local shell (5–20 MB) and some generation. **[judgement, low confidence]**
- **Cost:** 1–2 agent-weeks.
- **Risk:** complexity for a small win; the conservative version must not cull or skip anything. The 1.25 margin is simpler and already affordable.
- **Fidelity:** none if conservative.
- **Dependencies:** R2, #72 (horizon culling shares the geometry).
- **Experiment:** none scheduled; reuse `planet-turn-v1` at a ridge if R1/R3 leave a measured pop.

---

## 6. What changes in #49

#49 currently says: "A **background worker pool** that generates and uploads bricks for the rings in priority order (nearest and in-view first) … Edits made anywhere update every level and every ring that shows them. The scene rebuild/compaction moves off the UI thread."

The residency contract changes the priority rule, the scene structure and the Edit path, and adds the acceptance checks below:

1. **"in-view first" becomes "nearest first, all azimuths".** Priority is distance, then level, then screen centre as a tie-break. A complete shell is the turn guarantee (R1); in-view-first is a frustum policy in disguise and will show a hole on a fast turn. View direction may reorder the *refinement* of the shell and the beyond-horizon prefetch, never the shell's admission.
2. **Rings are per-level sparse `Scene`s, not one `Scene` over a `BrickStore`.** `BrickStore::build_lod` only holds a coarse brick where a level-0 brick under it is held (`remake`), which is the opposite of what a ring needs: a sparse level-8 brick with no level-0 bricks under it. Each level needs its own table, regions, refs and bricks, and its own `Scene::update`. `LodRefresh.changed` is already the exact `(level, coord)` patch list; build on that seam.
3. **Direct generation at any level (#47) is a hard dependency, not an optimisation.** Without it, the whole-Planet guard has to be downsampled from every level-0 crust brick: 2.6 M on the test Planet, about 3 × 10¹⁰ on the 16 km Planet. With it, levels 8–13 are 102 k bricks and 0.8 s. #49's "Blocked by" should say #47 first, then #48.
4. **The guard is generated before the spawn shell and is never evicted.** Startup order: guard (levels 8–13, 20 MB) → spawn shell at the entry point (120 m full resolution) → the rest of the shell. Orbit readiness is measured on the guard; it should be ~0.8 s on eight cores and does not need a disk cache (R2).
5. **Residency has a formula and a margin.** Add: `R_res = min(1.25 × horizon(h + 20 m + v_r·T), visible cap, far plane)`, complete in azimuth, with the guard as the fallback. The shell is full-sphere, not a cone; the 25% radial margin and the 20 m vertical margin are residency, not prefetch.
6. **Prefetch has keys, a lead and a cancellation rule.** `p̂ = p + v·T + ½a·T²`, `T = clamp(2·horizon/v, 1.0 s, 2.0 s)`; requests are `(epoch, level, coord, predicted distance, score)`, lazily dropped when stale; prefetch gets at most half the pool and never delays *needed* or Edit work.
7. **Edit propagation is journal-first and epoch-stamped.** Replace "Edits made anywhere update every level and every ring that shows them" with §4.4: append the Edit and bump the epoch, patch level 0, walk the chain, rebuild the 1–8 coarse bricks per level, upload before submit, discard prefetched bricks built under an older epoch. A rejected prediction uses the same path with `Undo::revert`.
8. **The disk cache stores only unedited regions.** The journal is the source of truth (ADR 0003); the cache is a seed-pure performance layer (ADR 0002). Region granularity (8³ bricks, ~100 KB) matches `Scene`'s regions and makes the index small (20k entries for 2 GB).

Acceptance checks to add or reword:

- **Keep, but sharpen:** "Keeps up with 50 m/s flight with no holes" → zero `hole_frames`, zero `soft_miss_frames` inside `R_res`, and zero `stale_frames` on a path that also includes a 360°/s turn and a 50 m/s vertical take-off. A missing brick is not enough to check; a *coarse* brick inside `R_res` is a fidelity failure too.
- **Reword:** "A crater dug at the surface is visible from orbit." On the 16 km Planet a 2 m-radius Brush is 1/8000 of the Planet's radius and is sub-pixel from orbit; the majority rule can also erase it above level 4. Test instead that (a) every Edit changes each coarse brick exactly as a full downsample would (`lod_hash` equality, already the `sim::edit` test pattern), and (b) an Edit large enough to survive the majority rule (e.g. a 100 m crater) is visible from orbit. On the 150 m test Planet the 2 m crater check still works and is the cheap CI version.
- **Keep:** "Whole-Planet coarse LOD ready within ~5 s after jumping to orbit." Expected ~0.8 s for levels 8–13; report it separately from the spawn time, and from cold (no disk) as well as warm.
- **Keep:** "No frame on the UI thread spends more than a few ms on scene work while flying and digging." Add "zero `SceneUpdate::Rebuilt`" and "`upload_bytes` per frame", because the 1.6 s rebuild is the real failure mode and a few ms of packing can hide it.
- **Add:** "Spawn ring ready within ~2 s" stays, but measured with the real generator: 120 m full resolution is 71 k bricks (k = 1), 0.55 s at the inferred 16 k bricks/s/core, 1.1 s at k = 2. The number scales linearly with the measured generator throughput.
- **Add:** the report carries `resident_bytes` (by tier), `prefetch_hit_rate`, `loads`, `evictions`, `generation_ms` p95, and the three zero-counters (holes, stale frames, soft misses) plus the rebuild count. #77 owns the schema; #49 owns the counters.
- **Not in #49:** ring radii and the LOD error rule (#73), the worker pool and upload mechanics (#75), the metric harness (#77), and the disk format (optional, R6). #49 implements the policy, not the catalogue.

## 7. Rejected and watchlist

| Idea | Why not |
|---|---|
| **Frustum-only residency** | A 180°/s turn needs a 104° region in 0.58 s, 16× the flight churn. It shows holes without a guard and visible coarse pops with one. Rejected for the shipped path; keep as a debug mode to measure how bad it is. |
| **Pure LRU, no required set** | Evicts near bricks after a long look at the sky, then reloads them on the next turn. It is the wrong objective: LRU is a *victim* policy, not a residency policy. |
| **Pure distance, no LRU** | Thrashes on a boundary and cannot use the overflow to absorb a reversal. |
| **A hard 2 GB cap that drops required bricks** | Turns a budget into a hole. Exceed the budget by a bounded overflow, then coarsen the LOD; never drop the required set. |
| **Pinning every edited brick forever** | Unbounded with play time; the journal is the source of truth, so pinning is a short-lived priority, not a lease. |
| **Caching edited bricks on disk** | The only way the cache can go stale. Store edits in the journal/DB, cache only seed-pure bricks. |
| **Slow compression on the hot path** | A zstd-19 decompress per brick is a hitch; LZ4/raw at region granularity is fine. |
| **Pre-warming the whole Planet** | 1.6 M bricks at levels 6–13 (327 MB) is affordable in VRAM but pointless: it is 100× the live set, and most of it is below the horizon. |
| **A faster generator instead of prefetch** | Helps, but cannot cover a 180°/s turn or a cold teleport; it is a complement, not a policy. |
| **Replicating terrain to fill holes** | Server-side interest management is out of scope (#71), and it does not fix the client's own generation latency. |
| **Foveated/frustum LOD reduction as a residency saving** | It changes the image outside the fovea; catalogue it as a fidelity-affecting technique (#73), not here. |
| **Watch: horizon prediction (R7)** | Plausible, but only worth it if the 1.25 margin measurably fails. |
| **Watch: guard near-threshold mask** | The exact way to propagate sub-voxel Edits to the guard; 64 B/brick (6.5 MB). Adopt only if the image-diff test catches an ambiguous voxel. |

---

## 8. Experiments on the 4090

Each is a `client bench` path plus counters. They extend #64's versioned path (e.g. `planet-flyover-v2`); #77 owns the final gate format. All run at 1280×720 internal / 1440p output. Until #45 lands, run them on the test Planet to prove the mechanism, then repeat on the 16 km Planet for the numbers.

1. **`planet-turn-v1` — the no-hole turn.** Hover at 2 m, yaw 360° at 360°/s for 3 s, then 180° at 720°/s in 0.25 s. Metric: `hole_frames` (a ray that misses or needs a level coarser than the rule inside `R_res`) and the image diff against a still reference at the same camera. **Pass:** 0 hole frames, 0 soft misses, max abs diff ≤ 1/255 on the reference's converged image. Runtime ~10 s. This is the experiment that would fail on a frustum-only design.
2. **`planet-flyover-v2` — 50 m/s flight and take-off.** 10 s at 50 m/s over the surface, then a 50 m/s vertical climb to 200 m, then a 50 m/s descent. Metric: prefetch hit rate, soft-miss frames, `resident_bytes` (max), generation queue p99, `frame_time` p99.9, hitch count > 2× budget. **Pass:** 0 holes, hit rate ≥ 99%, no hitch > 2 ms on the main thread, resident bytes ≤ the §3.3 budget × 1.2.
3. **`planet-zoom-v1` — ground↔orbit in 5 s each way.** Start at 2 m, zoom to 50 R in 5 s, hold 2 s, zoom back in 5 s. Metric: time to fill the destination shell, soft-miss frames, image diff at 1 s / 3 s / 5 s, cold-orbit readiness (guard from scratch). **Pass:** no holes; the guard ready ≤ 5 s from cold; the destination ground shell complete before arrival; no visible LOD step larger than 1 output pixel.
4. **`planet-edit-storm-v1` — 200 digs in 5 s while flying, plus a reject/rollback pair every 20 digs.** Metric: stale frames (drawn brick epoch < global epoch and content differs), edit-to-visible latency p99, `SceneUpdate::Rebuilt` count, hitch p99.9. **Pass:** 0 stale frames, 0 rebuilds, p99 ≤ 16.7 ms, and no frame over 33 ms.
5. **`planet-reverse-v1` — thrash and return.** Fly 500 m out at 50 m/s, reverse, return along the same line; repeat 5 times. Metric: bricks loaded on the return vs the first pass, evictions, hitch count. **Pass:** ≤ 20% of the first-pass load on the return with the overflow; compare against overflow = 0 to show the win.

A microbenchmark on the dev machine should pin the generator's bricks/s/core (the 16k number is from a release build on the agent's unknown CPU) before any of the pass/fail budgets above are treated as final. The 2 s spawn, 5 s orbit and 50 m/s numbers all scale linearly with it.

---

## 9. Metrics

Add to the bench report and the overlay; #77 owns the schema. The first three are the fidelity gates, not tuning knobs, and must be zero.

| Metric | Definition | Where |
|---|---|---|
| `hole_frames` | Frames with a primary ray that finds no brick at any level. | Renderer counter, per frame. |
| `stale_frames` | Frames where a drawn brick's `edit_epoch` < the global epoch and its content differs. | Scene upload path. |
| `soft_miss_frames` | Frames with a brick drawn at a coarser level than the LOD rule inside `R_res`. | LOD selection + residency. |
| `resident_bytes` | Packed bytes by tier (guard, local, prefetch, overflow) and by level, current and max. | Scene buffers. |
| `loads`, `evictions`, `evicted_bytes` | Counts and bytes per second, by tier. | Residency manager. |
| `prefetch_hits`, `prefetch_misses`, `hit_rate` | First-use bricks that were resident at frame start / all first-use. | Residency manager. |
| `generation_ms` | Time from request to brick ready, p50/p95/p99; queue depth. | Worker pool (#75). |
| `upload_bytes`, `upload_ms` | Per frame, and the main-thread cost. | Upload path (#75). |
| `frame_time` p99/p99.9/max | Extend `Stats` beyond the current mean/p95/max. | `bench/report.rs`. |
| `disk_hits`, `disk_bytes` | Region-cache hit rate and read bytes. | Disk cache (R6). |

The report schema is `SCHEMA_VERSION = 1` in `bins/client/src/bench/report.rs`; adding these is a schema bump, and the `--short` CI smoke test must still pass on lavapipe. The counters are cheap (a few atomics per frame), so they can stay on in release.

---

## 10. Open questions and doubts

- **The dev machine's core count and the real generator throughput.** The 16k bricks/s/core is inferred from the 2.6 M-brick test Planet in ~20 s on eight cores. If it is 8k/s/core, the 50 m/s budget doubles and the take-off spike needs the vertical margin plus a faster generator. Measure first.
- **The 16 km Planet's relief.** `planet_gen` is the 150 m test Planet with ±34 m relief; the full Planet (#45) may have caves, overhangs and taller relief. That changes k (the roughness factor, assumed 2) and the horizon margin. A 20 m vertical margin is plausible for 34 m relief; it may need to scale with the terrain's height field.
- **Ring centring.** This note assumes distance from the camera, complete in azimuth. If #73 centres rings on the ground point or clips them to the frustum, the working set shrinks but the turn guarantee is lost. The numbers here are the price of keeping it.
- **Whether 2 Voxels per output pixel is ever needed.** It multiplies the working set 2.5–3.3× for a sharper path-traced image. One Voxel/pixel already meets the bar; do not spend the budget without an image-diff win.
- **How exact the guard's Edit propagation must be.** The sub-voxel majority-flip case is rare but not impossible. The near-threshold mask is the exact fix; the background re-read is the cheap one. The image-diff gate decides.
- **Radiance-cache invalidation cost on a level swap.** Not measured here; it may make hysteresis more important than the byte win suggests.
- **The disk format and OS page cache.** A 2 GB cache can be a directory of 100 KB region files, one big file with an index, or a memory-mapped file. The choice affects hit latency and crash safety; not decided here.
- **Multiplayer Edits far away.** ADR 0003 broadcasts Edits Planet-wide, so a far crater can change a guard brick. The journal handles it; whether the background refresh keeps up with 64 players is untested.
- **Unverified:** no public source gives a shipped 10 cm Voxel Planet's streaming numbers; the estimates are first-principles plus the repo's own generator throughput. Cesium, clipmaps, Aokana and Nanite are the closest primary evidence, and none is a voxel path tracer at this scale.

---

## Sources

- Losasso & Hoppe, *Geometry clipmaps: terrain rendering using nested regular grids*, ACM TOG 23(3), 2004: [project page](https://hhoppe.com/proj/geomclipmap/), [paper](https://hhoppe.com/geomclipmap.pdf).
- Cesium, `Cesium3DTileset` API reference (cacheBytes, maximumCacheOverflowBytes, preloadFlightDestinations, cullRequestsWhileMoving, loadSiblings, skipLevelOfDetail, foveatedScreenSpaceError, dynamicScreenSpaceError): [docs](https://cesium.com/learn/cesiumjs/ref-doc/Cesium3DTileset.html).
- Cesium, `Cesium3DTilesetCache` source (LRU list with a selected-this-frame sentinel): [GitHub](https://github.com/CesiumGS/cesium/blob/main/packages/engine/Source/Scene/Cesium3DTilesetCache.js).
- Cesium, *Horizon Culling* (Kevin Ring, 2013): [blog](https://cesium.com/blog/2013/04/25/horizon-culling/).
- Fang, Wang & Wang, *Aokana: A GPU-Driven Voxel Rendering Framework for Open World Games* (2025): [arXiv](https://arxiv.org/abs/2505.02017), §3.4 (LOD and streaming).
- Epic Games, *Nanite Virtualized Geometry* (streaming on demand, SSD recommendation): [docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/nanite-virtualized-geometry-in-unreal-engine).
- LZ4 README (compression and decompression throughput): [GitHub](https://github.com/lz4/lz4).
- Repo: [ADR 0002](../../docs/adr/0002-brick-store-all-voxel-lod.md) (bricks, rings, unedited bricks are a cache), [ADR 0003](../../docs/adr/0003-client-generated-terrain-edit-ops.md) (Planet-wide Edits, clients generate), [`crates/render/src/scene.rs`](../../crates/render/src/scene.rs) (packed layout, `spare_words`, `SceneUpdate::Rebuilt`), [`crates/brick-store/src/store.rs`](../../crates/brick-store/src/store.rs) (`build_lod`, `refresh_lod`, `LodRefresh`), [`crates/sim/src/edit.rs`](../../crates/sim/src/edit.rs) (`apply`, `Undo::lod`), [`bins/client/src/fly.rs`](../../bins/client/src/fly.rs) (48 m/s), [`crates/planet-gen/src/lib.rs`](../../crates/planet-gen/src/lib.rs) (2.6 M test-Planet bricks, ~20 s on eight cores).
- Issues: [#47](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/47), [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48), [#49](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/49), [#62](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/62), [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64), [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71).
