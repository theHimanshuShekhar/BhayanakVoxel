# Research: conservative visibility and culling

Resolves [#72](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/72). Feeds the catalogue in [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71).
Checked on 2026-10-31. Vocabulary follows [`CONTEXT.md`](../../CONTEXT.md); geometry follows ADR [0002](../adr/0002-brick-store-all-voxel-lod.md) and [#8](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8) (16 km radius, 10 cm Voxels, 8³ bricks of 0.8 m).

**How the numbers in this note were made.** The geometry is exact arithmetic and a small Monte-Carlo run done on the agent machine (no GPU): 40 million uniformly sampled surface nodes per camera, 16 km radius, 70° vertical / 102.4° horizontal field of view, nodes as axis-aligned cubes with a half-diagonal support radius. The test costs are measured on one core of a Ryzen 7 5700G. The GPU-side ranges are estimates from first principles and the repo's own 4090 numbers, and they say so. Nothing here was measured on the 4090.

## TL;DR

- **Culling does not cut rays in this renderer, and that is the whole story.** `trace()` ([`shaders/trace.wgsl`](../../crates/render/src/shaders/trace.wgsl)) is a DDA that only visits the Voxels and regions a ray crosses. A per-ray frustum test is pure added work, a depth prepass would duplicate the primary pass, and a cull cannot remove a camera ray. Culling can only cut **what is resident, generated, uploaded and indexed** — and, in the hardware-RT path ([#50](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/50)), how big the TLAS is.
- **Only about 2.0 ms of the 8.6 ms frame is even addressable.** Primary visibility (1.0 ms) and Edits/BVH (1.0) touch the scene structure; the other 6.6 ms (shadows, GI and cache, denoise, atmosphere, upscale, post) run on rays whose cost does not depend on which bricks are resident. The realistic frame-time win is **0.05–0.25 ms, 1–3 % of the frame** (low confidence). The real wins are **hitches (2–3.5× fewer new bricks per second) and VRAM (2–8×)**.
- **The horizon cap is the best test in the set and the only one with no look-around risk.** It is a single dot product per node, it depends only on where the camera is and not where it looks, and it is exact for a convex Planet. It removes the far hemisphere, which a frustum test alone happily keeps: at 50 R the bare frustum keeps the whole Planet and the cap halves it; at ground level the bare frustum keeps **7.7 % of all surface bricks** while only **1.7 × 10⁻⁵** of them are visible.
- **A frustum test without a horizon test is nearly worthless on a spherical Planet.** The cone penetrates the sphere and takes the far side with it. Measured: frustum-only keeps 8.5× the visible bricks at 1 km, 27× looking 30° down and 94× looking 60° down. Frustum **plus** horizon keeps 1.0–2.1× at brick granularity — the residual is just the slack of axis-aligned bricks.
- **Occlusion culling does not pay.** Hi-Z and depth prepasses cannot reject camera rays (the trace *is* the depth buffer), and a shadow or GI ray leaving a visible surface goes *away* from that surface, so the depth buffer is in front of it, not behind. A conservative occupancy mip cannot prove occlusion either: "solid if any child is solid" over-culls, "solid if all children are solid" only ever proves the solid core, which `Core` already does. The only exact occluders available are the Planet's outer sphere and its solid core — and those are the horizon and `Core` tests.
- **Where the test belongs depends on the granularity, and only one answer survives.** Brick-granularity culling is too fine to cull (10⁵–10⁶ nodes, 12.5 ns each, so 1.3–13 ms per pass) and useless per ray. Region and ring-tile granularity is the right place, on the **CPU, as scene selection on a camera change — not per frame and not in the shader**. That is a state change, so it can be amortised to near zero and never appears in a GPU pass timer.
- **Two cheap wins hide in the existing `trace()`.** It clips rays to the occupied-region **AABB**; a ray through a box corner walks up to 0.73 R ≈ 11.7 km of empty space before reaching the Planet (1828 region hash probes at level 0). Clipping to the Planet's bounding **sphere** instead removes that. And the per-node bounds a cull needs do not exist in `Scene` at all today — that is the one real contract gap.

**Ranked shortlist for the catalogue** (frame time, then hitches, then VRAM — and since the frame-time column is ~0 for all of them, risk of a visible hole decides the order):

| # | Technique | Frame time | Hitches | VRAM | Risk | Confidence |
|---|---|---|---|---|---|---|
| 1 | Horizon cap (sphere silhouette) | ~0 | 1.5–2× on the orbit fill | 2× at orbit, 30–3,000× from 10 m to 1 km | low, if gated for caves | high |
| 2 | Frustum selection at ring-tile/region granularity, CPU, on camera change | 0–0.15 ms (BVH only) | 2.5–3.5× fewer new bricks in flight | 2–3× (with a turn margin) | medium (turn holes) | high on cost, medium on win |
| 3 | Sphere-clip the scene bounds in `trace()` | 0.02–0.15 ms at orbit | — | — | none | low–medium |
| 4 | Brick-granularity residency compaction (not culling) | ~0 | — | 1.5–3× | low | medium |
| 5 | Temporal reuse of the previous cull result | saves 3–10× of the *cull* cost (0.01–0.1 ms CPU) | — | — | none | high |
| — | Hi-Z / depth prepass occlusion | 0 or negative | — | — | — | medium (rejected) |
| — | Conservative occupancy mips | 0 | — | — | — | high (rejected) |
| — | Portal/cell for caves | — | — | cave-only | high | watchlist |

---

## Where the frame time actually is

The budget from [#10](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/10), and whether visibility culling can move each line:

| Stage | Budget | Culling can touch it? |
|---|---|---|
| Primary visibility | 1.0 ms | Only through BVH/TLAS size in the hardware-RT path (#50). In the DDA, not at all. |
| Sun shadows | 0.4 ms | No. Rays start at visible points and leave; see §3.1. |
| GI bounce and radiance cache | 1.5 ms | No for the bounce ray. Cache *updates* could be view-scoped, but that changes convergence, not geometry (#54). |
| Denoise, atmosphere, upscale, post | 2.3 ms | No. |
| Reflections, glass, volumetrics | 2.4 ms | No. |
| Edits and BVH updates | 1.0 ms | TLAS build/refit scales with instances; 10× fewer instances is a small win on a per-edit path that is already local. |
| **Total** | **8.6 ms** | **≈2.0 ms addressable, ≈0.25 ms realistic** |

The primary pass is 1.09 ns per ray at 1280×720 (1.0 ms / 921,600 rays, from the budget, not a measurement). At that price a ray costs less than one hash probe into `table`. Anything that adds per-ray work — a frustum test, a Hi-Z descent — has to save more than a handful of DDA steps to break even, and on a terrain scene the average ray takes very few.

So the honest question is not "what does culling save in the trace", it is "what does it save in **generation, upload, residency and TLAS size**". That inverts the usual ordering: culling is a streaming optimisation that also happens to shrink the BVH.

---

## 1. Frustum culling

### 1.1 What it can and cannot cut

- **It cannot cut rays.** One camera ray per pixel is generated whether or not the geometry behind it is resident; the ray either hits or misses. A cull removes *nodes*, and a DDA that never looks at a node it does not cross gains nothing. [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48)'s acceptance criterion is the giveaway: "Moving the camera re-centres rings without re-uploading unchanged bricks" — the win being tracked is uploads, not rays.
- **It cannot cut shadow rays.** They are one per visible pixel, fixed by [#10](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/10).
- **It cannot cut GI rays** once the radiance cache supplies the extra bounces ([#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54)); the one traced bounce per pixel is fixed too.
- **It can cut** which bricks exist at all, what is generated, what is uploaded, what sits in VRAM, and (in #50) how many AABB instances the TLAS holds.

### 1.2 Granularity: measured test cost and node counts

Test cost on one core (measured, agent machine; scale ~1.5–2× for a slower CPU, divide by ~3–4 for a 4-wide SIMD batch):

| Test | Cost per node | Notes |
|---|---|---|
| 4-plane frustum, AABB/sphere, early-out | **12.5 ns** | Conservative plane test with a per-node support radius |
| Horizon (dot product + two multiplies) | **3.7 ns** | §2.2 |
| Horizon + first frustum plane (819 m node) | **4.8 ns** | The cheap ordering: horizon first, frustum second |

Node counts for a full 4π ring set on the 16 km Planet at 0.8 m bricks: **5.0 × 10⁹ surface bricks** in total (4πR²/0.64). A resident working set is a fraction of that, but the table shows why granularity matters:

| Granularity | Node size | Nodes, whole surface | Full pass at 12.5 ns | Verdict |
|---|---|---|---|---|
| Brick | 0.8 m | 5.0 × 10⁹ | 63 s | hopeless |
| Brick, resident set | 0.8 m | 10⁵–10⁶ | **1.3–13 ms** | too slow per frame; fine amortised |
| Region | 6.4 m | 7.9 × 10⁷ | 0.98 s | too slow |
| Region, resident set | 6.4 m | 10³–10⁴ | **13–125 µs** | right unit for the CPU |
| Ring tile (a level's brick group) | 10²–10³ m | 10²–10³ | **1.3–13 µs** | right unit for a per-frame path |

Consequence: **cull at ring-tile granularity and let the ring layout imply the bricks.** If a tile is kept, its bricks are kept; there is no per-brick test at all. That is also what makes the pass affordable at 60 Hz, and the numbers say even the *tile* pass is only worth doing when the camera changes (§1.4).

### 1.3 False positives on a sphere: measured

The ticket asks for the false-positive rate on a sphere, and the answer is much worse than the textbook picture, because the frustum cone from a camera on a sphere **passes through the Planet**.

"Visible" means: the node's centre is front-facing on the sphere (`c · p > |p|²`) **and** inside the screen. That is the exact visibility test for a convex sphere, which is what the reference image would show. "Kept" means the test passed. Ratio = kept / visible, at 0.8 m nodes. (The visible fraction is the §2.1 cap intersected with the screen, so it is roughly a quarter of the cap — the screen is about 100° of the 360° around the camera.)

| Camera height | Pitch | Visible fraction of the sphere | Frustum-only keeps | Frustum+horizon keeps |
|---|---|---|---|---|
| 1.8 m | 0° | 1.7 × 10⁻⁵ | 7.7 % (**4,650×**) | 3.5 × 10⁻⁵ (**2.1×**) |
| 1.8 m | 30° down | 1.6 × 10⁻⁵ | 24.9 % (**16,000×**) | 3.2 × 10⁻⁵ (**2.1×**) |
| 1.8 m | 60° down | 2.5 × 10⁻⁸ | 42.9 % (**1.7 × 10⁷×**) | 5.0 × 10⁻⁸ (**2.0×**) |
| 100 m | 0° | 8.8 × 10⁻⁴ | 7.6 % (**87×**) | 9.0 × 10⁻⁴ (**1.0×**) |
| 1 km | 30° down | 9.3 × 10⁻³ | 24.8 % (**27×**) | 9.3 × 10⁻³ (**1.0×**) |
| 16 km (1 R) | 60° down | 14.2 % | 58.7 % (**4.1×**) | 14.2 % (**1.0×**) |
| 320 km (20 R) | 60° down | 47.6 % | 100 % (**2.1×**) | 47.6 % (**1.0×**) |
| 800 km (50 R) | 60° down | 49.0 % | 100 % (**2.0×**) | 49.0 % (**1.0×**) |

Read the first three rows twice, because they are the important ones:

- **Looking level at ground level, a frustum test keeps 7.7 % of the Planet's surface bricks** — 4,650 times the number that can contribute a pixel. The extra is the far side of the Planet, which is inside the cone: the bottom of a 70° view is 35° below horizontal, and everything from the horizon out to about 70° of central angle is inside that cone. Without a horizon or occlusion test there is no way to know it is hidden.
- **Looking 60° down makes it 1.7 × 10⁷×.** The cone now stabs straight through the Planet and takes a huge cap of the far side with it. A "frustum culling" pass measured on a down-looking camera looks like a triumph and is nearly all waste.
- **The horizon test fixes it completely at brick granularity** (1.0–2.1×), and the leftover 2.1× at ground level is the geometric slack of 0.8 m axis-aligned bricks straddling the cap edge, not a bug.

The slack grows with node size, which is the tension in §1.2. At 6.4 m regions on the ground the same test keeps **10×** the visible nodes; at 102 m tiles, **153×**; at 819 m tiles, **1,350×**. Coarse nodes near a horizon boundary are much larger than the boundary is wide. That is the cost of culling coarse: you cull the fine levels tightly and the coarse levels loosely, which is fine, because coarse levels are the cheap ones in bytes and the expensive ones in count.

**Comparison with the literature.** Plane-vs-AABB tests are known to be inaccurate for large objects near frustum corners, and Cesium's own write-up documents exactly this failure and their decision not to chase it, because the passes are cheap and the wins small ([Cesium, *Tighter Frustum Culling and Why You May Want to Disregard It*](https://cesium.com/blog/2017/02/02/tighter-frustum-culling-and-why-you-may-want-to-disregard-it)). Assarsson and Möller's optimisations — plane coherence, octant tests, cached previous distances and rotation angles — give 3–10× on the *test itself* and are the standard reference for the cheap forms ([JGT 5(1), 2000, PDF](https://www.cse.chalmers.se/~uffe/vfc_bbox.pdf)). Neither paper considers a spherical occluder, because neither is about planets; that is the horizon test's job.

### 1.4 CPU scene selection, GPU traversal, or both

**CPU scene selection, on camera change, at tile granularity.** The reasons, in order:

1. A ray already knows where it goes. A frustum test inside the traversal is strictly extra work.
2. Selection is a property of the camera, not of the frame. While the camera is still, the answer cannot change; re-running it every frame is pure waste. Gate on `Δyaw`, `Δposition` and `Δzoom` and the pass costs **0 µs on most frames** and 13–125 µs when it runs.
3. The pass is CPU-bound anyway (12.5 ns/node, single-threaded), and it produces the list that the uploader, the evictor and the TLAS consumer all need. Doing it on the GPU would require a readback before the CPU could act on it.

**GPU, secondarily, for the instance list.** In the hardware-RT path (#50) the TLAS is built from an instance list; a compute pass can compact and reorder it. That is a GPU copy of a CPU decision, not a second decision.

The one thing that belongs in the shader is the **scene bound** (§2.6) — a global constant, not a per-node cull.

### 1.5 What makes the test exact

- **Use the pixel-edge planes, not the projection matrix's planes.** They differ by half a pixel at the screen border; the difference matters at the horizon, where a single brick can be the difference between a pixel and a hole.
- **Expand by the node's support radius** (half-diagonal for an AABB, radius for a sphere) — that is what makes a plane test conservative. The version with no expansion is the classic source of pop-in at screen edges.
- **Add the jitter.** [#51](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/51) traces at 1280×720 with a sub-pixel jitter and upsamples. A sample can move the outermost ray by up to 0.5 internal pixels, which is an angular slack of `atan(0.5 / f)` — about 0.06° at 720p and 70° FOV. Cheap to add, and required for bit-exactness.
- **Add the temporal window, or cull only what is guaranteed gone.** With TAAU the output at frame *n* blends history from *n−1..n−k*. A node that was visible last frame contributes through reprojection even after it leaves the frustum. Culling it is correct — it is off-screen — but *re-adding* it later (after eviction) arrives with no history, which at 1 sample per pixel is a visible noise flash. The safe rule is that the retention set is the union of the frustum over the history window, plus a hysteresis band, or that a returning node is faded in from the coarser level. This is the single subtlest interaction in the ticket; §5 makes it a contract item.
- **Never cull by the near plane.** Geometry between the camera and the near plane still appears through the temporal history and still bounces light into the visible set.

---

## 2. Horizon capping on the 16 km Planet

### 2.1 The geometry

For a camera at distance `r = R + h` from the centre of a sphere of radius `R`, the visible surface is the cap whose edge is the horizon circle. Three numbers follow from it:

- **Horizon distance** (straight-line chord to the tangent point): `L = √(r² − R²) = √(2Rh + h²)`.
- **Visible-cap half-angle** (central angle of the cap, edge to sub-camera point): `θ = arccos(R / r)`.
- **Angular radius** (how big the cap's edge looks from the camera): `β = arcsin(R / r)`, which is also the angular radius of the Planet's silhouette.

At 16 km and the heights the Zoom visits:

| Camera height | h / R | Horizon distance | Cap half-angle θ | Visible fraction of the sphere | Angular radius β |
|---|---|---|---|---|---|
| 1.8 m (eye) | 1.1 × 10⁻⁴ | **240 m** | 0.86° | 5.3 × 10⁻⁵ | 89.14° |
| 10 m | 6.3 × 10⁻⁴ | 566 m | 2.03° | 3.1 × 10⁻⁴ | 87.97° |
| 100 m | 6.3 × 10⁻³ | 1.79 km | 6.39° | 3.1 × 10⁻³ | 83.61° |
| 1 km | 0.063 | 5.75 km | 19.75° | 2.9 × 10⁻² | 70.25° |
| 10 km | 0.625 | 20.5 km | 52.0° | 0.192 | 37.98° |
| 1 R (16 km) | 1 | 27.7 km | 60.0° | 0.250 | 30.00° |
| 10 R (160 km) | 10 | 175 km | 84.8° | 0.455 | 5.22° |
| 20 R (320 km) | 20 | 336 km | 87.3° | 0.476 | 2.73° |
| 50 R (800 km) | 50 | 816 km | 88.9° | **0.490** | 1.12° |

The 240 m at eye height matches [#8](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8)'s 240 m, which is a good check on the formula. The ceiling matters: **the visible fraction never exceeds one half.** From outside a sphere you always see slightly less than a hemisphere. So the horizon cap can never remove more than half of a *full 4π* ring set, no matter how far out the camera goes — 2.04× at 50 R, 2.1× at 20 R, 4× at 1 R.

### 2.2 The test, and how it meets the frustum

Do not build a cap and intersect it with the frustum. Test each node against both, in this order:

```
visible_if   c·p  >  |p|²                        # the node is on the near side of its own horizon
culled_if    c·m + |c|·ρ  ≤  (|m| − ρ)²          # conservative: the whole node ball is beyond it
```

where `c` is the camera position, `m` the node centre, `ρ` the node's bounding radius, and everything is in the Planet frame in f64 on the CPU. The `culled_if` form is a bound on `max(c·p − |p|²)` over the ball, so it is conservative by construction. It costs 3.7 ns, and it is the cheaper of the two tests, so run it first.

The clean way to combine them is index arithmetic rather than geometry. If ring tiles are **indexed by direction from the Planet's centre** (a cube-sphere face plus integer coordinates is the natural choice, and the generator already needs a cube-sphere for the world map), then:

- the horizon cap is a **dot-product threshold on the tile's direction**: keep tiles whose direction `u` satisfies `u·ĉ > cos(θ + slack)`, where `ĉ = c/|c|` — one compare per tile, no bounds at all;
- the frustum is a **screen-space range on the same index**, which reduces to a rectangle test per cube face for the side planes;
- both are independent of each other, so the kept set is their intersection and the tests never need to be merged.

That is why §5 asks #48 to make the ring layout angular. If rings stay camera-centred concentric shells with no directional index, every test needs a stored bounding sphere and the clean arithmetic is lost.

### 2.3 What it is worth

Per camera height, as a fraction of a full 4π ring set at that level (this is what VRAM, generation and the BVH see):

- **Ground to ~10 m:** the cap covers 5 × 10⁻⁵ to 3 × 10⁻⁴ of the sphere. Almost everything is cullable: `1/visible` is 20,000× at eye height and 3,000× at 10 m. This is why a walking game does not keep the far side of the world resident — but it also means those levels must not have been generated for the *current* frame's sake at all.
- **100 m to 1 km:** 300× to 34×. The Zoom spends real time here and this is where the win lands, both in bytes and in the orbit-fill hitch.
- **10 R and out:** 2.2× to 2.0×, and it stabilises at 2× because the visible fraction saturates below one half. In orbit the horizon cap is worth **exactly half the coarse Planet**, forever, and that is the single most reliable number in this note.

For the 2 GB budget in [#9](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/9) (which estimated 0.2–0.7 GB of need), duplicating the orbit view's coarse set is the largest avoidable cost, and it is avoidable with a view-independent test. Cesium's production experience is the same shape: with the whole Earth inside the frustum, "over half of them … are below the horizon and do not need to be rendered", and their tighter horizon-culling point removed about 15 % more tiles than their previous bounding-sphere test ([Cesium, *Horizon Culling*](https://cesium.com/blog/2013/04/25/horizon-culling)).

### 2.4 Caves and overhangs: where the test stops being exact

This is the part that can produce a visible hole, so state the condition exactly.

A straight ray from outside can enter the Planet's bounding sphere **only** through the cap, and because a sphere is convex it can leave only at the far side and never re-enter. Therefore:

- **Culling the far-side *outer surface* is exact.** No ray from the camera can reach a far-side outer face without leaving and re-entering the sphere, which is impossible.
- **Culling far-side *interior* geometry is not exact in general.** A ray that enters the near cap can travel inside the Planet through a cave and hit the inner face of a far-side brick. That is a real pixel: standing at a cave mouth, looking down a tunnel that comes out on the other side of a hill.
- **It becomes exact again at coarse levels.** At a level whose cell is larger than the widest cave, majority downsampling has turned the cave into solid, so the level's own geometry has no through-path — and the tracer renders that level's geometry, not the fine one. Exactness at the level's resolution is all that a bit-exact image needs.

Practical rules, in order of preference:

1. **Gate on cell size.** Apply the cap to level `L` only when `0.1 m · 2^L` is at least the maximum cave width the generator can make ([#46](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/46): caves within ~300 m of the surface, "tunnels, caverns, overhangs and cave mouths"). Levels finer than that are all inside the near cap anyway (the finest levels span ±128 m and the cap at eye height is 240 m), so in practice fine levels are *never* horizon-culled and the levels that are culled are the ones where caves are sub-resolution. This is the cheap, provable version, and it needs one number from the generator.
2. **Gate on altitude.** Do not apply it when the camera is inside the outer bound (`|c| < R + H_max`). It is meaningless there and would cull the entire surface.
3. **Prove it per frame with the ray-incidence oracle** (§4.2), including a camera standing in a cave mouth. If it ever disagrees, the cap is what is wrong, not the oracle.

Under overhangs the same reasoning holds one level down: an overhang is a local surface feature, it is erased by the mip that the cap applies to, and it never lets a ray leave and re-enter the sphere.

The mitigation if a case is found is a **cave margin**: inflate `R` in the test by the maximum lateral reach of a cave (a generator constant), or inflate `θ`. Do not use a small fixed margin like "10 voxels" — the failure mode is a kilometre-long tunnel, not a two-metre overhang.

### 2.5 How it behaves through the continuous Zoom

The cap grows monotonically with height and it grows *fast at first*: `θ ≈ √(2h/R)` near the ground, so going from 1.8 m to 1 km takes the cap from 0.86° to 19.8°, and the rest of the Zoom (1 km to 800 km) only takes it to 88.9°. Consequences:

- **Zooming out reveals geometry, so it is the risky direction.** The retention set must be computed for the height a second or two ahead, using the known Zoom rate (~×1.25 per notch, smoothed, [#58](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/58)), not for the current one. `θ(h(t+1 s))` is cheap and exact.
- **Zooming out reveals it in *all directions*.** Unlike a turn, which reveals one side, the growing cap is direction-independent. That makes the prefetch predictable and it means the "look-around" problem does not arise during a Zoom — a genuine advantage of the horizon test over the frustum test.
- **Zooming in needs no prefetch** (the set only shrinks) but must not evict immediately: the cap crosses a level's worth of bricks in a fraction of a second near the ground. Hysteresis, and the eviction side belongs to [#74](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/74).

### 2.6 The cheap cousin: clip rays to the bounding sphere

`trace()` currently clips to `scene.origin`/`scene.dims` — the **AABB** of the occupied regions ([`trace.wgsl`](../../crates/render/src/shaders/trace.wgsl), the "Clip the ray to the box of occupied regions" block). A box circumscribes a sphere with 1.91× its volume, and a ray through a box corner travels `(√3 − 1)R ≈ 0.73R` before it can reach the Planet: 11.7 km on a 16 km Planet, which at level 0's 6.4 m regions is **1,828 hash probes of pure nothing**, and 29 at level 6. A ray that misses tangentially does the whole walk.

Two changes, both small:

1. Store a **bounding sphere** for the scene in `SceneHeader` — the Planet's `R + H_max`, computed CPU-side, not derived from the AABB (the AABB's circumscribed sphere is larger than the box and would help nothing).
2. Clip to the sphere (`ray vs sphere` for entry and exit `t`) *before* the region walk, or intersect the sphere's entry `t` with the AABB's.

This is exact for any geometry inside the sphere — no margin, no gating, no test. It is the only item on the shortlist with no fidelity question at all. Its size is unknown without instrumentation: the win is 0 for rays that enter near the box face and up to 40 % of the walk for corner rays, and it is concentrated in views where the frustum sees the Planet's limb or sky (orbit, high altitude). Estimate **0.02–0.15 ms of a 1.0 ms primary pass** (low–medium confidence), and the experiment in §7 measures it directly by counting region steps per ray.

---

## 3. Occlusion

The short version: in this renderer there is no conservative occluder worth building except the two we already have — the outer sphere (the horizon, §2) and the solid core (`Core`, already in `Scene`). Here is why each candidate fails.

### 3.1 Hi-Z / depth prepass

- **For camera rays: circular.** The primary pass *produces* the depth. A prepass would trace the same rays once to build a depth buffer and then trace them again to test, which is strictly more work. Using last frame's depth is exact only if nothing moved (camera, Edits, Debris, Fluids), and with 1 sample per pixel a one-frame-stale rejection shows up directly in the image.
- **For shadow rays: the depth buffer is on the wrong side.** A shadow ray starts at a *visible* surface point and goes toward the sun. The depth buffer holds the surface *between the camera and that point*. So the ray leaves into space that is in front of the depth buffer, and a Hi-Z test ("is the ray behind the stored surface?") almost never fires. A shadow ray can only be occluded by geometry that is *off-screen* — a mountain behind the one you are looking at — which no screen-space structure can prove. That is a structural argument, not a measurement, and it is the reason "30–80 % of shadow triangles culled" (the rasteriser's win in [Haar & Aaltonen 2015](https://advances.realtimerendering.com/s2015/aaltonenhaar_siggraph2015_combined_final_footer_220dpi.pdf)) does not transfer to a ray-traced shadow pass: the rasteriser culls *casters* from a light's view, which is a different test.
- **For GI rays: the structure exists but it is not conservative.** Production engines do trace secondary rays against the screen first — Lumen "features trace rays against the screen first … before using a more reliable method if no hit is found, or the ray passes behind a surface" ([Epic, *Lumen Technical Details*](https://dev.epicgames.com/documentation/en-us/unreal-engine/lumen-technical-details-in-unreal-engine)). That is an *accelerator with a fallback*, not a cull: a screen-space miss is not evidence of a miss, so it cannot be used to skip the real trace. Adopting it means either tracing twice on a miss or accepting an approximation, and this project's referee is image equality against the reference tracer.
- **Net:** the build cost (a depth pyramid, ~0.05–0.1 ms at 720p) plus the per-ray descent buys a hit rate that is low for both secondary ray types. Reject.

### 3.2 Coarse-occupancy tests

The tempting idea is a second mip pyramid at "solid if **any** child is solid", used to prove that a fine brick is hidden behind coarse terrain. It cannot be conservative:

- **"Any child solid"** over-reports solidity, so a ray that hits it may hit nothing in the real geometry. Culling on that removes visible surfaces. Not allowed.
- **"All children solid"** under-reports, so a hit proves real solidity. But that set is essentially the deep interior — which is exactly what `Core` already represents as a ball, for free, with no per-level cost. The incremental power over `Core` is the shells between "all solid" and the surface, which is a thin band, and it is the band where the interesting geometry is.

There is no third option that is both cheap and conservative, because a *conservative* occluder must be a subset of the real geometry, and the real geometry is the thing you were trying to avoid looking at.

### 3.3 Portals and cells for caves

Inside a cave the visible set collapses to a corridor, so a cell-and-portal graph would be the one place occlusion culling could pay. It is also the one place it is expensive: the world is editable anywhere, so cells and portals must be derived from occupancy rather than authored, invalidated by every Edit ([#60](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/60) brushes), and kept exact through the same downsampling that makes level `L+1` from level `L`. The classic cell-and-portal methods assume static architecture ([Teller & Séquin 1991](https://doi.org/10.1145/122718.122725); [Luebke & Georges 1995](https://doi.org/10.1145/199404.199422)).

And even if it worked, the win would be residency in caves, not frame time: the tracer already walks only the corridor. Watchlist, not M1a.

### 3.4 What does pay

- **`Core`** (already there): the deep interior is one ball, not bricks. This is the single largest occlusion win in the project and it is already bought.
- **The outer sphere**, i.e. the horizon cap. It is the only *global* occluder that is exact, view-independent and free.
- **Nothing per-ray.** Which is why the answer to "is the real win in tracing, generation, upload, VRAM or BVH updates" is: **generation > upload > VRAM > BVH size > tracing**, in that order, for this renderer.

---

## 4. Conservative correctness, and how to prove it

### 4.1 The bounds each test needs

| Test | Exact when … | The bound to store |
|---|---|---|
| Frustum | the node's support volume is inside the (pixel-edge, jitter-expanded) planes | per-node bounding sphere or AABB, plus `ρ` |
| Horizon | the node is entirely outside the cap, and either it is outer-surface or its level has no through-cave | `R_outer = R + H_max`, the camera's `\|c\|`, and a per-level "cave-free" flag |
| Sphere clip in `trace()` | all geometry is inside the sphere | `bound_centre`, `bound_radius` in `SceneHeader` |
| Occlusion (if ever) | the occluder is opaque and provably between the ray and the node | none available cheaply (§3) |

Two rules that are easy to get wrong and impossible to see in a screenshot: **the expansion must use the *maximum* node support radius, not the centre's**; and **the cap slack must include the camera's own height above the bound** during a Zoom, not just the node's.

### 4.2 The three gates

1. **Bit-exact image equality, culling on vs off.** The tracer is deterministic: the RNG is seeded per pixel and sample ([`tracer.rs`](../../crates/render/src/tracer.rs), `RNG_SEED`), the accumulation is a running mean, and the CPU mirror is line-for-line. So the gate is not "within tolerance" — it is **identical output bytes** on every keyframe of the bench path, in both `primary` and `reference` mode. Any culled node that would have been hit changes a ray, and the diff shows it immediately. This is the same machinery `crates/render/tests/golden.rs` already uses.
2. **A ray-incidence oracle, which is stronger than an image diff.** Instrument `reference::trace` to record the set of bricks a ray visits, then assert `visited ⊆ retained` for a few million rays drawn from every camera on the bench path (including the cave dive and the orbit view). An image diff only tests the rays that happened to be generated; the oracle tests the *rule*, over random rays, at a cost of seconds on the CPU, and it points at the exact brick and the exact test that was wrong when it fails. This is the test that should gate the horizon cap, because it is the one that can find the cave case.
3. **A no-hole camera path.** A bench path that includes a 180° fast turn at 50 m/s, a dive into a cave mouth, a Zoom from first person to 50 R and back, and a teleport. Per frame, assert that no ray's hit brick was absent from the retained set, and that the image equals the unculled image. [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64) already wants the path; the assertions are the new part. [#77](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/77) owns the harness.

A useful asymmetry: **image equality catches over-culling, the oracle catches under-culling.** Run both. The first is cheap and runs in CI on lavapipe; the second is CPU-only and also runs in CI.

---

## 5. Contract impact on #48, #49, #50, #51

The current `Scene` is a flat hash of regions plus one global AABB ([`scene.rs`](../../crates/render/src/scene.rs)). It has **no per-node bounds and no hierarchy**, so today there is nothing to cull against except the region coordinate (which is free: a region's AABB is `rc · 64`). Everything below follows from that.

**#48 — LOD rings on the GPU.**
- Store a **bounding sphere (or AABB) per ring tile**, and give each level its own `origin`/`dims`. The per-region AABB is derivable and needs no storage.
- Make the ring layout **angular** — tiles indexed by direction from the Planet's centre — so the horizon test is a dot product on the tile index and the frustum test is a per-face range. This is the single change that makes culling cheap; without it every test needs stored bounds.
- Carry the **cull result as an instance/visible list** separate from the brick buffers, so one cull serves the uploader, the evictor and the TLAS.
- Add `bound_centre` and `bound_radius` to `SceneHeader` (§2.6).
- Acceptance criterion to add: "culling on and off produce identical images on the ring goldens".
- The cost is roughly **1–1.5 agent-weeks** on top of the existing ticket, mostly the angular index and the visible list.

**#49 — Streaming generation.**
- Priority order becomes **visible ∩ horizon > near > everything else**, and the never-visible set may be skipped from generation entirely rather than generated and then evicted.
- The prefetch cone is the frustum **×1.2–1.4** for turns, and the horizon cap **at `h(t + 1–2 s)`** for Zoom-out. Both numbers come from §1 and §2.
- The "no holes" criterion must be asserted **with culling on**, otherwise it tests a configuration that ships in neither mode.
- The orbit target (whole-Planet coarse LOD in ≤5 s) should be re-stated against the *capped* set, which is half of it, so the same code should hit 2.5–3.5 s and leave the rest as headroom.

**#50 — Hardware RT `trace()`.**
- The TLAS is built from the **visible instance list**, so the cull is upstream of the BVH, not inside it.
- Rebuild/refit the TLAS on a **cull change**, not per frame; per-ring BLAS with brick AABBs; instance order compacted to the cull's order improves locality.
- Fewer instances is the one place where a cull genuinely touches frame time: expect **5–15 %** of the primary pass from a 10× instance reduction, not 90 %.
- Culling is what makes "per-brick AABB instances" affordable at all in the orbit view, where the far side would otherwise be double the instances.

**#51 — Half-res tracing and TAAU.**
- The cull must be conservative over the **jittered** frustum (0.5 internal pixel, ~0.06° angular slack) — trivial, but required for bit-exactness.
- The retention set must cover the **history window**: a node that contributed to the last *k* frames still contributes through reprojection. Practical form: retain the union of the frustum over the window plus hysteresis, or fade in a returning node from the coarser level.
- **Culling changes invalidate history.** A visible cull change (a node appearing) is a new surface with no accumulation, which at 1 spp is a flash. Either the change is hidden by the coarser level or the fade is explicit; the debug history-length view already exists and should be part of the proof.
- Ask: "does the cull change at all while the camera is still?" If the angular/hysteresis design is right, the answer is no, and the whole class of history problems disappears for a still camera.

---

## 6. Ranked shortlist, per technique

Confidence is in the estimate, not in the geometry. "Win" is against the 8.6 ms / 2 GB / 16.7 ms frame baseline. All wins assume the technique ships with the culling-on goldens green.

### 6.1 Horizon cap — must-have

| | |
|---|---|
| **Buys** | Frame time ~0 (nothing on the ray path changes). Hitches: halves the orbit fill (5 s → 2.5–3.5 s, medium confidence) and cuts the resident coarse set in half, which is half the uploads after a Zoom. VRAM: **2.0–2.1× at 20–50 R**, 4× at 1 R, 30–3,000× between 10 m and 1 km; 50 % of the whole-planet coarse set is the reliable number. |
| **Implementation** | 1–1.5 agent-weeks, mostly the per-level gating and the oracle test. |
| **Risk** | Medium: the cave case (§2.4). Mitigated by cell-size gating and the oracle. |
| **Fidelity** | Zero, by construction, at the gated levels; proven bit-exactly. |
| **Depends on** | #48 (per-level bounds, angular index), #46 (cave width and depth), #49 (prefetch at `h(t+Δ)`), #74 (eviction hysteresis). |
| **Experiment** | §7 experiment 2. |
| **Confidence** | High on the geometry and the test cost; medium on the byte and hitch numbers. |

### 6.2 Frustum selection at tile/region granularity, CPU, on camera change — must-have

| | |
|---|---|
| **Buys** | Frame time 0 in the DDA; **0–0.15 ms** of the primary pass in the hardware-RT path (0.05–0.2× the pass, low–medium confidence). Hitches: the new-brick rate while flying scales with the horizontal field of view, so a 102.4° cone against 360° is **3.5× fewer new bricks/s**, 2.5× with a 1.2× turn margin. VRAM: a **2–3×** cut of a distance-bounded ring set with a 1.2× margin (the bare frustum is 23 % of all directions, 2.9 sr; ×1.2 is 33 %; ×1.4 is 41 %). |
| **Implementation** | 0.5–1 agent-week on top of the horizon work (same pass, second test). |
| **Risk** | Medium: a hole on a fast turn unless the retention cone is widened or the prefetch keeps up; TAA history on re-entry. |
| **Fidelity** | Exact with pixel-edge planes + node radius + jitter; the risk is operational (prefetch race), not mathematical. |
| **Depends on** | #48, #49, #74, #51 (jitter/history). |
| **Experiment** | §7 experiment 3. |
| **Confidence** | High on the kept fraction and the test cost (measured); medium on the frame-time win; low on the exact hitch percentage. |

### 6.3 Sphere-clip the scene bounds — high value, nearly free

| | |
|---|---|
| **Buys** | **0.02–0.15 ms** of the primary pass in views that see the limb or sky (orbit, high altitude), by removing up to 0.73 R of empty region walk on corner rays (1,828 probes at level 0). Zero VRAM, zero hitches. |
| **Implementation** | 1–2 days: a `SceneHeader` field, a ray/sphere clip, and a step counter to prove it. |
| **Risk** | None to fidelity if the sphere bounds the geometry (it does, by construction). |
| **Depends on** | #48 (header), the Planet's `R + H_max` constant (from #45). |
| **Experiment** | §7 experiment 4. |
| **Confidence** | Low–medium on the milliseconds; high that it is free and never wrong. |

### 6.4 Brick-granularity residency compaction — conditional

Not culling: the pass that drops bricks which are *resident but removed from the visible set*, so the buffer can shrink and the BVH can be rebuilt. It is the cleanup step after 6.1 and 6.2 and it is where the VRAM win actually lands. **1.5–3× VRAM** (medium confidence), ~0 frame time, costs ~1 week, and it must not free a brick that the oracle would have needed.

### 6.5 Temporal reuse of the cull — cheap

Cache last frame's decision and test only nodes inside the changed region (Assarsson & Möller's coherence tricks are the reference: 3–10× on the test itself). Saves **3–10× of a 13–125 µs pass**, i.e. 0.01–0.1 ms of CPU, and it is what makes a per-frame cull affordable at all. **Zero risk**, half a day.

### 6.6 Rejected and watchlist

| Technique | Why not |
|---|---|
| Hi-Z / depth prepass occlusion | §3.1. Circular for camera rays, wrong-sided for shadow rays, non-conservative for GI rays. Would cost 0.05–0.1 ms to build and almost never fire. |
| Conservative occupancy mips | §3.2. "Any-solid" over-culls, "all-solid" only reproduces `Core`. |
| Per-ray frustum test in `trace()` | Strictly extra work in a DDA; 12.5 ns per test against a ~1 ns per-ray budget. |
| Back-face culling | Nothing to cull: a DDA returns the *first* solid Voxel, so a back face is never shaded. Already free. |
| A distance/far cap on the primary | The sky is still shaded, and the horizon cap is the exact version of the same idea. |
| Sun-slab / shadow-ray culls | The existing `facing > 0` test already removes the half the sphere shadows, and the rest of the ray leaves through empty space. |
| Screen-space GI rays (Lumen-style) | An accelerator with a fallback, not a cull; would change the image or double the trace (§3.1). Watchlist for M2+, when the radiance cache makes a near-miss cheap. |
| Portal/cell for caves | §3.3. Exact in principle, invalidated by every Edit, expensive to build from occupancy. Watchlist, revisit if caves become the dominant scene. |
| Active-tile sky masks | Skips a few percent of primary rays in tiles that are pure sky, but the sky is needed for the atmosphere and the cache, and it saves well under 0.05 ms. |
| Per-object culling for Debris | Not this ticket; [#76](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/76) owns it. TLAS instances make it cheap, and it is the one place where culling a *dynamic* object pays. |

---

## 7. Experiments for the 4090

Each is a `bhayanak bench` variant, and each has a pass/fail that does not need a human to squint. The dev runs them on the Windows artifact; all of them are cheap (a minute or two each) because they reuse the existing path machinery.

**1. Culling-on vs culling-off equality and residency census.**
Add `--cull on|off` (default `on`) and report `bricks_resident`, `bricks_generated`, `upload_bytes`, `cull_us` in `report.json`. Run the existing `planet-flyover-v1` plus a new `cave-orbit-v1` path (a dive into a cave mouth, a 180° turn, a Zoom to 50 R and back).
*Pass:* every keyframe's PNG is byte-identical between `--cull on` and `--cull off`, in both modes; `cull_us` p95 < 100 µs; no frame reports a missing brick.
*Why this one first:* it is the gate for everything else, and it costs nothing beyond the flag.

**2. Horizon-cap byte win across the Zoom.**
`client bench --path zoom-out-v1 --cull horizon` vs `--cull off`, sampling `bricks_resident` and `bytes_resident` per frame on the way from 1.8 m to 50 R.
*Pass:* ≥ 1.9× fewer resident bytes at 20 R and 50 R, image-identical throughout, and the orbit readiness time (already reported by [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64)'s bench) drops from the ~5 s target to ≤ 3.5 s.
*Kills it if:* the orbit readiness does **not** improve, which would mean generation, not residency, is the bottleneck — useful either way.

**3. Frustum selection under 50 m/s flight.**
`client bench --path planet-flyover-v2 --cull frustum+horizon` vs `--cull horizon`, reporting `new_bricks_per_s`, `upload_bytes_per_s`, `frame_time.p99` and the worst hitch.
*Pass:* ≥ 2.5× fewer new bricks/s and no image difference on any keyframe, with the worst hitch not worse than the culling-off run.
*Kills it if:* the win is below 1.5×, which would mean the ring set is already dominated by near-field bricks that are in view.

**4. Region-step census for the sphere clip.**
Instrument the primary pass to count region hash probes per ray (a counter buffer, averaged in the report), then compare the AABB clip with the bounding-sphere clip at 1 R, 20 R and 50 R.
*Pass:* ≥ 20 % fewer probes/ray at 20 R and 50 R; identical images.
*Cheapest possible:* it is a counter and a `max` in the shader, and it also tells us whether the primary pass is traversal-bound or memory-bound, which nothing currently knows.

**5. Cull pass cost on the dev's CPU.**
Standalone: retain N = 10⁴ and 10⁵ tile bounds, run the horizon+frustum pass 1,000 times, report ns/node. (The 12.5 ns/3.7 ns figures here are from a Ryzen 7 5700G.)
*Pass:* < 0.1 ms for 10⁴ tiles. If it is not, the cull must move to a worker thread or to the GPU, which changes #48's design — worth knowing before the rings are built, not after.

---

## 8. Open questions

- **Cave width and lateral reach** are the only inputs the horizon test needs from generation, and [#46](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/46) does not state them. Until they are pinned, the cell-size gate (§2.4) has no number to compare against. **This is the one blocker for shipping the horizon cap.**
- **Does the ring set reach 4π, or is it a tangent-plane clipmap?** [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48) says "camera-centred LOD rings" without saying whether rings are shells or clipmap squares. The culling wins above are stated against a 4π set; a clipmap would change the denominators, and the angular-index recommendation assumes the shell reading.
- **Is the primary pass traversal-bound or memory-bound?** Nothing in the repo measures region probes per ray. Until experiment 4 runs, the sphere clip's value is an estimate (low–medium confidence), and so is every claim about how much a smaller BVH would buy in #50.
- **What does the GI cache cost, and is its update view-scoped?** [#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54) has no design yet. If cache updates are shot from a fixed set of cells, culling them to the visible set + margin is a real second-order win; if updates are spread over time, it is not. This belongs to #54, not here.
- **Debris and Fluids** are dynamic instances, and the frustum test pays properly there (a TLAS instance test is a real cull). [#76](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/76) should carry the numbers; nothing in this note applies to them unchanged.
- **Is a 2× win worth the cave risk in orbit?** The orbit view is the one place the horizon cap is a clean 2×, and it is also the view where a hole is most visible against the starfield. The answer is probably yes because the far hemisphere is *provably* unreachable from outside, but the oracle should run on an orbit path with the camera just above the terrain before anyone relaxes.

---

## References

Repo (first-party):
- [`crates/render/src/shaders/trace.wgsl`](../../crates/render/src/shaders/trace.wgsl) — the DDA, the AABB clip, `MAX_REGION_STEPS`.
- [`crates/render/src/scene.rs`](../../crates/render/src/scene.rs) — regions, `origin`/`dims`, `Core`, `SceneUpdate`.
- [`crates/render/src/reference.rs`](../../crates/render/src/reference.rs) — the CPU mirror the oracle would instrument.
- [ADR 0002](../adr/0002-brick-store-all-voxel-lod.md), [#8](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8), [#9](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/9), [#10](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/10), [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48)–[#51](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/51), [#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54), [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64), [#77](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/77).

External (primary):
- U. Assarsson, T. Möller, "Optimized View Frustum Culling Algorithms for Bounding Boxes", *journal of graphics tools* 5(1), 2000. [PDF](https://www.cse.chalmers.se/~uffe/vfc_bbox.pdf)
- K. Ring, "Horizon Culling", Cesium, 2013. [Blog](https://cesium.com/blog/2013/04/25/horizon-culling) · [`EllipsoidalOccluder` reference](https://cesium.com/learn/cesiumjs/ref-doc/EllipsoidalOccluder.html) · "Computing the horizon occlusion point" ([blog](https://cesium.com/blog/2013/05/09/computing-the-horizon-occlusion-point/))
- "Tighter Frustum Culling and Why You May Want to Disregard It", Cesium. [Blog](https://cesium.com/blog/2017/02/02/tighter-frustum-culling-and-why-you-may-want-to-disregard-it)
- N. Greene, M. Kass, G. Miller, "Hierarchical Z-Buffer Visibility", SIGGRAPH '93. [DOI](https://doi.org/10.1145/166117.166147)
- U. Haar, S. Aaltonen, "GPU-Driven Rendering Pipelines", SIGGRAPH 2015 Advances in Real-Time Rendering. [PDF](https://advances.realtimerendering.com/s2015/aaltonenhaar_siggraph2015_combined_final_footer_220dpi.pdf)
- Epic Games, "Lumen Technical Details". [Docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/lumen-technical-details-in-unreal-engine)
- J. Amanatides, A. Woo, "A Fast Voxel Traversal Algorithm for Ray Tracing", Eurographics '87. [DOI](https://doi.org/10.2312/egtp.19871000) · [PDF](http://www.cse.yorku.ca/~amana/research/grid.pdf)
- S. Teller, C. Séquin, "Visibility Preprocessing for Interactive Walkthroughs", SIGGRAPH '91. [DOI](https://doi.org/10.1145/122718.122725)
- D. Luebke, C. Georges, "Portals and Mirrors: Simple, Fast Evaluation of Potentially Visible Sets", I3D '95. [DOI](https://doi.org/10.1145/199404.199422)
