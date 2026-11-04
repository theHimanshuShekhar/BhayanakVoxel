# Research: spherical voxel Planets, precision, LOD and streaming to orbit

Resolves [issue #4](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/4). Researched 2026-10-27. Terms such as Planet, Voxel, Edit, Zoom, Debris and Fluid follow [`CONTEXT.md`](../../CONTEXT.md).

## TL;DR

- **Grid:** Use one ordinary Cartesian Voxel grid centred on the Planet's core, with gravity pointing at the centre. Space Engineers and Astroneer both work this way. It is the only layout with no seams ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)), and it keeps every Voxel an axis-aligned cube. A path tracer that steps through a grid needs exactly that. Use a cube-sphere (six warped faces) only for 2D inputs to procedural generation, such as height, biome and far-terrain maps. Do not use it to store Voxels.
- **Precision:** Store Voxel coordinates as integers. Even a 1000 km Planet at 2 cm needs only 27 bits per axis, so int32 is enough. Store entity and physics positions as doubles or int64. Before anything reaches the GPU, convert it to float32 relative to the camera. This is camera-relative rendering, also called "relative to eye/centre". Plain float32 Planet coordinates break above roughly 10–100 km radius at these Voxel sizes.
- **LOD and streaming:** Use a camera-centred clipmap of nested levels, each twice as coarse as the one inside it, over a sparse brick or octree store. Store only the Edits as a sparse delta on top of a deterministic generator, and regenerate everything else on demand. Space Engineers, Dual Universe and Godot Voxel Tools all use this "generator + edit delta" pattern. Seed of Andromeda and No Man's Sky switch to a 2.5D heightfield far from the camera.
- **Numbers:** Planet radius barely changes engine cost. It adds about one LOD level per doubling and does not change Edit storage at all. **Voxel size is the expensive setting.** Edit storage and near-field generation grow with 1/s³ and 1/s². Halving the Voxel size makes the same dug-out volume cost 8× more.

**Plain-language recommendation:** Build a single global grid of small cubes, with gravity pointing at the centre. Generate the terrain from a seed every time it is needed and save only what players change. Keep positions in big integers and doubles, and hand the GPU only small numbers measured from the camera. Draw near terrain at full detail and each band further out at half the detail of the band inside it. For the first Milestone, start around **R ≈ 4–16 km with 10 cm Voxels**. Keep radius and Voxel size as config values, not hard-coded constants. The precision and LOD scheme above works unchanged up to 1000 km. What limits 2 cm Voxels is Edit storage and generation throughput, not precision.

---

## 1. Grid choices for a spherical Planet

Any regular grid wrapped onto a sphere either distorts or has seams. voxel.wiki calls a distortion-free, seamless version "mathematically impossible" and judges the options on three things: perpendicularity (do built things line up with "up"?), continuity (do separate grids need stitching?) and distortion ([voxel.wiki: Planets](https://voxel.wiki/wiki/round-planets/)).

| Option | How it works | Prior art | Pros | Cons |
|---|---|---|---|---|
| **A. Plain Cartesian grid + radial gravity** ("naïve" on voxel.wiki) | One axis-aligned grid holds the whole Planet. "Up" is `normalize(pos - centre)`. | Astroneer ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)); Space Engineers ([source](https://github.com/KeenSoftwareHouse/SpaceEngineers), see §3.1) | "The *only* method that creates *truly* continuous planets" ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)). No seams or cube corners. Voxels are true cubes everywhere, so grid ray traversal (DDA), meshing and physics stay simple. | The grid axes do not match local "up" except at six points. With large cubic blocks the terrain looks jagged and "can badly disorient players" ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)). Player-built structures don't line up with the grid. |
| **B. Cube-sphere ("inflated cube") with radial layers** | Six face grids are normalised onto the sphere, and the third axis is altitude. | Seed of Andromeda: chunk positions are `(face, x, y, z)` and `voxelToWorld` normalises the face position and scales it by `radius + y` ([VoxelCoordinateSpaces.h](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/VoxelCoordinateSpaces.h), [VoxelSpaceConversions.cpp](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/VoxelSpaceConversions.cpp)) | Every Voxel's "up" really is up, which suits Minecraft-style building. | Distortion near cube corners and seams on the 12 edges ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)). Cells are not cubes, and they grow with altitude and shrink with depth, so you need "shells" that subdivide as you dig ([Bowerbyte: Blocky Planet](https://www.bowerbyte.com/posts/blocky-planet/)). Ray tracing through curved cells is much harder than a grid DDA. |
| **C. Shell-subdivided quad-sphere** | Like B, but each deeper "shell" has a quarter as many cells per layer. | Bowerbyte Blocky Planet: each shell "quadruples (2^2) the number of blocks per layer", and blocks at the bottom of a shell are "a quarter of the size" of those at the top ([post](https://www.bowerbyte.com/posts/blocky-planet/)) | Keeps cell size within 4× all the way to the core. | Three blocks meet at cube corners instead of four. Structures "can shrink across a boundary or wrap in on themselves at a corner" (same post). Complex neighbour lookups. |
| **D. Plates or pyramids** | Several flat grids, each facing outward. | StarMade unreleased "universe update" ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)) | Good for cubic blocks. | Hard seams, and nothing continuous can be built across them. |
| **E. Faked curvature** | A flat grid is bent in the shader. | none cited | Easy. | Can't Zoom to orbit ([voxel.wiki](https://voxel.wiki/wiki/round-planets/)). Rules out our Zoom requirement. |

**Distortion of cube-sphere maps.** Dimitrijević, Lambers and Rančić compare every cube-sphere projection used in planet renderers ([Facta Univ. 2016, PDF](http://casopisi.junis.ni.ac.rs/index.php/FUMathInf/article/download/871/pdf_75)). A later paper quotes their result: the adjusted spherical cube has a max/min area ratio of 1.4142, against 2.0 to 5.2 for other common mappings ([Semantic Scholar entry](https://www.semanticscholar.org/paper/COMPARISON-OF-SPHERICAL-CUBE-MAP-PROJECTIONS-USED-Dimitrijevic-Lambers/8cf18f4813589713b5b61af9a646141eb9a75156); I took the figures from the citing paper's summary and did not check them in the PDF myself). This matters if we use a cube-sphere for 2D generator inputs (option A plus cube-map inputs). Pick a low-distortion mapping so biome and height textures keep a roughly even texel size.

**Seed of Andromeda's lesson.** SoA reworked its cube-sphere mapping in 2015. The new scheme moved "the distortion away from the camera so it is not noticed". In exchange, the transition between faces needed a short fade, and "the transition to and from space is no longer seamless" ([ModDB dev summary, 21 Mar 2015](https://www.moddb.com/games/seed-of-andromeda/news/seed-of-andromeda-dev-summary-21st-march-2015)). **Unverified:** the page returned HTTP 403 to my fetcher, so these quotes come from a search-engine excerpt. That trade-off would directly hurt our continuous Zoom.

**Why A fits this project.** Our Voxels are 2–25 cm, not 1 m blocks, so the staircase you get where the grid is tilted against local "up" is only one Voxel high. A path tracer will draw it as fine surface roughness, not blocky terraces. Space Engineers uses 1 m Voxels (`VOXEL_SIZE_IN_METRES = 1f` in [MyVoxelConstants.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage/Voxels/MyVoxelConstants.cs)) on one Cartesian Voxel map per Planet. It handles building on the curve by letting blocks be "oriented to match the planet's curved surface rather than a fixed world axis" ([Planets! blog, 2015](https://blog.marekrosa.org/2015/11/planets-because-you-wanted-them_12/)). We can do the same. Player-placed shapes are snapped to a local tangent frame and then voxelised into the global grid. Debris is a separate rotated Voxel volume, much as Teardown uses "thousands of smaller volumes" that are not axis-aligned ([Game Developer](https://www.gamedeveloper.com/design/how-beautiful-voxels-laid-the-way-for-i-teardown-s-i-heist-y-framework)).

Remaining downsides of A, which we have to design around:

- The Planet's core and deep interior take up grid space. The sparse store (§3) handles that because uniform regions cost almost nothing.
- Fluid and sand settle along radial gravity, not along a grid axis, so Fluid rules must use the local "down" direction rounded to the nearest grid direction. **Open question for the Fluid ticket.**
- Building tools need a "local tangent snap" mode.

## 2. Large-Planet precision

### 2.1 Why float32 fails

A float32 has a 24-bit significand. The gap between neighbouring representable values near x is `2^(floor(log2 x) − 23)`. Godot's docs tabulate this: about 0.5 mm per step at 4–8 km and about 3.9 mm at 32–65 km. They call ~8 km the "highest recommended" single-precision range for third-person 3D and ~65 km the limit "for any 3D game" ([Godot: Large world coordinates](https://docs.godotengine.org/en/stable/tutorials/physics/large_world_coordinates.html)). Frozen Fractal measured about 0.38 m of jitter at Earth radius and saw visible ship jitter ([devlog](https://frozenfractal.com/blog/2024/4/11/around-the-world-14-floating-the-origin/)).

My working: the float32 step at the Planet surface, if the coordinates are centred on the Planet core.

| R | floor(log2 R) | step = 2^(e−23) | as % of a 2 cm Voxel |
|---|---|---|---|
| 1 km | 9 | 0.061 mm | 0.3 % |
| 10 km | 13 | 0.98 mm | 4.9 % |
| 100 km | 16 | 7.8 mm | 39 % |
| 1000 km | 19 | 62.5 mm | 312 % (worse than one Voxel) |

A step of a few percent of a Voxel already shows up as jitter in camera motion, shadows and ray origins. So **float32 Planet-centred coordinates are not viable at 2–5 cm Voxels above ~10 km radius**, and not at 2 cm above ~1 km if you also want stable physics.

### 2.2 The three standard fixes

1. **Double precision everywhere.** Unreal Engine 5's Large World Coordinates moved core types to 64-bit doubles and raised the maximum world size "from 21km to 88,000,000km" ([UE roadmap](https://portal.productboard.com/epicgames/1-unreal-engine-public-roadmap/c/1253-large-world-coordinates-on-gpu), [UE5 LWC docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/large-world-coordinates-in-unreal-engine-5)). Even so, Unreal does not use real doubles on the GPU. It emulates them as pairs of floats (`FDFScalar`/`FDFVector` with High and Low parts), and "many of the math functions … have a substantial performance cost" ([UE5 LWC rendering](https://dev.epicgames.com/documentation/en-us/unreal-engine/large-world-coordinates-rendering-in-unreal-engine-5)). Godot likewise says "shaders do not use double-precision floats for performance reasons" ([Godot docs](https://docs.godotengine.org/en/stable/tutorials/physics/large_world_coordinates.html)). So "doubles everywhere" really means doubles on the CPU plus a trick on the GPU.
2. **Floating origin.** Move the world so the viewer stays near (0,0,0). This was first described by Chris Thorne (CW 2005) as a fix for "spatial jitter" ([paper](https://www.researchgate.net/publication/331628217_Using_a_Floating_Origin_to_Improve_Fidelity_and_Performance_of_Large_Distributed_Virtual_Worlds)). Kerbal Space Program shifts the origin as the craft moves ("Krakensbane") and reworked it for precision in 1.2 ([KSP 1.2 changelog](https://wiki.kerbalspaceprogram.com/index.php?title=1.2)). The downside is that every system holding positions must handle the shift. Godot notes that origin shifting "introduces more complexity to game logic" ([Godot docs](https://docs.godotengine.org/en/stable/tutorials/physics/large_world_coordinates.html)). It also only works for one viewer, so a dedicated server tracking dozens of players can't use it.
3. **Camera-relative rendering (RTE/RTC).** Keep absolute positions in high precision on the CPU. For each frame, subtract the camera position in double or integer maths, then send float32 offsets to the GPU. AGI's "Precisions, Precisions" introduced "relative to eye" (RTE) rendering, including a GPU variant that splits each double into high and low floats ([Ohlarik 2008](https://help.agi.com/AGIComponents/html/BlogPrecisionsPrecisions.htm)). Cesium's variant is "relative to centre" (RTC): each tile stores float offsets from a nearby centre, and that centre is made camera-relative when drawing ([CESIUM_RTC](https://github.com/KhronosGroup/glTF/tree/main/extensions/1.0/Vendor/CESIUM_RTC)). Unity HDRP does this by default: objects and lights are translated "by the negated world space Camera position" ([HDRP docs](https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/Camera-Relative-Rendering.html)). Frozen Fractal stores 64-bit positions and converts them to float32 "relative to the player" at render time ([devlog](https://frozenfractal.com/blog/2024/4/11/around-the-world-14-floating-the-origin/)). Space Engineers' clipmap code works in `Vector3D`/`MatrixD` (doubles) on the CPU ([MyClipmap.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage.Render/Voxels/MyClipmap.cs)).

### 2.3 Recommended scheme for BhayanakVoxel

- **Voxel addresses are integers in the Planet frame.** You need `ceil(log2(2R/s))` bits per axis: 13–17 bits at 1 km, 17–20 at 10 km, 20–24 at 100 km, and 23–27 at 1000 km (table in §4.2). **int32 covers every combination in scope**, with room to spare. Brick or chunk keys can pack (level, x, y, z) into a 64-bit key, as Space Engineers does with a 64-bit cell key that has 4 LOD bits and 20 bits per axis ([MyCellCoord.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage/Voxels/MyCellCoord.cs)).
- **Entity, Debris and physics positions use double** (or int64 fixed-point) in the Planet frame. This works on the server for any number of players, with no origin shifting. Physics can run in per-island local frames if the physics library is float-only. That is an open question for the physics and multiplayer tickets.
- **The GPU only sees camera-relative float32.** Bricks are uploaded with their (integer brick origin − camera) offset computed in double or int64 on the CPU each frame, which is the RTC idea. Ray origins in the path tracer then start at about (0,0,0).
- **Depth:** For any rasterised parts (Debris meshes, UI, atmosphere proxies), use reversed-Z with a float depth buffer. NVIDIA's analysis found "zero error rate" in its test and that the far plane choice then does not affect precision ([Depth Precision Visualized](https://developer.nvidia.com/content/depth-precision-visualized)). Outerra's logarithmic depth is an alternative that reaches cosmic scales, but it costs about 8–10 % and breaks early-z when depth is written per pixel ([Outerra blog](https://outerra.blogspot.com/2012/11/maximizing-depth-buffer-range-and.html)). A pure path tracer needs no depth buffer for primary visibility, but it still needs float32 ray distances. Measured from the camera, those stay accurate relative to the pixel footprint at every Zoom level.

## 3. LOD, streaming and the Zoom to orbit

### 3.1 Store the generator plus the Edit delta, regenerate the rest

The pattern every shipped editable voxel planet uses:

- **Space Engineers.** `MyOctreeStorage` has two kinds of octree leaf. `ContentLeafProvider`/`MaterialLeafProvider` leaves read from a procedural `IMyStorageDataProvider`. `ContentLeafOctree`/`MaterialLeafOctree` leaves hold edited data as micro-octrees ([MyOctreeStorage.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Storage/MyOctreeStorage.cs)). The Planet provider samples heightmaps stored on a cubemap (`MyCubemapHelpers.CalculateSampleTexcoord`, [MyPlanetStorageProvider.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Planet/MyPlanetStorageProvider.cs)). Planets reached "up to 120 km in diameter" and were "fully destructible, volumetric and persistent" ([Planets! blog](https://blog.marekrosa.org/2015/11/planets-because-you-wanted-them_12/)). SE2 still starts from a cube-projected heightmap (1 m Voxels, one heightmap pixel per 8 Voxels plus a detail map). It adds 3D overhang and boulder "voxel storages" and subtracts caves ([SE2 dev diary, Aug 2025](https://blog.marekrosa.org/2025/08/mareks-dev-diary-august-21-2025/)).
- **Dual Universe.** "A planet is basically a seed number, nothing more", and the game only stores "the difference between the procedurally generated planet" and player modifications ([Baillie Q&A](https://thekoalition.com/2016/dual-universe-gameplay-debut-qa-with-novaquarks-founder-jean-christophe-baillie)). Novaquark advertised "25cm precision voxel technology" and a "64bit Planetary Engine" ([press release, 2016](https://www.prweb.com/releases/novaquark_announces_the_first_boundless_mmo_dual_universe/prweb13415044.htm)). **Unverified:** I took these press-release quotes from a search excerpt and did not re-fetch them.
- **Godot Voxel Tools.** In full-load mode, "Non-edited chunks will cause the generator to be queried on the fly instead of being cached". Uniform chunks are stored as a single value ([docs](https://voxel-tools.readthedocs.io/en/latest/smooth_terrain/)).
- **No Man's Sky.** Worlds are recomputed from a seed ([GDC 2017 talk](https://www.gdcvault.com/play/1024265/Continuous_World_Generation_in__No_Man_s_Sky_)). **Unverified:** a secondary write-up says Voxel regions are octrees on a cube projected to a sphere. It also says the Voxels model only a 128 m-thick layer on top of a low-frequency elevation base ([jianshu summary](https://www.jianshu.com/p/ea0fbd7abd6d)). I couldn't check this against the talk video.

**Implications:**

1. The generator must be **deterministic**. The server, every client, and every LOD level must agree on the same Voxel for the same inputs.
2. **Edits must flow up the LOD chain.** Godot Voxel Tools allows Edits only at full resolution ("only full-resolution voxels can be edited"). Its detail normal maps don't show edited regions when streaming ([docs](https://voxel-tools.readthedocs.io/en/latest/smooth_terrain/)). If we want a big crater visible from orbit, each Edit must also update the coarser cached levels above it (downsampling). Coarse levels then become "generated + edit-influenced" and need caching or storage too. That cost is small, because each level has 1/8 the cells of the one below.

### 3.2 LOD structure: octree versus clipmap

- **Geometry clipmaps** (Losasso and Hoppe, SIGGRAPH 2004) cache terrain in "a set of nested regular grids centered about the viewer", refilled as the viewer moves. They achieved 60 fps flight over a 40 GB US heightmap compressed about 100× ([project page](https://hhoppe.com/proj/geomclipmap/)). That version is heightfield-only. The voxel generalisation uses nested 3D boxes.
- **Space Engineers** renders its voxel Planets with a `MyClipmap` of `LodLevel`s, using a 64-bit cell key with 4 LOD bits ([MyClipmap.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage.Render/Voxels/MyClipmap.cs), [MyCellCoord.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage/Voxels/MyCellCoord.cs)). Its 2014 LOD rewrite asks for "only coarse low-detail procedural generation" for distant geometry ([blog](https://blog.marekrosa.org/2014/12/space-engineers-super-large-worlds_17/)).
- **Godot Voxel Tools** offers both. "Octree" is the original, single-viewer system. "Clipbox" "loads blocks in concentric boxes and updates only differences as viewers move" and supports multiple viewers, which is what a server with many players needs. At each LOD step "voxels and blocks double in size … However resolution of blocks doesn't change" ([docs](https://voxel-tools.readthedocs.io/en/latest/smooth_terrain/)).
- **Seams.** For meshed terrain, Transvoxel (Lengyel 2009/2010) stitches neighbouring meshes at full and half resolution "without cracks" using transition cells ([transvoxel.org](https://transvoxel.org/)). A ray-traced renderer has no mesh seams, but it still has to blend or dither between levels to avoid popping. SE's clipmap has `UseDithering` ([MyClipmap.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage.Render/Voxels/MyClipmap.cs)), and Godot has `lod_fade_duration`.

**Recommendation:** use a **camera-centred clipmap** (nested boxes, each level 2× coarser, the same cell count per level) over a **sparse store of fixed-size bricks**. Use an octree or hash map keyed by (level, brick xyz). Server interest management uses the same structure per player. The clipmap gives a constant, predictable budget per level. That suits a beginner and the 60 fps target better than a free-form octree split by screen error.

### 3.3 Rendering representations for path tracing

- **Sparse voxel octrees.** Laine and Karras (I3D 2010) showed SVO ray casting "competitive with triangle-based representations" with unique per-Voxel detail ([NVIDIA Research](https://research.nvidia.com/publication/2010-02_efficient-sparse-voxel-octrees)). Mip levels of the tree give you LOD for free.
- **Sparse voxel DAGs.** Kämpe, Sintorn and Assarsson (SIGGRAPH 2013) merge identical subtrees. They stored a 128K³ scene (19 billion Voxels) in 945 MB, where an SVO needs 5.1 GB, and ray-traced it at 170–240 MRays/s on a GTX 680 ([paper PDF](https://www.cse.chalmers.se/~uffe/HighResolutionSparseVoxelDAGs.pdf)). The paper's data is geometry only. Materials and edits need follow-up work. **Not researched here.**
- **John Lin** argues there is no single best format. SVOs are "acceptable (not even great)" for storage and rendering and weak for collision and other systems. His answer is to "use whatever voxel format is best for the job" with conversions between formats, and he renders through Vulkan ray tracing with per-format intersection shaders ([The Perfect Voxel Engine](https://voxely.net/blog/the-perfect-voxel-engine/)). He calls world structure and LOD "out of the scope for this post", so his approach to Planet-scale LOD is **unknown/unpublished**.
- **Far away, a 2.5D surface is enough.** Seed of Andromeda draws far terrain as a quadtree of heightfield patches on the cube faces, with 33×33 vertices and skirts (`PATCH_WIDTH = 33`, `NUM_SKIRTS = 4`, [TerrainPatchConstants.h](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/TerrainPatchConstants.h), [TerrainPatch.h](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/TerrainPatch.h)). Space Engineers notes that "mountain ranges goes flat once you hit a critical distance" (sic) ([Planets! blog](https://blog.marekrosa.org/2015/11/planets-because-you-wanted-them_12/)). Caves and overhangs are far below a pixel at orbit (§4.5), so a heightfield or coarse-brick "impostor" of the whole Planet is a valid top LOD.

**Suggested hybrid:**
1. Near: path-traced bricks, LOD 0…k, built from generator + Edits.
2. Mid and far: the same brick format at coarse levels.
3. Orbit: one cube-sphere height, albedo and normal map (or the coarsest brick levels) plus the atmosphere.

The Zoom is just the camera moving outward. Fine levels fall away because their extent is smaller than the camera's altitude.

### 3.4 Prior-art summary

| Project | Planet representation | Voxel size / Planet size | LOD / far field | Precision | Source quality |
|---|---|---|---|---|---|
| Space Engineers 1 | One Cartesian Voxel map, octree storage with procedural provider leaves plus edited leaves; heightmap cubemap generator | 1 m; up to 120 km diameter | Voxel clipmap, LOD bits in a 64-bit key | CPU doubles (`Vector3D`, `MatrixD`) | Source code + dev blog |
| Space Engineers 2 | Cube-projected heightmap + 3D overhang storages + caves | 1 m | not stated | not stated | Dev diary |
| Seed of Andromeda | Cube-sphere with radial altitude (face + xyz), f64 Voxel positions | not found | Heightfield quadtree patches far away | f64 positions | Source code; mapping change unverified (403) |
| No Man's Sky | Octrees on cube faces projected to a sphere; voxel layer over elevation base | not verified | not verified | not verified | GDC abstract only; details second-hand |
| Dual Universe | Seed + edit diff | "25cm precision"; planet radii not verified | "Unlimited LOD" (marketing) | "64bit Planetary Engine" (marketing) | Interview + press release |
| Astroneer | Cartesian density grid, marching cubes | not official; fan estimate radius ≈ 8 km (unverified) | octree LOD (community claim, unverified) | not stated | [Game Developer](https://www.gamedeveloper.com/design/what-i-astroneer-i-s-devs-learned-while-leaving-early-access) + forums |
| Outer Wilds | Not voxel; planets are rotating rigid bodies with mesh colliders ([Mobius blog](https://www.mobiusdigitalgames.com/news/archives/07-2015)) | n/a | n/a | Floating origin is widely claimed but **not confirmed** by a primary source | Dev blog |
| Avorion | No landable planets; planets are backdrop ([FAQ](https://www.avorion.net/faq/)) | n/a | n/a | n/a | FAQ |
| Teardown | Many small, non-axis-aligned voxel volumes, ray traced | 10 cm ([SE Daily podcast](https://softwareengineeringdaily.com/podcasts/teardown-and-voxel-based-rendering-with-dennis-gustafsson/)) | n/a (small levels) | n/a | Podcast + talk |

## 4. Feasibility arithmetic

Symbols: R = Planet radius, s = Voxel edge. Surface area A = 4πR². Volume V = (4/3)πR³. "Surface Voxels" means A/s², one Voxel layer over the whole surface. "Crust shell" means a 200 m-thick layer (A·200/s³), which is enough for mountains, caves and digging. I computed these with a short Python script (formulas exactly as written here).

### 4.1 Planet geometry

| R | A (m²) | V (m³) | Circumference | Horizon from 1.8 m eye height √(2Rh) | Area per player, 32 players |
|---|---|---|---|---|---|
| 1 km | 1.26e7 | 4.19e9 | 6.3 km | 60 m | 0.39 km² |
| 10 km | 1.26e9 | 4.19e12 | 63 km | 190 m | 39 km² |
| 100 km | 1.26e11 | 4.19e15 | 628 km | 600 m | 3,900 km² |
| 1000 km | 1.26e13 | 4.19e18 | 6,283 km | 1.9 km | 393,000 km² |

On small Planets the ground horizon is very close: 60 m at 1 km and 190 m at 10 km. "Huge render distance" then mostly matters for mountains, flight and the Zoom. Dozens of players on a 100–1000 km Planet would rarely meet.

### 4.2 Voxel counts and storage if everything were stored

"Surface @1 B" assumes one byte per surface Voxel after sparse storage, which is an optimistic lower bound. "Levels" is log2(2R/s), the number of 2× LOD steps from one Voxel up to a single cell spanning the Planet. "Bits" is the bits per integer axis, ceil(log2(2R/s)).

| R | s | Total Voxels (V/s³) | Surface Voxels (A/s²) | 200 m crust (A·200/s³) | Surface @1 B | Levels | Bits |
|---|---|---|---|---|---|---|---|
| 1 km | 2 cm | 5.2e14 | 3.1e10 | 3.1e14 | 31 GB | 16.6 | 17 |
| 1 km | 5 cm | 3.4e13 | 5.0e9 | 2.0e13 | 5.0 GB | 15.3 | 16 |
| 1 km | 10 cm | 4.2e12 | 1.3e9 | 2.5e12 | 1.3 GB | 14.3 | 15 |
| 1 km | 25 cm | 2.7e11 | 2.0e8 | 1.6e11 | 201 MB | 13.0 | 13 |
| 10 km | 2 cm | 5.2e17 | 3.1e12 | 3.1e16 | 3.1 TB | 19.9 | 20 |
| 10 km | 5 cm | 3.4e16 | 5.0e11 | 2.0e15 | 503 GB | 18.6 | 19 |
| 10 km | 10 cm | 4.2e15 | 1.3e11 | 2.5e14 | 126 GB | 17.6 | 18 |
| 10 km | 25 cm | 2.7e14 | 2.0e10 | 1.6e13 | 20 GB | 16.3 | 17 |
| 100 km | 2 cm | 5.2e20 | 3.1e14 | 3.1e18 | 314 TB | 23.3 | 24 |
| 100 km | 5 cm | 3.4e19 | 5.0e13 | 2.0e17 | 50 TB | 21.9 | 22 |
| 100 km | 10 cm | 4.2e18 | 1.3e13 | 2.5e16 | 12.6 TB | 20.9 | 21 |
| 100 km | 25 cm | 2.7e17 | 2.0e12 | 1.6e15 | 2.0 TB | 19.6 | 20 |
| 1000 km | 2 cm | 5.2e23 | 3.1e16 | 3.1e20 | 31 PB | 26.6 | 27 |
| 1000 km | 5 cm | 3.4e22 | 5.0e15 | 2.0e19 | 5.0 PB | 25.3 | 26 |
| 1000 km | 10 cm | 4.2e21 | 1.3e15 | 2.5e18 | 1.3 PB | 24.3 | 25 |
| 1000 km | 25 cm | 2.7e20 | 2.0e14 | 1.6e17 | 201 TB | 22.9 | 23 |

Example row (10 km, 10 cm): A = 4π(10⁴)² = 1.257e9 m². Surface Voxels = 1.257e9 / 0.1² = 1.26e11, so 126 GB at 1 B each. Levels = log2(2·10⁴ / 0.1) = log2(2e5) = 17.6.

**Conclusion:** Apart from the 1 km / 25 cm corner, no combination can store even the bare surface on a client. The Planet **must** be regenerated on demand, with only Edits stored. Every combination fits int32 Voxel coordinates (at most 27 bits).

### 4.3 What must be stored: Edits

Edit storage depends on how much players dig and build, not on R. A cubic metre contains 1/s³ Voxels.

| s | Voxels per m³ | One 32 m × 32 m × 32 m base (32,768 m³) at 1 B/Voxel raw | 32 players × 10,000 m³ each (assumed lifetime activity) |
|---|---|---|---|
| 2 cm | 125,000 | 4.1 GB | 40 GB |
| 5 cm | 8,000 | 262 MB | 2.6 GB |
| 10 cm | 1,000 | 33 MB | 320 MB |
| 25 cm | 64 | 2.1 MB | 20 MB |

These are raw bytes. Edited regions are mostly uniform (air, or a single Material), so brick or uniform-block compression should cut them a lot. I haven't measured that. The "10,000 m³ per player" figure is an **assumption for scale**, not data. Edits could also be stored and sent over the network as operations (e.g. "sphere brush, radius r, at p") rather than Voxel payloads. Dual Universe's "store the difference" is consistent with either approach.

### 4.4 What is regenerated: the screen-bounded working set

At 1920 px wide and 90° horizontal field of view, the focal length is f = 960 px. A Voxel of size s covers about one pixel at distance r₀ = s·f:

| s | r₀ (one Voxel ≈ one pixel) |
|---|---|
| 2 cm | 19 m |
| 5 cm | 48 m |
| 10 cm | 96 m |
| 25 cm | 240 m |

Full-resolution data beyond r₀ cannot be seen. A clipmap whose finest level spans ±r₀ has n = 2r₀/s ≈ 1,920 cells across at every Voxel size. The surface inside each level is about n² ≈ 3.7e6 cells, or ¾ of that for the ring that isn't covered by the finer level. The total over L levels is roughly 3e6·L. **That figure is set by screen resolution, not by R.** R only adds levels.

| R | Levels above the r₀ level, until one level covers the Planet, log2(2R/(s·f)): 2 / 5 / 10 / 25 cm |
|---|---|
| 1 km | 6.7 / 5.4 / 4.4 / 3.1 |
| 10 km | 10.0 / 8.7 / 7.7 / 6.4 |
| 100 km | 13.3 / 12.0 / 11.0 / 9.7 |
| 1000 km | 16.7 / 15.3 / 14.3 / 13.0 |

The working set can be halved per axis with "one Voxel per 2 px", which divides it by 4. The **one-pixel-per-Voxel** budget is an assumption. A path tracer also needs off-screen data for bounce lighting, which coarse levels can supply.

**Regeneration rate while moving.** Model: level k covers width W·2ᵏ with cells s·2ᵏ. Moving at speed v adds v·W/(2ᵏs²) new surface cells per second at level k, and summing over all levels gives at most **2·v·W/s²**. With W = 2r₀ = 2sf, that simplifies to **4·v·f/s**. At v = 10 m/s (assumed fast travel near the ground):

| s | New surface cells per second (W = 2r₀) | With a fixed W = 128 m |
|---|---|---|
| 2 cm | 1.9e6 | 6.4e6 |
| 5 cm | 7.7e5 | 1.0e6 |
| 10 cm | 3.8e5 | 2.6e5 |
| 25 cm | 1.5e5 | 4.1e4 |

Each surface cell needs several Voxels of depth (a brick column), so multiply by about 8–16 for Voxel evaluations. Smaller Voxels make this roughly linearly or quadratically more expensive, and **it does not depend on R.** All of these rates look achievable for a GPU compute generator, but **I haven't benchmarked that.**

### 4.5 Orbit view

From orbit, with the Planet spanning about 1920 px, one pixel covers 2R/1920: 1 m at R = 1 km, 10 m at 10 km, 104 m at 100 km, and 1 km at 1000 km. Caves, overhangs and individual Edits smaller than that are invisible. The top LOD can be a 2.5D heightfield or coarse bricks plus an albedo/normal map.

## 5. Recommendations for the decision tickets

**For "Choose Voxel size & Planet radius":**

- Radius is mostly a **gameplay** choice: walking times, player density, horizon distance and visible curvature. With integer addressing, camera-relative rendering and a generator-plus-delta store, the engine cost grows only by about one LOD level per doubling of R.
- Voxel size is the **engine-cost** knob. Edit storage and network payload grow with 1/s³ (125,000 Voxels per m³ at 2 cm against 1,000 at 10 cm). Near-field generation grows with 1/s to 1/s². r₀ shrinks with s, so the finest LOD level covers only ~19 m at 2 cm.
- Suggested starting point: **s = 10 cm** (Teardown's proven scale, and 1,000 Voxels per m³) and **R ≈ 4–16 km** (the horizon at 1.8 m is 120–240 m). That gives about 200–3,200 km² of surface (6–100 km² per player for 32 players) and 17–19 LOD levels. Make both values config constants and test 5 cm later.

**For "Choose voxel data structure & LOD scheme":**

- Use a sparse brick store keyed by (level, brick xyz) in int32 Planet-frame coordinates. Each brick is either uniform, generated-only (re-derivable, can be evicted), or edited (persisted).
- Use a camera- or viewer-centred clipmap with 2× levels. Its multi-viewer form serves server interest management.
- Propagate Edits upward into coarser levels.
- Render with a GPU sparse structure such as an SVO/DAG or brick map with mips, uploaded camera-relative.
- Use a cube-sphere 2D map only as generator input and for the orbit-level heightfield.

## 6. Open questions and doubts

1. **Deterministic generation across machines.** The server and clients must generate identical Voxels. Float noise on different GPUs or compilers may not be bit-identical. The options are to generate on the CPU, use integer/fixed-point noise, or send authoritative bricks from the server. **I haven't researched this; it affects multiplayer.**
2. **Edit compression ratio.** The Edit storage figures are raw. Real figures need a prototype, such as a brick store with uniform-brick and palette compression.
3. **Edits seen from afar.** How much should coarse levels reflect Edits (craters from orbit)? Each level is cheap (1/8 of the cells below it), but it needs a cache invalidation design.
4. **Fluid and Debris on a Cartesian grid with radial gravity.** "Down" is not a grid axis. Fluid rules and Debris re-voxelisation (when Debris lands and merges back) need a rule for that.
5. **Unverified prior-art details:** the Seed of Andromeda 2015 mapping trade-offs (page blocked), the No Man's Sky 128 m voxel layer (second-hand), Astroneer's size and octree LOD (fan or community sources), Dual Universe's planet radii, and whether Outer Wilds uses a floating origin.
6. **John Lin-style rendering at Planet scale.** No published source explains how his engine handles LOD or large worlds. Our path tracer's LOD (mip bricks versus a DAG) is still an open design question for the rendering ticket.
7. **Physics library precision.** If the physics engine is float-only, Debris and players need local physics frames (per-island origins) on the server. That needs checking against the chosen library.

## Sources

Primary (source code, papers, official docs, official dev blogs):
- Space Engineers source: [MyVoxelConstants.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage/Voxels/MyVoxelConstants.cs), [MyOctreeStorage.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Storage/MyOctreeStorage.cs), [MyPlanetStorageProvider.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Planet/MyPlanetStorageProvider.cs), [MyClipmap.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage.Render/Voxels/MyClipmap.cs), [MyCellCoord.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/VRage/Voxels/MyCellCoord.cs)
- Marek Rosa blog: [Planets! (2015)](https://blog.marekrosa.org/2015/11/planets-because-you-wanted-them_12/), [Super-large worlds (2014)](https://blog.marekrosa.org/2014/12/space-engineers-super-large-worlds_17/), [SE2 dev diary Aug 2025](https://blog.marekrosa.org/2025/08/mareks-dev-diary-august-21-2025/)
- Seed of Andromeda source: [VoxelCoordinateSpaces.h](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/VoxelCoordinateSpaces.h), [VoxelSpaceConversions.cpp](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/VoxelSpaceConversions.cpp), [TerrainPatch.h](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/TerrainPatch.h), [TerrainPatchConstants.h](https://github.com/RegrowthStudios/SoACode-Public/blob/develop/SoA/TerrainPatchConstants.h)
- Losasso and Hoppe, [Geometry Clipmaps (2004)](https://hhoppe.com/proj/geomclipmap/)
- Lengyel, [Transvoxel](https://transvoxel.org/)
- Laine and Karras, [Efficient Sparse Voxel Octrees (2010)](https://research.nvidia.com/publication/2010-02_efficient-sparse-voxel-octrees)
- Kämpe, Sintorn and Assarsson, [High Resolution Sparse Voxel DAGs (2013)](https://www.cse.chalmers.se/~uffe/HighResolutionSparseVoxelDAGs.pdf)
- Dimitrijević, Lambers and Rančić, [Comparison of Spherical Cube Map Projections (2016)](http://casopisi.junis.ni.ac.rs/index.php/FUMathInf/article/download/871/pdf_75)
- Thorne, [Using a Floating Origin (2005)](https://www.researchgate.net/publication/331628217_Using_a_Floating_Origin_to_Improve_Fidelity_and_Performance_of_Large_Distributed_Virtual_Worlds)
- Ohlarik, [Precisions, Precisions (2008)](https://help.agi.com/AGIComponents/html/BlogPrecisionsPrecisions.htm); Khronos [CESIUM_RTC](https://github.com/KhronosGroup/glTF/tree/main/extensions/1.0/Vendor/CESIUM_RTC)
- Epic: [UE5 Large World Coordinates](https://dev.epicgames.com/documentation/en-us/unreal-engine/large-world-coordinates-in-unreal-engine-5), [LWC Rendering](https://dev.epicgames.com/documentation/en-us/unreal-engine/large-world-coordinates-rendering-in-unreal-engine-5), [roadmap](https://portal.productboard.com/epicgames/1-unreal-engine-public-roadmap/c/1253-large-world-coordinates-on-gpu)
- [Godot: Large world coordinates](https://docs.godotengine.org/en/stable/tutorials/physics/large_world_coordinates.html); [Godot Voxel Tools: smooth terrain/LOD](https://voxel-tools.readthedocs.io/en/latest/smooth_terrain/); [Unity HDRP camera-relative rendering](https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/Camera-Relative-Rendering.html)
- Reed (NVIDIA), [Depth Precision Visualized](https://developer.nvidia.com/content/depth-precision-visualized); Outerra, [Logarithmic depth buffer](https://outerra.blogspot.com/2012/11/maximizing-depth-buffer-range-and.html)
- John Lin, [The Perfect Voxel Engine](https://voxely.net/blog/the-perfect-voxel-engine/)
- Indie devlogs: [Frozen Fractal: floating the origin](https://frozenfractal.com/blog/2024/4/11/around-the-world-14-floating-the-origin/), [Bowerbyte: Blocky Planet](https://www.bowerbyte.com/posts/blocky-planet/)
- Interviews and talks: [Dual Universe / Baillie Q&A](https://thekoalition.com/2016/dual-universe-gameplay-debut-qa-with-novaquarks-founder-jean-christophe-baillie), [NMS GDC 2017 (Vault)](https://www.gdcvault.com/play/1024265/Continuous_World_Generation_in__No_Man_s_Sky_), [Teardown / SE Daily](https://softwareengineeringdaily.com/podcasts/teardown-and-voxel-based-rendering-with-dennis-gustafsson/), [Avorion FAQ](https://www.avorion.net/faq/), [Mobius: Demaking Outer Wilds](https://www.mobiusdigitalgames.com/news/archives/07-2015), [KSP 1.2 changelog](https://wiki.kerbalspaceprogram.com/index.php?title=1.2)

Secondary (used only where marked):
- [voxel.wiki: Planets](https://voxel.wiki/wiki/round-planets/)
- [jianshu NMS GDC summary](https://www.jianshu.com/p/ea0fbd7abd6d)
- [Game Developer: Astroneer](https://www.gamedeveloper.com/design/what-i-astroneer-i-s-devs-learned-while-leaving-early-access)
- [Game Developer: Teardown](https://www.gamedeveloper.com/design/how-beautiful-voxels-laid-the-way-for-i-teardown-s-i-heist-y-framework)
- SoA [ModDB summary](https://www.moddb.com/games/seed-of-andromeda/news/seed-of-andromeda-dev-summary-21st-march-2015) (blocked; quoted via search excerpt)
- Dual Universe [press release](https://www.prweb.com/releases/novaquark_announces_the_first_boundless_mmo_dual_universe/prweb13415044.htm) (quoted via search excerpt)
