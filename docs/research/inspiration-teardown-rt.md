# Research: the Teardown developers' new voxel RT technology

Resolves issue [#132](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/132), part of the inspirations map [#130](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/130). Researched November 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md).

## TL;DR

- **The video is a DF commentary on a conference talk, so the talk is the primary source.** The DF Clips video (S1) is an 11:42 segment cut from DF Direct Weekly #262 (4 May 2026). In it Alex Battaglia summarises "Raytracing Voxels in Teardown and Beyond" by Dennis Gustafsson and Gabe Rundlett of Tuxedo Labs, given at the Graphics Programming Conference in Breda on 18–20 November 2025. The full 61-minute recording (S2) and the 63-page slide deck (S3) are public. Every technical claim below comes from them or from Gustafsson's blog, not from DF, unless marked **DF**.
- **The new engine is close to what #10 already decided for us.** It is a from-scratch Vulkan rewrite that has run for ~2 years (calendar time) with two engineers [S2 59:50–60:40]. Every voxel object is cut into **8×8×8 chunks**. Each non-empty chunk becomes one procedural AABB, **tightly fitted to the voxels inside it**, in a per-object BLAS. A custom intersection shader then runs the Amanatides & Woo DDA inside each chunk [S2 31:40–33:40]. **Primary visibility is ray traced too**: a GPU-driven re-meshing rasteriser was slower than tracing primary rays, "even on AMD" [S2 38:00–41:20]. The talk also confirms that Teardown uses **10 voxels per metre** [S2 13:58], the same 10 cm we chose in #8.
- **4³ chunks used the least VRAM, but 8³ rendered faster, so they chose 8³** [S2 43:00–43:40]. That is the same trade ADR 0002 made.
- **They now store full PBR data per voxel (albedo, roughness, metalness, emissive), which we rejected.** It lets any voxel be recoloured at runtime (scorch marks, paint, skid marks). They keep it at ~3.125 bytes per voxel using BC4 compression on each channel [S2 41:40–48:00; S3 p.52–60]. This **conflicts** with #9's "no per-Voxel colour". Our generated **Planet** with ~55 **Materials** does not need it.
- **What is genuinely new for us is small and practical:**
  - AABBs fitted to the occupancy mask, not the full 8³ box.
  - Regular, never irregular, grouping of terrain into acceleration structures. Irregular hand-split terrain gave them a bad TLAS [S2 37:00–38:00].
  - A traversal-cost heatmap to find wasted intersection work on occluded bricks [S2 34:00–37:00].
  - A one-layer transparency rule: shadow and GI rays ignore transparent voxels [S2 52:00–53:20].
  - Distance-adaptive secondary rays, from shipped Teardown [S2 21:20–22:20].
- **DF's commentary has three errors:**
  - It says "no one's really done it before" [6:40]. John Lin and Rundlett's own gvox_engine did per-brick AABB hardware RT earlier (see [voxel-path-tracing.md](voxel-path-tracing.md)).
  - It describes "really crazy fluid dynamics" in the demo [9:00]. The footage is piles of thousands of **rigid voxel boxes**, not fluids [9:30–10:10].
  - It presumes primary rays will be traced but says this "wasn't said directly" [8:40]. Rundlett did say it, in the Q&A [S2 52:20].
- **Art:**
  - Shipped Teardown is a muted, hazy, dusk-and-fog look with soft blob shadows.
  - The new engine is brighter daylight, with crisp contact-hardening sun shadows, visible bounce light and coloured light from emissive objects. Cinematic shots use strong depth of field.
  - At 10 cm, single voxels stay clearly readable at arm's length, and per-voxel colour noise works as the "texture".
  - The demo's debug panel shows distance fog with height parameters, volumetrics, camera-only motion blur and a night mode [S1 9:15].
- **No frame rates or GPUs are given for the new engine.** The only cost figure is that full path tracing "can take like … 10 milliseconds" on higher-end GPUs [S2 56:20–56:50].

## Sources and how they were used

| Key | Source | Type | Used for |
|---|---|---|---|
| **S1** | DF Clips, ["Teardown Developers Reveal New Voxel RT Technology"](https://www.youtube.com/watch?v=Hr_Olt52Xl0), 7 May 2026, 11:42, cut from [DF Direct Weekly #262](https://youtu.be/W8QDhsOmJ8A) (4 May 2026, segment "News 2" at 26:31–38:10) | Secondary (commentary over first-party slides and footage) | Auto-captions (en); 35 frames at 20 s intervals; 26 more at 5 s for 8:05–10:10; 12 at 8 s for 1:25–2:53; 4 at 6 s for 0:25–0:43; 2 full-resolution crops (9:15, 9:56) |
| **S2** | Gustafsson & Rundlett, ["Raytracing Voxels in Teardown and Beyond"](https://www.youtube.com/watch?v=IM1Dr98f3xU), Graphics Programming Conference 2025 (Breda, 18–20 Nov 2025), recording posted 10 Apr 2026, 60:47 | **First-party talk** | Full auto-captions; 18 frames at 30 s for 29:00–33:00 and 38:00–42:00; 15 at 4 s for the closing demo reel 48:10–49:06 |
| **S3** | [Slide deck PDF](https://static.graphicsprogrammingconference.com/public/2025/talks/raytracing-voxels-in-teardown-and-beyond/Rundlett-Gustafsson-raytracing-voxels-in-teardown-and-beyond.pdf) (63 pages), linked from the [GPC 2025 archive](https://graphicsprogrammingconference.com/archive/2025/#raytracing-voxels-in-teardown-and-beyond) | **First-party** | All pages rendered and viewed; slide titles, chart captions and comparison stills cited as `p.NN` |
| W1 | Gustafsson, ["Year summary"](https://blog.voxagon.se/2024/12/29/year-summary.html), blog.voxagon.se, 29 Dec 2024 | First-party blog | Prototype status in 2024: DLSS-RR, deformable voxels, physics, threading |
| W2 | Gustafsson, ["Teardown multiplayer"](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html), 13 Mar 2026 | First-party blog | How shipped destruction is replicated |
| W3 | D. Jagneaux, ["Teardown developer breaks down multiplayer and voxel destruction tech"](https://80.lv/articles/teardown-developer-breaks-down-multiplayer-and-voxel-destruction-tech), 80 Level, 17 Mar 2026 | Interview (Gustafsson) | Next game is "a new game in a completely redesigned engine" |
| W4 | Gustafsson [on X, 12 Nov 2025](https://x.com/voxagonlabs/status/1988600435096346936) | First-party post | Talk covers "methods used both in Teardown as well as our new engine" |
| W5 | wgpu-types / naga **30.0.1** source (our pinned `wgpu = 30.0.1`, `Cargo.lock`) | Installed dependency | What our API can express: `BlasAABBGeometrySizeDescriptor`, `RayQueryFunction::GenerateIntersection`, `EXPERIMENTAL_RAY_QUERY`; no shader-clock feature |

Timestamp formats:

- `[1:23]` is S1, the DF clip.
- `[S2 41:00]` is the talk. Talk times come from 20-second caption windows, so they are accurate to about ±20 s.
- `p.NN` is a slide page in S3.

S1's on-screen material after 0:25 is the talk itself (slides plus a picture-in-picture of the stage). The exception is 1:25–2:55 and 11:15–11:42, which is DF's own B-roll of **shipped Teardown**.

## 1. What it is

- **Who and what.** Tuxedo Labs (Teardown, 2020) is building "a new game in a completely redesigned engine" (W3). In the talk, Gustafsson (founder and CTO) covers Teardown's renderer, and Rundlett (lead rendering engineer, per the GPC listing) covers the new one [S2 2:20–3:00].
- **Not Teardown 2.** Asked "Are you going to make Teardown 2 with this new engine?", the answer was "No". The follow-up question about whether a Teardown 2 will exist at all was not answered [S2 53:40]. DF reads this as "not Teardown 2" and adds that the game will have "a lot of physical stuff". DF's guess of "lighting-based gameplay" is its own speculation [10:20–10:55].
- **Effort.** "Just me and Dennis … I started on it 2 years ago … it's really the whole engine … a total rewrite", "maybe more like 1 year effective time for the two of us" [S2 59:50–60:40]. It was built on [Daxa](https://github.com/Ipotrick/Daxa), an open-source C++ Vulkan layer that Rundlett co-develops, for its bindless resources and render graph [S2 30:40–31:40; p.38].
- **Why rewrite.** Teardown's renderer has six limitations: sparse shapes, an indexed palette, a maximum level size, no GI, CPU cost for dynamic objects, and quantization artifacts [p.34]. The bottom four come from the **shadow volume** [S2 29:00–29:20]. The new engine removes all six [p.61].

## 2. Techniques

Each item is labelled **Stated** (said or shown by the developers), **DF** (Digital Foundry's commentary) or **Inferred** (judged from frames), with a confidence.

### Rendering in shipped Teardown (the baseline)

- **Stated: raster-then-march, with no compute shaders.** The renderer was written against OpenGL 3.3 on a Mac. Each voxel object is a 3D texture, one byte per voxel, indexing a palette of 255 entries per shape [S2 4:20–6:20]. The pipeline per object:
  - Rasterise the back faces of the object's oriented box, then march the ray through the voxels with Amanatides & Woo's DDA.
  - The last step direction gives the normal.
  - Mipmaps of the volume act as an octree for empty-space skipping [S2 5:00–8:40].
- **Stated: overdraw is handled by depth binning.** Objects are drawn in distance bins. Each bin compares against a copy of the depth buffer, so occluded boxes stop early [S2 9:00–11:20].
- **Stated: lighting traces a separate world bitmap, the "shadow volume".** It is one bit per voxel for the whole level. A 400 × 340 × 29 m level at 10 voxels/m is about 4 billion voxels: 4 GB at a byte each, ~500 MB packed 8 per byte [S2 12:40–14:50; p.13–15]. Properties:
  - The CPU updates it whenever objects move or break, spread over frames. Gustafsson says this makes "the code very complex" more than slow [S2 15:00–16:20, 27:00–27:40].
  - Because it holds no colour or normal, there is no second bounce, so **no GI**, and reflections fall back to screen-space colour [S2 17:20–18:20].
  - Shading uses blue-noise stochastic sampling. Separate spatiotemporal denoisers per channel sample in a spiral pattern, then TAA [S2 17:00, 18:40].
  - Volumetric light is traced per light at half resolution [S2 18:20].
  - Quantization artifacts from rotated objects are hidden by jittering voxel positions per ray [S2 28:00–28:40].
- **Stated: trace quality varies per ray and along the ray.** Three modes:
  - Exact DDA for sun shadows.
  - Fixed-step sampling that can cut corners, used for reflections on some settings, which gives jagged edges.
  - For AO, start exact, then "double the step size and walk down the MIP chain" as the ray gets longer. Contact shadows stay crisp and far hits are cheap [S2 20:00–22:20; p.21–24].

  Gustafsson calls this "pretty hard to do with modern hardware ray tracing" [S2 22:20].

### Rendering in the new engine

- **Stated: hardware RT with procedural AABBs over 8³ chunks.**
  - Objects are still oriented voxel volumes, but each is split into a regular grid of 8×8×8 "chunks" anchored to the object's origin, so resizing an object "doesn't actually restructure the grid" [S2 31:40–32:40].
  - Each object gets one BLAS of chunk AABBs, "tightly fitted to the geometry that's within that chunk". This matters for thin things like a flagpole [S2 32:40–33:20; p.39–42 show tree AABBs before and after fitting].
  - A custom intersection shader runs the same DDA inside a chunk **without mips**: "because these volumes are so much smaller, we only need to usually take a few steps" [S2 33:20–33:40].
  - The intersection shader returns the voxel coordinate as a hit attribute, and the closest-hit shader does the shading [S2 54:20–54:40].
- **Stated: the hardware gives no control over which candidate AABB is tested first.** A far brick can be marched in full before a nearer occluder is found [S2 33:40–34:40]. Nsight's traversal heatmap is useless here because it only sees the AABBs. Instead they read `clockARB()` before and after `traceRayEXT` and map the difference to a heatmap [S2 34:40–36:20; p.43–45]. The heatmap showed wasted work on occluded geometry over large areas, which pointed to the TLAS [S2 36:20–37:00].
- **Stated: irregular terrain splits gave a bad TLAS.** Teardown's level designers hand-split terrain into irregular shapes to cut empty space. In the new engine "a much worse generated TLAS than if we were to actually just break up the terrain in a regular fashion", and the empty-space reason is gone because the chunks already skip air [S2 37:00–38:00; p.46–47].
- **Stated: primary visibility is traced.**
  - Triangle meshes of voxel faces would need "on the order of gigabytes" of VRAM [S2 38:20–38:40].
  - Rundlett built GPU-driven occlusion culling that re-meshed only visible voxels every frame and drew the quads with a compute rasteriser. It "was still too slow … slower than just ray tracing the primary ray" [S2 39:00–40:40; S1 8:20–8:50 shows it].
  - Marching into the culled chunk AABBs was faster still. Once tracing was optimised, ray-traced primary visibility was "the fastest solution", "even on AMD" [S2 40:40–41:20].
  - In the Q&A he describes the main pass as sending "G-buffer ray[s]" [S2 52:20].
  - **DF** said this "wasn't said directly" and "presumed" it [8:40]. That is incorrect.
- **Stated: transparency is one refracted layer.** The G-buffer ray that hits a transparent voxel sends one refracted secondary ray. It writes the hit position and the absorption applied, so there is only "a single transparency". Shadow and GI rays "ignore all transparency" [S2 52:20–53:20]. The design is "up in the air" [S2 52:00]. Teardown itself used a checkerboard screen-door with one level [S2 53:20–53:40].
- **Stated: GI exists but was not described.** "I don't have enough time to talk about GI today" [S2 41:20]. The chair comparison [p.35 vs p.36; S2 29:20–30:10] shows Teardown's soft, blurred shadows next to the new engine's sharp, contact-hardening shadows with bounce light under the chairs.
- **Stated, 2024 status only: DLSS Ray Reconstruction.** In Dec 2024 the prototype denoised "using NVIDIA DLSS Ray Reconstruction" and relied on "importance sampling" (W1). The 2025 talk only says Vulkan gives "access to things like DLSS and FSR and denoisers like NRD" [S2 30:40]. **Which denoiser ships is not stated.**
- **Stated: full path tracing is expensive.** "Full path tracing on even higher end GPUs can take like I don't know 10 milliseconds even" [S2 56:40]. Primary visibility is "actually faster than in Teardown" [S2 56:20]. No GPU, resolution or frame time is given.
- **Stated: hardware voxel intersection would be nice but is not needed.** "Both Nvidia and ourselves were quite surprised by the performance level we got" from custom intersection [S2 57:20–58:00]. **DF** repeats this [5:40].
- **DF, incorrect: "no one's really done it before"** [6:40–7:00]. Our [voxel-path-tracing.md](voxel-path-tracing.md) already cites per-brick AABB hardware RT in John Lin's engine and in Rundlett's own open-source gvox_engine, where he wrote "each HWRT AABB is 8x8x8 voxels" in April 2024.
- **Inferred from frames (medium confidence): TAA-style temporal smearing.** In fast motion the closing reel shows softness and slight ghosting around the falling boxes [S1 9:30, 10:00]. The reel is also motion-blurred and depth-of-field-blurred, so these cannot be separated.

### Data structures and memory

- **Stated: each voxel now carries albedo RGB, roughness, metalness and emissive.** The goal is to "recolor any voxel to be any color with any PBR properties" [S2 41:40–42:40]. Teardown's palette allowed only ~20–30 artist colours per shape, because gradients to black (scorch) and yellow (spray paint) had to be precomputed into the same 255 entries [S2 24:40–26:20; p.28].
- **Stated: sparse chunks in VRAM too.** Chunk-size trade-off [S2 43:00–43:40; p.52–54, 59–60]:
  - Uncompressed, a dense volume would cost ~5.5× Teardown's level-data VRAM.
  - 4³ chunks used the least memory, but **8³ rendered faster**, so 8³ was chosen and the render grid and storage grid are the same.
  - 16³ used more memory than either; its render speed is not given.
- **Stated: BC4 on each channel brings it to 3.125 bytes per voxel.** BC7 for albedo looked near-identical through AMD Compressonator but was too slow to re-encode in real time. Microsoft's GPU BC7 encoder showed pink fringing. BC4 on each channel works: 3.125 B/voxel. The charts' totals also count acceleration structures and AABBs [S2 44:00–47:40; p.59–60 captions]. With the shadow volume gone, the new engine uses about the same or less VRAM than Teardown on the levels shown. At 8³ the range is ~0.67–1.05× [S2 48:00 "less VRAM basically always"; p.60]. Bandwidth was not the bottleneck: compression gave "about 1% on average" because "the main thing that's restricting our performance is the actual traversal itself" [S2 49:40–50:20].
- **Stated, Rundlett's earlier personal engine: a palette per chunk.** Every chunk had "its own independent palette" with bit-packed indices [S2 51:00–51:40]. That is ADR 0002's scheme.
- **Stated, 2024: "8x8x8 voxel 'chunks', which are tracked with a 3D bitmap"; "updates are fast"; negative coordinates allowed** (W1).

### Simulation

- **Stated, 2024: physics.** It now uses "substepping instead of solver iteration", with "a parallel solver that can solve large piles of objects on multiple threads" (W1, with a picture of a large pile).
- **Inferred, high confidence: the demo is rigid bodies, not fluids.** The closing reel [S1 9:30–10:10; S2 48:54–49:10; p.62–63] shows thousands of ~0.5–1 m voxel boxes poured over a glass greenhouse, sliding and piling. They are clearly rigid bodies. The coloured ones are emissive and light their neighbours [S1 9:55].
- **DF, incorrect: "really crazy fluid dynamics"** [9:00–9:20]. Nothing in the talk, slides or blog mentions fluids.
- **Stated, 2024: deformable voxel volumes.** The blog shows voxel "sausages" that bend. Ray tracing inside a skewed box was "a much harder problem than I anticipated" and is solved approximately (W1). The talk does not mention it.
- **Stated: destruction.** "We have explosions, we have this break[age]" [S2 45:40]. These force real-time re-encoding of voxel data, which is why BC7 was rejected.

### Streaming and networking

- **Stated, shipped Teardown multiplayer (W2):**
  - Destruction is "done deterministically" in **fixed-point integer maths**. Each breakage becomes "a stream of deterministic commands … replicated on all clients" over a reliable stream. The commands are the same size regardless of object size.
  - Body transforms and velocities use state sync with eventual consistency: a server priority queue, unreliable packets, about 1 Mbit per client.
  - The host player acts as the server.
  - Late joiners replay the recorded command stream. Join-in-progress is disabled when the buffer fills.
  - New engine: "simplified approaches to multiplayer … which we're now trying out in the new engine" (W2), not detailed.
- **Streaming: not stated.** The 2024 blog says dropping the shadow volume enables "unlimited world size" (W1). The talk shows no streaming system.

## 3. Art style

Judged from the frames named. Shipped Teardown is DF's B-roll; the new engine is the talk's footage.

- **Voxel scale.** Both use 10 cm voxels ("10 voxels per meter" [S2 13:58]).
  - At arm's length single voxels are crisp squares: brick courses, window muntins and the sledgehammer head are a few voxels across [S1 2:13, 2:21].
  - Teardown deliberately leaves them readable. Rundlett: "you can see every single voxel in full clarity" [S2 44:28].
  - This is the closest available preview of how our **Voxels** will read at character scale, under different lighting.
- **Material looks.**
  - **Shipped Teardown:** flat-coloured voxels with a little per-voxel tone noise (concrete, brick, wood planks) [S1 2:05, 2:45]. Glass panes are screen-door transparent. Fire and smoke are particles [S1 2:13, 11:20].
  - **New engine:** per-voxel colour noise does the work of texture. Stucco walls are a mottle of cream, sand and ochre voxels, and tiles are mixed blues [p.51]. Rock is grey-blue with dark speckle [S1 9:05; p.46]. Trees and palms are hand-built voxel clusters [p.39–42, 51]. Greenhouse glass reflects and tints [S1 9:35–9:40; p.63]. Emissive panels, lamps and headlights light the scene at night [S1 9:20–9:25].
  - **Water and fluids:** not shown.
- **Palette and mood.**
  - **Teardown:** desaturated, hazy and atmospheric. There are golden-hour dusk scenes (warm haze, long shadows, lamp bloom) [S1 1:49, 1:57, 2:05; p.31], foggy overcast [S1 2:37, 2:53] and wet night with headlights in fog [S1 1:41].
  - **New engine:** brighter daylight with a clean sky and saturated but natural colours. There are Mediterranean whites, blues and terracotta [p.51], a white-and-green sci-fi base [S1 8:25; p.49, 55] and a pastel garden with lavender flowers [p.35–36, 63]. There is also a dark blue night mode with point lights [S1 9:15–9:25].
- **Lighting.** The new engine shows sharp sun shadows that soften with distance from the caster (contact hardening) [p.36], and bounce light under objects and in interiors [p.36 vs p.35; S1 9:05]. Emissive objects cast coloured light: magenta, blue, red and green on neighbouring boxes [S1 9:55; full-res crop at 9:56]. Teardown's no-GI interiors read as dark and unnatural, which Gustafsson points out himself [S2 26:40–27:00; p.30].
- **Atmosphere and post-processing.**
  - The debug panel in the reel [S1 9:15; S2 48:38] has these controls:
    - **Motion Blur:** "Enable (Camera-only)".
    - **Volumetrics:** Enable.
    - **Distance Fog:** type, limit, start (50.0), thickness (0.003) and two height parameters.
    - **Other:** Debug Tasks UI, Profile mode, Render, **Night mode**, Debug visualization, Physics, Stats.
  - Distance haze is visible in every daytime outdoor shot [S1 9:40, 10:05].
  - **Strong depth of field** with big bokeh is used in the cinematic close-ups [S1 9:00, 9:30, 9:45, 10:00; p.62].
  - Bloom shows on lamps and explosions in shipped Teardown [S1 0:37, 1:49, 2:13]. TAA-style softness shows in motion.
- **UI and HUD (shipped Teardown).** Minimal and text-led:
  - Small white small-caps tool name at bottom centre (SLEDGE, BLOWTORCH, SHOTGUN) with ammo above it [S1 1:57–2:53].
  - A tiny ring crosshair [S1 2:21].
  - A boxed key-hint panel at bottom right that changes with context. In a vehicle: "WASD Drive / LMB RMB Shovel / SPACE Brake / E Exit vehicle" [S1 1:41, 1:49, 11:20].
  - A thin "VEHICLE CONDITION" bar, and "Required ○○ / Optional ○" objective dots at bottom left [S1 2:21, 2:29].
  - A hand icon when grabbing [S1 2:00].
  - The new engine's footage has no game HUD, only an ImGui debug panel and a mouse cursor [S1 9:15].

## 4. Controls

- **New engine: nothing to judge.** The footage is a free-flying debug camera with a visible mouse cursor (fly-throughs, a car driven at night) [S1 9:00–9:25]. No control scheme is stated or shown.
- **Shipped Teardown (DF B-roll):**
  - First person with a held tool view-model [S1 1:57–2:53].
  - WASD movement. Tools on the mouse buttons, switched between sledge, blowtorch and shotgun.
  - Grab and carry with a hand cursor [S1 2:00].
  - Enter vehicles with E; the vehicle's own tool (a loader shovel, a dump bed) goes on LMB/RMB [S1 1:49, 11:20].
  - Edits are aimed at the crosshair and applied by tool: blunt impact, cutting and blasting, not a previewed Brush.
  - **Inferred (high confidence):** there is no **Ghost**-style preview. The tools act on contact.
- **Input feel** cannot be judged from DF's edited B-roll.

## 5. Features, and what is demo versus shipped

| Feature | Status | Source |
|---|---|---|
| Fully destructible voxel world, physics, fire, vehicles, mods | **Shipped** (Teardown, 2020) | [S2 3:20–4:20] |
| Multiplayer with deterministic destruction | **Shipped** (2026 update) | W2, W3; DF [10:00] |
| Teardown on current consoles at 60 fps | **Shipped** (DF claim; W3 credits Saber with console performance work) | DF [11:00] |
| Hardware-RT renderer: pixel-sharp shadows, reflections, GI, unlimited level size, no shadow volume | **Demo / in development** for an unannounced game | [S2 29:20–30:10, 48:20]; W1 |
| Runtime recolour of any voxel with full PBR | **Demo** (implemented, shown as charts and stills) | [S2 41:40–48:20] |
| Thousands of rigid bodies piling, emissive bodies lighting the scene | **Demo** (closing reel) | [S1 9:30–10:10]; W1 |
| One-layer refractive transparency | **In development** ("up in the air") | [S2 52:00–53:20] |
| Night mode, volumetrics, height fog, camera motion blur | **Demo** (debug toggles) | [S1 9:15] |
| Deformable (bending) voxel objects | **Prototype** (2024 blog only) | W1 |
| Fluids | **Not shown or stated** (DF's claim is wrong) | — |
| The next game itself | **Unannounced**; "not Teardown 2" per the Q&A | [S2 53:40]; W3 |

## 6. What this changes in our voxel-path-tracing research

[voxel-path-tracing.md](voxel-path-tracing.md) (#5) is mostly confirmed. Four things change:

1. **The denoiser is dated.** "Denoising: using NVIDIA DLSS Ray Reconstruction" is the Dec 2024 prototype (W1). The Nov 2025 talk does not say what ships [S2 30:40]. It should not be cited as the current choice.
2. **The shadow-volume figures** come from a third-party frame analysis (Montoya: 2504×256×2504, >200 MB). They can now be backed by Gustafsson's own example: 4000 × 3400 × 290 voxels, 8 per byte, ~500 MB [S2 13:58–14:50; p.14–15].
3. **The "hardware RT vs compute" table** can add first-party detail on Teardown's next engine:
   - One BLAS per object, with AABBs tight-fitted to each 8³ chunk's content.
   - No mips inside a chunk.
   - Primary visibility traced, and faster than GPU-driven re-meshing "even on AMD".
   - The developers and NVIDIA were both "surprised" by intersection-shader performance.

   It also adds a debugging cost: GPU tools can't see inside custom intersection shaders [S2 34:40–35:20].
4. **New evidence for the 8³ brick size**: 4³ saves VRAM, but 8³ traces faster [S2 43:20].

## 7. Cross-check against what is already decided

Sources:

- The [vertical-slice map #1](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/1) Decisions so far (#7–#20).
- ADRs 0001–0004 and 0006 in [`docs/adr/`](../adr/).
- [`CONTEXT.md`](../../CONTEXT.md).
- The current code (`crates/render`, `crates/materials`; wgpu 30.0.1).

| # | Finding | Dimension | Verdict | Against |
|---|---|---|---|---|
| 1 | Teardown uses 10 voxels/m [S2 13:58] | Art / technique | **Already decided** | #8 (10 cm **Voxels**) |
| 2 | Hardware RT, one procedural AABB per non-empty 8³ chunk, DDA inside the chunk | Technique | **Already decided** | #10 (wgpu ray queries over brick AABBs), ADR 0002 (8³ bricks). Not built yet: the renderer is still compute DDA |
| 3 | AABBs fitted to the occupied voxels inside each chunk, not the whole 8³ box | Technique | **New** (detail) | #10 says one AABB per non-empty brick |
| 4 | 4³ chunks use least VRAM, but 8³ trace faster | Technique | **Already decided** (confirms) | ADR 0002 |
| 5 | Primary visibility ray traced; re-meshing plus GPU-driven raster was slower | Technique | **Already decided** (confirms) | #10 (primary 1 ray/px into a G-buffer) |
| 6 | One BLAS per moving object, TLAS instances, chunk grid anchored to the object origin; no resampling into a world grid | Technique | **Already decided** (adds detail) | #10 ("Debris become instances later"), #18 |
| 7 | Irregular terrain splits gave a much worse TLAS than regular ones | Technique | **New** (guidance) | #10 (BLAS/TLAS granularity "tuned by benchmark") |
| 8 | Intersection work is wasted on occluded bricks; GPU tools can't see it; a per-pixel cost heatmap finds it | Technique | **New** | #13 per-pass GPU timers only; `DebugView` has no traversal-cost view; wgpu 30.0.1 has no shader-clock feature (W5) |
| 9 | Distance-adaptive secondary rays (exact near, coarser far) | Technique | **New** | #10 (1 bounce plus radiance cache); ADR 0004 guard (levels 8–13 always resident) |
| 10 | Shadow volume: a second world bitmap for lighting | Technique | **Already decided** (path not taken) | #10 traces the real brick structure |
| 11 | Full PBR stored per voxel, for runtime recolour | Technique / art | **Conflicts** | #9 ("No per-Voxel colour"), ADR 0002 (occupancy plus Material id), #15 (~55 Materials) |
| 12 | Scorch, paint and skid marks on voxels | Feature / art | **New** | #15 Edits are dig/place/smooth/flatten; would need Material variants or per-Voxel data |
| 13 | Per-voxel colour noise as the "texture" | Art | **Already decided** | `materials.toml` `noise_scale` / `hue_jitter` / `value_jitter` per Material |
| 14 | One refracted transparency layer; shadow and GI rays ignore transparent voxels | Technique | **New** (detail) | #10 (glass with refraction, 0.7 ms, M1b) |
| 15 | Sharp, contact-hardening sun shadows plus GI as the look target | Art | **Already decided** (GI) / **New** (shadow softness) | #10; `sun.rs` uses a ~2.3° sun "so that penumbrae are wide enough to see at Voxel scale" |
| 16 | DLSS Ray Reconstruction as the denoiser (2024), DLSS/FSR/NRD available | Technique | **Conflicts** (reference only) | #10 (own WGSL denoiser toward A-SVGF, no NRD/DLSS), ADR 0001 |
| 17 | Full path tracing costs ~10 ms even on high-end GPUs | Technique | **Already decided** (confirms) | #10 (1 bounce plus radiance cache; reference path tracer separate; ~8.6 ms budget) |
| 18 | Daxa (C++ Vulkan, bindless, render graph) | Technique | **Conflicts** | ADR 0001 (Rust plus wgpu, Vulkan only) |
| 19 | Deformable, bending voxel objects | Feature / technique | **Conflicts** | ADR 0002 (one Planet grid; Debris are rigid) |
| 20 | Destruction replicated as deterministic commands, bodies by state sync (~1 Mbit/client) | Technique | **Already decided** | ADR 0003 (~24 B Edit ops), #11 (Debris as snapshots), #12 (bit-exact Rust instead of fixed point) |
| 21 | Late join replays the command stream, capped, then join-in-progress is disabled | Technique | **Already decided** (differently) | ADR 0003 (edited-brick snapshots on join, no cap) |
| 22 | Host-as-server, no dedicated server | Technique | **Conflicts** | #11 (authoritative dedicated server, real server in-process for single-player) |
| 23 | Thousands of rigid bodies piling on a parallel substepping solver | Feature | **Already decided** (lower cap) | #18 (rapier3d, ≤ ~1,000 live Debris, server step ≤ 8 ms) |
| 24 | Emissive objects lighting their surroundings in colour | Art | **Already decided** | #10 (emissives in M1), #15 (light Materials) |
| 25 | Height fog controls, volumetrics, camera-only motion blur, cinematic depth of field | Art | **Already decided** (volumetrics, atmosphere) / **New** (height fog parameters, motion blur, DoF) | #10 |
| 26 | Night mode | Art | **New** | #10 (sun plus Hillaire atmosphere); no time-of-day decision |
| 27 | Mood references: Teardown's hazy dusk and fog vs the new engine's clean daylight | Art | **New** (art-direction input) | No art direction recorded yet (#130 fog) |
| 28 | Minimal HUD: tool name bottom-centre, ring crosshair, contextual key-hint box | Controls / art | **New** | #15 (hotbar plus palette UI) |
| 29 | First-person tool view-model, tools that act on contact with no preview | Controls | **Conflicts** | #16 (over-shoulder third person), CONTEXT **Ghost** |
| 30 | Vehicles; explosives and fire as Edit sources | Feature | **New** (out of scope) | #15 Edit tools; gameplay loop is outside #1 |
| 31 | Two engineers, ~2 calendar years (~1 effective) for a full engine rewrite | Planning | **New** (calibration) | #20 Milestones |
| 32 | BC4 compression of per-voxel attributes | Technique | **Conflicts** (does not apply) | ADR 0002: we store Material ids, not attributes |
| 33 | Palette per chunk with bit-packed indices | Technique | **Already decided** | ADR 0002 |
| 34 | Fluids | Feature | **Not applicable** (not shown) | #19 unaffected |

## 8. Candidates to adopt

Costs assume a beginner developer with AI agents writing the code: **Low** = a ticket of days, **Medium** = one to two weeks, **High** = several weeks or a spike first.

| # | Candidate | Dimension | Rough cost | Recommendation |
|---|---|---|---|---|
| A | **Tight brick AABBs.** Build each brick's AABB from its 512-bit occupancy mask (min/max occupied voxel), not the full 8³ box. Fewer false candidates and shorter in-brick DDA; matters for foliage, thin builds and Edit-carved bricks. wgpu 30.0.1 takes arbitrary AABBs (`BlasAABBGeometrySizeDescriptor`, W5). | Technique | Low | **Adopt** in the hardware-RT ticket. The compute fallback can use the same box for its brick entry test. Must be recomputed with each Edit, which is cheap. |
| B | **Traversal-cost heatmap debug view.** Count AABB candidates and DDA steps per pixel and show them as a heat ramp next to `Depth`/`Normals`. Tuxedo Labs used a shader clock; wgpu has none (W5), but step counters work in WGSL and on both tracers. | Technique | Low | **Adopt** now: it helps the compute DDA today and is what makes hardware-RT tuning (#10's "tuned by benchmark") possible later. |
| C | **Regular acceleration-structure grouping.** Group terrain bricks into BLASes by regular regions of the ring grid, never by irregular content-driven splits. | Technique | Low (a rule) | **Adopt as guidance** for the hardware-RT ticket's granularity benchmark. |
| D | **Debris as instanced BLASes with an object-local brick grid** anchored at the body's origin, so a moving or rotating body is never resampled into the Planet grid. That resampling is the source of Teardown's quantization artifacts. | Technique | Medium | **Adopt at M3.** It is #10's "Debris become instances" with Tuxedo Labs' proven details. |
| E | **One-layer glass rule.** The primary ray refracts once through the first transparent Voxel and records the hit and the absorption; shadow and GI rays skip transparent Voxels. | Technique / art | Low–Medium | **Adopt as the M1b glass starting point.** It keeps glass out of the denoiser's hardest cases and fits the 0.7 ms glass budget. Revisit if stacked glass or water-behind-glass matters. |
| F | **Distance-adaptive secondary rays.** Rays for AO, GI and the radiance cache trace the full-res scene near the hit and switch to coarser LOD (eventually the always-resident guard) beyond a distance. | Technique | Medium (needs a 4090 measurement) | **Defer** to the radiance-cache / GI work in M1b, as a measured option. It could also bound GI cost in the orbital **Zoom**. |
| G | **Look-dev references.** Two poles: the new engine's clean daylight with crisp contact-hardening shadows, visible bounce and coloured emissive light, and Teardown's hazy dusk and fog moods. Both show that 10 cm Voxels with per-voxel colour noise read as deliberate "voxel art". Principles only, no assets. | Art | Low | **Adopt as input** to the map's look-dev question, alongside #131's references and John Lin's. |
| H | **A sharper sun option.** Try a smaller sun angular radius than `sun.rs`'s 0.04 rad in look-dev, now that GI and denoising carry the softness. | Art | Low | **Fold into M1b look-dev.** It is a constant, but it affects the denoiser and the reference comparison, so decide it with the look. |
| I | **Post and atmosphere extras.** Height-based distance fog parameters (start, thickness, height falloff) in the atmosphere pass; optional camera-only motion blur; depth of field for a photo or cinematic mode. | Art | Low–Medium | **Adopt height fog** into the atmosphere work; **defer motion blur and DoF** to a cinematic/photo mode after M1b. |
| J | **Night mode toggle** as a look-dev and test switch for emissive Materials and the radiance cache under no sun. | Art | Low | **Adopt** as a debug/look-dev toggle. A time-of-day cycle stays fog. |
| K | **Contextual key-hint panel** and a minimal text-led HUD (selected tool and Brush name, small crosshair). | Controls | Low | **Adopt** in the Edit-tool UI work. It costs little and teaches the controls. |
| L | **Scorch / paint marks** via extra Materials (e.g. "scorched rock") set by an Edit, not per-Voxel colour. | Feature / art | Low–Medium | **Defer** until explosives or fire exist; out of scope for M1. |
| M | **Per-voxel PBR storage (with BC4).** | Technique | High | **Reject.** Conflicts with #9 and ADR 0002. Our terrain is generated from ~55 Materials and per-Voxel noise already varies colour. It would multiply GPU memory (3.125 B per voxel vs a Material id). |
| N | **Shadow volume, Daxa/C++, DLSS-RR, deformable voxels, a first-person tool view-model.** | Mixed | — | **Reject.** Each conflicts with a decision: #10, ADR 0001, #10, ADR 0002 and #16 respectively. |

## Open questions this research raises

- **Shadow softness as art direction.** Our sun is wide on purpose so penumbrae read at Voxel scale. The new engine's look is crisp contact-hardening shadows. Which one is "gorgeous" for M1b, and does a smaller sun make the denoiser's job harder?
- **How good is wgpu's hardware ray query for voxels in practice?** Tuxedo Labs' result (surprisingly fast, faster than raster for primary visibility) used native Vulkan ray-tracing pipelines with intersection shaders. Ours is wgpu's experimental ray query plus a shader-side DDA. Only a 4090 measurement against the compute DDA will tell whether the same win carries over. This sharpens #10's existing "tuned by benchmark" note into a spike.
- **Time of day.** Night mode and dusk moods keep coming up (here and in #131). Is a day/night cycle in scope, and when?
- **Rubble scale.** The demo's thousands of piling rigid boxes look great but exceed #18's ~1,000 live Debris. This joins #131's cosmetic-rubble-versus-Debris question.

## Unverified or not checked

- **Not verified:** the talk's GI method and current denoiser. Rundlett deferred GI to a later talk [S2 41:20, 48:40], and the 2025 sources do not confirm DLSS-RR.
- **Not verified:** frame rates, resolution and GPU for any new-engine footage. None are given anywhere.
- **Not verified:** the number of rigid bodies in the closing reel. "Thousands" is a visual estimate.
- **Not checked:** Gustafsson's [Software Engineering Daily podcast](https://x.com/voxagonlabs/status/1876573416603648181) (Jan 2025), the X threads by Gustafsson and Rundlett beyond the ones cited, and the Keynote (`.key`) version of the slides, which may carry speaker notes and the embedded videos.
- **Approximate:** talk timestamps come from 20-second caption windows (±20 s). Only the 29:00–49:10 stretch of the talk video was downloaded for frames; the rest was judged from the slide PDF and the captions.
- **Inference:** that BhayanakVoxel's wgpu ray-query path will see the same intersection performance as Tuxedo Labs' native Vulkan pipeline is an assumption. They also used ray-tracing pipelines (closest-hit shaders), and wgpu exposes those only as `EXPERIMENTAL_RAY_TRACING_PIPELINES` (W5).
