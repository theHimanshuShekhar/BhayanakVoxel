# Research: John Lin's voxel water physics

Resolves issue [#133](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/133), part of the inspirations map [#130](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/130). Researched November 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). This builds on the John Lin section of [voxel-path-tracing.md](voxel-path-tracing.md) §1 and does not repeat it.

## TL;DR

- **His water is a particle/grid hybrid, not a cellular automaton.** The creator says it is "the same cell/particle hybrid method" as his August 2020 research demo (S1 description). That demo is "a hybrid lagrangian-eulerian method based on the work by" Hu, Fang, Ge, Qu, Zhu, Pradhana and Jiang (2018) (S2 description), which is the **MLS-MPM** paper (S8). That is a different family from our decided Fluid model ([#19](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19): a server-side CA with 8-bit fill levels). The water carries momentum, so it arcs off cliffs, surges through doorways and throws spray. A "down first, then sideways" CA does none of that.
- **Stated numbers:**
  - Water runs "sparsely and inline with the world generation", in a world 4096 Voxels tall ("0-4095 y").
  - The pipeline is "99% multithreaded". "4 CPU threads were used and the simulation time never exceeded 8ms."
  - Saving and loading, natural generation, spawning, and "not pouring into unloaded areas" all work.
  - Water interacts with terrain and buildings only; player and object interaction is "coming soon" (S1).
  - The 2020 demo ran on one core with AVX2, on an i7-8700K and a 2080 Ti (S2).
  - **No frame rate, GPU, step rate or particle count is given for this video.**
- **The water is finite, and the creator has no answer to that.** The waterfalls are fed by "faucets … that will run indefinitely (and eventually fill up the whole world)". He says a waterfall fed by a real body of water "will eventually run out", and "I still don't have a good idea to solve this" (S1). Our #19 has the same problem hidden inside "rivers simulate continuously". It needs an explicit source and sink rule.
- **This is the old engine, and the water never reappeared.**
  - The video (8 Jan 2021) predates the May 2021 engine rewrite. Its description promises the rewrite "will make a return … fluid dynamics eventually" (S5).
  - The channel's last upload is that reveal (S1–S5 upload dates). No public footage shows his water in the engine that is our visual reference.
  - Boats and buoyancy (one-way coupling) and water wheels followed in Jan 2021, still in the old engine (S3, S4).
- **How it looks:**
  - **Moving water** is a dense cloud of white, roughly one-Voxel-sized spray splats. Fast or freshly spawned water reads as white foam, often tinted brown or tan by the terrain behind it [0:10, 0:29, 2:07].
  - **Settled water** is clear turquoise-to-deep-blue. It refracts the bed, takes crisp tree shadows, shows marbled surface detail and a faint rainbow in the spray [0:00, 0:37, 0:50, 1:53].
  - The water's edges against walls are Voxel-stepped [1:50, 1:53].
  - Viewers' most common criticism is that the spray is "too noisy" (S10).
- **The simulation updates at least as often as the 30 fps video shows** (inferred, medium confidence). With the camera static, the water region changes on every frame while a static wall does not [1:45–1:47]. Spray splats sit in new positions on every frame [1:50 burst]. Whether that is a ≥30 Hz step or render-side interpolation cannot be told from the video. Either way it looks smoother than our 20 Hz plan would, *for particles*.
- **Best candidates to take:**
  1. **Client-side cosmetic spray and foam** driven by CA flux. It gets most of his look with no server cost.
  2. **Explicit spring and sink cells** from the world map's rivers. This solves his unsolved problem for us.
  3. **Foam/aerated shading** for falling and fast water.
  4. **Momentum in the CA** (flux memory), so waterfalls arc and floods surge. Spike it in M4.
  5. **A Fluid debug view.**

  Rejected: an MPM/FLIP particle simulation on the server, and an infinite-faucet source as a design.

## Sources and how they were used

| Key | Source | Type | Used for |
|---|---|---|---|
| **S1** | John Lin, ["Voxel Water Physics - Waterfalls, Rivers and Tunnels"](https://www.youtube.com/watch?v=1R5WFZk86kE), YouTube, 8 Jan 2021, 2:20, 1080p30 | **First-party** video and description | Description (all technical statements). **No narration or captions exist** (no manual or auto subtitle track). Full 1 fps contact sheets of all 140 s. 33 full-resolution stills. Bursts at full frame rate at 0:45 and 1:50. Frame-difference measurement at 1:45–1:47. HUD crops at 0:00. Frames at 0:23.4–0:25.4 and 2:11.5–2:13 |
| **S2** | John Lin, ["This Real-Time 3D Fluid Sim Runs on One CPU Core."](https://www.youtube.com/watch?v=4Y58Pg9tpSo), 21 Aug 2020, 1:53 | **First-party** video and description | The method's origin (the Hu et al. citation), hardware and threading. 1 frame per 3 s contact sheets of 0:00–1:33, cited as `[S2 m:ss]` |
| **S3** | John Lin, ["Water Physics + Custom Voxel Boats, Rigid Bodies & Destruction"](https://www.youtube.com/watch?v=BoPZIojpbmw), 18 Jan 2021 | First-party description only | Coupling and buoyancy as they followed this video |
| **S4** | John Lin, ["Water Wheels, Motor Concept, and Dynamic Object Building"](https://www.youtube.com/watch?v=8O1bsNaFZLE), 23 Jan 2021 | First-party description only | Water driving built objects |
| **S5** | John Lin, ["New Voxel Engine Reveal - Crystal Islands Experiment"](https://www.youtube.com/watch?v=8ptH79R53c0), 13 May 2021 | First-party description | The rewrite ("8x (512 cubic) detail increase") and "fluid dynamics eventually" making a return |
| **S6** | [@ProgrammerLin on X, Aug 2020](https://x.com/ProgrammerLin/status/1296646481286324224) | First-party post, **search snippet only** (x.com returned HTTP 402) | "real-time CPU fluid sim runs on one CPU core (with GPU rendering of course)" |
| **S7** | N. Clayton, ["John Lin's physics sandbox returns with the best water I've ever seen"](https://www.pcgamer.com/john-lins-physics-sandbox-returns-with-the-best-water-ive-ever-seen/), PC Gamer, 8 Jan 2021 | Secondary | Adds nothing technical beyond S1. It speculates about "a full evaporation and rainfall cycle" as his next step, which Lin did not say |
| **S8** | Hu et al., ["A Moving Least Squares Material Point Method with Displacement Discontinuity and Two-Way Rigid Body Coupling"](https://yzhu.io/publication/mpmmls2018siggraph/), ACM TOG 37(4), SIGGRAPH 2018, [doi:10.1145/3197517.3201293](https://dl.acm.org/doi/10.1145/3197517.3201293); code: [taichi_mpm](https://github.com/yuanming-hu/taichi_mpm) (MIT) | Primary paper | Identifies the method S2 cites. MLS-MPM, which yields APIC-style transfers |
| S9 | J. Lin, ["The Perfect Voxel Engine"](https://voxely.net/blog/the-perfect-voxel-engine/), voxely.net, 18 Sep 2021 | First-party blog | Checked for fluid content: it lists "Water" as a world requirement only. No fluid design |
| S10 | YouTube comments on S1, 417 of ~434 extracted with yt-dlp, sorted by top | Viewer reception | No comment or reply is by the uploader. Used only for reception (spray noise, audio panning) |
| S11 | John Lin, ["Ray Traced Reverb, Wind and Sound Occlusion"](https://www.youtube.com/watch?v=UHzeQZD9t2s), 19 Mar 2020, description | First-party | The old engine's audio is "RTX reverb, wind and sound occlusion using OpenAL" |

Timestamp formats:

- `[m:ss]` is S1.
- `[S2 m:ss]` is the 2020 design demo.

S1 has a music and sound track but no speech. Everything "stated" below comes from the descriptions (S1–S5) or S6.

## 1. What it is

- **A feature video for the old "Path Traced Voxel Project" engine.** The water was "the fluid simulation design from a few months ago" (S2, Aug 2020), now "implemented in our voxel project" (S1).
- **The order of events:**
  - S2 (Aug 2020): a standalone fluid demo in a test box.
  - S1 (Jan 2021): the fluid in the game world.
  - S3, S4 (Jan 2021): boats, buoyancy and water wheels.
  - S5 (May 2021): a rewritten engine, where "Rigid body physics will make a return, so will fluid dynamics eventually".
- **The channel has posted nothing since S5** (the yt-dlp channel listing). So none of the water work was ever shown at the "Crystal Islands" detail level that [#1](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/1) uses as its visual reference.
- **Scale relative to our reference.** S5 says the new engine has "an 8x (512 cubic) detail increase". The water footage is therefore 8× coarser per axis than the Crystal Islands footage (stated ratio). The metric Voxel size of either engine is never stated.

## 2. Techniques

Each item is labelled **Stated** (the creator's own words), **Inferred** (judged from frames, with a confidence) or **Paper** (what the cited method does).

### Simulation

- **Stated: a cell/particle hybrid, "fully volumetric with no height range limit beyond the 0-4095 y world boundary"** (S1). It flows "through buildings", falls "into caves", goes "over and under cliffs", fills "containers and lakebeds" and navigates "through tunnels".
- **Stated: the method comes from Hu et al. 2018** (S2, "a hybrid lagrangian-eulerian method based on the work by …").
  - **Paper:** MLS-MPM keeps the fluid's mass and velocity on particles and solves pressure and forces on a background grid every step. It uses affine (APIC-style) transfers, which keep detail and momentum (S8).
  - **Inferred, high confidence:** "cell/particle hybrid" means exactly this particle-plus-grid scheme, not a CA with decorative particles. Two things support it:
    - The S2 demo shows particles as round blobs in spray [S2 1:21], and dyed volumes that advect and mix [S2 0:21–1:00].
    - In S1 the water keeps horizontal momentum: it shoots out of doorways and surges across a lawn [2:02–2:09], and a released volume cascades off a ledge in a wide sheet [0:59–1:05].
- **Stated: sparse, chunked, inline with world generation, and bounded by loaded terrain.** It was "adapted to run sparsely and inline with the world generation". "Not pouring into unloaded areas" is listed as implemented (S1).
  - **Inferred, medium confidence:** simulation is per loaded chunk, and a chunk boundary next to unloaded terrain acts as a wall.
- **Stated: parallel with a tiny serial part.** "99% multithreaded, with a 1% critical section that has the smallest execution time. In this video, 4 CPU threads were used and the simulation time never exceeded 8ms" (S1).
  - Whether 8 ms is per step or per frame, and at what step rate, is **not stated**.
  - The 2020 demo ran "on the same main thread", "mostly accelerated using AVX2". The CPU–GPU transfers "actually have a big impact on frame times" (S2).
- **Stated: persistence and spawning.** "Saving to/loading from disk, generating it naturally, spawning and controlling it" are implemented (S1).
- **Stated: finite water, infinite faucets.** The waterfalls use faucets that "will run indefinitely (and eventually fill up the whole world), because there's no evaporative water cycle". "With water being finite, waterfalls spawning from a body of water will eventually run out. I still don't have a good idea to solve this" (S1). PC Gamer's "evaporation and rainfall cycle" as a next step is the journalist's reading, not Lin's plan (S7).
- **Stated: terrain and buildings only.** "Currently it only interacts with terrain and buildings … player and object interactions will come soon" (S1).
  - Ten days later: "One-way coupling between rigid bodies and water". "Buoyancy is simulated volumetrically, and the fluid velocity field is sampled where the objects collide with it." The splashes from objects "are just visual", and "moving objects themselves will not fill up". Two-way coupling is a hope. Trees are not affected (S3).
  - Water wheels then turn under flow, and "mass distribution and surface area greatly affect the behavior" (S4).
- **Inferred, medium confidence: the simulation advances at least as often as the 30 fps video shows.**
  - Over 1:45–1:47 the camera is static. Mean absolute luma difference between consecutive frames is ~0–0.3 on a wall region and 1.1–7.3 on the water region, and it is never zero.
  - In a six-frame burst at 1:50, every spray splat has moved on every frame.
  - This rules out a visibly stepped 10–20 Hz update. It cannot separate a ≥30 Hz step from rendering that interpolates particle positions, or from path-tracing noise on the bulk water.
- **Inferred, low confidence: the active water is local and modest.** Every scene is one pond, a pool, a short river or a flooding building. No shot shows water simulating across a landscape. The active water is probably in the 10⁵–10⁶ cell range, against our ≤2M Voxel budget. **This is a guess from scene size and is not measured.**

### Rendering

- **Stated, from the old engine generally:** path-traced lighting on Vulkan RTX (the S1 tags: "ray tracing", "path tracing", "rtx", "vulkan"; [voxel-path-tracing.md](voxel-path-tracing.md) §1 gives the X-post history). For water specifically, "Some improvements to be made include … rendering and supplemental particles" (S1).
- **Inferred, high confidence: two visual states of water.**
  1. **Settled water is a clear volume** with refraction of the bed, absorption that deepens to blue with depth, and sun shadows from trees falling across it [0:37, 1:20, 1:53]. The surface shows marbled, flow-like streaks [0:33–0:38], and the bulk reads as smooth.
  2. **Moving water is a cloud of small square splats about one Voxel across**, mostly white. Each splat's position changes every frame [1:50 burst; 0:45 burst]. Waterfalls are solid white curtains of these splats with blue-grey speckle [0:44–0:47].
- **Inferred, medium confidence: the bulk water is drawn as Voxels.** Where it meets walls, the pool's waterline is Voxel-stepped [1:53, left edge]. At 1:50 the falling blob's silhouette has stair steps. So the grid part of the hybrid is rendered as water Voxels, and the particles add spray.
- **Inferred, medium confidence: spray takes on the colour of what is behind it.**
  - Fast water over brown rock or soil turns tan or brown, not white or blue [0:10, 0:29, 1:02, 2:07, 2:12].
  - **Low-confidence cause:** temporal accumulation reprojects these fast, thin splats onto the background's history, which is a known failure of temporal-only denoisers on objects without good motion vectors. The engine's denoiser was still temporal-only in May 2021 (S5).
  - The alternative cause is deliberate turbidity or scattering.
- **Inferred, medium confidence: a faint rainbow or prismatic tint in spray** in direct sun [0:00 left of the falls; 0:49–0:50 above the pool]. It is subtle and may be colour noise rather than a deliberate effect.
- **Inferred, high confidence: no special water caustics.** The bright cyan patches on pool floors follow the sunlit gaps between tree shadows [0:33–0:38, 1:20]. That is shadowing, not caustic focusing.
- **Inferred, medium confidence: smoothed normals on terrain.** Rock reads as smooth, rounded and almost photo-textured inside blocky silhouettes [1:20 pillar, 1:33]. Our renderer uses per-face DDA normals (`crates/render/src/reference.rs`, `face()`), so this look would be **new** for us.
- **Inferred, high confidence: no visible LOD transition or fog wall.** Every scene is short-range (tens of metres).

### Streaming and audio

- **Stated:** water does not pour into unloaded areas, and it is saved and loaded with the world (S1). This is how it works with streaming. Nothing else about streaming is stated.
- **Stated:** "sound calculations" are among the improvements still to make (S1). The old engine has ray-traced reverb and occlusion through OpenAL (S11). The video has a sound track, which **was not analysed**. A top viewer comment reports hard left/right panning on headphones (S10).

## 3. Art style

Judged from the frames named.

- **Palette: a saturated autumn storybook look.**
  - Foliage in pure red, orange, yellow and lime green [0:00, 0:44, 1:24].
  - Magenta, violet and yellow flower Voxels carpet the ground [0:41, 1:41].
  - Warm pinkish-brown rock and soil [0:59, 1:20].
  - Turquoise-to-cobalt water with white foam [0:02, 0:50].
  - A floating rock island hangs in the sky [0:02].
  - Viewers compare it to "Gameboy Zelda vibes" (S10).
- **Voxel scale.**
  - Single Voxels are clearly readable everywhere. Leaves are visible clumps of Voxels, flowers are a few Voxels across, and brick courses are a few Voxels tall [0:00, 1:29, 1:52].
  - It is 8× coarser per axis than the Crystal Islands reference (S5).
  - Water spray is drawn at about Voxel size, so the spray looks pixelated up close [1:50].
- **Material looks.**
  - **Rock:** smooth-shaded with a soft photographic texture.
  - **Built Materials:** brick (dark red) and pale stone blocks with crisp mortar lines [1:41–1:55].
  - **Wood:** planks and posts on a bridge and fence [0:20–0:22].
  - **Foliage:** flat-coloured Voxel clumps.
  - **Emissives and glass:** no emissive or glass Material is visible in the scene, although "Glass" and "Light" categories exist in the HUD (§4).
- **Lighting.**
  - Bright midday sun with crisp but soft-edged tree shadows on ground and water [0:37, 1:53].
  - Caves and tunnels go near-black with only bounce light [0:52–0:53, 1:08–1:11, 1:36].
  - One tunnel is flooded with saturated red light [1:12–1:15]. The source is not visible; it is most likely sun filtered through red foliage, judged with low confidence.
- **Sky and atmosphere.**
  - Bright blue sky speckled with white dots [0:00, 1:25–1:40]. These are either a stylised sky texture or airborne particles; low confidence.
  - No visible distance fog or volumetric shafts. The scenes are short-range.
  - Falling red leaf particles drift through shots [0:44, 0:59].
- **Post-processing.** The image is soft overall, as from TAA or a temporal denoiser, though video compression adds to it. No depth of field, no motion blur, and little bloom.
- **UI and HUD** (crops at 0:00), always on screen, semi-transparent dark-olive panels with thin cream outlines:
  - **Bottom-left: a quarter-circle Brush panel.**
    - The label "Sphere" and "1/5", with five Brush-shape icons on an arc. The shapes are lumpy, rock-like solids, not perfect primitives.
    - A large preview of the selected shape, and "Volume: 0.0K".
    - A six-step vertical bar at the left edge, possibly Brush size.
    - Material category tabs reading "Earth / Rough: 0 / Light: 1 / Glass: 2 / Earth: 3 / Other: 4", above a scrollable row of Material swatches.
  - **Bottom-centre: a 10-slot hotbar** of Material swatches, each with a number: 16, 8, 4, 16, 8, 8, 8, 32, 16, 4. These are probably Brush sizes in Voxels (inferred, medium).
  - A plain OS mouse cursor, and no crosshair.

## 4. Controls

- **Camera: a free-fly spectator camera** (inferred, high confidence).
  - No character or avatar appears.
  - The camera flies through tunnels [0:51–0:54, 1:06–1:16], over treetops [1:22–1:40] and straight down onto a structure with a tilted horizon [1:55–2:00].
- **Edits are aimed with a visible mouse cursor, not a crosshair** (high confidence). The cursor sits on screen throughout, and the Ghost follows it [0:29, 1:44, 2:13]. Mouse-look presumably runs on a held button or a mode toggle. That is not shown (low confidence).
- **The Ghost** (high confidence):
  - It is drawn as a thin outline of the Brush shape at the cursor [1:44: an outline over the pool; 0:29].
  - Or it is a translucent white solid of the lumpy Brush shape [2:12–2:13: a rock-shaped white Ghost beside a brick wall].
- **Water is placed with the Brush** (inferred, medium-high confidence):
  - A white Brush-shaped volume appears at the cursor and collapses within about a second into falling spray that fills a pit [0:24.6 → 0:25.4 → 0:29].
  - The same thing happens above a ledge [0:59–1:05] and in the stone pool [1:42–1:50].
- **Digging a channel releases water** (high confidence). Digging with the sphere Brush beside a flooded area opens a trench, and the water follows it [2:10–2:16].
- **Input feel:** the camera motion is smooth and floaty, editor-like. No gameplay controls are shown.

## 5. Features, and what is demo versus shipped

Nothing is shipped. The project was never released, and Lin hoped "to eventually turn it into a game" (S7).

| Feature | Status | Source |
|---|---|---|
| Volumetric water through caves, tunnels, over and under cliffs, filling containers and lakebeds | **Demo** (old engine) | S1; [0:13–2:19] |
| Waterfalls fed by infinite faucets | **Demo** workaround; finite-water problem **unsolved** | S1 |
| Water persistence (save/load), natural generation, spawning, no flow into unloaded areas | **Implemented** (old engine) | S1 |
| Placing water with a Brush; digging channels | **Demo** | [0:24–0:29, 1:42–1:50, 2:10–2:16] |
| Dam break and flooding through a building's doorways | **Demo** | [2:02–2:09] |
| Water against players and objects | **Planned** in S1; one-way coupling and buoyancy shown 10 days later | S1, S3 |
| Water wheels driving a gate (constraints, motor) | **Demo** | S4 |
| Water audio (spatial, ray-traced) | **Demo, WIP** ("sound calculations" to improve) | S1, S11 |
| Evaporation or rain cycle | **Not planned**: "I still don't have a good idea" | S1 |
| Water in the rewritten engine (our visual reference) | **Not shown**: "fluid dynamics eventually" | S5 |

## 6. Performance and hardware

| Claim | Value | Source |
|---|---|---|
| Fluid simulation time, this video | "never exceeded 8ms" on **4 CPU threads** | S1 |
| Parallelism | "99% multithreaded, with a 1% critical section" | S1 |
| 2020 demo | One CPU core (the main thread), AVX2, i7-8700K + RTX 2080 Ti; GPU renders | S2, S6 |
| Frame rate, GPU, resolution, step rate, particle or cell count in S1 | **Not stated** | — |
| Simulation update as seen in the 30 fps video | At least every video frame, with no visible stepping (inferred) | [1:45–1:50] |

**Against our budget (#19):**

- Our budget is ≤ ~4 ms of Fluid time per 60 Hz server tick, a 20 Hz CA step and ≤ ~2M active Voxels. It serves dozens of players, each with a ~500 m simulated radius, on a headless server that must also run physics.
- His figure is 8 ms on 4 threads for one local scene with one viewer.
- Neither number converts into the other without his step rate and cell count. The difference in kind matters more:
  - A particle hybrid has to move every particle every step.
  - Our CA touches only cells whose fill level is changing and sleeps the rest.

## 7. Compared with our decided Fluid model (#19)

| Aspect | John Lin (S1–S3) | BhayanakVoxel (#19) | Consequence |
|---|---|---|---|
| Method | MLS-MPM-style particle/grid hybrid with momentum (S1, S2, S8) | CA on the Voxel grid, 8-bit fill level, down then sideways, volume conserved | Ours has **no momentum**: water falls straight down off a ledge instead of arcing [cf. 0:59–1:05], spreads instead of surging [cf. 2:02–2:09], and makes no spray |
| Where it runs | Client = the whole game (single-player), 4 threads | Headless server, active bricks only, brick deltas to clients ≤ 20 Hz | Particle state cannot be replicated in a ≤ 20 Hz brick-delta budget. Any spray must be generated on the client |
| Update rate (seen) | Every video frame or better | 20 Hz geometry, continuous shading ([dynamic-content.md](dynamic-content.md) §4.5) | Settled and slow water hide 20 Hz steps. Waterfalls and dam breaks would show them. Cosmetic client particles fill the gap |
| Scale | Local scenes; sparse, per loaded chunk | ≤ 2M active Voxels, ~500 m around each player; rivers and the ocean surface simulate continuously | Ours is far larger in scope. His sparse, chunked, bounded-by-loaded-terrain design matches our active-brick and sleep approach |
| Sources | Infinite faucets; finite water otherwise; unsolved | Ocean = infinite source; rivers "simulate continuously" | **Gap in #19:** a continuously simulated river drains unless something feeds it. It needs spring cells at river heads and sink cells at mouths (§9, candidate B) |
| Persistence | Saved and loaded; "generating it naturally" | Settled Fluid sleeps and persists as diverged bricks ([dynamic-content.md](dynamic-content.md) F3) | Same idea |
| Unloaded boundary | Does not pour into unloaded areas | Sleeps beyond ~500 m | Same idea. We should make "no flow into a brick that is not simulated" an explicit rule |
| Coupling | One-way: buoyancy volumetric, velocity sampled at contact (S3) | Currents push swimmers and Debris; buoyancy | Already decided. His one-way-first order is a sensible staging for M4 |
| Look: surface | Clear refracting volume, Voxel-stepped waterline, marbled streaks | Glass/refraction path IOR 1.33, glossy reflection, animated normal detail along the flow | Already decided. His marbling is the "animated normal detail" |
| Look: foam and spray | Dense white Voxel-sized splats; fresh or fast water is white | Not specified | **New** (candidates A, C) |
| Look: transparency, refraction, caustics | Strong absorption to blue with depth; refraction of the bed; tree shadows on and through water; no caustics | Refraction; underwater uses the volumetric fog pass | Already decided. Caustics are not needed for this look |
| Interactions | Terrain and buildings; no lava or sand | Lava + water → rock, sand sinks, currents, buoyancy | Ours is broader. Nothing to take |

## 8. What this changes in our earlier research

- **[voxel-path-tracing.md](voxel-path-tracing.md) §1** treats "John Lin's engine" as one engine. The water, boats and water-wheel videos are the **pre-rewrite** engine, which is 8× coarser per axis and renders roughly 10× slower (S5). Footage from before May 2021 should not be used to judge the look of our visual reference.
- **[dynamic-content.md](dynamic-content.md) §4.5** says Fluid shading must not depend on simulation state, so that the 500 m sleep boundary is invisible.
  - Foam (candidate C) is simulation-dependent shading.
  - The two can coexist if foam is only ever generated well inside the simulated radius and fades to zero before the boundary. At 500 m a foam patch is sub-pixel anyway.
  - This is a refinement, not a reversal.

## 9. Cross-check against what is already decided

Sources:

- [Vertical-slice map #1](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/1) Decisions so far (#7–#20).
- ADRs 0001–0004 and 0006 in [`docs/adr/`](../adr/).
- [`CONTEXT.md`](../../CONTEXT.md).
- The current code (`crates/materials/materials.toml` water: id 27, transparency 0.9, IOR 1.33, `fluid = "water"`; `crates/render/src/reference.rs` face normals; `crates/sim` has Brush and Edit but no Fluid yet).

| # | Finding | Dimension | Verdict | Against |
|---|---|---|---|---|
| 1 | Water simulated as an MLS-MPM-style particle/grid hybrid with momentum | Technique | **Conflicts** | #19 (CA, 8-bit fill levels, down then sideways) |
| 2 | Sparse, per-chunk simulation, inline with generation | Technique | **Already decided** | #19 (active bricks only), ADR 0002 (8³ bricks) |
| 3 | No flow into unloaded areas | Technique | **Already decided** (in spirit) / **New** (as an explicit rule) | #19 (sleep beyond ~500 m); ADR 0004 (residency shell) |
| 4 | 99% parallel with a 1% critical section; 4 threads; ≤ 8 ms | Technique | **New** (guidance) | #19 (≤ ~4 ms per tick; threading unspecified) |
| 5 | Simulation runs on the playing machine, single-player | Technique | **Conflicts** | #11, ADR 0003 (server-authoritative; Fluids as server deltas) |
| 6 | Updates at least as often as the 30 fps video (inferred) | Technique | **Conflicts** (rate) | #19 (20 Hz CA step) |
| 7 | Finite water fed by infinite faucets; unsolved run-out | Feature | **New** (exposes a gap) | #19 (ocean infinite; rivers "continuous" with no stated source) |
| 8 | Water saved and loaded with the world | Technique | **Already decided** | #17 (SQLite edited bricks), dynamic-content F3 |
| 9 | Placing water with the Brush and Ghost | Controls / feature | **Already decided** | #15 (placeable Fluids, Brushes), CONTEXT **Ghost** |
| 10 | Digging a channel lets water follow | Feature | **Already decided** | #19 (an Edit wakes Fluid; a channel from the sea floods) |
| 11 | One-way rigid-body coupling, volumetric buoyancy, velocity sampled at contact | Technique | **Already decided** | #19 (currents push Debris, buoyancy), #18 |
| 12 | Water wheels and constraints on built objects | Feature | **New** (out of scope) | #18 (Debris are rigid, no constraints); gameplay outside #1 |
| 13 | Settled water: clear refraction, absorption to deep blue, tree shadows, no caustics | Art / technique | **Already decided** | #10 (glass, volumetrics), #19 (IOR 1.33, glossy reflection) |
| 14 | Marbled flow streaks on the surface | Art | **Already decided** | #19 ("animated normal detail along the flow") |
| 15 | White spray and foam splats around falling and fast water | Art / feature | **New** | #19 (no spray or foam); #130 fog (cosmetic particles vs Debris) |
| 16 | Spray tinted by the background (temporal-reprojection artefact, inferred) | Technique | **New** (a pitfall to avoid) | #10 (our own A-SVGF-style denoiser) |
| 17 | Voxel-stepped waterline where water meets walls | Art | **Already decided** | ADR 0002 (all-Voxel); dynamic-content §4.2 (the analytic sea is only for the ocean) |
| 18 | Faint rainbow in spray | Art | **New** | — |
| 19 | Smooth-shaded rock inside blocky silhouettes | Art / technique | **New** | `reference.rs` face normals; no decision on normals |
| 20 | Saturated autumn palette, flower carpets, floating island | Art | **Already decided** (floating islands, glowing flora) / **New** (palette as look-dev input) | #14; #130 fog (look-dev) |
| 21 | Near-black caves lit only by bounce | Art | **Already decided** | #10 (1 bounce + radiance cache), #14 (caves) |
| 22 | Speckled sky | Art | **New** (look-dev input, low confidence) | #10 (Hillaire atmosphere) |
| 23 | Free-fly spectator camera, no avatar | Controls | **Already decided** (fly mode) | #16 (fly at 10–50 m/s; third-person avatar otherwise) |
| 24 | Edits aimed with an OS mouse cursor, no crosshair | Controls | **Conflicts** (likely) | #16 (over-shoulder third person implies a crosshair or screen-centre aim) |
| 25 | Lumpy, rock-like Brush shapes (a "Sphere" that is not a perfect sphere) | Controls / art | **New** | #15 (sphere, cube, cylinder Brushes) |
| 26 | Quarter-circle Brush panel: shape carousel, Volume readout, Material category tabs (Rough, Light, Glass, Earth, Other), 10-slot hotbar with per-slot sizes | Controls / art | **Already decided** (hotbar + palette) / **New** (category tabs, Volume readout, per-slot size) | #15 |
| 27 | Ray-traced spatial water audio | Feature | **New** | No audio decision in #1 |
| 28 | Water not shown in the rewritten (reference) engine | Planning | **New** (calibration) | #1's visual reference is S5 |
| 29 | 8× coarser Voxels than the reference engine | Art | **Not applicable** | #8 (10 cm Voxels); metric size is unstated in both |
| 30 | Old-engine footage ≠ reference look | Planning | **New** (corrects [voxel-path-tracing.md](voxel-path-tracing.md) §1 framing) | — |

## 10. Candidates to adopt

Costs assume a beginner developer with AI agents writing the code: **Low** = a ticket of days, **Medium** = one to two weeks, **High** = several weeks or a spike first.

| # | Candidate | Dimension | Rough cost | Recommendation |
|---|---|---|---|---|
| A | **Client-side cosmetic spray and foam particles.** Spawned where the CA reports water dropping over an edge, high vertical flux, or a fill level jumping. About one Voxel in size, white, short-lived, never replicated and never colliding with gameplay. It gets most of S1's look without putting particles on the server. | Art / feature | Medium (a small GPU particle system, plus a hook into the delta stream) | **Adopt for M4.** Share the particle system with #131's cosmetic rubble question. Learn from S1: keep spray **sparse and fading** (viewers called it noisy, S10), and give particles proper motion vectors so the denoiser doesn't smear background colour into them (finding 16). |
| B | **Spring and sink cells.** The world map's river heads get infinite-source cells, and river mouths, where they meet the ocean, get sink cells. Placed Fluid stays finite. This fixes the run-out S1 left unsolved, and makes "rivers simulate continuously" (#19) actually stable. | Feature / technique | Low (two Material flags or cell kinds, plus generation) | **Adopt** as an amendment to #19 before M4 starts. |
| C | **Foam / aerated shading.** Water whose fill changed recently or that is falling shades as white aerated foam and decays to clear water over ~1 s. It is driven by a per-Voxel or per-brick "agitation" value from the CA, faded to zero well inside the simulated radius. | Art | Low–Medium | **Adopt for M4.** It is the cheapest single change that reads as "his water". It must respect [dynamic-content.md](dynamic-content.md) §4.5 (see §8). |
| D | **Momentum in the CA (flux memory).** Keep a small per-cell outflow vector, as virtual-pipe / shallow-water CAs do, so water keeps going sideways off a ledge and surges out of breaches. | Technique | Medium–High (server cost and determinism unknown) | **Spike in M4** on the server, against the ≤ 4 ms budget. Adopt only if the 20 Hz result looks wrong without it. |
| E | **Explicit "no flow into non-simulated bricks" rule.** At the edge of the active set, a neighbouring brick that is not simulated acts as a wall, and the cell is woken later, never dropped. | Technique | Low | **Adopt** as an M4 rule and test. It mirrors S1's "not pouring into unloaded areas". |
| F | **Fluid debug views:** colour by active / asleep / source / sink, by flow direction, by agitation. S2's dyed-volume views [S2 0:21–1:00] show how much a coloured view helps you read fluid behaviour. | Technique | Low | **Adopt with M4**, next to the traversal-cost heatmap from #132. |
| G | **Parallel sim with a tiny serial merge.** Per-brick parallel CA steps; only cross-brick transfers are reconciled serially. | Technique | Medium | **Adopt as guidance** for the M4 sim design. It is S1's stated shape (99% / 1%). |
| H | **Brush HUD details:** Material category tabs, a Volume readout, and a per-slot Brush size on the hotbar. | Controls | Low | **Adopt as input** to the Edit-tool UI work (#15). The quarter-circle layout itself is optional. |
| I | **Organic, lumpy Brush shapes** as a variant of the sphere, so dug holes look natural, not geometric. | Art / controls | Low–Medium (noise on the Brush SDF; the Ghost must match exactly) | **Defer** to Edit-tool polish after M1b; the dev should react to a prototype first. |
| J | **Smooth-shaded Voxels** (normals from neighbouring occupancy) for rock and soil, keeping blocky silhouettes. | Art / technique | Medium (shader work, a denoiser interaction, the reference-path-tracer comparison) | **Defer to M1b look-dev** as an option to try. Not obviously wanted: it softens the "readable Voxel" look that #131/#132 praised. |
| K | **Look-dev references** from S1: a saturated autumn palette, a turquoise-to-cobalt water gradient, near-black caves. Principles only, no assets. | Art | Low | **Adopt as input** to the map's look-dev question. |
| L | **Spatial water audio.** | Feature | Medium–High | **Defer.** There is no audio decision yet; raise it as fog. |
| M | **MLS-MPM / FLIP particle Fluid** (his method) on the server or the client. | Technique | High | **Reject.** It conflicts with #19 and ADR 0003 / #11. Particles can't be replicated in ≤ 20 Hz brick deltas, it would be authoritative state on clients, and it is far beyond ≤ 4 ms at our ≤ 2M-Voxel scale. Candidates A + C + D get most of the look. |
| N | **Infinite faucets** as the way to feed waterfalls, and a rainbow-in-spray effect. | Feature / art | — | **Reject** faucets (B is the better rule). **Reject for now** the rainbow (subtle, and needs spectral or angle-dependent spray shading). |
| O | **Two-way coupling** (objects displace and fill with water). | Technique | High | **Defer.** S3 itself stops at one-way; #19's one-way scope is enough for M4. |

## Open questions this research raises

- **Where do river sources and sinks come from**, and is placed water always finite? #19 makes rivers continuous but gives them no source. Candidate B is the proposal; it touches the world map ([#14](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/14)).
- **Does a 20 Hz CA with no momentum look acceptable for waterfalls and dam breaks**, or does M4 need flux memory (D)? Only a prototype or a spike will tell.
- **Who owns cosmetic particles?** Spray (A) joins #131's cosmetic-rubble question. One client particle system, never replicated, is the natural answer. It needs a decision on when it lands (M1b, M3 or M4).
- **Is audio in scope at all?** All three of John Lin's old-engine feature videos lean on sound. The vertical slice has no audio decision.
- **Smooth versus face normals:** which way should the M1b look go?

## Unverified or not checked

- **No speech exists** in S1; every technical statement comes from the descriptions. The S1 audio track was not analysed.
- **Not stated anywhere:** the step rate, the particle or cell count, the GPU, the frame rate, and whether "8ms" is per step or per frame.
- **Not verified:** the X post S6 (x.com returned HTTP 402; text from a search snippet). Reddit posts by the creator were not reached (reddit.com is blocked to the fetcher and returned an HTML wall to curl), so any Reddit discussion by Lin is unchecked.
- **Not checked:** the S3 and S4 videos themselves (descriptions only); the full S2 video beyond the 3 s contact sheets; the 88-line MLS-MPM reference code.
- **Inference, low confidence:** the scale of the active water (10⁵–10⁶ cells), the cause of the background-tinted spray, the cause of the red tunnel light, the speckled sky, and the hotbar numbers being Brush sizes.
- **Approximate:** S1 timestamps come from 1 fps contact sheets and are accurate to about ±1 s. Bursts and differences use exact frame times.
