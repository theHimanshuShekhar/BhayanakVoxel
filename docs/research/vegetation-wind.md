# Research: wind animation for voxel vegetation in a path tracer

Resolves issue [#152](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/152), part of the inspirations map [#130](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/130). It follows [Decide which inspiration features to adopt](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/138), which adopted vegetation wind sway for M1b, subject to this research. Researched November 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). Voxels are **5 cm** ([#135](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/135); the switch is [#147](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/147), not built yet, so the code still says 10 cm).

Every factual claim links to its source. **[estimate]** marks my own arithmetic, with the working shown. **[judgement]** marks engineering opinion. **[unverified]** marks what I could not check against a primary source. No GPU was run for this ticket. The 4090 numbers quoted here are the dev's own bench reports on merged PRs.

## TL;DR

- **Nobody has published a method for swaying voxels in a ray tracer, and the two closest projects both found it hard.**
  - John Lin did show trees blowing in the wind, in September 2020 and in his *older* engine (S1, S2). That engine had just moved to "a custom ray tracer written with Vulkan Compute" with "an extremely lightweight acceleration structure that actually allows for this type of dynamic scene" (S3). He never said how the animation works.
  - Teardown's lead developer tried tree sway and "could never really get it to work properly with lighting because of the shadow volume". He found ray tracing voxels in a skewed box "a *much* harder problem than I anticipated" and solved it only approximately (S5).
- **Correction to the Crystal Islands research.** Lin's Crystal Islands description says only "animation support" (S4); it never mentions wind. The "Gentle breeze animations and leaf palettes" quote that doc credited to Lin (its S7, unverified) is by a different developer, @voxelbee, per two search indexes (S6). Lin's real first-party wind evidence is his 2020 post and video.
- **Three of the ticket's candidate methods conflict with decisions already made:**
  - **Re-voxelising animated plants into their bricks every frame** would mean about 0.8 M canopy bricks and ~100–160 MB rewritten per frame at 5 cm, plus ~120 ms of scene-thread repacking **[estimate, §6]**. It also breaks "unedited bricks are a cache" ([ADR 0002](../adr/0002-brick-store-all-voxel-lod.md), [ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md)) and #136's rejection of deformable voxels. Rejected.
  - **Rigid instances per plant or branch cluster on the Debris path:** vegetation is Voxels in the Planet's bricks precisely so that Edits work ([dynamic-content.md §2.1](dynamic-content.md), V1/V8). The instance path is M3. Branch clusters would need ~10⁵ instances. Rejected for M1b.
  - **General ray bending** (Barr 1986; Kurzion & Yagel 1995) needs curved rays. That is the problem Teardown could only approximate. Rejected.
- **What works here is moving the rays, not the store: "sway in the tracer".** Vegetation stays static Voxels in bricks. The store, the generator, collision, Edits, the server and the acceleration structures never see wind. A closed-form wind field `w(x, t)`, evaluated in the shaders, drives two tiers:
  - **Tier A, shading flutter (M1b, both backends).** Foliage Materials tilt their shading normal, and slightly their brightness, with Crysis-style triangle waves (S8). No geometry moves, so there are no AS updates and no motion vectors. Cost ≈ 0 (< 0.05 ms).
  - **Tier B, geometric sway (M1b spike, hardware backend only).** Each brick holding a *sway-class* Material moves its sway Voxels rigidly by a small offset `d_b(t)`, at most **one Voxel (5 cm)**. The offset is applied to the ray inside the brick intersection, and the brick's AABB is inflated by one Voxel once, when the BLAS is built. Rigid per brick means the in-brick DDA stays exact. Overlapping inflated AABBs are resolved by the ray query's closest-hit rule. **No per-frame BLAS or TLAS work.** Only level 0 sways (out to 2·d0 ≈ 103 m at 5 cm), fading to zero before the level-1 boundary. Primary writes an analytic per-pixel object motion for sway hits so that TAAU and the denoiser keep their history.
- **Cost on the 4090 [estimate, low confidence, §6]:** Tier A < 0.05 ms. Tier B adds about 0.15–1.0 ms (central ~0.45 ms) across all trace passes in a dense forest view, because inflated sway AABBs give 1.6–2.3× more foliage candidates. At the full 103 m radius it will likely exceed the budget line below, so the spike should expect to pull the radius knob. The motion texture adds ~0.01–0.02 ms.
- **Recommended budget line:** **vegetation wind ≤ 0.3 ms of GPU time**, measured as wind-on minus wind-off on a new dense-forest bench view, inside the existing 8.6 ms. It adds **zero** BVH work and **zero** brick uploads. If Tier B is over, shrink its radius, then fall back to Tier A alone.
- **Decisions this forces (for the map):**
  1. Wind lives only in the tracer; the generator stays time-independent.
  2. The wind clock is client-only and cosmetic. It freezes while the Reference accumulates a still view, which completes #101's "nothing animating" rule, and it runs on a fixed step in the bench and the goldens.
  3. TAAU gains per-pixel object motion for sway hits.
  4. Tier B depends on [Hardware RT trace()](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/50) and amends its tight-AABB line ("sway bricks: tight box + 1 Voxel").
  5. If #50's bench makes compute the default, M1b ships Tier A alone.

## Sources and how they were used

| Key | Source | Type | Used for |
|---|---|---|---|
| **S1** | J. Lin, ["Animated Voxel Trees - Detail Enhancement Preview"](https://www.youtube.com/watch?v=BObFTsNeeGc), YouTube, 27 Sep 2020, 31 s, 720p30; description and frames pulled with yt-dlp | **First-party** video | Description: "an experimental enhancement to the engine that finally enables trees to blow in the wind". 1 fps contact sheet and nearest-neighbour crops at 0:16 and 0:18. No captions |
| **S2** | [@ProgrammerLin, 26 Sep 2020](https://x.com/ProgrammerLin/status/1309866612065878017), read through `cdn.syndication.twimg.com/tweet-result` | **First-party** | "No voxel is safe from the wind anymore, thanks to finally implementing the #1 most requested feature - animated trees! Sound on!" |
| **S3** | J. Lin, ["Exploring an Infinite Voxel Forest"](https://www.youtube.com/watch?v=1wufuXY3l1o), YouTube, 16 Jun 2020, description | **First-party** | The engine moved from RTX to "a custom ray tracer written with Vulkan Compute … an extremely lightweight acceleration structure that actually allows for this type of dynamic scene" |
| **S4** | J. Lin, ["New Voxel Engine Reveal - Crystal Islands Experiment"](https://www.youtube.com/watch?v=8ptH79R53c0), YouTube, 13 May 2021, description | **First-party** | Lists "animation support". No mention of wind |
| **S5** | D. Gustafsson, ["Year summary"](https://blog.voxagon.se/2024/12/29/year-summary.html), blog.voxagon.se, 29 Dec 2024 | **First-party** blog | Teardown tree-sway experiments failed on lighting; deformable voxels in a skewed box are "a *much* harder problem", solved approximately; "vast amounts of moving objects at a very small cost" once the shadow volume is gone |
| S6 | Web-search snippets of [@voxelbee's X profile](https://x.com/voxelbee) (two separate queries) | Search snippet, **status ID not found** | The "Gentle breeze animations and leaf palettes … higher res #voxel tree (and vegetation) models" post is voxelbee's, not Lin's. **[unverified]** beyond the snippets; dated about 2016 by the index |
| **S7** | Epic Games, [Ray Tracing Performance Guide](https://dev.epicgames.com/documentation/en-us/unreal-engine/ray-tracing-performance-guide-in-unreal-engine) (UE 5.8 docs) | **First-party** engine docs | The cost of World Position Offset (WPO) wind in a ray tracer: per-frame BLAS rebuilds, unique memory per instance, a 50 m default radius, and self-intersection artefacts when ray tracing ignores the animation |
| **S8** | T. Sousa, ["Vegetation Procedural Animation and Shading in Crysis"](https://developer.nvidia.com/gpugems/gpugems3/part-iii-rendering/chapter-16-vegetation-procedural-animation-and-shading-crysis), GPU Gems 3 ch. 16, 2007 | **First-party** (Crytek author) | Main and detail bending, triangle-wave approximation, frequencies, phase from world position, per-instance wind vectors |
| **S9** | Zioma, ["GPU-Generated Procedural Wind Animations for Trees"](https://developer.nvidia.com/gpugems/gpugems3/part-i-geometry/chapter-6-gpu-generated-procedural-wind-animations-trees), GPU Gems 3 ch. 6, 2007 | First-party | Rigid branch hierarchy (2–3 levels, ~80 branches a tree), wind as a 2D force field, stateless, instancing-compatible |
| **S10** | L. Yang, S. Liu, M. Salvi, ["A Survey of Temporal Antialiasing Techniques"](http://behindthepixels.io/assets/files/TemporalAA.pdf), CGF 39(2), 2020 | Peer-reviewed survey | How TAA handles moving objects: motion vectors for animated objects only, depth plus camera for the rest; shading changes that motion vectors miss |
| S11 | A. Barr, "Ray tracing deformed surfaces", SIGGRAPH 1986, [doi:10.1145/15886.15918](https://doi.org/10.1145/15886.15918); Y. Kurzion, R. Yagel, ["Space Deformation using Ray Deflectors"](https://link.springer.com/chapter/10.1007/978-3-7091-9430-0_3), EGWR 1995 | Papers (**abstracts only**) | Deforming the rays instead of the objects. Kurzion & Yagel: "we deform the rays used to render the scene" |
| S12 | B. Lloyd, O. Klehm, M. Stich, ["Implementing Stochastic Levels of Detail with Microsoft DirectX Raytracing"](https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/), NVIDIA, 2020 (already cited in [dynamic-content.md](dynamic-content.md)) | First-party vendor | All rays along a path must see the same geometry; 0.22 ms TLAS build for 3,840 instances on a 2080 Ti |
| S13 | wgpu / naga **30.0.1** source, our pinned version (`Cargo.lock`) | Installed dependency | `RayQueryFunction::GenerateIntersection { hit_t }`, AABB candidates (`RayQueryCandidateIntersectionAABBKHR`), `AccelerationStructureFlags::{ALLOW_UPDATE, PREFER_FAST_TRACE}` |
| S14 | This repo at `146cae6` (`main`): `crates/render/src/shaders/{trace,primary,upscale,pathtrace,common}.wgsl`, `crates/render/src/{tracer,lod,camera,upscale}.rs`, `crates/planet-gen/src/*`, `bins/client/bench-budgets.toml`; the 4090 bench comments on PRs [#126](https://github.com/theHimanshuShekhar/bhayanakvoxel/pull/126) and [#128](https://github.com/theHimanshuShekhar/bhayanakvoxel/pull/128) | Our code and measurements | What the engine can do today, and the cost baseline |

## 1. What the engine has today

Read from `main` at `146cae6` (S14).

- **No vegetation exists yet.** `planet-gen` has no tree, foliage or leaf code; the nearest thing is the `GRASSY_SOIL` Material (`crates/planet-gen/src/planet.rs`). The terrain decision puts vegetation in the per-brick "decoration pass … at any LOD" ([#14](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/14)). [dynamic-content.md §2](dynamic-content.md) settled that it stays Voxels in bricks, generated per LOD level from prefab species. The art direction asks for "leaf-cluster masses" and individually shaped large leaves ([#135](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/135), item 10).
- **One `trace()` serves every ray.** `primary.wgsl` and the Reference `pathtrace.wgsl` both call `trace.wgsl`'s `trace()`, and the Reference uses it for shadow and bounce rays too. Inside a brick, `hit_brick` walks the occupancy mask with a DDA and reads the Material from the palette. `reference.rs` is a line-for-line CPU copy (`trace.wgsl` header).
- **TAAU reprojects from the camera only.** `upscale.wgsl` rebuilds each pixel's point from the G-buffer hit distance, moves it by the camera shift (`from_previous = dir * t + shift`) and projects it with the previous camera. It then clamps the history to the 3×3 neighbourhood and drops the history if the clamp moved it by more than `CLAMP_REJECT = 0.1`. There is no per-pixel object motion. The G-buffer is `rg32uint`, holding the hit distance plus packed albedo and face.
- **The Reference resets on `View { camera, sun, size }`** (`tracer.rs`). [#101](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/101) defined "still" as "camera, sun and size unchanged, no Edit and **no animation**, for 2 frames; enter slowly, leave instantly".
- **The hardware path is not built.** [#50](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/50) will add wgpu ray queries over one procedural AABB per non-empty brick, with the in-brick DDA run on each candidate through `GenerateIntersection` (S13). It keeps the compute DDA as the fallback, uses tight AABBs, and benches both backends at 5 cm.
- **Measured 4090 baseline** (1440p output, 1280×720 traced, compute DDA, planet flyover; PR #128's bench): `primary` 0.261–0.267 ms, `upscale` 0.083 ms, `gpu_total` ≈ 0.35 ms. A cave-mouth view costs 0.54–0.60 ms in primary (PR #126), so views vary about 2×.
- **Distances at 5 cm.** `lod.rs` gives d0 = 1028.2 Voxels at 1440p and 70° (its own test). That is **51.4 m** at 5 cm, so level 0 owns `[0, 2·d0)` = **0–103 m**. A 5 cm displacement covers `51.4 / D` output pixels at distance D: 10 px at 5 m, 2.6 px at 20 m, 1 px at 51 m, 0.5 px at 103 m **[estimate, from the band rule]**.

## 2. Prior art

### 2.1 John Lin

- **What he showed.** In September 2020 he posted animated trees as "the #1 most requested feature" (S2), with a 31-second clip (S1). The clip holds a nearly still camera on a sunlit conifer forest with god rays. The foreground crown moves visibly between 0:16 and 0:18. Its silhouette stays axis-aligned and stair-stepped in both frames. That fits translation or re-voxelisation, but not a rotated voxel grid. The footage cannot tell those two apart.
- **Which engine.** This was his old engine (S1, 2020). Three months earlier he had dropped RTX for a Vulkan compute tracer with a light acceleration structure that "actually allows for this type of dynamic scene" (S3). Inference: the animation relied on an acceleration structure cheap enough to rebuild. He never said so, and he never described the method.
- **Crystal Islands (2021)** went back to the Vulkan hardware RT pipeline ([voxel-path-tracing.md §1](voxel-path-tracing.md)). It lists "animation support" (S4), but the flythrough never holds still long enough to show any ([Crystal Islands research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-crystal-islands/docs/research/inspiration-john-lin-crystal-islands.md) §5).
- **Takeaway:** the look is proven to read well in a path-traced voxel forest, especially the dappled sun through moving leaves. The method is unknown. Do not design around guesses about it.

### 2.2 Teardown and Tuxedo Labs

- Teardown's trees don't sway. Gustafsson's experiments failed because the shadow volume could not follow them (S5).
- His next engine's "deformable voxels" (bending volumes) need ray tracing inside a skewed box. Eberly's verdict was that "an invertible affine transformation between a unit cube and an arbitrarily deformed cuboid does not exist", so Gustafsson uses "an approximate solution that is good enough for my use case" (S5). We already rejected deformable voxel objects ([#136](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/136); [Teardown research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-teardown-rt/docs/research/inspiration-teardown-rt.md) §7 row 19).
- **What carries over:** with hardware RT and no shadow volume, the new engine handles "vast amounts of moving objects at a very small cost" (S5). Moving geometry is cheap when what moves is a whole rigid thing the AS can hold as is. That is the property Tier B keeps, per brick and without touching the AS.

### 2.3 Mesh engines (the established method, for contrast)

- **Crysis** (S8) splits wind into:
  - **main bending:** the whole plant moves along the wind, scaled by normalised height, with a length correction for "spherical limited movement";
  - **detail bending:** the leaves move, from "four vectorized triangle waves" in place of sines. The phase comes from world position (`dot(worldPos.xyz, 1)`), and "good frequencies" are 1.975, 0.793, 0.375 and 0.193.
  Wind is a per-instance vector summed from directional and omnidirectional sources, "very similar … to light sources", and "wind variation is done on the CPU".
- **GPU Gems 3 ch. 6** (S9) uses a rigid branch hierarchy, "two to three nodes deep", with ~80 branches per tree. It treats wind as "a two-dimensional force field over terrain", animates with noise or periodic functions, and is stateless: it "doesn't require information from previous iterations".
- **Unreal with ray tracing** (S7) shows what vertex wind costs in a ray tracer:
  - WPO "requires dynamically rebuilding the BLAS, as well as unique memory for each instance";
  - on instanced foliage it "effectively converts every instance in a single mesh with a separate BLAS";
  - it is evaluated only "for a short distance around the camera, 50 meters unless otherwise specified";
  - ray tracing ignores WPO by default, which "can cause self-intersections artifacts".

**Takeaways for us [judgement]:**
- Every engine uses a *stateless closed-form* wind function, which is what makes it cheap and deterministic.
- Every engine limits geometric wind to a radius around the camera.
- A ray tracer either pays per-frame BLAS work or accepts that the rays see a different shape than the screen. Tier B avoids both.

## 3. Candidate methods for a ray-traced voxel world

### 3.1 Per-frame re-voxelisation into the bricks: **reject**

Each frame, write the plants' Voxels at their swayed pose into the Planet's bricks, then repack and re-upload them and rebuild their BLASes.

- **Cost at 5 cm [estimate].** Take a dense forest (one tree per 20 m², 4 m crowns, as in [dynamic-content.md §2.5](dynamic-content.md)) over level 0's 103 m disc: π·103² ≈ 33,200 m² gives ~1,660 trees. A 4 m crown in 0.4 m bricks touches ~470 of the 1,000 bricks in its box (the same shell fraction as the 10 cm estimate's 78 of 125, at twice the resolution). That is **~780k bricks** to rewrite each frame, at 136–204 B each (the packed-brick table in [dynamic-content.md](dynamic-content.md)): **~106–159 MB per frame, 6–10 GB/s**. On the CPU, the canopy layer is ~2 regions (3.2 m each at 5 cm) deep over ~3,200 region columns, so ~6,500 regions × 18 µs of repacking each ([ADR 0006](../adr/0006-scene-thread.md)) ≈ **117 ms a frame** on the scene thread. Each changed region's BLAS would also need a rebuild against the 1 ms Edits+BVH line.
- **Conflicts:**
  - The generator would depend on time, so "unedited bricks are a cache" stops holding (ADR 0002, ADR 0003; [dynamic-content.md §2.3](dynamic-content.md) "Wind or animation").
  - Collision and aiming read the same store.
  - #136 rejected deformable voxels.
- **Looks wrong anyway:** motion is quantised to whole Voxels, so leaves crawl a Voxel at a time. Displacement below one Voxel, which is most of a breeze at 5 cm, cannot show at all.

### 3.2 Rigid instances per plant or branch cluster, on the Debris path: **reject for M1b; watchlist after M3**

Each plant, or each branch cluster, becomes an object-local brick grid traced as a TLAS instance whose transform sways, the path Debris takes in M3 ([#18](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/18), amended by #136).

- **What's attractive:** exact, sub-Voxel, rotational sway. The TLAS rebuild is cheap at plant granularity: 3,840 instances in 0.22 ms on a 2080 Ti (S12), so ~1,660 trees cost well under 0.1 ms on a 4090 **[estimate]**.
- **Why not:**
  - It takes the plants out of the Planet's bricks. That is the two-representation design [dynamic-content.md §2.1](dynamic-content.md) rejected (V8), because "the tree you can see is the tree you can cut".
  - The instance path and its compute-fallback structure (D9) are M3 work.
  - Branch clusters at S9's ~80 per tree would mean ~130k instances, roughly 7.6 ms of TLAS build on a 2080 Ti by linear scaling of S12 **[estimate]**.
  - A rotating object grid shows the Debris look (rotated stair-steps), not a voxel forest.
- **Later:** a few hero plants (a giant flower, a hanging vine) could use it once instancing exists, if look-dev wants big rotations.

### 3.3 Bending rays or space at foliage hits: **reject in general; adopt its piecewise-rigid special case (Tier B)**

Barr traces deformed surfaces by deforming the ray; Kurzion & Yagel "deform the rays used to render the scene" (S11). For a non-rigid deformation the ray becomes a curve in rest space, and the grid DDA no longer applies. That is exactly where Gustafsson ended up with an approximation (S5). But if the deformation is **constant over each brick** (a rigid translation), the bent ray is a straight ray in each brick's frame, and the existing `hit_brick` DDA is exact on the shifted ray. Tier B (§3.5) is that special case.

### 3.4 Animating only a foliage Material class: **adopt, as two tiers**

- **Time-varying occupancy** (flicker leaf Voxels on and off): reject. It is noise by construction, and TAAU's clamp would reject the history on every flickering pixel.
- **Time-varying shading** of a foliage class: **Tier A** (§3.6).
- **Time-varying offset** of a foliage class: **Tier B** (§3.5).

### 3.5 Tier B: per-brick rigid offset in the brick intersection

**How it works:**

1. **Sway class.** Some Materials (canopy leaves, large leaves, flower heads) carry a `sway` stiffness in `materials.toml`. Grass and moss caps don't: they sit in ground bricks, where Tier B would double the DDA for the whole ground (§6). A brick is a *sway brick* if its palette holds a sway-class Material. That is a flag computed when the brick is packed, recomputed after an Edit.
2. **Offset field.** `d_b(t) = m · clamp(k_mat · s(t) · g(x_b − v·t), −1, 1) · ŵ(t)`, roughly:
   - `x_b` is the brick centre;
   - `ŵ` is the wind direction in the local tangent plane (the Planet is a sphere, so "horizontal" is perpendicular to the radial direction at `x_b`);
   - `s(t)` is a slowly varying strength;
   - `g` is a smooth gust noise scrolled downwind, after S9's 2D force field;
   - `m` is the bound, **1 Voxel**.
   The field must be smooth at brick scale: wavelength ≥ ~16 m, so that neighbouring bricks differ by at most ~0.8 cm, a sixth of a Voxel (**[estimate]**: the steepest slope of a 5 cm wave 16 m long is 2π·0.05/16 ≈ 0.02, times the 0.4 m brick). Fast flutter belongs to Tier A. The field is stateless, like S8 and S9.
3. **Hardware path.** Each sway brick's AABB is its tight box plus `m` on every side, built once (#50 already rebuilds a brick's box on Edits). On a candidate sway brick, the intersection runs `hit_brick` twice:
   - the plain ray against non-sway Voxels;
   - the ray shifted by `−d_b(t)` against sway Voxels;
   It then reports the nearer through `GenerateIntersection` (S13). Overlapping inflated boxes are each reported as candidates, and the ray query keeps the closest hit, so the result is exact whatever order they come in. **Nothing in the BLAS or TLAS changes per frame.**
4. **Compute fallback: no Tier B.** The compute march visits brick cells in order. Content shifted out of its cell would need each visited cell to test up to 26 neighbours, which is a second traversal design **[judgement]**. The fallback renders sway Materials at rest, with Tier A.
5. **LOD.** Level 0 only. The amplitude fades to zero across `[d0, 0.95·2·d0)` (51–98 m at 5 cm), so it is gone before the level-1 boundary and its hysteresis (`lod.rs`, `HYSTERESIS = 0.05`). At 103 m, 5 cm is 0.5 px anyway (§1).
6. **Every ray type sways.** Primary, shadow, GI, reflection and Reference rays all call the same `trace()`. Rays that see a different shape than the screen cause leaks and self-intersection (S12; S7's "self-intersections artifacts").

### 3.6 Tier A: shading flutter

For sway-class Materials, plus grass, moss and flower caps, tilt the shading normal about the axis across the wind. The angle comes from Crysis-style smoothed triangle waves at S8's frequencies, phased by world position (S8), with a small albedo or brightness modulation. #135 computes smooth normals from neighbouring occupancy, and Tier A rotates the result. It runs on every backend and every level, and fades out with distance where the flutter would average away inside a pixel **[judgement]**. No geometry moves.

## 4. Effects on the pipeline

- **Motion vectors (TAAU, later the denoiser).**
  - Today's TAAU assumes static geometry (§1). A swaying leaf reprojects to the wrong history, and the clamp throws it away. Sway pixels then lose TAAU's accumulated supersampling and look aliased and noisy.
  - Use the standard split (S10): "some engines only explicitly compute and store motion vectors for animated and moving objects", and the other pixels are reprojected "using the depth buffer … and the camera matrices".
  - Primary writes `Δ = d_b(t) − d_b(t_prev)` for Tier B hits, and zero elsewhere, into a small motion texture (or a widened G-buffer). The upscaler uses `from_previous = dir·t + shift − Δ`. Δ is analytic because the field is closed-form.
  - Tier A needs nothing: like a moving shadow, it is a shading change that the clamp already handles. S10 warns that relaxing the clamp ghosts "animated textures, since these changes are not reflected by the motion vectors", so keep the clamp.
- **Acceleration structures:** no change per frame (§3.5.3). The Edits+BVH line is untouched.
- **Shadows and GI.**
  - Moving leaves cast moving dappled shadows on the static ground, which is the look in S1. Those ground pixels have no motion, so the denoiser sees a lighting change. A-SVGF's temporal gradients exist for exactly this ([voxel-path-tracing.md §4](voxel-path-tracing.md)). The plain accumulator will lag or clamp.
  - The world-space radiance cache will lag a little behind the swaying canopy. That is a sub-Voxel change to one-bounce light and invisible **[judgement]**.
- **The Reference stays comparable:**
  1. The Reference runs the same `w(x, t)` code at the same wind time (one shader include).
  2. The **wind clock is part of the View key and freezes while the Reference accumulates a still view**. Under #101's rule "still" requires "no animation", so with wind always running the Reference would never accumulate. Freezing completes the rule ([#94](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/94)'s Reference must accumulate over a fixed scene). It resumes from the frozen value on leaving, so nothing jumps. This is the same pattern as #120, where loading pauses only while the Reference accumulates a still view.
  3. The bench and the goldens drive wind time from the frame index (a fixed step), so runs repeat.
  4. #50's "hardware and compute produce matching images" goldens run with Tier B off, because compute has no Tier B. Tier B gets its own CPU test: the shifted-ray `hit_brick` in `reference.rs` against the rest brick translated by `d`.

## 5. Effects on gameplay

- **Collision and Edits never see wind** (as #138 requires). Physics, the server and Edits all read the store at rest. The wind clock is client-only and is never replicated, because sway is cosmetic and clients may differ in phase **[judgement]**.
- **Aiming and the Ghost** use the frame thread's CPU copy of the scene ([ADR 0006](../adr/0006-scene-thread.md)), which is at rest. With `m` = 1 Voxel, the Voxel you aim at can be off by at most one Voxel, the Edit grain. The Ghost previews the rest Voxels, so swaying leaves may sit up to 5 cm off its outline. That is acceptable at a breeze **[judgement]**.
- **Edits on foliage** work unchanged. A cut tree's remaining leaves keep swaying by Material, and placed leaf Material sways too. Debris made of leaves is rigid and doesn't sway.
- **At distance:** Tier B stops at level 0 (≤ ~98 m). Tier A carries the impression further, and coarse foliage generated per level (V3) keeps its Material, so the flutter applies at every level.

## 6. Cost on the 4090 at 5 cm, dense forest view

All **[estimate]**, low confidence. Nothing here can be measured until vegetation (M1b) and #50 exist.

| Item | Working | Estimate |
|---|---|---|
| Forest-view trace cost, wind off | Primary 0.26 ms on the flyover (PR #128), with ~2× view variation (PR #126) and +15–45% for a forest ([dynamic-content.md §2.5](dynamic-content.md)): primary ~0.35–0.6 ms. Shadow and GI bounce rays through the canopy ~0.3–0.5 ms each | ~1.0–1.5 ms of tracing |
| Foliage share of that tracing | The +15–45% forest increment of primary is foliage; shadow rays from the ground through the canopy are mostly foliage | 25–50% |
| Tier B multiplier on foliage tracing | Inflating a tight box w Voxels wide by 1 on each side multiplies its cross-section by ((w+2)/w)²: 1.56× at w = 8 and 2.25× at w = 4, so more candidates. Add the second `hit_brick` in mixed static and sway bricks, and ~20 ALU for `d_b` per candidate | 1.6–2.3× |
| **Tier B delta** | 1.0–1.5 ms × 25–50% × (0.6–1.3); midpoints 1.25 × 37.5% × 0.95 | **~0.15–1.0 ms, central ~0.45 ms** |
| Tier A | Four triangle waves plus one noise per foliage shading hit, ~40–80 ALU, on ~0.9 M traced pixels per shading pass | **< 0.05 ms** |
| Motion texture | 8 B per traced pixel written by primary and read by upscale, 7.4 MB a frame; the whole upscale pass costs 0.083 ms today | **~0.01–0.02 ms** |
| BVH / uploads | None (§3.5.3) | **0** |
| Re-voxelisation instead (§3.1), for comparison | ~780k bricks, 106–159 MB a frame, ~117 ms CPU | out of budget by 100× |

The central estimate is above the 0.3 ms line in §7. That is deliberate: the 8.6 ms total is fully allocated, and Tier B has to earn its place. **Knobs, in the order to pull them** if Tier B is over:
1. Narrow the sway radius from 2·d0 to d0 (51 m). In a dense forest most foliage pixels are near the camera, so this saves less than the 4× area drop suggests.
2. Shrink the sway class (drop flowers, keep crowns).
3. Ship Tier A alone.

Raising `m` to 2 Voxels (10 cm, for stronger wind) multiplies the candidate term to 2.25–4×. It is a strong-wind setting only.

## 7. Recommendation

**Sway in the tracer, never in the store:**

- **M1b, with the vegetation generator:**
  - the closed-form wind field and a client-only wind clock that freezes while the Reference accumulates;
  - **Tier A shading flutter** on both backends;
  - a wind on/off/freeze toggle and an amplitude multiplier for look-dev;
  - a displacement and motion-vector debug view.
- **M1b spike, after #50 and the vegetation generator: Tier B**, with:
  - 1-Voxel per-brick rigid offsets on the hardware path, level 0 only, fading out by 98 m;
  - inflated sway AABBs built once;
  - analytic per-pixel motion for TAAU.
  Adopt it if (a) the forest bench delta stays within the budget line, (b) Primary's FLIP against the Reference at a frozen wind time is no worse than with wind off (the #98 metric), and (c) the look-dev flythrough ([#148](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/148)) reads as sway. If #50 makes compute the default backend, ship Tier A alone and revisit Tier B with Debris instancing in M3.
- **Not now:** per-frame re-voxelisation (rejected), plant or branch instances (watchlist after M3), general ray bending (rejected).

**Budget line:** **Vegetation wind ≤ 0.3 ms GPU** on the 4090 at 1440p (1280×720 traced), measured as wind-on minus wind-off on a new dense-forest bench view, summed over all passes. **0 ms of BLAS/TLAS work and 0 brick uploads.** It sits inside the existing ≈ 8.6 ms ([#10](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/10)) as headroom carried by the primary, shadow and GI lines (primary measures 0.26 ms against 1.0 today), not as a new top-level line. Within it, Tier A gets ≤ 0.05 ms.

## 8. Risks for a graphics-beginner team working with AI agents

- **Motion-vector bugs are invisible in stills.** A missing or wrong Δ shows as smearing or shimmer only in motion, and agents judge from text. *Mitigation:* a motion debug view; a test that a swayed brick's reprojected point lands on its previous-frame hit; a playtest clip, not just goldens.
- **Two backends, two images.** Tier B is hardware-only, so the compute fallback and CI (lavapipe) never exercise it. *Mitigation:* the shifted-ray `hit_brick` CPU test in `reference.rs`; goldens run with Tier B off; the 4090 bench runs it on.
- **Clipping at brick faces** if someone forgets the AABB inflation, or applies it before #50's tight-box step instead of after. It shows as leaves sliced along 40 cm planes. *Mitigation:* the same CPU test with an offset that pushes Voxels across a face; the #149 traversal heatmap shows the inflated boxes.
- **Self-intersection or light leaks** if any ray type (a future reflection or glass ray) skips the offset (S7, S12). *Mitigation:* the offset lives inside `trace()`, never in a caller.
- **A Reference that silently accumulates a blur of sway** if the wind clock is left out of the View key, which is a bias the 64-spp golden cannot see (#98). *Mitigation:* a unit test that a wind-clock change resets accumulation.
- **Visible brick seams on solid leaf masses** if the field varies too fast. *Mitigation:* keep the wavelength ≥ 16 m and fast motion in Tier A; check in look-dev.
- **Dappled-shadow shimmer through the denoiser**, which motion vectors cannot fix (S10). *Mitigation:* expect it until A-SVGF's gradients land; tune the flutter amplitude down rather than relaxing the clamp.
- **Cost is a wide estimate** (0.15–1.0 ms for Tier B), and no part of it is measured. *Mitigation:* the spike is gated on the bench delta, with the knobs in §6.
- **Scope creep toward branch hierarchies** (S9) or physical wind. The look needs neither **[judgement]**.

## 9. Cross-check against what is already decided

| # | Finding | Status | Decision it touches |
|---|---|---|---|
| 1 | Wind never enters the generator or the store; unedited bricks stay a cache | **Already decided** (consistent) | ADR 0002, ADR 0003; dynamic-content.md V9 (its "makes the generator time-dependent" objection no longer applies) |
| 2 | Per-frame re-voxelisation | **Conflicts** | ADR 0002/0003 cache rule; #136 (deformable voxels rejected); ADR 0006 (scene-thread cost) |
| 3 | Plants as rigid instances | **Conflicts** for M1b | dynamic-content.md V1/V8 (vegetation is Voxels in bricks); #18 and #136 (instances are M3 Debris) |
| 4 | Tier B's per-brick rigid offset is not a "deformable voxel object": no volume bends, and the store is untouched | **Already decided** (consistent with #136's rejection) | #136, ADR 0002 |
| 5 | Sway never affects collision or Edits | **Already decided** | #138 |
| 6 | Wind clock freezes while the Reference accumulates; fixed step in bench and goldens | **New** (completes #101's "no animation" rule) | #101, #94, #120 |
| 7 | Wind clock is client-only and cosmetic, separate from the world clock | **New** | #138 (the world clock is server-owned from M2) |
| 8 | TAAU gains per-pixel object motion for sway hits | **New** | [#51](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/51) (TAAU, closed), #10 |
| 9 | Tier B needs the hardware backend; sway bricks' AABB = tight box + 1 Voxel | **New** (amends #50) | #50, #10 |
| 10 | Budget: ≤ 0.3 ms inside the 8.6 ms, 0 BVH work | **New** | #10 frame budget, `bench-budgets.toml` |
| 11 | Tier A rotates the computed smooth normals | **Already decided** (builds on) | #135 item 8 |
| 12 | No motion blur needed; sway is shown sharp | **Already decided** | #135 ("clean post") |
| 13 | Amplitude and radius in metres (5 cm, level 0 to 103 m) | **Already decided** | #135, #147 |
| 14 | Wind audio pairs with sway (Lin's clip is "Sound on!") | **Already decided** | #138 (audio floor includes "water and wind") |

## 10. Corrections to earlier research

- **[Crystal Islands research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-crystal-islands/docs/research/inspiration-john-lin-crystal-islands.md)** (S7 row, §5 "Animation of voxel models (wind)", and the closing "unverified" list):
  - The "Gentle breeze animations and leaf palettes" quote is @voxelbee's, not Lin's (S6).
  - The Crystal Islands description says "animation support", not wind (S4).
  - Lin's wind is first-party, but from his 2020 compute-traced engine (S1–S3).
- **[dynamic-content.md](dynamic-content.md) §2.3 and V9** put wind on the watchlist because it "would make the generator time-dependent". This research shows a route that leaves the generator alone. The watchlist row can point here.

## Open questions this research raises

- Does wgpu 30's experimental ray query on the 4090 report overlapping procedural AABBs efficiently enough for Tier B? Only #50's hardware path can answer that.
- How strong should the wind be? It is a look-dev question for #148. One Voxel is a breeze; a storm would need `m` = 2 and its cost.
- Should sway amplitude grow with height above the plant's base, like Crysis' main bending? Per-brick offsets don't know the base. A per-Material stiffness may be enough, or the generator could write a 2-bit height class into the palette at no time cost.

## Unverified or not checked

- **Lin's method** is undocumented (S1–S4). The animated-trees video has no captions or uploader comments (yt-dlp, top 200 comments). The reading that his compute AS was rebuilt per frame is an inference from S3.
- **The voxelbee attribution (S6)** rests on two search-index snippets; the status ID was not found.
- **Barr 1986 and the Kurzion & Yagel paper:** only abstracts and bibliographic data were read (S11). The "Non-linear sphere tracing" paper (SIGGRAPH Asia 2019) returned HTTP 403 and is not used.
- **Every cost figure in §6** is arithmetic on proxies. There is no vegetation and no hardware path to measure yet.
- **GPU Gems ch. 6 timings** (S9) were garbled in the fetched copy and are not used.
- **The Lin clip's motion type** (translation vs re-voxelisation) cannot be told apart at 720p.
