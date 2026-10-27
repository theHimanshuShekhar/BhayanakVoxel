# Research: real-time voxel path tracing on mid-range RT GPUs

Resolves issue [#5](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/5). Researched October 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md).

## TL;DR

- **60 fps path-traced-looking lighting on an RTX 3060 or RX 6700 is feasible, but not by brute force.** Every shipped or published system that runs at real-time rates does the same four things:
  1. Trace very few rays (about 1 per pixel or fewer) at a reduced internal resolution.
  2. Reuse light across pixels and frames with ReSTIR or a radiance/irradiance cache.
  3. Denoise.
  4. Upscale with FSR or DLSS.
- **No public source explains how John Lin's engine actually lights its scenes.** First-party sources confirm only four things: Vulkan hardware ray tracing with per-format intersection shaders, 5-bounce path-traced GI, a temporal-only denoiser in May 2021, and RTX cards as the expected target. Everything else is unknown.
- **Two traversal approaches are proven for voxels:**
  - Compute-shader ray marching (DDA) through a sparse grid. Used by Teardown and Douglas Dwyer's Octo.
  - Hardware RT with one AABB per 8×8×8 brick and a DDA intersection shader inside each brick. Used by John Lin, Teardown's next engine and gvox_engine.
  Both can work. Compute is simpler and behaves the same on AMD and NVIDIA. Hardware RT pays off most with many moving objects (**Debris**).
- **For denoising on the target GPUs, the vendor ML denoisers are ruled out.** DLSS Ray Reconstruction costs **10.27 ms at 1440p on an RTX 3060** per NVIDIA's table. AMD's ML denoiser requires RDNA 4. The realistic choices are NRD (REBLUR) or a hand-written SVGF/A-SVGF-style temporal and spatial filter.
- **The estimated GPU frame is about 8–16 ms (midpoint about 12 ms) on an RTX 3060** at 1440p output from about 720p internal. That fits the 16.7 ms budget with little to spare. The RX 6700 is likely slower if hardware RT is used.
- **For a beginner working with AI agents, the biggest risk is debuggability, not GPU speed.** Build a slow reference path tracer first and measure every real-time shortcut against it.

## Recommended pipeline (plain language)

Each frame, for each pixel at roughly half the output resolution in each direction (about 1280×720 for 1440p output):

1. **Find what the camera sees (primary visibility).** Shoot one ray per pixel into the voxel structure. Record which **Voxel** was hit, its **Material**, its normal and its depth. This is the "G-buffer".
2. **Sun shadows.** From each hit point, shoot one ray toward a random point on the sun's disk. If it hits something, the point is in shadow.
3. **Bounce light (GI).** From each hit point (or from one pixel in every 2×2 group), shoot one ray in a random direction. Where it lands, do not keep bouncing. Look up a **world-space radiance cache** instead: a sparse 3D hash table of "how much light leaves this small region of space", updated a little every frame. The cache supplies the extra bounces, including light from emissive voxels and the sky. Later upgrade: add ReSTIR GI so neighbouring pixels and past frames share their best rays.
4. **Sky and atmosphere.** Rays that escape the **Planet** read a sky lookup table, the precomputed-texture approach from Hillaire's atmosphere technique.
5. **Denoise.** Blend each pixel with its own history from previous frames (temporal accumulation), then blur carefully, using depth and normals so edges are not smeared (spatial filter). Use NRD REBLUR or a hand-written A-SVGF-style filter.
6. **Upscale and anti-alias.** FSR 2/3 on any GPU (DLSS SR optionally on NVIDIA) turns the ~720p image into a clean 1440p image.
7. **Distance and orbit.** Further from the camera, rays read coarser LOD levels of the same voxel structure, and the cache cells get bigger. In orbital **Zoom**, lighting becomes mostly sun, shadows on the coarse planet, sky light and atmosphere. Bounce light is too small to see at that scale and can come from the cache or from per-brick values precomputed at LOD build time.

Hide all of this behind one shader function, `trace(ray) -> hit`. The tracing backend (compute DDA or hardware RT) can then be swapped and benchmarked without rewriting the lighting.

---

## 1. John Lin's approach: what is public

### Confirmed by first-party sources

- **Video description, "New Voxel Engine Reveal – Crystal Islands Experiment"** ([YouTube](https://www.youtube.com/watch?v=8ptH79R53c0), published 2021-05-13 on the "John Lin" channel; text extracted from the watch page metadata):
  - "Improved path traced global illumination that features **5 bounces from the sun, atmosphere and all emissive objects**".
  - "the path traced denoiser is **lacking a spatial filter**". The denoiser at that point was temporal only.
  - "The effective world size here is **256K^3**".
  - "this level of detail will be **reserved for RTX graphics cards**". He also lists tuning knobs, including "level-of-detail threshold, **lighting refresh rates**, and the world size itself".
  - "An 8x (512 cubic) detail increase … per-voxel material attributes" and "A 10x speedup in rendering over the previous engine".
- **Blog post, "The Perfect Voxel Engine"** ([voxely.net](https://voxely.net/blog/the-perfect-voxel-engine/), 2021-09-18):
  - The engine uses the Vulkan hardware ray tracing pipeline.
  - BLAS geometries are tagged by voxel format, so the shader binding table (SBT) can bind "specific intersection shaders" per format.
  - "Callable shaders can also be bound in order to decode attributes".
  - The design deliberately uses multiple voxel formats ("whatever voxel format is best for the job") rather than one sparse voxel octree (SVO), which he criticises.
  - The promised follow-up on the rendering architecture was never published. The [blog index](https://voxely.net/blog/) lists only three posts, the latest from 2023, about an unrelated RNN.

### Reported but not verified (search-index snippets of his X posts; x.com could not be fetched)

- Dec 2019: a voxel scene rendered "100% rtx using the … Vulkan ray tracing extension".
- Mar 2020: path tracing made "noiseless" and "20x faster (60fps HD!)".
- May 2021: "Rendering is now 10x faster on average", with ray-traced soft shadows, AO and specular.

Source: [x.com/ProgrammerLin](https://x.com/programmerlin), access blocked (HTTP 402). Treat these as **unverified**.

### Unknown

None of the sources above say:

- which GPU or resolution he used, or what his frame-time breakdown was;
- whether lighting is cached per voxel or in world space ("lighting refresh rates" hints at amortised or cached lighting, but **this is inference**);
- which sample-reuse scheme or denoiser he used;
- how he handled distant LOD.

Do not design around claims about his private engine beyond the confirmed points.

### Comparable first-party data points

- **Teardown (shipped, 2020)** is a fragment-shader voxel ray marcher. Sources:
  - Gustafsson's [80.lv interview](https://80.lv/articles/teardown-developer-breaks-down-multiplayer-and-voxel-destruction-tech) (2026-03-17): "the GPU marches through these voxel volumes along the ray". The game "doesn't implement global illumination". Ray tracing is used "for ambient occlusion, soft shadows and specular occlusion". Lacking GI is partly because the renderer "was made before hardware raytracing became available".
  - A third-party frame analysis ([Montoya](https://juandiegomontoya.github.io/teardown_breakdown.html), not first-party) shows:
    - OpenGL 3.3, with per-object 3D textures traced using mip-accelerated DDA;
    - a 2504×256×2504 one-bit "shadow volume" with 2×2×2 voxels packed per byte, over 200 MB;
    - 1–2 rays per pixel for AO and shadows, with spatiotemporal denoising and TAA.
- **Teardown's next engine** ([Voxagon "Year summary"](https://blog.voxagon.se/2024/12/29/year-summary.html), 2024-12-29) is the closest public analogue to John Lin's design:
  - "relies heavily on hardware raytracing (using intersection shaders, still no triangles!), importance sampling".
  - Drops the shadow volume, which enables "unlimited world size … no light leakage".
  - Denoising: "using NVIDIA DLSS Ray Reconstruction".
  - Sparse format: "8x8x8 voxel 'chunks', which are tracked with a 3D bitmap".
  - Gabe Rundlett took over the prototype.
- **Gabe Rundlett's open-source [gvox_engine](https://github.com/GabeRundlett/gvox_engine)** (MIT licence, C++, Vulkan via the Daxa library, GLSL):
  - [`src/renderer/rt.glsl`](https://github.com/GabeRundlett/gvox_engine/blob/rewrite/src/renderer/rt.glsl) contains a brick intersection shader (`reportIntersectionEXT`) and a `rayQueryEXT` path over `gl_RayQueryCandidateIntersectionAABBEXT`. He [described it on X](https://twitter.com/RundlettGabe/status/1781349885603668035) as "each HWRT AABB is 8x8x8 voxels".
  - [`src/renderer/kajiya/`](https://github.com/GabeRundlett/gvox_engine/tree/rewrite/src/renderer/kajiya) ports kajiya's ReSTIR GI (`rtdgi`), irradiance cache (`ircache`), reflections (`rtr`), TAA and shadow denoiser. It also integrates FSR.
  - It is the single most relevant open codebase for this project to read.
- **Douglas Dwyer's Octo engine** (Rust, WebGPU):
  - Uses "voxel ray marching with compute shaders" and "Realtime path-traced lighting (ambient occlusion, shadows, and emissive voxels)" with LODs ([octo-release README](https://github.com/DouglasDwyer/octo-release)).
  - Later added DDGI probes for multi-bounce GI ([Devlog #23](https://www.youtube.com/watch?v=L1vhle74AEU), 2025-06-28).
  - The ray-marching rewrite and the case for per-voxel normals are in [Devlog #17](https://www.youtube.com/watch?v=aY4Zet_C9Zs) (2024-03-29).

## 2. Tracing: compute ray marching vs hardware RT

| | Compute DDA / sparse-tree marching | Hardware RT (AABB per brick + intersection shader or ray query) |
|---|---|---|
| Shipped or open examples | Teardown, Octo, [dubiousconst282's 64-tree](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/) | John Lin, Teardown's next engine, gvox_engine |
| What the hardware does | Nothing special. A loop of grid steps ([Amanatides & Woo 1987](http://www.cse.yorku.ca/~amana/research/grid.pdf)) | Traverses the BVH down to brick AABBs. The voxel DDA inside each brick still runs in software |
| Edits | Write bits and bytes into buffers; the change is visible immediately | Also rebuild or refit the affected BLAS. NVIDIA: refits are "significantly cheaper than rebuilds", but quality degrades, and "many rebuilds on a single frame can lead to stuttering" ([NVIDIA best practices](https://developer.nvidia.com/blog/best-practices-for-using-nvidia-rtx-ray-tracing-updated/)) |
| Moving objects (**Debris**) | Needs your own top-level structure | TLAS instances handle this natively. Gustafsson cites "vast amounts of moving objects at a very small cost" |
| LOD | Natural: coarser mip levels of the same tree | Needs per-LOD BLASes or brick swapping |
| AMD RDNA2 (RX 6700) | Runs as well as any compute shader | "RDNA™2/3 ray tracing implemented with shader-based traversal and intersection accelerator" ([AMD, Kramer](https://gpuopen.com/download/RDNA3_Beyond-the-current-gen-v4.pdf)), so expect a weaker gain |
| API surface | Compute shaders only (also possible in WebGPU) | `VK_KHR_acceleration_structure`, plus [`VK_KHR_ray_query`](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_ray_query.html) or the RT pipeline, and [AABB geometry](https://registry.khronos.org/vulkan/specs/latest/man/html/VkAccelerationStructureGeometryAabbsDataKHR.html) |

### Evidence on speed

- **Triangle BVHs ([Tellusim](https://tellusim.com/rt-perf/), 2021-09-24, 1600×900).** Hardware RT beats a compute BVH tracer by about 7.7× on NVIDIA and 3.5× on AMD.
  - RTX 3080: 0.54–0.60 ms with hardware RT vs 4.92 ms in compute.
  - RX 6700 XT: 1.71–2.55 ms vs 6.13 ms.
  - Caveat: this compares hardware RT with a *software BVH*, not with a tuned voxel DDA. Voxel grids need no BVH, so the real gap is smaller. No source benchmarks a tuned voxel DDA against hardware RT on the target GPUs.
- **Voxel grids ([Söderlund, Evans, Akenine-Möller, JCGT 2022](https://jcgt.org/published/0011/03/06/), SDF voxel grids, RTX 3090).** "the fastest method uses the GPU's ray tracing hardware to trace against a bounding volume hierarchy, built around all non-empty voxels".
- **Brick size trade-off.** Lacewell's "Ray-Tracing Small Voxel Scenes" ([Ray Tracing Gems II ch. 37](https://link.springer.com/chapter/10.1007/978-1-4842-7185-8_37)) compares one AABB per voxel against 8³ blocks with a DDA intersection shader. Blocks save memory and BVH leaves but use the RT hardware less. *(Full text could not be fetched; summarised from a search excerpt, so treat it as unverified detail.)*
- **Large-scale compute baseline ([Aokana, I3D 2025](https://arxiv.org/html/2505.02017)).** A GPU-driven SVDAG renderer with chunked LOD and streaming, measured on an **RTX 3060 Ti**. It renders 64K³-resolution scenes "in an average of 6 milliseconds" using about 5% of scene data resident in VRAM (424 MB for San Miguel at 64K). Screen resolution is not stated, and shading is basic only: no shadows or GI. This is evidence that primary visibility over a huge voxel world fits the budget. It is **not** evidence for GI cost.
- **Memory.** dubiousconst282 measures about 0.62 bytes per voxel for a 64-tree vs about 1.02 for ESVO on Bistro (358 M voxels). Grazing rays near flat planes remain a weak spot ([post](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/)).

### Takeaway

Use compute DDA over a sparse brick hierarchy as the M1 baseline. Choose a brick layout of 8³ voxels with an occupancy bitmask, which is also directly usable as a hardware-RT AABB later. This keeps a hardware-RT backend open for **Debris** in M3, and for benchmarking on NVIDIA.

## 3. Sample reuse: ReSTIR GI, caches, probes, surfels

| Technique | What it is | Published cost | Fit |
|---|---|---|---|
| **ReSTIR GI** ([Ouyang et al., HPG 2021](https://research.nvidia.com/publication/2021-06_restir-gi-path-resampling-real-time-path-tracing)) | Pixels keep a "reservoir" holding their best bounce path and share it with neighbours and future frames | RTX 3080 at 1920×1050: initial sampling **3.2 ms** (1 bounce) or 4.2 ms (2 bounces), plus reuse **4.6 ms**. MSE 9.3–166× lower than plain path tracing at 1 spp. The paper concedes it "may still be too high … especially with lower-end GPUs", since games usually allow "less than a 2 ms computational budget for global illumination" ([PDF](https://d1qx31qr3h6wln.cloudfront.net/publications/ReSTIR%20GI.pdf)) | Best quality; expensive at full resolution. Viable at half resolution (see kajiya) |
| **kajiya** (Embark, [GI overview](https://github.com/EmbarkStudios/kajiya/blob/main/docs/gi-overview.md); Rust/Vulkan, Apache-2.0, **archived**) | Half-resolution ReSTIR GI plus a world-space irradiance cache (12 sparse 32³ clip maps, at most 65,536 entries) for later bounces; temporal denoiser; TAA | RX 6800 XT at 1080p, whole frame **8.4 ms**: G-buffer 1.15, indirect diffuse 2.3, irradiance cache 0.55, reflections 2.2, sun shadows 0.52, sky 0.1 ms | Very close to what we want; already ported to voxels in gvox_engine |
| **GI-1.0** (AMD, [paper](https://arxiv.org/html/2310.19855)) | Screen-space probes (one 8×8 probe per 8×8 tile, about ¼ spp) plus a world-space **hash-grid radiance cache** whose cells grow coarser with distance | RX 6900 XT / RTX 3080 at 1080p, total including denoise: **1.9–3.4 ms** (e.g. Kitchen #1 2.73 / 2.98 ms). The authors judge per-pixel ReSTIR "impractical for the current generation of hardware" | Cheapest proven option; the hash grid suits a sparse planet |
| **Brixelizer GI** (AMD [FidelityFX](https://gpuopen.com/fidelityfx-brixelizer/), MIT) | Simplified GI-1.0 over sparse distance-field bricks, compute only, DX12 and Vulkan | No ms figures published | Built for triangle scenes; more useful to read than to adopt |
| **DDGI probes** ([Majercik et al., JCGT 2019](https://jcgt.org/published/0008/02/01/); [McGuire](https://morgan3d.github.io/articles/2019-04-01-ddgi/)) | A grid of probes, each ray-traced and storing irradiance plus visibility | RTX 2080 Ti at 1080p: "DDGI contributed **1 ms**/frame"; glossy GI another 2 ms; 1–2 Mrays per frame. Lighting "flows slower on low-end GPUs"; cannot prevent leaks from zero-thickness walls | Cheap but blurry (probe resolution); weak for fine voxel caves. Douglas Dwyer uses it |
| **Surfel GI** | Surface patches that cache lighting | No primary source was gathered in this pass | Not recommended: surfels need surfaces, and **Voxels** already give a world-space grid |

### Recommendation

Phase it in three steps:

- **(a)** 1-bounce rays at half resolution, terminating in a **hash-grid world radiance cache** in the style of GI-1.0 or kajiya's ircache. This gives multi-bounce light from the sun, sky and emissive voxels, as in John Lin's demo, at about 2–4 ms.
- **(b)** Add ReSTIR GI on top of the half-resolution rays once (a) is stable.
- **(c)** If many emissive voxels make direct light noisy, add ReSTIR DI for emitters.

## 4. Denoisers and temporal accumulation

| Denoiser | Published cost | Notes |
|---|---|---|
| **SVGF** ([Schied et al., HPG 2017](https://research.nvidia.com/publication/2017-07_spatiotemporal-variance-guided-filtering-real-time-reconstruction-path-traced)) | "10ms (+- 15%) … at 1920x1080" on 2017 hardware | Temporal accumulation plus variance-guided à-trous blur. Ghosting under fast lighting change |
| **A-SVGF** ([Schied, Peters, Dachsbacher, HPG 2018](https://cg.ivd.kit.edu/publications/2018/adaptive_temporal_filtering/adaptive_temporal_filtering.pdf)) | "runs in 2 ms at 1080p"; gradient pipeline under 1 ms | Adds temporal gradients so history is discarded where lighting changed. This matters for **Edits** and moving sun |
| **NRD REBLUR / RELAX / SIGMA** ([NVIDIA-RTX/NRD](https://github.com/NVIDIA-RTX/NRD) v4.18) | RTX 4080 at **1440p native**: REBLUR diffuse+specular 2.55 ms; RELAX 3.25 ms; SIGMA shadow 0.40 ms | API-agnostic compute (D3D12, Vulkan, D3D11); the app issues dispatches. Strict input rules: demodulated albedo, normalised hit distance, non-jittered matrices; no resize. AMD performance is not published |
| **DLSS Ray Reconstruction** ([DLSS-RR guide](https://github.com/NVIDIA/DLSS/blob/main/doc/DLSS-RR%20Integration%20Guide.pdf), Aug 2026) | **RTX 3060: 5.83 ms at 1080p, 10.27 ms at 1440p** (Performance mode, preset default/F) | Combined denoiser and upscaler; NVIDIA only. Used by Teardown's next engine. **Too expensive for 60 fps on a 3060 at 1440p** |
| **AMD FSR Ray Regeneration** ([GPUOpen](https://gpuopen.com/amd-fsr-rayregeneration/)) | RX 9070 XT at 1440p: 5.34–8.10 ms | **RX 9000 / RDNA 4 only**, DX12 only, Windows 11. Not usable on the RX 6700 or on Vulkan |
| **Teardown / kajiya style** (see above) | Part of kajiya's 8.4 ms frame | Temporal reprojection with neighbourhood clamping, a light spatial blur, then TAA. Simplest to write |

### Scaling 4080 numbers to a 3060

NVIDIA's own DLSS SR table ([DLSS Programming Guide](https://github.com/NVIDIA/DLSS/blob/main/doc/DLSS_Programming_Guide_Release.pdf), 31 March 2026) gives these 1440p ratios:

- 3060 vs 4080: about **3.0×** (0.98 vs 0.33 ms, presets E/F) to **3.4×** (2.39 vs 0.71 ms, presets J/K).

Applied to NRD, this is an **assumption**: DLSS is tensor-heavy, NRD is ordinary compute. On that basis REBLUR would cost about 7–9 ms at native 1440p on a 3060. Denoising at a ~720p internal resolution (a quarter of the pixels) would cost about **2–2.5 ms**. Measure before relying on it.

### Recommendation

- Write a minimal temporal accumulator with clamping first. It is also the "reference mode": when the camera is still, it accumulates without limit and converges toward the ground truth.
- Then either adopt NRD REBLUR (diffuse) and SIGMA (sun shadow) at internal resolution, or extend the accumulator toward A-SVGF.
- Keep DLSS-RR as an optional NVIDIA high-end path only.

## 5. Upscaling

| Upscaler | Cost on target-class GPU | Notes |
|---|---|---|
| **FSR 2** ([FidelityFX docs](https://gpuopen.com/manuals/fidelityfx_sdk/techniques/super-resolution-temporal/)) | **RX 6700 XT**, 1440p output: Quality 0.9 ms, Performance 0.8 ms. 1080p output: 0.5 ms | Open source, any GPU, Vulkan and DX12. Needs colour, depth, motion vectors, an optional reactive mask, and exposure |
| **FSR 3.1 / 4** ([FSR SDK release v2.3.0](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/releases)) | — | The ML-based FSR 4 upscaling targets RDNA 4; SDK 2.3 adds RDNA 3 (RX 7000). RDNA 2 is not listed, so **assume the RX 6700 cannot use it** and plan on the shader-based FSR 2/3 there |
| **DLSS SR** ([DLSS Programming Guide](https://github.com/NVIDIA/DLSS/blob/main/doc/DLSS_Programming_Guide_Release.pdf)) | **RTX 3060**, 1440p Performance mode: presets E/F **0.98 ms**; J/K 2.39 ms; M 5.05 ms. 1080p: 0.60 / 1.45 ms | NVIDIA only. On a 3060, prefer the cheaper presets |

Both upscalers need the same inputs, so supporting FSR first and DLSS second is cheap. Upscaling is also the main lever that makes path tracing affordable. Tracing at 1280×720 instead of 2560×1440 means **4× fewer rays**.

## 6. Distant LOD and the orbital view

No public source describes GI for a planet-scale voxel **Zoom**. The following combines proven pieces with engineering judgement, labelled as such.

### Evidence

- **Aokana** builds LOD chunks by merging 8 children: a parent voxel exists if at least 2 of its 8 children are filled, and takes their averaged colour. It selects LOD by distance from the camera ([paper](https://arxiv.org/html/2505.02017)). The same mip chain can be traced for distant secondary rays.
- **GI-1.0** coarsens cache cells with distance so that each cell receives "a roughly constant number of samples" ([paper](https://arxiv.org/html/2310.19855)). **kajiya** uses camera-centred clip maps ([GI overview](https://github.com/EmbarkStudios/kajiya/blob/main/docs/gi-overview.md)). Both keep cache cost bounded no matter how far you can see.
- **Lumen** stops detailed GI at a 200 m trace distance and uses "Far Field" traces against simplified HLOD out to 1 km. "Past the highest Lumen Scene distance, only Screen Traces are active". At its Epic quality setting, GI plus reflections cost "around 8 ms" on consoles at 1080p internal ([Lumen technical details](https://dev.epicgames.com/documentation/en-us/unreal-engine/lumen-technical-details-in-unreal-engine)). Even Epic hands distant GI to cheaper approximations.
- **Atmosphere** ([Hillaire, EGSR 2020](https://diglib.eg.org/items/8a3e5350-18b3-46bd-9274-3add5af88c75); [reference code](https://github.com/sebh/UnrealEngineSkyAtmosphere)), on a GTX 1080 at 1280×720:
  - The LUTs (lookup textures) cost 0.01–0.07 ms each.
  - Sky plus aerial perspective total 0.31 ms.
  - **"Space view"** (the planet seen from orbit, ray-marched) costs a total of **0.5 ms**.
  - Volumetric shadows raise it to 1.0 ms.
  - The multi-scattering LUT also cuts path-traced atmosphere cost; for 5 scattering orders it drops from 0.74 to 0.29 ms.

### Proposed scheme (judgement, not sourced)

1. **Near field (up to about 50–200 m):** full pipeline, rays against full-resolution **Voxels**.
2. **Mid field:** primary rays hit coarser LOD bricks. Bounce rays may be shortened, or they may sample the cache only. Sun shadow rays still march the coarse LOD for long shadows such as mountains.
3. **Orbit:** primary visibility hits very coarse LOD. Lighting is sun times a shadow ray on coarse LOD, plus sky ambient. Add a per-brick "sky visibility / AO" value precomputed when the LOD brick is built and refreshed after **Edits**. Finish with Hillaire's space-view atmosphere. Indirect bounce is negligible at this scale.
4. **Blending:** fade cache detail and LOD levels by distance so the **Zoom** has no visible pops. Temporal accumulation must tolerate the fast camera motion of the Zoom; disoccluded pixels will be noisy for a few frames (ReSTIR GI paper, §6.1).
5. **Precision:** use camera-relative coordinates for all ray origins, because a planet-sized world exceeds float32 precision at cm scale. This is a known general issue; no specific source was gathered.

## 7. Estimated frame budget

Target: RTX 3060, 1440p output from about 1280×720 internal. Sources are linked in the sections above.

| Stage | Est. ms | Basis |
|---|---|---|
| Primary visibility (voxel DDA, G-buffer) | 1.5–3 | Aokana's 6 ms whole frame on a 3060 Ti at unknown resolution and 64K scenes; kajiya's G-buffer is 1.15 ms at 1080p on an RX 6800 XT (raster). **Estimate** |
| Sun shadows (1 ray/px) + denoise | 0.6–1.2 | kajiya 0.52 ms (full 1080p, 6800 XT); NRD SIGMA 0.40 ms (1440p, 4080) scaled. **Estimate** |
| Indirect diffuse (half-res rays + world radiance cache) | 2–4 | GI-1.0 at 1.9–3.4 ms total at 1080p on a 3080 / 6900 XT; kajiya 2.3 + 0.55 ms. Scaled to a 3060 at 720p. **Estimate** |
| ReSTIR GI upgrade (optional) | +2–4 | ReSTIR GI paper: 3.2 + 4.6 ms at full 1080p on a 3080; would run at half resolution. **Estimate** |
| Reflections (glossy materials) | 0–2 | kajiya 2.2 ms at 1080p. Skip or simplify in M1 |
| Diffuse denoise (NRD REBLUR or A-SVGF) | 1.5–2.5 | Derived from NRD's 4080 numbers × about 3 ÷ 4 pixels (assumption); A-SVGF "2 ms at 1080p" |
| Sky and atmosphere | 0.3–0.5 | Hillaire: 0.31 ms (sky) / 0.5 ms (space view) on a GTX 1080 at 720p |
| Upscale | 0.8–1.0 | DLSS SR E/F 0.98 ms (3060, 1440p); FSR 2 0.8 ms (6700 XT, 1440p Performance) |
| Edits, brick uploads, BVH or tree updates | 0.5–1.5 | **Unsourced estimate**; depends on the data-structure decision |
| Post (TAA if no upscaler, tonemap, UI) | 0.3–0.5 | **Estimate** |
| **Total (without ReSTIR GI)** | **≈ 8–16 ms** | Fits 16.7 ms only at the optimistic end; plan on 720p internal and on cutting reflections |

On the RX 6700, compute stages should run similarly. Hardware-RT stages are likely slower, because AMD traverses the BVH in shaders and Tellusim measured the 6700 XT 3–4× slower than a 3080 on hardware RT. At 1080p output (about 960×540 internal), every resolution-dependent row drops by roughly 40%.

## 8. Difficulty for a beginner working with AI agents

| Piece | Difficulty | Why |
|---|---|---|
| Reference progressive path tracer (accumulate when still) | Low–medium | Straightforward maths, and it is the ground truth every later shortcut is compared against. **Build it first** |
| Compute voxel DDA / brick traversal | Medium | Well documented ([Amanatides & Woo](http://www.cse.yorku.ca/~amana/research/grid.pdf), [dubiousconst282](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/)); easy to unit-test against CPU traversal |
| Hardware RT backend (AABB bricks, BLAS/TLAS, SBT) | High | Lots of Vulkan state, synchronisation and per-vendor behaviour. Copy patterns from gvox_engine |
| World radiance cache (hash grid) | Medium–high | Concurrency (atomics), eviction, leak handling ([GI-1.0](https://arxiv.org/html/2310.19855)); kajiya and gvox give working code |
| ReSTIR GI | High | Subtle bias and validation bugs that look "almost right". Defer |
| Denoiser | Medium (own temporal) to high (NRD integration rules) | Artefacts such as ghosting, smearing and fireflies need human eyes; agents cannot easily judge them from text |
| FSR / DLSS integration | Low–medium | Well-documented SDKs; motion vectors must be correct |
| Atmosphere | Low–medium | Reference implementation available |

### Process advice

Give agents things they can measure:

- **Debug views:** normals, albedo, cache cells, sample counts, and history length.
- **A GPU timestamp per pass**, displayed on screen, with budgets enforced in CI-like benchmark scenes.
- **An error metric:** image difference (e.g. MSE or FLIP) against the reference path tracer for fixed camera paths.

Without these, an agent cannot tell whether a lighting change made things better.

## Open questions for downstream decisions

- **Choose lighting & rendering pipeline**
  - Compute DDA baseline, or hardware RT from day one? This research favours compute-first behind a `trace()` interface.
  - Is NRD worth its integration rules, or is a hand-written A-SVGF enough?
  - Is a DLSS-only high-end path wanted at all?
- **Choose engine foundation & language**
  - The pipeline needs Vulkan (or D3D12) for NRD, FSR, DLSS and hardware RT. WebGPU/wgpu would limit us to compute-only tracing and block these SDKs.
  - The closest working references are C++/Daxa/GLSL (gvox_engine) and Rust/Vulkan (kajiya, archived). Copying patterns is easiest in one of those stacks.
- **Choose voxel data structure & LOD scheme**
  - It must provide fast DDA traversal and per-voxel material ids, plus cheap updates after **Edits**.
  - It must provide a mip/LOD chain for distant rays.
  - Ideally it uses 8³ bricks with an occupancy bitmask, so the same bricks can become hardware-RT AABBs.
  - Voxel size (2–25 cm) and planet radius determine total voxel count and streaming; Aokana-style chunked LOD with about 5% VRAM residency is the reference point.

## Sources checked but not fully read

- Lacewell, *Ray-Tracing Small Voxel Scenes*, Ray Tracing Gems II: Springer paywall redirect; summarised from a search excerpt.
- Lumen SIGGRAPH 2022 slides ([PDF](https://advances.realtimerendering.com/s2022/SIGGRAPH2022-Advances-Lumen-Wright%20et%20al.pdf), 26 MB): not read; the Lumen docs were used instead.
- John Lin's X posts: access blocked; listed above as unverified.
