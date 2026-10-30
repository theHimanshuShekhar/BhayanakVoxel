# Research: LOD selection and transition fidelity

Resolves [#73](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/73). Feeds the catalogue [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71). Researched 2026-10-31. Planet, Voxel, Edit, Zoom, Debris and Fluid follow [`CONTEXT.md`](../../CONTEXT.md); the store is [ADR 0002](../adr/0002-brick-store-all-voxel-lod.md) and the tracer is `crates/render/src/shaders/trace.wgsl` with its CPU mirror `crates/render/src/reference.rs`.

Where this note says "measured", it means measured on this repository's test Planet (R = 1,500 Voxels = 150 m, seed `0xB4A7A9AC`) with a throwaway example, `crates/planet-gen/examples/lod_error.rs` (not committed); the method and results are in [§4](#4-measured-how-far-majority-downsampling-moves-the-surface). Everything else is arithmetic (pixel sizes, switch distances, brick counts, refill rates) or a scaled estimate for the 16 km Planet, and is labelled where it matters.

## TL;DR

- **The rule is one line: a brick may be drawn at level `L` only while one level-`L` Voxel covers at most one *output* pixel.** With 1440p output and a 70° vertical field of view that is a switch distance of **`d = 102.8 · 2^L` metres**. Measured surface displacement at those switch distances is **0.27 output px mean, 0.55 at p95, 0.87 worst** (high confidence: measured). The looser rule people reach for first — one *internal* pixel per Voxel, since tracing is at 1280×720 (#51) — doubles all of that: **0.54 / 1.10 / 1.74 px** (measured). The strict rule is the default; the loose one is a 4× work saving that only the image-diff gate can approve.
- **Selection is per brick, by distance bands, with hysteresis.** Level `L` owns the bricks whose nearest point is in `[2^L·102.8 m, 2^(L+1)·102.8 m)`. That is a partition of space, so every surface point is drawn at exactly one level. **13 levels (0…12) cover ground to 50 R**; at most **6 rings** are populated at once (peak at low altitude), and at 20–50 R exactly **one**.
- **Cost is set by output resolution, not by Planet radius.** A full shell ring holds `3πf²/64 ≈ 156,000` surface bricks, ~0.93 M bricks at the 6-ring peak (**≈ 130 MB** at ~140 B a brick; the 2 GB budget is not the constraint), and the whole orbital Planet is **a few hundred bricks**. Refilling at 50 m/s costs **≈ 64,000 bricks/s ≈ 33 M Voxel evaluations/s** (medium confidence), about half the measured generator throughput.
- **Cracks are impossible; steps are not.** Every brick is a closed solid and the levels partition space, so a ray can never see *through* the seam — it sees a step of ≤ 1 coarse Voxel. Measured: **0 of 512 radial rays that hit at level 0 missed at levels 1–6**. The seam is ≤ 1 px, so it needs no stitching, no skirts and no transition cells.
- **But the transition is not invisible today, for one reason: Materials.** Majority downsampling turns the top Voxel from SOIL to ROCK on **51 % of columns at level 3, 85 % at level 4, 99 % at level 5** (measured). That is a whole-terrain colour change at a switch, orders of magnitude more visible than the 1 px of geometry. [#79](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/79) (prefer the exposed surface) is therefore **mandatory for invisibility**, not an orbit-view polish.
- **Direct generation at any level ([#47](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/47)) is the thing that cuts the residency work.** Today's pyramid is bottom-up: a coarse brick is only exact if every level-0 brick under it is held, so an orbital view of the real Planet needs the whole level-0 crust resident — **≈ 30 M bricks, ≈ 4 GB** (estimate, high confidence in the ratio). With a level-agnostic `BrickSource`, the 50 R view needs **≈ 150–300 bricks**. That is the 10⁵× win; LOD selection then only has to *choose* them.
- **Plain-language recommendation.** Give each brick a level from its distance to the camera using the 102.8 m doubling above; keep a 5 % dead band so a brick standing on a boundary does not flip; draw each level as its own `Scene`, nearest ring first, stop at the first hit; and fix the Material rule before judging any of it, because today the colour pops even when the shape does not.

## 1. What the current 8³-brick / 2× scheme fixes, and what is still free

ADR 0002 and `crates/brick-store` already decide more of the LOD scheme than the ticket implies. This matters because most "better" transition schemes are not expressible without changing the store.

| Decided | Where | What it forces |
| --- | --- | --- |
| Bricks are 8³ Voxels; a level-`L` brick is 8 Voxels of `2^L` level-0 Voxels | `coord.rs`, `BRICK_SIZE` | Level granularity is exactly **2×**. There is no 1.5× or 4/3 ring ratio; a level's Voxels are always twice the previous level's. |
| A coarse Voxel is solid when ≥ 4 of its 8 children are solid; Material by majority (today) | `lod.rs::downsample` | The coarse surface is **not** conservative in occupancy: it can sit up to ~1 coarse Voxel *above* or below the fine surface (§4). Thin *sheets* survive and thicken; thin *rods* and sub-Voxel caves vanish. |
| Levels are powers of two and a brick is the unit of selection | `coord.rs::children` | A seam is a **brick-aligned staircase**, 8 Voxels = 8 px per step at the switch distance. A surface point's level can only change at a brick boundary, never smoothly. |
| One `Scene` = one level (own `table`, `regions`, `brick_refs`, `bricks`) | `scene.rs` header comment | Mixing levels in one ray means either 13 `Scene`s traced near-to-far (§5.2 shows they partition space) or a new per-brick level field. |
| `Hit.voxel` and all `VoxelCoord`s inside a level are in **that level's units** | `reference.rs`, `store.rs::brick_at` | A cross-ring comparison must scale `t` and `voxel` by `2^L`; the `voxel_tint` hash and the `FAR_BLEND` hack in `primary.wgsl` assume one level and must go (see §1.1). |
| 24 DDA steps per brick, 8³ bricks per region, regions 64³ Voxels | `trace.wgsl` | Cost per ray is proportional to **bricks crossed**, not Voxels crossed. At 1 px per Voxel a brick covers 8×8 px, so a ray crosses O(screen/8) bricks. That is the property LOD buys: it keeps a brick's *angular* size constant, so ray cost stops growing with distance. |

**Free to choose** — and this ticket is about these:

- the switch distance (a bias multiplier on 102.8 m, §2.4);
- ring centring (camera, ground point, or view cone) and the resident subset;
- hysteresis, and whether any transition blending exists at all;
- whether the top of the pyramid is bricks or something else;
- how Edits are routed to the rings, and how a ring re-centres;
- the Material rule (being changed by #79 — but the solidity rule stays).

### 1.1 Two hacks that assume there is no LOD

`primary.wgsl` blends the hit normal towards the radial direction from 100 to 1,500 Voxels (`FAR_BLEND_START/END`), and multiplies albedo by a per-Voxel hash (`voxel_tint`), both blended out at the same range. The comment says why: *"Until coarser levels of detail exist, blend towards the direction away from the Planet's centre as the distance grows."* Both are compensation for aliased Voxel normals and are **fidelity losses that LOD replaces**. They must be removed when #48 lands, or the seam between two levels becomes a visible texture change: the same surface at levels 3 and 4 hashes different Voxel coordinates, so `voxel_tint` would draw a different pattern on each side, and `FAR_BLEND` would flatten the near ring but not the far one at the same distance. This is a concrete acceptance item for #48/PR #69.

## 2. The error metric

### 2.1 Screen-space size of a Voxel

Projected size of a length `v` perpendicular to the view at depth `z` (distance along the optical axis), for a vertical field of view `θ` and image height `H`:

```
pixels = v · f / z,      f = (H/2) / tan(θ/2)
```

At `θ = 70°` (the camera's `fov_y`, `camera.rs`):

| Image | Height | `f` (px/rad) | 1 px subtends | `v` at 1 px at 1 km |
| --- | --- | --- | --- | --- |
| internal 1280×720 (#51) | 720 | **514.1** | 1.95 mrad | 1.95 m |
| output 2560×1440 | 1440 | **1028.3** | 0.97 mrad | 0.97 m |

In solid-angle terms the same relation is `Ω_voxel = (v/d)²` against `Ω_pixel ≈ 1/f²` steradians, so the number of pixels a Voxel covers is `Ω_voxel / Ω_pixel = (v·f/d)²` — which is why the rule below is about linear size and not area: a Voxel's *area* grows with the square.

So one output pixel is exactly half an internal pixel in angle: the upscaler interpolates between traced samples but **cannot invent geometry**. Any surface feature finer than one internal pixel is already below the tracer's Nyquist limit; anything displaced by more than one *output* pixel is a visible difference in the upscaled image.

Two corrections matter and both go the safe way if you use the nearest point of a brick:

- **Depth is `z`, not `d`.** A pixel at the edge of the frame sees a Voxel at distance `d` through a shallower depth `z = d·cos α`, so its Voxel is `1/cos α` larger on screen: up to 1.44× at the corner of a 70° frame. Rule of thumb: use `z_min` (the minimum depth over the brick) rather than `d`.
- **Silhouettes are the worst case, not the best.** A height error `Δh` moves the image by `Δh·f·sin β / d`, where `β` is the angle between the surface normal and the view ray. Face-on (`β = 0`) it only changes depth — invisible in position. At the limb (`β = 90°`) it changes the silhouette by the full `Δh·f/d`. So the gate has to look at **limbs and ring boundaries on slopes**, where the metric's worst case is realised, not at a flat view from above.

### 2.2 What "within a pixel" has to bound

The LOD does not shrink a Voxel by rounding it; it **replaces the surface with a coarser one**, and majority downsampling moves that surface. Define the error as the **displacement of the visible surface**, in coarse Voxels, and require

```
displacement_in_coarse_Voxels × (pixels per Voxel) ≤ 1 output pixel
```

This is the metric to test, not the Voxel's size. It is also why the "1 Voxel ≈ 1 px" folklore works: the two are within a factor of ~3 (§4). This distinction matters for #47, where the natural acceptance criterion is "fraction of differing Voxels", a metric that can be 50 % with a sub-pixel displacement (a one-Voxel phase shift of a whole surface) or 1 % with a visible silhouette speck.

### 2.3 Measured displacement (test Planet, seed `0xB4A7A9AC`, majority downsampling)

Radial rays from outside the Planet, 512 directions over the sphere, first solid along the ray at level 0 against the same at level `L`; step 0.02 coarse Voxels (0.05 level-0 Voxels on the fine side), so quantisation is negligible:

| Level | mean \|Δt\| | p95 | max | rays that hit fine and missed coarse |
| --- | --- | --- | --- | --- |
| 1 | 0.23 | 0.55 | 0.87 | 0 / 512 |
| 2 | 0.27 | 0.39 | 0.73 | 0 / 512 |
| 3 | 0.27 | 0.35 | 0.66 | 0 / 512 |
| 4 | 0.29 | 0.34 | 0.76 | 0 / 512 |
| 5 | 0.28 | 0.39 | 0.67 | 0 / 512 |
| 6 | 0.27 | 0.44 | 0.70 | 0 / 512 |

Axis-aligned columns at the +y pole (no slant error at all, 1,024 columns), giving the clean distribution for near-horizontal ground:

| Level | mean \|Δh\| | p95 | max | fraction ≥ 0.5 | fraction ≥ 1.0 |
| --- | --- | --- | --- | --- | --- |
| 1 | 0.26 | 0.00 | 0.50 | 0.52 | 0.00 |
| 2 | 0.31 | 0.25 | 0.25 | 0.37 | 0.00 |
| 3 | 0.35 | 0.13 | 0.25 | 0.37 | 0.00 |
| 4 | 0.37 | 0.13 | 0.31 | 0.37 | 0.00 |
| 5 | 0.37 | 0.16 | 0.34 | 0.36 | 0.00 |
| 6 | 0.41 | 0.13 | 0.27 | 0.40 | 0.00 |

Three useful facts come out of this:

1. **The error does not grow with the level.** It is ~0.3 coarse Voxels at every level. A coarser Voxel is *bigger in the world*, but it is also drawn *further away*, so the error in pixels is what stays constant — which is exactly what a distance-based rule is designed to achieve.
2. **The bias is one-sided: the coarse surface sits slightly above the fine one** (signed mean −0.23 to −0.38 coarse Voxels). Majority-of-8 inflates a rough surface; it does not erode it. That is the benign direction: an inflated surface cannot open a see-through gap at a seam.
3. **Worst case ≈ 1 coarse Voxel**, not 0.5. A smooth flat case (the pole) tops out at 0.5, but a tilted surface can put a coarse Voxel solid with a diagonal half of its children, and the radial-rays max reaches 0.87. Budget for 1.

### 2.4 The rule and its switch table

Take the strict rule — one level-`L` Voxel ≤ 1 output pixel:

```
d_switch(L, L+1) = 2^(L+1) · f_out · v_0 = 205.7 · 2^L m      (v_0 = 0.1 m)
```

Level `L` is used for distance `[205.7·2^(L-1), 205.7·2^L)`, i.e. its Voxel spans **1.00 output px at the inner edge and 0.50 at the outer edge**; the next level takes over exactly where this one's Voxel would reach 1 px.

| Level | Voxel | Used from | to | Voxel at inner edge | at outer edge | ≈ test-Planet level |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 0.1 m | 0 | 206 m | 1.0 px | 0.5 px | 0 |
| 1 | 0.2 m | 206 m | 411 m | 1.0 | 0.5 | 1 |
| 2 | 0.4 m | 411 m | 823 m | 1.0 | 0.5 | 2 |
| 3 | 0.8 m | 823 m | 1.65 km | 1.0 | 0.5 | 3 |
| 4 | 1.6 m | 1.65 | 3.29 km | 1.0 | 0.5 | 4 |
| 5 | 3.2 m | 3.29 | 6.58 km | 1.0 | 0.5 | 5 |
| 6 | 6.4 m | 6.58 | 13.2 km | 1.0 | 0.5 | 6 |
| 7 | 12.8 m | 13.2 | 26.3 km | 1.0 | 0.5 | 7 |
| 8 | 25.6 m | 26.3 | 52.6 km | 1.0 | 0.5 | 8 |
| 9 | 51.2 m | 52.6 | 105 km | 1.0 | 0.5 | 9 |
| 10 | 102 m | 105 | 211 km | 1.0 | 0.5 | 10 |
| 11 | 205 m | 211 | 421 km | 1.0 | 0.5 | 11 |
| 12 | 410 m | 421 | 842 km | 1.0 | 0.5 | 12 |
| 13 | 819 m | 842 km | 1.68 Mm | 1.0 | 0.5 | 13 |

The last column is where the *same* Voxel-to-Planet ratio sits in the test Planet's own pyramid: the real Planet is 106.7× the test Planet's radius, `log2 = 6.74` levels up. Everything measured at test level `L` applies to real level `L + 6.7` (high confidence; it is the same geometry at a different scale, and the test Planet's terrain is a scaled-down stand-in for relief, not for roughness).

Applying the measured displacement at the *inner* edge of each band (the worst point):

| Rule | Voxel per px | Mean | p95 | Max | Relative bricks |
| --- | --- | --- | --- | --- | --- |
| **Strict (recommended default)** | 1 output px | 0.27 px | 0.55 px | 0.87 px | 1× |
| Loose | 1 internal px = 2 output px | 0.54 px | 1.10 px | 1.74 px | ¼× |

Both are "about a pixel" at p95; only the strict rule is invisible by construction at the worst pixel. The repo's own image-diff tolerance already allows ~0.5 % of pixels to be far off (`PRIMARY` in `tests/golden.rs`), so E1 should be judged against that, not against a hand-made threshold. The loose rule is a real option — a 4× cut in resident bricks, generation rate and VRAM — and the experiment in §9 decides whether the gate tolerates it. Do not make it the default before the gate says yes.

**A bias knob is therefore the right API**: one float, "output pixels per Voxel", default 1.0. All switch distances scale by it and the 4×-per-doubling cost follows automatically.

### 2.5 What the metric does not bound

- **Materials.** A Material layer thinner than a coarse Voxel is outvoted (§5.4). Colour is not a displacement and the metric above cannot see it.
- **Sub-pixel casters.** A ridge seen edge-on can be sub-pixel in width yet cast a shadow tens of pixels long. This renderer's sun has a half-angle of 0.04 rad (`sun.rs`, "a little over 2°"), so a caster's shadow is diluted over a penumbra of `0.04 × distance` — at 300 m that is a 12 m penumbra, ~40 px. A caster 1 px wide therefore dims a 40-px-wide region by ~2.5 %: below an 8-bit diff threshold. So the soft sun, plus the fact that a shadow's *width* is the caster's width, means the 1-px primary rule also bounds the shadow error (medium confidence; worth the E3 experiment with a low sun).
- **Occlusion and GI.** Secondary rays sample the same coarse surface, so an occluder's *edge* is off by the same ≤ 1 px; the shadow it casts on a nearer receiver is off by less (scaled by the distance ratio). Fine.
- **The atmosphere and the sky.** Separate pass (#53), unaffected by LOD.

## 3. Ring geometry

### 3.1 How many levels, and how wide

The bands in §2.4 are the rings. **Level `L` owns `[2^L d_0, 2^(L+1) d_0)` with `d_0 = 102.8 m`** — ratio exactly 2, which the store forces anyway. That ratio is also why the working set per ring is constant: as the radius doubles the surface area goes up 4×, and so does the face area of a brick (its Voxels double in size), so the brick count per ring is unchanged. This is the same property that makes geometry clipmaps cheap ([Losasso & Hoppe 2004](https://hhoppe.com/proj/geomclipmap/): "a set of nested regular grids centered about the viewer … incrementally refilled").

**13 levels (0…12) cover the whole documented Zoom range; level 13 is headroom.** A 50 R orbit is 784 km above the surface (R = 16 km, so the centre is 800 km away), and the surface is then 784–800 km away — inside level 12's band (421–842 km). Level 12's Voxel is 410 m, which subtends 0.52 px at 800 km. The store's comment that the Planet "needs about 19 or 20" levels is about a *single Voxel spanning the disc* (2R/0.1 = 320,000 = 2^18.3), not about pixels: the deepest level a ring ever needs is 12. Levels 13–20 are storage/save concepts, not render levels.

### 3.2 Camera-centred or the ground point beneath?

**Recommendation: centred on the camera, as spherical shells of slant distance.** The reasons, in order:

1. **The metric is a function of distance from the camera**, by definition. Centring anywhere else means the ring radius is not the quantity the error rule uses.
2. **A shell works at every altitude and in every direction.** A ground-point-centred set is a heightfield-clipmap idea: it assumes the terrain is a single-valued surface seen from above. At 50 R the ground point is 800 km away and the camera is outside every ring; the near shells would be empty space and the far ones would not contain the Planet. The shell degenerates gracefully to one populated ring at orbit.
3. **Secondary rays go everywhere.** GI and shadow rays are not confined to the view frustum; a shell is isotropic, so "everything within `d`" is exactly the set a ray of length `d` can reach.

The cost of a shell is that on the ground roughly half of it is inside the Planet (no bricks: the core is uniform and not held) and ~¾ of the surface annulus is behind or beside the camera. Keeping the *whole* shell resident is ~4× the bricks the screen needs; keeping only the view cone + a turn margin is #74's decision, and it trades VRAM and generation rate against a hitch when the player spins round.

### 3.3 Working set: bounded by pixels, not by radius

Per ring, for a full annulus on flat ground (`Δarea = 3πr²`, brick face `(8v)²`, `r = 2fv`):

```
N_ring = 3π f² / 64
```

| Rule | `f` | Bricks per full ring | 6 rings (peak) | VRAM at ~140 B | Whole Planet at 20 R |
| --- | --- | --- | --- | --- | --- |
| Strict | 1028 | **155,700** | 0.93 M | **130 MB** | ~600–1,200 bricks |
| Loose | 514 | 38,900 | 0.23 M | 33 MB | ~150–300 (one level coarser) |

The "6 rings" peak comes from the next table. The number of *populated* rings is `log2(far/near)`, capped by the near clamp and by the horizon:

| Height above the surface | Near | Far, flat horizon | Far, 1 km mountains | Rings populated (flat) | (with mountains) | Planet disc |
| --- | --- | --- | --- | --- | --- | --- |
| 2 m | 2 m | 253 m | 5.8 km | 0–1 (2) | 0–5 (6) | — |
| 10 m | 10 m | 566 m | 5.8 km | 0–2 (3) | 0–5 (6) | — |
| 100 m | 100 m | 1.8 km | 6.0 km | 0–4 (5) | 0–5 (6) | — |
| 1 km | 1 km | 5.7 km | 8.2 km | 3–5 (3) | 3–6 (4) | — |
| 10 km | 10 km | 20 km | 22 km | 6–7 (2) | 6–7 (2) | 1,360 px |
| 100 km | 100 km | 115 km | 116 km | 9–10 (2) | 9–10 (2) | 285 px |
| 304 km (20 R from the centre) | 304 km | 320 km | 321 km | 11 (1) | 11 (1) | 98 px |
| 784 km (50 R from the centre) | 784 km | 800 km | 801 km | 12 (1) | 12 (1) | 40 px |

Notes:

- **The peak is at low altitude, not at orbit.** At most ~6 rings are populated, around 2–100 m above the ground, because that is where the surface spans the most octaves of distance. At 20–50 R exactly one ring intersects the Planet.
- **The mountain horizon, not the geometric one, sets the top ring on the ground.** Terrain up to `H` high is visible from `sqrt(2R(a+H))` away, so a 1 km ridge keeps the far ring alive at 6 km even from eye height. That is +2–3 levels over the flat horizon. Without it, standing on the ground would need only levels 0–1, which is wrong: the horizon ridge is on screen. (The visibility ticket #72 decides whether that band is culled; if it is, the top rings shrink.)
- **Two extra effects push the count up, and both are bounded**: a ring near the horizon is seen at a grazing angle, so its surface is foreshortened and the distance metric spends more bricks than a screen-space metric would (`1/sin β`); and the store also holds the few Voxel layers below the surface, ~2–5× for the test Planet's 38-Voxel shell.

### 3.4 At 20–50 R the Planet is tiny on screen

At 20 R the whole Planet is a **98-px disc**; at 50 R, **40 px**. That is the entire LOD problem for the orbital view: one ring, a few hundred bricks, ~150 k rays. The expensive part of the Zoom is not orbit — it is the *pass through* 1–4 km altitude, where 3–5 rings are populated, and the 50 m/s flight at 100 m, where 5–6 are. Any "orbit is slow" theory should be checked against this table first.

A corollary for the benchmark: **an orbital image of the real Planet cannot be diffed against brute force**, because brute force means the whole level-0 crust (~30 M bricks, ~4 GB). The image gate needs the test Planet (2.6 M bricks, which does fit) or a smaller radius for the level-by-level comparisons, and the real Planet for timing. This matters for #77.

### 3.5 Refill rate at 50 m/s (the binding constraint)

As the camera moves at `v`, each ring's outer boundary sweeps a strip of area `2rv` per second, refilled with bricks of face `(8v_L)²`:

| Ring | Outer radius | Bricks/s |
| --- | --- | --- |
| 0 | 206 m | 32,100 |
| 1 | 411 m | 16,100 |
| 2 | 823 m | 8,000 |
| 3 | 1.65 km | 4,000 |
| total, rings 0–11 | | **≈ 64,000 bricks/s** (≈ 33 M Voxel evaluations/s) |

The existing generator produces 2.63 M bricks in ~20 s on 8 cores (the `bench` docs' own figure) — **≈ 130,000 bricks/s**. So 50 m/s at the strict rule uses **~50 % of the current generator throughput** on the leading edge alone, before any in-view margin, Debris or Fluids (medium confidence: the brick/s extrapolates the measured 20 s, but the model treats the surface as flat and one brick thick, whereas rough terrain needs several layers and the real Planet's per-brick generation will be more expensive than the test Planet's). This is why the strict-vs-loose choice matters for more than VRAM: it is a 4× cut in generation rate. It is also the first thing a per-ring *bias* could buy: strict near the camera, loose beyond ~10 km.

## 4. Measured: how far majority downsampling moves the surface

The measurement, run as `cargo run --release -p planet-gen --example lod_error` on `generate_planet(0xB4A7A9AC)` with `build_lod(10)`:

1. It builds the pyramid and counts bricks per level.
2. It casts 512 Fibonacci-sphere radial rays inward, at each level, and compares the first-solid distance with level 0.
3. It compares axis-aligned columns at the +y pole (no slant error).
4. It compares a "direct generation" proxy — a coarse Voxel solid iff the surface field at its centre is solid — with the majority downsample, Voxel by Voxel.

Bricks per level, which is what §3.3 scales:

| Test level | Non-air bricks | Real-Planet equivalent |
| --- | --- | --- |
| 0 | 2,633,386 | 6.7 |
| 1 | 406,752 | 7.7 |
| 2 | 70,085 | 8.7 |
| 3 | 13,604 | 9.7 |
| 4 | 2,946 | 10.7 |
| 5 | 655 | 11.7 |
| 6 | 150 | 12.7 |
| 7 | 32 | 13.7 |
| 8 | 8 | 14.7 |

The level-0 count is the whole *held* store, i.e. the surface shell (the test Planet holds every non-air brick, and leaves the uniform core to `BrickSource`); it is **not** what a ring needs. The last column is the point: the 150-brick level-6 planet is the real Planet at level 12.7 — a few hundred bricks for the entire orbital view, *provided the coarse bricks can be generated without their level-0 children*.

Why the error is bounded by ~1 coarse Voxel, in one paragraph (derivation, high confidence): in a 2×2×2 cell a coarse Voxel is solid when 4 children are. For a plane crossing the cell, the count is 0, 4 or 8 solid depending on where the plane lies, so the coarse Voxel flips to solid as soon as the plane is above the cell's lower sub-layer — and the rendered surface jumps to the top face of the coarse Voxel. The worst case is therefore one whole coarse Voxel; the average is ~0.3 because the plane is more often near the middle. Recursing (level `L` = majority of majorities) rounds to half-voxel offsets, which is why the flat-ground max is 0.5 and the rough-ground max is ~0.87.

## 5. Transitions

### 5.1 Cracks

**A ray tracer over closed Voxel bricks cannot open a see-through crack at a level boundary.** The argument, which is why this ticket needs no stitching machinery:

1. Every level is a partition of space into bricks; each brick is a *closed solid* — its surface is the boundary between its solid and non-solid Voxels, and it has no "open" faces, because the tracer treats a missing neighbour as air only at the brick's own boundary and the coarse brick is itself a solid volume.
2. At a point `p`, exactly one level is selected (§2.4, §5.2), so the ray is only ever stepped through the chosen level's bricks; there is no pair of half-open meshes to leave a gap between.
3. The two levels' surfaces differ by ≤ 1 coarse Voxel and the coarse one is biased *outward* (§2.3), so at the seam the coarse surface is at worst a step of one Voxel above the fine one. The fine brick's side face is solid, so a ray from the fine side hits it rather than escaping.

Measured: **0 of 512 radial rays** that hit at level 0 missed at levels 1–6, and the same for the column test. The thing to expect at a seam is a **step**, not a hole; a step of ≤ 1 px is exactly what the error rule already permits. This is a genuine advantage over meshed voxel LOD, where Transvoxel-style transition cells exist precisely because neighbouring meshes at different resolutions do not tile space ([Lengyel, Transvoxel](https://transvoxel.org/)).

The one residual risk is the *air* side: a cave whose opening is smaller than a coarse Voxel closes at that level. That is correct behaviour at the distance the level is used (a 1-Voxel-wide cave at the switch distance is ~1 px), and it is the same rule that hides a 1-px cave in the reference image. But it means the sequence "walk up to a small cave" must re-open it as you approach, which is a *transition* event the dither/hysteresis analysis applies to.

### 5.2 The partition, exactly

Select a level-`L` brick when its **nearest** distance `d_min` satisfies `2^(L-1)·205.7 ≤ d_min < 2^L·205.7` (with `L = 0` below the first). Every level's bricks tile space, so for a surface point `p` at distance `d` the bricks that contain `p` all have their reference point within half a brick diagonal of `d`, i.e. within `6.9·v_L = 6.9 d/(2f) = 0.34 %` of `d`. So all the bricks containing `p` are within 0.34 % of the same distance, and the bands (100 % wide) put them on the same side of a boundary except within 0.34 % of one. **Result: every point is covered exactly once, apart from a sliver ≤ 0.34 % of the band where two levels may both claim it; the renderer takes the nearest hit, so the sliver is a sub-pixel tie, not a hole.** The brick's *centre* is as good as its nearest point for the bricks that contain `p`; nearest point is still the right choice because it is what stops a large brick that only grazes the near band from being classified by its far-away centre.

That also answers "which distance": use the brick's AABB nearest point, not its centre. It is a few extra flops in the selector and it is the difference between a guarantee and an approximation.

### 5.3 Popping, and why no blending is needed

At a band boundary the image changes from level `L` to `L+1`, i.e. the surface jumps by ≤ 1 coarse Voxel = ≤ 1 output px, and the *voxel pattern* doubles in size. The options:

| Scheme | What it does | Cost | Fidelity | Verdict |
| --- | --- | --- | --- | --- |
| **Hard switch + hysteresis** | one level at a time; switch out at 1.05× the distance, back in at 0.95× | ~0 (a float compare) | ≤ 1 px step, once per crossing; TAAU's history clamp blends the two frames | **Recommended** |
| Dither two levels | per-pixel hash picks fine or coarse by the fraction through the band; both levels resident | 2× rays and 2× bricks in the band; temporal noise for TAAU | no pop, but noisier | Watchlist: only worth it if you push the bias to 2–4 px |
| Cross-fade | blend the two images | 2× tracing, and blending two depth-discontinuous images | ghosting on silhouettes | Rejected: worse than a 1-px step, at 2× the cost |
| Geomorph | move the surface continuously | not expressible: there are no vertices, and interpolating occupancy changes topology | — | Rejected |
| Keep the finer level in a band around every boundary | no switch ever happens where it is visible | +20–40 % bricks (the finer level covers a 10 % larger radius) | strictly better than hysteresis alone | Conditional: cheap insurance, and it also removes the *second* switch when the camera bobs |

Two details that make the hard switch behave:

- **Hysteresis.** Without it, a camera hovering at exactly the switch distance flips a brick every frame (a 50 m/s camera covers the dead band in 0.2 s at ring 0). ±5 % is ample and costs nothing.
- **The step is at the *same* place every frame.** The boundary is a brick-aligned staircase whose steps are 8 px at the switch distance; the *distance* at which a brick switches varies across its face by ±0.34 % (§5.2). So the step does not crawl across the screen; it appears and disappears with the brick. This is much easier for TAAU than a smooth, sliding boundary.

### 5.4 The Material problem (the one that breaks invisibility today)

Majority Material voting loses a thin surface layer. The test Planet has 4 Voxels of SOIL over ROCK, i.e. exactly one level-2 Voxel and half a level-3 Voxel. At the +y pole, the Material of the topmost solid Voxel, fine against coarse:

| Level | Columns where the top Material differs | … coarse ROCK where fine was SOIL |
| --- | --- | --- |
| 2 | 0.6 % | 0.6 % |
| 3 | **50.7 %** | **50.7 %** |
| 4 | **85.0 %** | **85.0 %** |
| 5 | **98.9 %** | **98.9 %** |
| 6 | **100 %** | **100 %** |

This is the same effect the dev saw in the #69 orbit bench ("grey and patchy"), and it is what [#79](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/79) fixes by letting exposed solid children vote first. Two consequences for *this* ticket:

1. **No transition scheme can be invisible while the Material rule is majority.** A 50–100 % colour change per switch dwarfs a 1-px silhouette move. #79 must land *before* any image-diff gate on ring boundaries is meaningful.
2. **#79 has to be consistent across levels, not just at the top.** If level 3 shows SOIL but level 4 shows ROCK at the same place, the ring boundary is a colour seam. A surface-first rule that reads across brick borders (as #79 requires) gives the same answer at every level for the same column, so the seam disappears. The acceptance test for #79 should include *two adjacent levels*, not only the surface at one level.
3. It also implies the *biome/vegetation* surface (later grass, snow, sand) is a sub-Voxel-thin layer and must be treated the same way; otherwise every LOD switch changes the biome colour. This is a constraint on #46/#45.

### 5.5 Edits and transitions

- An Edit changes level-0 bricks; `BrickStore::refresh_lod` remakes the ancestors and returns `changed: Vec<(level, coord)>`. Route each of those to the ring `Scene` for that level *if the brick is resident*; the update is a region patch, which `Scene::update` already does. A level that is not resident needs nothing.
- **The crater fades with level exactly as the rule says it should.** A 1 m (10 Voxel) dig changes one Voxel in a level-4 cell of 4,096, so no level-4 brick changes; the crater is visible only while it is ≥ ~1 px, i.e. up to the level whose Voxel is ~its size. That is fidelity-by-construction, but it means the spec's "see the crater from orbit" is only testable with a *large* blast: at 20 R, 1 px ≈ 310 m, so a 1 m crater is 1/310 px and brute force would not show it either. The acceptance criterion in #41 needs a crater of ~1 km or an orbital camera much closer. (Flagged for the synthesis ticket.)
- The recursion cost is why #47 matters here too: `refresh_lod` remakes each ancestor from its 8 children via `brick_at`, which reads the source for unheld children. If the source generates directly at that level, one Edit costs 8 direct generations per level; if the source is only `Interior` (today), every fine child must be held, which is what forces the whole crust resident.

### 5.6 TAAU

TAAU (#51) is a jittered 2× upscale with history clamping. It helps in two ways and does not help in a third:

- A ≤ 1-px geometric step is a change of one *internal* sample's worth of history, which the clamp absorbs; a hard switch is therefore much less visible than it would be without TAAU.
- It cannot recover geometry coarser than the internal grid, which is why the LOD rule must be sized to output pixels, not internal ones: the upscaler interpolates *samples*, not *surfaces*.
- It does not fix a Material pop: a whole-region colour change passes any reasonable history clamp and stays.

## 6. Direct generation at any level (#47)

The tolerance question: how much may a directly generated coarse brick differ from the downsample of its fine children and still be invisible?

**The criterion should be surface displacement, not Voxel count.** Measured disagreement between the downsample and the crudest direct generator (the surface field sampled at the coarse Voxel's centre):

| Level | Voxels differing | Solid Voxels differing |
| --- | --- | --- |
| 1 | 0.7 % | 0.6 % |
| 2 | 1.8 % | 1.6 % |
| 3 | 2.5 % | 2.6 % |
| 4 | 3.0 % | 3.4 % |
| 5 | 3.5 % | 4.1 % |

A centre-sampled generator — the *worst* reasonable direct generator — disagrees on only 0.7–3.5 % of Voxels. That is a useful bound: it means the acceptance criterion "fraction of differing Voxels, per level" will be easy to meet, and therefore that it is the wrong thing to gate on. **The displacement of a direct generator against the downsample was not measured here** — only the Voxel fraction was — and that is exactly the gap the Voxel count cannot close. What matters is:

> A coarse brick is indistinguishable if its visible surface is within **~0.5 coarse Voxels** of the downsample's (p95), because at the inner edge of that level's band one coarse Voxel is one output pixel (§2.4). A Voxel-count difference up to ~5 % is consistent with that; a larger count is not automatically bad, and a smaller one is not automatically good (a rigid phase shift of a whole surface is 50 % of Voxels and sub-pixel).

**How to test it** (concrete, deterministic, no GPU):

1. Extend `planet-gen`'s tests with a per-level comparison over a fixed set of bricks: for each level 1…6, generate a brick directly and downsample its held children, then compare the two *surfaces* — for each of a fixed set of columns and radial directions, the distance from the brick to the first solid Voxel in each. Assert (a) p95 radial displacement ≤ 0.5 coarse Voxels, and (b) the Voxel-fraction difference ≤ 5 % for levels 1–5. Deterministic and fast (thousands of bricks).
2. Reuse the `lod_error` harness for the distribution (it already measures both).
3. Image gate: on the test Planet, render the same camera twice — once with the direct generator, once with the downsample — at the level's switch distance, and require the diff to be within the same tolerance as the E1 ring gate. That catches the thing the Voxel count cannot: a small number of differing Voxels on a silhouette.
4. Keep #47's "downsampled edited content wins" test; it is orthogonal and already specified.

One design note for #47 that this ticket depends on: the `BrickSource` trait already takes `level`, so a generated source is a drop-in. But `BrickStore::remake` calls `brick_at(level-1, child)` for all 8 children, so a *chain* of direct generations is 8 per level, not one. That is fine (9× a direct brick) but it is worth measuring: at orbit, 150 bricks × 9 × 512 Voxels ≈ 700 k Voxel evaluations, i.e. milliseconds. The win is not that direct generation is fast per brick; it is that it removes the requirement to hold the 30 M fine bricks underneath.

## 7. Ranked shortlist

Win ranges are estimates (no GPU here); confidence is stated per row. "Cost" is agent-weeks for one agent.

| # | Technique | Tier | Win | Cost | Risk | Fidelity | Depends on |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Per-brick distance bands, strict rule, 13 levels | **must** | resident bricks 30 M → ~1 M (−97 %); primary 3–10× on rays that graze distant terrain | 1 | low | ≤ 0.87 px measured | #48 |
| 2 | Direct generation at every level as `BrickSource` | **must** | orbit residency 30 M → ~150 bricks; generation 10⁵× less at orbit | 1–2 (it is #47) | medium (determinism, Edits) | 0.7–3.5 % Voxels, sub-Voxel | #47, #43 |
| 3 | Surface-first Materials (#79) | **must** | removes a 50–100 % top-Voxel colour flip at L ≥ 3 | 0.5 | low | colour-correct, no geometry change | #79 |
| 4 | Hysteresis ±5 % on every band | high | 0; removes per-frame flip-flop at a boundary | 0.1 | none | none | 1 |
| 5 | Ring handoff in the tracer (13 `Scene`s, near to far, first hit) | high | enables 1; +5–15 % on sky rays (low confidence) | 0.5–1 | medium (t in level units, `Hit.voxel` scaling) | exact if §5.2 holds | 1, #48 |
| 6 | Incremental selection from a ring index | high | avoids 5–10 ms/frame of CPU at ~1 M bricks | 0.5 | low | none | 1 |
| 7 | Bias knob exposed in the overlay + gates | high | 4× work for ≤ 1 extra px; makes the trade measurable | 0.2 | none | up to 1.74 px if set to 2 | 1 |
| 8 | Remove `FAR_BLEND` and `voxel_tint` | high | small frame win; removes a fidelity loss | 0.2 | low | **improves** fidelity | #48 |
| 9 | Keep the finer level in a margin around each boundary | conditional | removes the double switch when the camera bobs | 0.2 | +20–40 % bricks | strictly better | 1 |
| 10 | View-cone residency with a turn margin | conditional | 3–4× fewer resident bricks, 3–4× less generation | 1 | medium: hitches on a fast turn | none if the margin is right | #74 |
| 11 | Per-ring bias (strict near, loose far) | conditional | up to 4× generation cut beyond ~10 km | 0.3 | medium | 1–2 px far away; gate decides | 7, #72 |
| 12 | Dither two levels across a band | watchlist | no pop at 2–4 px bias | 1–2 | 2× cost in the band, temporal noise | changes the image | 1 |
| 13 | Cross-fade / geomorph | rejected | — | — | — | ghosting / not expressible | — |
| 14 | Heightfield or impostor top LOD | rejected | small | — | — | removes caves/overhangs, moves the limb | ADR 0002 |
| 15 | Per-frame full re-selection of every brick | rejected | — | — | 5–10 ms CPU | none | — |

Reading the table: **#2 is the biggest win and it is not an LOD-selection technique at all** — it is what makes the coarse levels affordable. #1 is what makes them *correct*, and it is cheap once #2 exists. #3 is non-negotiable for "invisible". #4–#8 are the small pieces that stop the scheme from having visible seams. #10–#12 are where the remaining work/quality trade is, and all of them are measurable.

## 8. What the dev should do first (plain language)

1. **Fix the colour.** Land #79. Until then, every switch changes the ground from soil to rock and no amount of clever geometry will hide it.
2. **Pick the distance rule.** One Voxel ≤ one output pixel, `d = 102.8 · 2^L` metres. That is the whole selection metric. Keep the loose variant behind a number so you can try it.
3. **Draw each level as its own scene, nearest first, and stop at the first hit.** That is the smallest change to `trace()`: no mixing of levels inside one brick, no stitching, no cracks.
4. **Generate coarse bricks directly (#47).** Otherwise the first zoom to orbit has to generate the entire surface at full detail, which is 30 M bricks and 4 GB.
5. **Measure the seam, not the Voxel.** A patch test that reports the *displacement* of the surface, plus one image diff at a ring boundary on a slope and one at a limb. Those three numbers are the whole fidelity gate for LOD.

## 9. Experiments for the 4090

All of these are additions to `client bench` (#64) and are runnable from the Windows artifact. E1 and E2 are the ones that decide the design.

**E1 — ring-vs-brute-force image diff, per band.** Needs #48's ring path; until it exists the same test can be run by rendering each band's bricks alone, which measures everything except the selector. Add a bench path `lod-bands-v1` (alongside `planet-flyover-v1`, `bins/client/src/bench/path.rs`) with one still camera at the middle of each band, on the test Planet, looking at a slope and at the limb. Render primary with rings, then the same camera with only the level-0 `Scene` (the existing one-scene path *is* brute force on the test Planet). Report, per band: bricks resident, primary ms, `gpu_total`, and the diff against the level-0 image (fraction of pixels off by more than 1/255, p99 and max). Pass: the same tolerance the goldens already use — `PRIMARY` in `crates/render/tests/golden.rs` (MSE ≤ 4.0, at most 0.5 % of pixels with any channel more than 40/255 off) — and no band that needs a looser one. That tolerance is already calibrated for "a few edge pixels land on the neighbouring Voxel", which is exactly what a ≤ 1 px LOD step does. Report the max displacement per band alongside it.
*Why it matters:* it is the referee for every claim in §2.4, and it also catches the `FAR_BLEND`/`voxel_tint` seam.

**E2 — bias sweep.** The same path, with the bias at 1, 2 and 4 output px per Voxel (i.e. the loose rule and looser). Report diff and ms for each. Pass: the bias the dev is willing to ship is the largest one that still passes E1's tolerance. If that is 2, the streaming budget in §3.5 drops 4×, which is the single biggest remaining win.

**E3 — seam and low-sun golden.** One camera with a ring boundary across a sunlit slope, and one with a 10° sun and a thin ridge straddling the boundary. Screenshot diffs against level 0. Pass: no step and no shadow shift beyond E1's tolerance. *Why:* this is the only test of the "soft sun bounds the caster error" argument in §2.5 and of the seam's *shading* (as opposed to geometry).

**E4 — 50 m/s refill.** Extend the path with a straight 50 m/s run at 100 m altitude over rough terrain, and report per frame: bricks generated, bytes uploaded, missing-brick count in view, and `frame_time` p99/max. Pass: zero missing bricks, and no frame above the 16.7 ms budget. *Why:* §3.5 says refill, not VRAM, is the constraint; this measures it and gives #49 its target.

**E5 (CI, no GPU) — the displacement test.** The `lod_error` measurement as a `cargo test`: per level 1–6, the surface of the directly generated brick against the downsampled one, p95 radial displacement ≤ 0.5 coarse Voxels and ≤ 5 % differing Voxels, on a fixed seed. This is the only one that runs on every PR, and it is the one that would have caught the Material problem (add a top-Material equality assertion per level).

## 10. Open questions

1. **What is the real Planet's relief and crust thickness (#45)?** The 1 km mountain horizon in §3.2, the top ring at ground level, and the whole level-0 crust figure (~30 M bricks) all depend on it. If relief is 100 m rather than 1 km, the ground uses one ring fewer and the crust is 10× thinner.
2. **Does the image-diff gate tolerate the loose rule?** Only E2 can answer. If it does, the streaming budget halves twice.
3. **Can #49 keep up with 50 m/s at the strict rule?** §3.5 says ~50 % of today's generator on the leading edge, on the test Planet. The real Planet's generator will be more expensive per brick (biomes, caves) and the flight is over the real Planet. This is the main risk to "strict by default".
4. **Is a per-ring bias perceptually justified?** Distant terrain is also hazier and lower-contrast, so it may tolerate 2 px where near terrain does not. That is an image question, and the atmosphere pass (#53) changes the answer.
5. **How does the probe/dig ray choose a level?** `World::dig` traces the level-0 `Scene`; if rings ever replace level 0 near the camera, the dig ray must use the finest resident level, or it will pick a coarse Voxel as the dig centre and the Edit will be visibly coarse. Keep level 0 always resident around the camera and trace it for Edits (recommended, 0 cost).
6. **Unverified:** the exact real-Planet brick counts (scaled, not measured); the generator's 130 k bricks/s on a real Planet with more Materials; and whether `bhayanak bench`'s image diff should be in linear or sRGB space — the 1-px bar is a position, not a colour, but the threshold in E1 needs a definition before it can fail a build.

## Sources

Primary, external:

- Losasso and Hoppe, [Geometry Clipmaps: Terrain Rendering Using Nested Regular Grids (SIGGRAPH 2004)](https://hhoppe.com/proj/geomclipmap/) — nested regular grids centred about the viewer, incrementally refilled; the 40 GB dataset and 100× compression.
- Strugar, [Continuous Distance-Dependent Level of Detail for Rendering Heightmaps (2009)](https://doi.org/10.1080/2151237X.2009.10129287), [paper PDF](https://github.com/fstrugar/CDLOD/blob/master/cdlod_paper_latest.pdf) — LOD as a function of true 3D distance to the observer, and a transition that avoids stitching; the source of "distance, not height", which §3.2 adopts.
- Ulrich, [Chunked LOD (2002)](http://tulrich.com/geekstuff/chunklod.html) — per-chunk screen-space error and the 2:1 level ratio this scheme inherits.
- Lengyel, [Transvoxel](https://transvoxel.org/) — why *meshed* LOD needs transition cells, and therefore why a closed-volume Voxel scheme does not.
- Space Engineers `MyClipmap` ([source](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage.Render/Voxels/MyClipmap.cs)) — the shipped voxel-planet clipmap this design resembles, including a dithering switch; already cited in [`spherical-planets.md`](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/spherical-planets/docs/research/spherical-planets.md).

Primary, this repository:

- [ADR 0002](../adr/0002-brick-store-all-voxel-lod.md): 8³ bricks, 2× rings, 4-of-8 downsampling, Edits propagating up.
- `crates/brick-store/src/lod.rs` (`downsample`, `BrickSource`), `coord.rs` (`within_ball_at`), `store.rs` (`build_lod`, `refresh_lod`, `brick_at`).
- `crates/render/src/scene.rs` (one level per `Scene`; the "Level of detail, later" note), `shaders/trace.wgsl` (`BLOCK_STEPS = 24`, level-local coordinates), `shaders/primary.wgsl` (`FAR_BLEND`, `voxel_tint`), `sun.rs` (`DEFAULT_ANGULAR_RADIUS = 0.04`), `camera.rs` (`fov_y`).
- `crates/planet-gen/src/lib.rs` (`PLANET_RADIUS`, `CORE_RADIUS`, `generate_planet`'s "about 2.6 million bricks … about 20 seconds").
- `bins/client/bench-budgets.toml` (primary 1.0 ms, total 8.6 ms), `docs/agents/benchmarking.md` (bench shape, 22 s generation).
- [#47](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/47), [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48), [#49](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/49), [#51](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/51), [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64), [#79](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/79), [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71).
- Measurements: this ticket's throwaway `crates/planet-gen/examples/lod_error.rs`, run 2026-10-31 on the seed above; results in §2.3, §4 and §5.4.
