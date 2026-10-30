# Research: vegetation, Debris and Fluids on the client

Resolves [#76](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/76). Feeds the ranked technique catalogue in [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71), and the open contracts in [#47](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/47), [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48), [#49](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/49) and [#50](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/50).

Checked on 2026-10-31. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). Every factual claim links to its source. Lines marked **[estimate]** are my own arithmetic, with the working shown. Lines marked **[judgement]** are engineering opinion, not measured fact. Lines marked **[unverified]** could not be checked against a primary source.

This note covers **client-side** behaviour only: what the client culls, LODs, keeps and draws. It does not design server interest management, replication policy or simulation budgets (M2–M4), and it does not replace the brick store, the LOD rings or the path tracer.

---

## TL;DR

1. **The ticket's premise is only half true, and the half that is false is the useful half.** Unedited **vegetation** and the **ocean** are as regenerable as terrain, so the client can treat them as a cache exactly like terrain. What is genuinely unrecoverable is (a) any **brick whose content has diverged from the generator** — an **Edit**, settled **Fluid**, or merged **Debris** — and (b) **live Debris and active Fluid**, which exist only as state the server sent. One rule — *the diverged-brick set* — gives all three content types the same residency, persistence and invalidation policy.
2. **Majority downsampling silently deletes vegetation.** The rule "a coarse Voxel is solid when at least 4 of its 8 children are solid" ([`lod.rs`](../../crates/brick-store/src/lod.rs), [ADR 0002](../adr/0002-brick-store-all-voxel-lod.md)) is a solidity filter tuned for closed, thick, near-axis-aligned terrain. A 1-Voxel-thick leaf shell at 45° puts 2–3 of 8 children in the coarse cell and **vanishes at the first downsample**; an axis-aligned one sits exactly on the 4-of-8 tie. So a forest LODed by the terrain rule has holes at the first ring boundary and is gone by the second. **Vegetation must be generated at each LOD level from its placement model, not downsampled** — the #47 pattern applied to a second content layer.
3. **Edits make that worse, not better.** Today `refresh_lod` remakes a coarse brick by downsampling all eight children ([`store.rs`](../../crates/brick-store/src/store.rs)). Cut one tree, and the coarse brick above it is rebuilt by majority downsampling — which erases the *other, untouched* trees in that same coarse brick. The fix is to compose **per child**: downsample only the edited octants, generate the untouched ones at the coarse level. This is a one-paragraph change to the #47/#50 contract with a visible failure mode if skipped.
4. **In a path tracer there is almost no per-object culling to do.** Every ray — primary, shadow, GI — goes through the one `trace()` ([`trace.wgsl`](../../crates/render/src/shaders/trace.wgsl)), so anything dropped from the scene is dropped from shadows and multi-bounce light too. NVIDIA's own guidance is explicit that all rays along a path must see the same LOD of every object, or you get light leaks, objects missing from reflections and wrong shadows, and that this applies to multi-bounce GI as well ([stochastic LOD](https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/)). The BVH and the region hash *are* the culling. For dynamic content the real levers are **generation, LOD and residency** — plus a subpixel drop distance below which removal cannot show.
5. **Debris is cheap in memory and expensive in hitches.** 1,000 live Debris is ~5 MB typical (100 MB worst case), and a 1,000-instance TLAS rebuild is ~0.03–0.06 ms — 3,840 instances cost 0.22 ms on a 2080 Ti in NVIDIA's measured sample [estimate from that figure]. What hurts is building a BLAS per spawn and rebuilding many per frame: NVIDIA reports that "many rebuilds on a single frame can lead to stuttering" and recommends distributing them over frames ([RTX best practices](https://developer.nvidia.com/blog/best-practices-for-using-nvidia-rtx-ray-tracing-updated/)). Cap builds per frame and hand the object over from static bricks to its instance across one frame; that handoff is invisible.
6. **Fluid deltas break the current scene layout.** `Scene::update` is append-only and only compacts when garbage exceeds `len/16 + 4096` words ([`scene.rs`](../../crates/render/src/scene.rs)); compaction is a full `Scene::new`, measured at about **1.6 s** after ~1,000 digs. At 20 Hz with a few hundred bricks changing per tick, the garbage threshold is crossed every **~0.5–2 s**, so the client would spend most of its life in a 1.6 s rebuild. Fluid needs **stable per-brick slots patched in place**, outside the append-only path. This is the single largest hitch finding in this note.
7. **The ocean should be a source, not bricks.** It is ~50% of the Planet's surface by construction (sea level is the median by area, [`world_map.rs`](../../crates/planet-gen/src/world_map.rs)) and its body is uniform. A `BrickSource` like the existing `Core`/`Interior` makes the water volume free; the *surface* is the one place to spend precision, because a smooth surface seen at a grazing angle multiplies a sub-voxel height error into a many-pixel horizontal shift. That is where an analytic sea pays for itself.
8. **Fidelity:** the things that pop are exactly the ones that change *coverage* or *silhouette* at a ring or LOD boundary — vegetation under the terrain downsample rule, Debris decimated while it rotates, half-applied fluid deltas, and fluid shading that depends on simulation state. The things that cannot pop are the diverged-brick rule (never stale), the analytic sea (one surface, evaluated exactly), frame-atomic delta batches (never torn), and the bricks-then-instance Debris handoff (never a hole).
9. **Ranked, per the map's order (frame time, then hitches, then VRAM):** for M1b, generate vegetation per LOD level, compose LOD per child, and prefab the species; for M3, batch and cap BLAS builds, rebuild the TLAS every frame from instance transforms, and apply merge bricks before removing the instance; for M4, slotted fluid overlay, frame-atomic deltas, and the sea as a source with exact surfacing. Everything else in this note is conditional or rejected.

---

## Plain-language recommendation

Write one rule down and make all three content types obey it: **a brick is either regenerable from the seed or diverged, and diverged bricks are the only ones worth persisting.** Terrain, unedited vegetation and the sea are regenerable. An Edit, a settled pool of water, and a merged pile of rubble all produce diverged bricks, and the client treats them identically: keep them, persist them, and never regenerate over them.

Then keep the three content types in the one voxel representation they already want to be in, and spend the effort on **how each one is generated at a coarse level**, because that is where the visible failures are:

- **Vegetation** stays Voxels inside bricks. That is not a compromise — it is what makes Edits work, keeps one tracer, and gives vegetation the ring machinery for free. But it must be *generated* at every LOD level, from a deterministic placement model with prefabricated species, because majority downsampling eats it.
- **Debris** is a small brick store per object with its own BLAS and a TLAS instance. Do not frustum-cull it and do not decimate it at first; instead spend the week on smooth spawn, capped builds and a merge order that can never show a hole.
- **Fluids** are a slotted overlay on the brick scene plus an exact surface for the ocean. The overlay exists because the append-only scene cannot absorb 20 Hz churn without a 1.6 s rebuild every second or two.

If only three things get built, build the **diverged-brick rule**, the **vegetation generator per LOD level**, and the **slotted fluid overlay**. Those three carry almost all of the fidelity and hitch risk. The rest can wait for the benchmark.

---

## 0. What the client is today, and why it constrains the answer

Five properties of the current code decide most of what follows. They are not opinions; they are the shape of the thing.

1. **One scene, one tracer.** `Scene` packs every brick into `table`, `regions`, `brick_refs`, `bricks` ([`scene.rs`](../../crates/render/src/scene.rs)) and `trace()` is the only thing that knows how to walk it ([`trace.wgsl`](../../crates/render/src/shaders/trace.wgsl)). Primary ([`primary.wgsl`](../../crates/render/src/shaders/primary.wgsl)) and the reference path tracer ([`pathtrace.wgsl`](../../crates/render/src/shaders/pathtrace.wgsl)) both call it, including for shadow and bounce rays. There is no separate "visible set".
2. **The scene only grows, then rebuilds.** `Scene::update` patches whole 64³-Voxel regions in place but appends replacement bricks to the end of the buffers; what it replaced becomes `garbage`, and when that passes `spare_words(live) = live/16 + 4096` the whole scene is packed again with `Scene::new`. The code itself notes that this "takes about 1.6 s for the test Planet" ([`scene.rs`](../../crates/render/src/scene.rs)).
3. **LOD is majority downsampling, one level at a time.** `remake` reads the eight children of a coarse brick — held bricks, or the `BrickSource` where none is held — and downsamples them ([`store.rs`](../../crates/brick-store/src/store.rs), [`lod.rs`](../../crates/brick-store/src/lod.rs)).
4. **Unedited content is a cache.** Only bricks touched by Edits are persisted; everything else is regenerated from `(seed, GENERATOR_VERSION)` ([ADR 0002](../adr/0002-brick-store-all-voxel-lod.md), [ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md)). Live Debris and moving Fluids have no such luxury, which is what this ticket is about.
5. **The budgets are known.** 1440p60, ≈ 8.6 ms of GPU work, primary 1.0 ms, edits/BVH 1.0 ms, ~2 GB client cache ([`bench-budgets.toml`](../../bins/client/bench-budgets.toml), [vertical-slice spec](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/21)).

Consequence: **any technique that removes content removes it from light transport, not just from the screen.** Conservatism is therefore much stricter here than in a rasteriser, and it is why the ranked lists below keep pointing at *what is generated* and *what is resident* rather than at visibility tests.

### What a packed brick costs

Useful for every estimate below. A mixed brick is a 16-word occupancy mask, a header, a palette and packed indices; a uniform brick is a single flagged word in `brick_refs` with no data at all ([`scene.rs`](../../crates/render/src/scene.rs)).

| Brick kind | Words | Bytes | Bytes per solid Voxel |
|---|---|---|---|
| Uniform (one Material) | 0 (flagged ref only) | 4 | 0.008 at full |
| Mixed, 1–2 Materials (1 bit/index) | 34 | 136 | 0.27 at full |
| Mixed, 3–4 Materials (2 bits/index) | 51 | 204 | 0.40 at full |
| Mixed, 5–16 Materials (4 bits/index) | 89 | 356 | 0.70 at full |
| Mixed, ~4% full (a leaf shell) | 34–51 | 136–204 | **2–7** |

That last row is the whole vegetation cost story in one line: **sparse content pays the full brick tax.** A foliage brick holding 20 Voxels costs 5–25× as much per solid Voxel as a full rock brick. [estimate, high confidence — it follows directly from the packing rules]

---

## 1. The idea that covers all three: regenerable, diverged, transient

Three classes of client-side content, with three different residency rules.

| Class | Examples | Can the client rebuild it? | Residency rule | Persist? |
|---|---|---|---|---|
| **Regenerable** | terrain, unedited vegetation, the ocean body, unedited water in a lake | Yes — `(seed, GENERATOR_VERSION, coordinate)` | Pure cache. Evict anything outside the rings; prefetch ahead. | No |
| **Diverged** | bricks touched by an Edit, by settled Fluid, or by a merged Debris | **No.** The generator never saw the player. | Resident as long as the brick is in a ring; evict to the client cache, never to the seed | **Yes** — same rule as Edits today ([ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md)) |
| **Transient, server-owned** | live Debris content, active Fluid bricks, pose snapshots | **No.** It exists only because the server said so. | Resident while near enough to matter; refetch if evicted and re-entered | No (the server still has it), but losing it shows |

Two things follow, and they are the spine of the rest of the note.

**The diverged-brick set is one set, not three.** "Which bricks differ from what `generate()` would produce" is already the persistence predicate for Edits; extend it to Fluid settling and Debris merging and all three content types get one cache, one eviction policy and one invalidation path. A settled pool is not a special case of terrain — it *is* an Edit as far as residency is concerned.

**[judgement] The transient class is where the client has real choices.** The server decides what is live; the client decides *what it keeps* and *when it drops it*. Dropping is only invisible below the subpixel distance, which is a number the client can compute from the camera's pixel angle — so eviction policy is a fidelity question, not a networking one. That is squarely in scope here; interest *selection* is not.

### The subpixel distance

The one number used throughout. The camera has a **70° vertical field of view** ([`camera.rs`](../../crates/render/src/camera.rs)), and the bench traces at 1280×720 internally: one pixel subtends `2·tan(35°)/720 = 1.9 mrad` ≈ 2 mrad. So a feature of size `s` is one pixel at

```
d_px ≈ s / 0.002  =  500 · s      (metres)
```

A 20 cm Voxel is one pixel at ~100 m; a 10 cm Voxel at ~50 m; an 8 m Debris at ~4 km. **Everything below that distance can change; everything above it must not be allowed to change without a deliberate decision.** This is the criterion each LOD switch below is checked against.

---

## 2. Vegetation (M1b)

### 2.1 Voxels inside bricks, or separate instanced geometry?

The honest comparison, in terms of the four things the ticket asks about.

| | Voxels inside bricks (recommended) | Instanced tree geometry |
|---|---|---|
| **Culling** | Free: the region hash and occupancy masks are the cull ([`trace.wgsl`](../../crates/render/src/shaders/trace.wgsl)) | Needs a second traversal path and a BLAS per species; instances get culled by the TLAS, which is the same work |
| **LOD** | Free-form but must be *generated*, not downsampled (§2.2) | Standard discrete/stochastic LOD with a BLAS per level ([stochastic LOD](https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/)) |
| **Residency** | Regenerable from the seed like terrain; no new cache | Needs an asset pipeline, species assets and a per-instance placement stream |
| **Edits** | Works today: `sim::apply` writes Voxels, `refresh_lod` propagates | Broken. A cut tree has to be *converted* from an instance into Voxels at the moment it is damaged, then back again if it ever becomes static. Two representations of one thing, forever |
| **Light transport** | One `trace()`, one set of normals, GI and shadows just work | A second intersection path inside `trace()`, and foliage needs alpha or stochastic transparency, which is noisy by construction |

**Recommendation: Voxels in bricks.** Instancing is the better answer in a mesh renderer, and it is the wrong answer here for two decisive reasons: [judgement]

- **Edits are the core mechanic.** The tree you can see is the tree you can cut, and it must become Voxels the instant it is cut. A conversion path that has to be exact (same silhouette, same Material, same collision) is a permanent source of drift between two representations — precisely the "second renderer" ADR 0002 rules out.
- **Foliage is the worst possible case for alpha-tested instancing in a path tracer.** Leaves are sub-Voxel detail that must either be opaque (then a leaf is a 10 cm slab, same as a Voxel) or alpha-tested (then shadow rays need stochastic alpha, which is noise the denoiser has to eat). At 10 cm resolution the voxel representation is not a downgrade; it is the same geometry.

The instanced route stays in the **rejected/watchlist** section with that reasoning, not silently dropped.

### 2.2 The killer: majority downsampling deletes a forest

The coarse rule is "solid when at least 4 of the 8 children are solid" ([`lod.rs`](../../crates/brick-store/src/lod.rs), and its test `four_solid_voxels_are_enough_and_three_are_not`). Now consider what that does to the three shapes the Planet is made of:

| Shape | Children solid in a 2×2×2 cell | Survives one level? |
|---|---|---|
| Solid rock under a surface | 8 of 8 | Yes |
| A near-horizontal surface (terrain) | 4 of 8 | Yes, exactly on the tie |
| A 1-Voxel-thick surface at 45° | 2–3 of 8 | **No** |
| A 1-Voxel-thick axis-aligned shell | 4 of 8 | Yes, exactly on the tie |
| A 2-Voxel-thick shell | 8 of 8 | Yes |

Terrain is a heightfield: thick below, locally near-axis-aligned, so it survives by construction. **Vegetation is almost entirely thin and slanted**, so it does not. A canopy shell that is one Voxel thick loses its slanted parts at level 1 (20 cm Voxels) and is a scattered dust of Voxels by level 2–3, which then fails the 4-of-8 test again. The result is not a lower-detail tree: it is **holes at the first ring boundary and an empty forest beyond it**, with the shadows to match. [estimate, high confidence — it follows directly from the downsample rule]

Two consequences:

- **Do not generate vegetation fine and downsample it.** Generate it at the level being asked for, from the placement model, exactly as #47 already does for terrain. A tree at level `L` is a function of `(seed, species, position, L)`, not a mip of a tree.
- **A single cut tree must not drag its neighbours down with it.** Today `refresh_lod` remakes an entire coarse brick by downsampling all eight of its children ([`store.rs`](../../crates/brick-store/src/store.rs)). Once coarse bricks are generated rather than downsampled, that rule becomes wrong: a coarse brick containing one edited tree would be rebuilt from fine bricks, and the untouched trees in the other seven octants would be majority-downsampled away. The rule must compose **per child**: the eight children of a coarse brick are each either *edited* (downsample that child) or *untouched* (ask the source at the coarse level). That is a small change to `BrickStore::remake` and a one-line addition to the `BrickSource` contract, and it is a hard dependency of the whole approach.

### 2.3 How to LOD vegetation, then

Three layers, in order:

1. **Placement** — a deterministic per-cell decision: which species, where, at what rotation. Same rules as `planet-gen`: integer hashing of the cell, no transcendentals, no order dependence ([deterministic-generation](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/deterministic-generation/docs/research/deterministic-generation.md)). The `Biome` and fantasy `Regions` masks already exist in the world map ([`world_map.rs`](../../crates/planet-gen/src/world_map.rs)), so placement has the inputs it needs.
2. **Species** — a small set of voxel templates generated once ("prefab"), then stamped at placements. This is memoisation of *generation*, not instancing of *rendering*: the store still receives ordinary bricks, byte for byte what a from-scratch generator would produce. It exists because vegetation is the heaviest `planet-gen` workload the spec already flags as a risk, and because stamping costs a memcpy instead of 5,000 noise evaluations.
3. **Coarse rule** — at level `L`, a coarse Voxel is foliage-solid when the *coarse-scale coverage* of the species at that Voxel exceeds one half. This is a different rule from majority downsampling: it is the rule that keeps the silhouette. It has to be conservative to within half a coarse Voxel, because that is the LOD's own error budget (§5).

**Rotation determinism.** Stamping a template at an arbitrary tangent-frame orientation is float work and would break the determinism matrix. Snap each placement to a fixed set of integer orientations (the 24 cube rotations, optionally × 8 yaw steps): exact, cheap, and enough — a forest of eight orientations is indistinguishable from a continuous one at 10 cm. [judgement]

**Wind or animation.** The vertical-slice spec does not mention wind for vegetation ([spec](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/21)), so vegetation is static and can be baked, which is what makes prefabs and coarse generation valid in the first place. Adding wind later would make the generator time-dependent, which conflicts with "unedited bricks are a cache": the regenerated brick would differ every frame. It goes in the watchlist, not in M1b.

### 2.4 Culling: there is nothing to cull, and that is the point

Vegetation inherits terrain's culling wholesale: the region hash skips empty 64³-Voxel regions in one probe, and the per-region mask skips empty bricks. A tree is surrounded by air, so the structure is already sparse exactly where it should be. There is no frustum test to add, and adding one would break shadows and GI (§3.2). The vegetation-specific lever is **not generating it** beyond the distance where it matters — which is a residency/LOD decision, not a culling one.

The one thing worth measuring is the **cost of the bricks vegetation adds**, because a foliage brick is sparse and therefore pays the brick tax: in a closed canopy the near ring gains roughly 80 bricks per tree (a 4 m crown touches ~78 of the 125 bricks in its bounding box) [estimate].

### 2.5 Numbers

Assumptions, all of which move the answer: a closed temperate canopy, 4 m crown diameter, 1-Voxel-thick leaf shell, one tree per ~20 m² (≈ 63% cover), 10 m tall with a 0.4 m trunk; spawn ring of 120 m radius as in #49. [estimate]

| Quantity | Value | Confidence |
|---|---|---|
| Trees in the spawn ring (π·120² ≈ 45,000 m²) | ~2,250 | Medium |
| Solid foliage Voxels per tree | ~5,000 (shell of 4πr² at r = 2 m) | Medium |
| Non-empty bricks per tree | ~80 | Low–medium |
| Bricks added to the spawn ring | ~180,000 | Low–medium |
| Bytes added, at ~136 B a foliage brick | ~24 MB (range 15–40) | Low–medium |
| Terrain bricks in the same ring, at ~3.1 bricks/m² (k = 2) | ~140,000 | Medium |
| **Near-ring brick count, vegetation vs terrain** | **+60% to +130%** | Low–medium |
| Generation work added | +15% to +40% of Voxel evaluations | Medium |
| Primary pass, dense forest vs bare ground | **+15% to +45%** (0.15–0.45 ms of the 1.0 ms budget) | Low |

The last two lines are the important split. **Vegetation is cheap per Voxel and expensive per brick.** It adds a sixth to a third more generation work, because a tree is only ~8,000 Voxel evaluations against a terrain brick's 512; but it adds 60–130% more *bricks*, because a leaf shell fills 4% of each brick it touches. Generation and memory scale with different things, and the brick side is the one that is close to a limit.

Over the whole ring stack the brick side is what bites. Each ring holds roughly the same number of bricks by design ([ADR 0002](../adr/0002-brick-store-all-voxel-lod.md)); if every ring gets individually generated crowns, the stack gains roughly the same +180k bricks per ring, i.e. **~300–500 MB against a ~2 GB budget** [estimate, low confidence: it assumes 19 rings and that the coarse placement stays as dense as the fine one]. A coverage model at coarse levels — one solid where a crown *is*, rather than a crown shape — should cut that by half or more, which is the main reason §2.3's third layer matters for VRAM and not only for looks.

### 2.6 Vegetation shortlist

Tiers as the map defines them. "Win" is relative to the naive version of the same thing; costs are agent-weeks.

| # | Technique | Tier | Win (range, confidence) | Cost | Risk | Fidelity | Depends on |
|---|---|---|---|---|---|---|---|
| V1 | Vegetation as a second **generator layer inside the same bricks**; no second representation | **Must-have** | The only option that keeps Edits, one tracer and free culling | 1–2 | Low. It is the default, not a change | Exact — the same Voxels any correct design produces | #47, `planet-gen` |
| V2 | **Prefab species** stamped at deterministic placements, integer orientations only | **Must-have** | −50% to −80% of the ~0.2–0.3 s vegetation share of the spawn ring's generation | 0.5–1 | Medium: orientation determinism, palette compatibility | Zero — output is byte-identical to generating in place | V1, determinism matrix |
| V3 | **Generate at each LOD level** from placement + a coverage rule, instead of majority downsampling | **Must-have** | Removes the entire "forest disappears at level 1–2" failure; no frame-time win | 1–2 | Medium: coarse/fine silhouette must agree within ½ coarse Voxel | Invisible *if* the coverage rule is conservative; otherwise a ring seam | #47, #48, V1 |
| V4 | **Per-child LOD composition**: downsample edited octants, generate untouched ones | **Must-have** | Prevents one cut tree erasing its whole coarse brick | 0.5–1 | Low–medium: touches `remake` and the `BrickSource` contract | Invisible by construction (untouched octants are regenerated, not approximated) | V3, #47 |
| V5 | **Shorter full-resolution vegetation radius** than terrain (vegetation drops to the coarse model earlier) | **High-value** | −20% to −45% of near-field vegetation bricks and generation | 0.2–0.5 | Low: it is a ring-radius policy | Invisible if the switch is at ≤1 px (~100 m for level 1) | #48 |
| V6 | **Coverage model at coarse levels** (a crown becomes a solid/void region, not a crown shape) | **High-value** | Cuts the ring-stack vegetation VRAM by ~40–60% | 1–2 | Medium: the aggregate's silhouette and its shadow are approximations | Gated by the image diff; at ring distances the error is ≤1 px | V3, #48 |
| V7 | Screen-space or frustum culling of vegetation | **Rejected** | None: the region hash already culls; a frustum test would break GI and shadows | — | — | Would change the image | — |
| V8 | Instanced or billboard trees, alpha-tested | **Rejected** | Standard in rasterisers, wrong here | 3–5 | High: two representations, Edits, alpha noise | Impostors are an explicit fidelity loss (map's out-of-scope) | — |
| V9 | Wind / vertex animation | **Watchlist (not in spec)** | — | 2–3 | High: makes the generator time-dependent | Shimmer by construction | A spec change first |

### 2.7 The M1b experiments

**`forest-flyover-v1`** (needs a bench scene flag, e.g. `--scene forest`, and a `bricks`/`scene_bytes` counter in the overlay):

1. Same camera path as `planet-flyover-v1`, over a fixed temperate coast with vegetation on; a second run with vegetation off.
2. Report `primary`, `reference`, `gpu_total`, `frame_time` p95/max, brick count and scene bytes for both.
3. **Pass:** the p95 frame is inside 8.6 ms, and the on/off difference in the *primary* mean is inside the range predicted in §2.5 — the number that decides V5's radius and V6's tier.

**`canopy-lod-v1`** (needs a `--scene forest` bench plus a still-camera screenshot mode):

1. Six stills over forest at 90, 100 and 110 m (across the level 0→1 ring switch) and at 190, 200 and 210 m.
2. For each, render the LOD-selected view and a full-resolution reference of the same camera, and require MSE ≤ 1.0 with far-off ≤ 0.2% — the reference tolerances already in [`golden.rs`](../../crates/render/tests/golden.rs).
3. Repeat after digging one 1 m sphere at a tree 60 m away, to exercise V4's per-child composition.
4. **Pass:** every still within tolerance, with no ring-boundary seam visible in the diff image.

---

## 3. Debris (M3)

### 3.1 What replication gives the client, and what it costs

From the Debris decision ([#18 resolution](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/18)): the server sends each new Debris's Voxel contents once, then pose snapshots; the client interpolates. Limits are ~1,000 live, ≤500 m³ each, merging back after ~3 s at rest, and the merge is a re-Voxelisation, i.e. an Edit.

So the client holds three separate things per Debris: **content** (once, small), **pose** (every snapshot, tiny), and **identity** (to drop it on merge). Only the content is expensive, and it is *not* regenerable.

| Item | Typical (5 m³) | Worst case (500 m³) | 1,000 live |
|---|---|---|---|
| Solid Voxels | ~5,000 | ~500,000 | 5 M typical |
| Bricks (one AABB each) | ~20 | ~1,000 | ~20,000 typical |
| Packed content | ~5 kB | ~100 kB | ~5 MB typical, ~100 MB worst |
| BLAS | ~2–5 kB | ~50–100 kB | ~5 MB |
| TLAS instance | 64 B | 64 B | 64 kB |

[estimate, medium confidence: interior bricks are uniform and cost 4 B, shell bricks 136–204 B]

**Debris is not a VRAM problem.** It is a hitch and CPU problem, and the numbers below say where.

### 3.2 The BVH is the culling

This is the part that is genuinely different from a rasteriser, and the ticket is right to ask about it.

- A rasteriser culls Debris against the frustum because that skips *draw calls*. A path tracer has no draw calls: the TLAS traversal already skips every instance whose AABB the ray misses. **Frustum culling for primary rays would save the traversal of the BVH nodes above the instances — a small, already-cheap part.**
- Worse, it would be *wrong* for everything else. Every secondary ray starts somewhere and travels anywhere; an instance removed because it is off-screen is an object that casts no shadow and appears in no reflection. NVIDIA states the constraint directly: all rays along a path must see the same LOD of every object, or you get light leaks, objects disappearing in reflections and incorrectly shadowed areas — and the problem "applies to higher-level bounces as well, for example … multi-bounce global illumination" ([stochastic LOD](https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/)). Culling is LOD ∞, so the same warning applies with more force.
- The one hardware-supported culling lever is the **8-bit TLAS instance mask** combined with a per-ray mask ([`TlasInstance`](https://docs.rs/wgpu/latest/wgpu/struct.TlasInstance.html)). That is per-ray-class, not per-object-distance, so it can only express "this class of rays ignores this class of instance". Using it to drop far Debris from shadow or GI rays **changes the image** and belongs in the watchlist, gated by the image diff.

**Conclusion: per-object culling for Debris is rejected.** What remains is a **subpixel drop**: below ~0.5 px an object's primary contribution and its shadow are both under the gate, so the client may drop it entirely. With 8 m Debris and the 500·s rule that is ~4 km — far beyond any distance at which Debris is on the client at all, because the server already keeps Debris near players. [judgement] The practical policy is therefore "trust the server's set; do not add a second distance policy", and spend the effort on residency and hitches instead.

### 3.3 LOD and decimation

Two options, and the answer is "not yet".

- **Decimation** (a coarse brick chain per Debris, 2–3 levels, one BLAS each): the object's own AABBs drop from ~20 to ~4, so traversal gets cheaper, but a 20-AABB BLAS is already near the noise floor of a 1.0 ms pass. [estimate: +0.02–0.1 ms saved at 1,000 instances, low confidence]
- **The cost is real:** a *rotating* rigid body has no stable object-space voxel grid, so the coarse-ification of the silhouette changes as it tumbles. You would see the object's outline swim. Hysteresis does not fix it; only switching at a distance where the change is subpixel does, which is the same threshold at which dropping it is cheaper still.

**Verdict: conditional at best, likely rejected.** Build it only if the `debris-storm` benchmark shows traversal, not build, is the cost. This is the sort of "not everything deserves a technique" call the catalogue needs.

### 3.4 Spawn, despawn and merging back

These are where Debris can actually pop, and each has a rule that makes it invisible.

| Event | Naive failure | Rule | Why it is invisible |
|---|---|---|---|
| **Spawn** | Instance appears before its BLAS is built → a one-frame hole; or 100 builds land in one frame → a 2–20 ms hitch | Keep the content in the **main scene as static bricks** for the frame(s) its BLAS is not ready, then remove those bricks and add the instance. Cap BLAS builds per frame (~8–16). | The object is frozen for one frame at its spawn pose — a frame of latency, not a change of shape or place. The handoff changes representation, not pixels. [judgement] |
| **Despawn / merge** | Instance removed before the merged bricks are applied → a hole. Or applied after → double geometry | **Bricks first, instance second, same frame.** On the merge frame, stop interpolating that object and draw it at the snapshot pose the merge was built from. | In a ray tracer, double geometry that occupies the same space is occluded and identical — the failure mode is *slower*, not *wrong*. The opposite order is a hole. |
| **Pose** | Snapped to the last snapshot → visible 60 Hz stepping at close range | Interpolate between the last two snapshots | Error at 50 m/s is < 0.83 m per tick, subpixel beyond ~400 m; near the player it is the difference between smooth and stuttering |
| **Merge exactness** | Merged bricks differ from the instance's world-space Voxels by one Voxel → a visible seam or a duplicated shell | The server's re-Voxelisation must be bit-identical to the pose the client last interpolated; apply at the snapshot pose, not the interpolated pose | Makes the handoff a no-op in the store |
| **Over-budget merge** | The server merges the farthest Debris early → a large object vanishes | It is an Edit like any other: bricks arrive, instance goes | Same-frame rule above |

### 3.5 Acceleration structure policy

- **One BLAS per Debris**, its bricks as AABBs. NVIDIA's guidance agrees: "geometries that move independently should usually be in their own BLASes", and merging them would create a huge mostly-empty world AABB plus unnecessary rebuilds ([RTX best practices](https://developer.nvidia.com/blog/best-practices-for-using-nvidia-rtx-ray-tracing-updated/)).
- **Rebuild the TLAS every frame from instance transforms.** This is the normal pattern, and the measured cost is small: 3,840 instances with 8 LODs each rebuilt in 0.22 ms on an RTX 2080 Ti, and LOD selection took 0.018 ms ([stochastic LOD](https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/)). A 1,000-instance TLAS on a 4090 is therefore **well under 0.1 ms**, comfortably inside the 1.0 ms edits/BVH budget. [estimate from NVIDIA's figure, medium confidence]
- **`PreferUpdate` is optional.** wgpu exposes `AccelerationStructureUpdateMode::{Build, PreferUpdate}`, where the latter means "if possible, perform an incremental update … useful for e.g. skinning" ([wgpu](https://docs.rs/wgpu/latest/wgpu/type.AccelerationStructureUpdateMode.html)). For pose-only changes it may save a little; NVIDIA in fact recommends the opposite for TLASes — prefer fast-trace and full rebuilds, because a high-quality TLAS "doesn't cost too much" ([RTX best practices](https://developer.nvidia.com/blog/best-practices-for-using-nvidia-rtx-ray-tracing-updated/)). Try `Build` first.
- **A TLAS must be rebuilt after any BLAS it references changes** ([wgpu ray tracing spec](https://github.com/gfx-rs/wgpu/blob/trunk/docs/api-specs/ray_tracing.md)). So decimation (a BLAS swap) forces a TLAS rebuild, which is another small reason not to do it per frame.
- **The compute fallback needs its own instance structure.** With no ray queries, `trace()` must test the Debris set itself, and a per-instance loop over 1,000 objects is far too slow. It needs a uniform grid or a small BVH over instance AABBs, with the ray transformed into each instance's local frame and `hit_region` reused. That is a second implementation and the strongest argument in the repo for the hardware path in M3 (the path-tracing decision already says hardware RT "pays off most with many moving objects (Debris)").

### 3.6 Debris shortlist

| # | Technique | Tier | Win (range, confidence) | Cost | Risk | Fidelity | Depends on |
|---|---|---|---|---|---|---|---|
| D1 | **One brick store per Debris, one BLAS of brick AABBs, one TLAS instance** | **Must-have** | The design itself | 1.5–2 | Medium: BLAS build API in wgpu is experimental | Exact | #50 |
| D2 | **Content cache with a subpixel eviction rule** and refetch on re-entry | **Must-have** (for the 2 GB budget) | Bounds content at ~100 MB worst case, ~5 MB typical | 1 | Medium: a refetch is a network stall; prefetch by pose velocity | Invisible below ~0.5 px | D1, M2 snapshot stream |
| D3 | **Capped, batched BLAS builds + bricks-then-instance handoff** | **High-value** | Removes a 2–20 ms spawn hitch on a collapse | 1 | Medium: needs a "pending instance" state in the client | Invisible by construction | D1 |
| D4 | **Full TLAS rebuild every frame from transforms** (try `Build` before `PreferUpdate`) | **Must-have** | ~0.03–0.1 ms at 1,000 instances; keeps the 1 ms edits/BVH budget | 0.5 | Low | Exact | D1, #50 |
| D5 | **Merge ordering: bricks first, instance second, same frame** | **Must-have** | Prevents a one-frame hole | 0.2 | Low | Invisible by construction | D1, `sim` |
| D6 | **Decimated BLAS chain** per Debris | **Conditional** | +0.02–0.1 ms at 1,000 instances | 1–1.5 | High: silhouette swims on a rotating body | Visible unless switched below 1 px | D1, benchmark first |
| D7 | **Frustum / per-object culling** | **Rejected** | None — the TLAS already culls; breaks GI and shadows | — | — | Changes the image | — |
| D8 | **Instance-mask culling of far Debris from shadow/GI rays** | **Watchlist** | 0.05–0.3 ms in the shadow and GI passes | 0.5 | High: missing shadows, missing bounce light | Visible; image-diff gated | D1, #50 |
| D9 | **Compute-fallback instance structure** (grid/BVH over instance AABBs) | **Must-have if the fallback ships** | Keeps the fallback usable with Debris | 1 | Medium: a second traversal path to keep in step | Exact if it mirrors `hit_region` | #50 |

### 3.7 The M3 experiment

**`debris-storm-v1`** (needs a bench scene that spawns scripted Debris and a recorded delta stream, plus a `rebuilds` counter):

1. 1,000 Debris pre-generated, spawned at 100/frame over 10 frames, 5 s of free fall, then all merged in one frame.
2. Report the `edits`/BVH pass, `gpu_total`, `frame_time` p95/max, the count of `SceneUpdate::Rebuilt`, and the count of BLAS builds per frame.
3. **Pass:** no frame over 8.6 ms; no single hitch over 2× the frame budget; zero scene rebuilds; the spawn frame's golden has no hole and the merge frame's golden matches the pre-merge one.
4. Run with D3 disabled to measure what the cap is worth.

---

## 4. Fluids (M4)

### 4.1 Three fluid states on the client

The Fluid decision ([#19 resolution](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19)) gives the client: a cellular automaton running server-side near players at 20 Hz over ≤2M active Voxels, brick deltas at ≤20 Hz, rivers and the ocean simulating within ~500 m and sleeping beyond, and the ocean as an infinite source.

On the client that is three different things, and they should not share a data path:

| State | What it is | Regenerable? | Where it lives |
|---|---|---|---|
| **Static water** | the ocean body, lakes and rivers at rest, as Voxels in bricks | Yes, from the seed | The brick store, like terrain |
| **Settled, diverged water** | water that flowed somewhere the seed did not put it | **No** | The diverged-brick set (§1) |
| **Active water** | the ≤2M Voxels the server is currently simulating | **No** | A slotted overlay, patched at 20 Hz |

The mistake to avoid is letting the third one go through the second one's machinery. That is exactly what happens today if a delta is applied as a `Scene::update`.

### 4.2 The ocean: a source, not bricks

The ocean is about half the Planet's surface — sea level is the *median by area* by construction ([`world_map.rs`](../../crates/planet-gen/src/world_map.rs)) at radius 16 km. As stored Voxels that is an enormous volume of uniform water with a surface on top. It should be handled the way the core already is: a `BrickSource` that answers "solid water inside this sphere, air outside", exact for bricks wholly inside, with the crust bricks held ([`lod.rs`](../../crates/brick-store/src/lod.rs), `Interior`). That makes the water body **free** — no bricks, no memory, no generation — and leaves only the surface to get right.

Why the surface deserves its own treatment:

- **A smooth surface at a grazing angle amplifies height error into horizontal error.** A height error `ε` shifts the apparent shoreline or horizon by about `ε / tan θ`, where `θ` is the angle between the ray and the surface. At `θ = 1°` a half-coarse-Voxel error of 5 m displaces the horizon by ~290 m. [estimate, high confidence — plain geometry] From orbit along the limb, that is a shoreline that moves as the rings re-centre.
- **A voxel water surface has six normals.** A flat sea made of Voxels is a mirror with no normal variation until the ring is coarse enough to show treads, and at a ring switch the treads change. The spec already asks for "animated normal detail" ([#19](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19)); an exact sphere gives a continuous normal to perturb.
- **It is half the planet's surface bricks.** Removing them from every ring is the largest single VRAM and traversal win available in this note. [estimate: −25% to −50% of ring bricks, low confidence — it depends on how much of each coarse ring is ocean at that level]

The analytic sea must still be **clipped by terrain** (land above sea level) and joined to the **CA-filled channels** at exactly `SEA_LEVEL`. The join is exact if both use the same level: the analytic surface is the sea's surface everywhere the terrain is below it, and the CA only changes where the terrain is *not* — so a dug channel floods up to the same height and meets the analytic surface with no seam. [judgement]

**Verdict: high-value, not must-have.** It adds a special case to `trace()` (one sphere test, one analytic normal) and it pays for itself in far-field bricks. If M1a ships "oceans as static water" as ordinary Voxels, this is the M4 upgrade that removes them.

### 4.3 Deltas: the compaction trap, with arithmetic

This is the most important finding for M4 and it is not close.

`Scene::update` is append-only: for each changed region it writes a new region record, re-appends every brick in that region, and counts what it replaced as `garbage`. When `garbage > live/16 + 4096` words, it calls `Scene::new` and repacks everything — a **~1.6 s** stall, per the code's own note ([`scene.rs`](../../crates/render/src/scene.rs)).

Put 20 Hz Fluid through that path. Take a near-ring scene of ~320k bricks (terrain 140k + vegetation 180k, §2.5) at ~170 B of data plus a 4 B ref each: live ≈ 14M words, so the threshold is ≈ 875k words ≈ **3.5 MB of garbage**. A tick that changes 2,000 bricks across 60 regions orphans:

- 60 regions × 64 brick refs × 4 B ≈ 15 kB, plus
- 2,000 repacked bricks × ~170 B ≈ 340 kB,
- **≈ 355 kB per tick**, and that is a conservative tick.

At 20 Hz that crosses 3.5 MB every **~0.5–2 s** [estimate, medium–high confidence: it follows from the code's own accounting]. The client would spend a large fraction of its life in a 1.6 s rebuild. Even the *un*conservative case — a few hundred bricks a tick — hits it every few seconds.

There is no tuning that fixes this. `spare_words` is a sixteenth plus a constant; making it larger merely delays an unbounded rebuild. **Fluid bricks need stable slots that are overwritten in place**, outside the append-only arrays: a fixed-capacity overlay of, say, 32k bricks × 136–204 B ≈ 5–6 MB, indexed by a second hash or by a per-region fluid mask, consulted by `trace()` for bricks that have one.

The cost of the indirection: one extra lookup per brick for bricks that *might* have an overlay. The cheap form is a **per-region flag** in the existing region record — for a non-uniform region the `first_ref` word's bit 30 is free, because only bit 31 (`SOLID_FLAG`) means "uniform", so the common case costs nothing and the offset merely has to mask the flag off (with the offset then limited to 2³⁰ words, 4 GB — far beyond the budget). Only flagged regions do the second lookup. [estimate: +2% to +5% on the primary pass in a flooded area, low confidence]

Two contract notes for #48/#49:

- The overlay is the first thing in the renderer that *mutates* bricks in place, so it also gives Edits a fast path later. If #48's scene layout is going to grow a second buffer anyway, it should be designed as this overlay from the start.
- The overlay must be **frame-atomic**. All deltas for one server tick are written before the frame's `trace()`, or none are. A half-applied tick is a water surface with a step in it, and a specular surface is exactly where a one-brick step shows.

### 4.4 Invalidation and persistence

| Event | What the client must do | If it does not |
|---|---|---|
| Delta for a brick it holds | Apply, bump the brick's revision | — |
| Delta for a brick it does not hold (new active region) | Fetch the brick's current state, not the queued deltas | Applies changes to the wrong starting point |
| Delta with a revision gap | Request that brick's state | A water level that is permanently wrong |
| Region leaves the active set (settles) | Keep the last state; mark the brick **diverged** | Re-entering regenerates from the seed → the flood disappears |
| Region re-enters the active set | Hand the stored state to the server's next delta base | The server and client disagree on the starting level |
| Client evicts an active brick | Must not: active content is not regenerable | A hole in the water until refetched |

The settling row is the one that is easy to get wrong: **settling is a persistence event, not a simulation event.** A pool that has stopped moving is indistinguishable from a player-dug pond as far as the cache is concerned, and it must be persisted under the same rule ([ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md)). That is the concrete answer to "how unrecoverable state changes residency".

### 4.5 Shading and the sleep/wake boundary

The spec animates flow beyond the simulated radius with "animated flow shading taken from the world map's river direction" and wakes it "seamlessly" ([#19](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19)). "Seamlessly" is a shading constraint, not a simulation one:

- **Make the animated normal a pure function of world position, time and the static flow field.** Then the same point shades identically whether it is inside or outside the simulated radius, and the boundary is invisible by construction.
- **Do not let the CA change the shading.** If the flow direction or speed fed to the normal comes from the simulation, the boundary shows as a ring where the pattern changes character, and it moves with the player — the worst kind of artefact, because it is attached to the camera.
- **Time quantization follows from that.** The geometry updates at 20 Hz while the shading is continuous; the eye tracks the shading, so a 50 ms geometry step is hidden as long as the surface only moves by a Voxel or two per step near the player (5–50 cm at river speeds) [estimate, low confidence].

### 4.6 Fluid shortlist

| # | Technique | Tier | Win (range, confidence) | Cost | Risk | Fidelity | Depends on |
|---|---|---|---|---|---|---|---|
| F1 | **Slotted fluid overlay**, patched in place, per-region flag in the region record | **Must-have** | Removes a 1.6 s rebuild every 0.5–2 s — the largest hitch in this note | 1.5–2.5 | Medium–high: a second lookup in the hottest shader; a second buffer to keep consistent | Exact | #48, #49, #50 |
| F2 | **Frame-atomic delta batches**; drop a whole tick rather than half | **Must-have** | Prevents a torn specular surface | 0.5 | Low | Invisible by construction | F1 |
| F3 | **Settled fluid joins the diverged-brick set**, with per-brick revisions for invalidation | **Must-have** | Prevents stale or vanished water on revisit | 1 | Medium: revision bookkeeping on both sides | Invisible by construction | §1, ADR 0003 path |
| F4 | **Ocean as a `BrickSource`** (uniform water inside the sea sphere) + an **analytic surface** in `trace()` | **High-value** | −25% to −50% of ring bricks; subpixel sea at every distance | 1–2 | Medium: clipping, the land/sea join, the glass path | Invisible if the sphere matches the Voxel sea the LOD would have produced | F1, `world_map` ocean mask, M1b glass |
| F5 | **Animated normals as a world-space function** of position, time and the static flow field | **Must-have** | Makes sleep/wake invisible | 1 | Low | Invisible by construction | `world_map` hydrology |
| F6 | **Lake and river water in the LOD rings by generation**, like the sea | **Conditional** | Avoids thin-water disappearance at coarse levels | 1 | Medium: rivers are 1–2 Voxels deep | Visible if downsampled; fine if generated | V3-style generation, F4 |
| F7 | Client-side fluid prediction or interpolation of geometry | **Rejected** | — | — | High: divergence from the authoritative grid | Visible | — |
| F8 | Fluid deltas through `Scene::update` | **Rejected** | — | — | Guarantees recurring 1.6 s stalls | — | — |

### 4.7 The M4 experiment

**`flood-bench-v1`** (needs a recorded delta stream and a `cpu_scene_ms` + `rebuilds` report field):

1. Replay a recorded 60 s stream (2M active Voxels, 20 Hz, a dug channel from the sea) against the client scene, once through `Scene::update` and once through the overlay.
2. Report `cpu_scene_ms` per frame, `frame_time` p95/max, the rebuild count, and GPU `primary` for both.
3. **Pass:** zero rebuilds through the overlay; no frame with more than ~2 ms of CPU scene work; the GPU cost of the overlay within the predicted 2–5%.
4. This one is CPU-bound, so its *throughput* half belongs in CI as a test, not in the GPU bench — the bench only sees the stall it causes in `frame_time` ([`benchmarking.md`](../agents/benchmarking.md)).

---

## 5. Fidelity: what pops, what cannot

The map's bar is "invisible by construction"; the referee is the image diff. Here is the classification for everything above, and the mechanism in each case.

| Content / technique | Pops or shimmers? | Mechanism | The rule that removes it |
|---|---|---|---|
| Vegetation via majority downsample | **Yes, severely** | 1-Voxel-thick slanted shells fail the 4-of-8 test at level 1, so the canopy thins then vanishes, with its shadows | V3: generate at the level asked for |
| Vegetation, generated per level | **No**, if the coarse rule is conservative | Coarse silhouette else drifts from fine | Conservative within ½ coarse Voxel; switch at ≤1 px; image-diff each ring boundary |
| One tree cut near others | **Yes** | `refresh_lod` downsamples the whole coarse brick, erasing untouched trees | V4: per-child composition |
| Vegetation at full detail in every ring | No (but VRAM) | — | V5/V6 |
| Debris spawn before BLAS ready | **Yes** — a one-frame hole | Instance not yet traversable | D3: static bricks first, then the instance |
| Debris decimation while rotating | **Yes** — the outline swims | No stable object-space grid on a tumbling body | D6 rejected; or switch only below 1 px |
| Debris merge | **No** if bricks-first; **yes** if instance-first | The order decides between a one-frame double (invisible in a ray tracer) and a hole | D5 |
| Debris pose without interpolation | **Yes** — 60 Hz stepping at close range | Snapped transforms | Interpolate; error is subpixel beyond ~400 m |
| Debris subpixel drop | **No** below ~0.5 px | Both the object and its shadow are under the gate | Drop distance from the 500·s rule |
| Debris frustum culling | **Yes** — missing shadows and bounce light | Culling is LOD ∞, and every bounce sees the set | D7 rejected |
| Fluid deltas per brick | **Yes** — a torn specular surface | Half a tick applied | F2: frame-atomic |
| Fluid deltas via `Scene::update` | **Yes** — a 1.6 s freeze every 0.5–2 s | Compaction | F1: slotted overlay |
| Settled fluid not persisted | **Yes** — the flood vanishes on revisit | Regenerated from the seed | F3: diverged-brick set |
| Ocean as coarse Voxels | **Yes at grazing angles** | Height error ÷ tan θ | F4: analytic surface |
| Fluid shading driven by simulation state | **Yes** — a ring that follows the player | Discontinuity at the sleep/wake boundary | F5: world-space shading function |
| Stale delta / revision gap | **Yes** — a permanently wrong water level | Applied to the wrong base | Per-brick revision + refetch |

The pattern is worth stating plainly: **nothing pops because it is low-detail. Things pop because the client changes its mind about what is there.** Every "no" in the table is a rule about *when* a representation changes, not about how much detail it has. That is the fidelity story for all three content types, and it is why the ranked lists are full of ordering and slot rules rather than cleverer approximations.

---

## 6. Experiments at a glance

Four GPU experiments for the dev's 4090 and one CPU unit test. Each is a variant of `client bench` except the last, and each names the number that would change a tier.

| # | Name | Milestone | Path or scene | The number that decides something | Pass |
|---|---|---|---|---|---|
| E1 | `forest-flyover-v1` | M1b | `planet-flyover-v1` over forest, on and off | primary delta, p95 frame | p95 ≤ 8.6 ms; delta inside the §2.5 range |
| E2 | `canopy-lod-v1` | M1b | six stills across two ring switches, before and after one cut tree | image diff at each switch | MSE ≤ 1.0, far-off ≤ 0.2% |
| E3 | `debris-storm-v1` | M3 | 1,000 Debris, 100 spawns/frame, fall, all merge | edits/BVH pass, rebuild count, worst hitch | no frame > 8.6 ms, no hitch > 2× budget, zero rebuilds |
| E4 | `flood-bench-v1` | M4 | recorded 60 s fluid delta stream, `Scene::update` vs overlay | `cpu_scene_ms`, rebuild count, primary delta | zero rebuilds, ≤2 ms CPU scene work, overlay ≤5% |
| E5 | `subpixel-audit` | all | unit test, no GPU | every ring and LOD switch distance | `voxel_size / distance ≤ 1 px` at every switch |

E5 is the one that keeps the other four honest: it is pure arithmetic against the camera's 70° field of view and the ring table, and it fails the moment someone widens a ring or coarsens a level without moving the switch distance.

---

## 7. What belongs to which Milestone

| Milestone | Techniques | Why then |
|---|---|---|
| **M1a (already scheduled)** | The diverged-brick predicate (§1) only as far as Edits need it; the scene layout decision that leaves room for a slotted overlay (#48) | Cheap now, expensive to retrofit; #48 is where the buffer layout is chosen |
| **M1b** | V1–V6; the `forest-flyover-v1` and `canopy-lod-v1` experiments | Vegetation is M1b scope, and the LOD rule is a generator change that must land with it |
| **M3** | D1–D5, D9; `debris-storm-v1`; D6/D8 only as benchmark follow-ups | Debris needs #50's TLAS first |
| **M4** | F1–F5, F6 conditional; `flood-bench-v1` | Fluids are server-simulated only from M4, and F1 depends on #48's layout |
| **M5 / later** | V6's coverage model refinement, D6 decimation, D8 instance masks, F4's analytic sea if M1b ships it as Voxels | All are measurable only once the earlier tiers exist |

---

## 8. Rejected and watchlist

Kept here rather than dropped, per the map, with the reason.

| Technique | Status | Reason |
|---|---|---|
| Instanced or billboard trees | **Rejected** | Two representations of one editable thing; alpha-tested foliage is noisy in a path tracer; impostors are an explicit fidelity loss |
| Frustum/per-object culling of Debris or vegetation | **Rejected** | A path tracer's BVH already culls; culling changes shadows and GI, and every bounce must see the same set |
| Debris decimation (coarse BLAS chain) | **Conditional → likely rejected** | Small win on an already-tiny BLAS; the silhouette swims on a rotating body |
| Instance-mask culling for shadow/GI rays | **Watchlist** | The one hardware-supported lever; changes the image, so it needs the image-diff gate and a measured win |
| Wind / vegetation animation | **Watchlist (not in spec)** | Makes the generator time-dependent, which conflicts with regenerable bricks; shimmer by construction |
| Dust particles (< 8 Voxels, "visual only") | **Watchlist** | Not geometry, not in `trace()`; any sprite representation is a second renderer and is outside the fidelity gate. Decide after M3 whether they earn their place |
| Client-side Fluid prediction | **Rejected** | Authoritative server grid; prediction error is visible in a specular surface |
| Fluid deltas through `Scene::update` | **Rejected** | Arithmetic above: recurring 1.6 s stalls |
| Stochastic LOD for vegetation or Debris | **Watchlist** | The right answer for many instances in a mesh renderer ([NVIDIA](https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/)): it cross-dissolves between levels in expectation and converges under accumulation. It needs two instances per object and 8 mask bits, and our vegetation is not instanced. Revisit only if V6's aggregate model proves too coarse |

---

## 9. Dependencies on the open contracts

The map allows #48–#51 to be reshaped. Four changes fall out of this note, each small and each expensive to discover later.

| Contract | Refinement | Why |
|---|---|---|
| #47 (generate at any LOD) | The generator is asked at **level `L` for a content layer**, and `remake` composes **per child**: edited octants downsample, untouched octants generate | Otherwise one cut tree erases the rest of its coarse brick (§2.2) |
| #48 (LOD rings on the GPU) | Leave room for a **fixed-capacity, in-place overlay** of fluid bricks, with a spare bit in the region record as the flag | The append-only scene cannot take 20 Hz churn without a 1.6 s rebuild every second (§4.3) |
| #49 (streaming generation) | Priority is per **content layer**: terrain first, then vegetation, then fluid overlay; vegetation beyond its own full-res radius must not be generated fine | Generation is on the spawn-ring critical path, and vegetation is cheap per Voxel but expensive per brick (§2.5) |
| #50 (hardware RT `trace()`) | Debris is a second TLAS; the compute fallback needs an instance-level structure, not a per-instance loop; the fluid overlay is a branch inside `hit_region` | Debris and fluid are the two things that do not fit the "one static scene" shape |

---

## 10. Open questions

1. **How dense is "dense vegetation"?** Every vegetation number here scales with it, and it is currently a word in a spec, not a number. A target like "closed canopy in temperate, 1 tree per 20 m²" would turn ranges into estimates.
2. **Is the ocean stored as Voxels in M1a, as the spec says, or as a source from the start?** If it ships as Voxels, F4 becomes a migration that has to be proven through the image diff; if #48 can carry a source from the beginning, it is free.
3. **Does the client ever need Debris content it evicted?** The answer depends on how far the server keeps Debris alive, which is M2/M3 policy. If "near players only", the content cache may never need to evict, and D2 collapses into D1.
4. **Does `planet-gen` generate vegetation, or a separate `flora` layer that shares its determinism rules?** Separate is cleaner for prefabs and for the GENERATOR_VERSION bump; shared is less code. [judgement: separate]
5. **What is the real cost of the scene rebuild on the full-size ring scene?** The 1.6 s figure is from the test Planet. If it is much larger, F1 moves from must-have to "nothing else can ship".
6. **Unverified here:** no public source measures TLAS build cost for *procedural AABB* BLASes on a 4090; NVIDIA's 0.22 ms figure is triangles on a 2080 Ti and is used only for scaling. The Debris LOD win (their 2.2×) is a triangle-mesh result and is **not** assumed for voxel AABBs, which NVIDIA notes use the hardware less efficiently than triangles ([RTX best practices](https://developer.nvidia.com/blog/best-practices-for-using-nvidia-rtx-ray-tracing-updated/)).

---

## Sources

**In this repo**

- [`crates/render/src/scene.rs`](../../crates/render/src/scene.rs) — the packed scene, region records, `Scene::update`'s append-and-compact behaviour, the 1.6 s rebuild.
- [`crates/render/src/shaders/trace.wgsl`](../../crates/render/src/shaders/trace.wgsl) — the one traversal path, shared by every ray.
- [`crates/brick-store/src/lod.rs`](../../crates/brick-store/src/lod.rs) — majority downsampling, `BrickSource`, `Interior`.
- [`crates/brick-store/src/store.rs`](../../crates/brick-store/src/store.rs) — `refresh_lod`, `remake`, the per-brick (not per-child) composition.
- [`crates/planet-gen/src/lib.rs`](../../crates/planet-gen/src/lib.rs), [`world_map.rs`](../../crates/planet-gen/src/world_map.rs) — determinism rules, generation cost, biomes, sea level as the median by area.
- [ADR 0002](../adr/0002-brick-store-all-voxel-lod.md), [ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md) — all-Voxel LOD; regenerable-unless-edited persistence.
- [Vertical-slice spec](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/21) — M1b, M3, M4 scope, budgets, known risks.
- [#18 resolution](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/18) — Debris detection, physics, limits, merge.
- [#19 resolution](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19) — Fluid model, sleeping, deltas, rendering.
- [`docs/research/voxel-path-tracing.md`](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/voxel-path-tracing/docs/research/voxel-path-tracing.md) — hardware RT vs compute, Debris as the case for hardware RT.

**External**

- NVIDIA, *Best Practices for Using NVIDIA RTX Ray Tracing (Updated)*, 2022-07-25 — <https://developer.nvidia.com/blog/best-practices-for-using-nvidia-rtx-ray-tracing-updated/>. Instance culling by size and distance, one BLAS per independently moving object, rebuilds causing stutter and the recommendation to distribute them over frames, AABB vs triangle efficiency.
- B. Lloyd, O. Klehm, M. Stich, *Implementing Stochastic Levels of Detail with Microsoft DirectX Raytracing*, NVIDIA, 2020-06-15 — <https://developer.nvidia.com/blog/implementing-stochastic-lod-with-microsoft-dxr/>. All rays along a path must see the same LOD (including multi-bounce GI); instance/ray masks; measured 0.22 ms TLAS build for 3,840 instances and 0.018 ms LOD selection on an RTX 2080 Ti; up to 2.2× from LOD.
- wgpu, `TlasInstance` — <https://docs.rs/wgpu/latest/wgpu/struct.TlasInstance.html>. Per-instance 3×4 affine transform, 24-bit custom index, 8-bit mask combined with the ray's cull mask.
- wgpu, `AccelerationStructureUpdateMode` — <https://docs.rs/wgpu/latest/wgpu/type.AccelerationStructureUpdateMode.html>. `Build` vs `PreferUpdate` ("if possible, perform an incremental update … useful for e.g. skinning").
- wgpu ray-tracing API spec — <https://github.com/gfx-rs/wgpu/blob/trunk/docs/api-specs/ray_tracing.md>. A TLAS must be rebuilt after any BLAS it references changes; acceleration structures are gated by `EXPERIMENTAL_RAY_QUERY`.
