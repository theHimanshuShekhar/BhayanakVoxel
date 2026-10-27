# Research: voxel data structures for micro-voxels at Planet scale

Resolves [#3](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/3). Feeds [#8 Choose Voxel size & Planet radius](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8) and [#9 Choose voxel data structure & LOD scheme](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/9).

Researched 2026-10-27. Every factual claim links to its source. Lines marked **[estimate]** are my own arithmetic, with the working shown in section 3. Lines marked **[unverified]** could not be checked against a primary source. Lines marked **[judgement]** are engineering opinion, not measured fact.

## TL;DR

- **Memory follows surface area, not volume, if the structure is sparse.** A sparse structure stores roughly the Voxels near the surface, and solid rock and empty sky collapse into cheap uniform nodes. That is what makes a fully voxel Planet affordable ([Laine & Karras 2010, §3.1](https://research.nvidia.com/sites/default/files/pubs/2010-02_Efficient-Sparse-Voxel/laine2010tr1_paper.pdf)).
- **With LOD tied to screen pixels, the amount of resident data barely depends on Voxel size.** If far-away Voxels get coarser at the rate where each one stays about one pixel, the GPU holds roughly 50–90 million surface Voxels. That is about **0.2–0.7 GB**, whether Voxels are 2 cm or 25 cm **[estimate]**. Voxel size mostly drives the costs at full resolution: the region around each player where Edits and physics happen, server memory, generation time and disk.
- **Full-resolution memory per km² of surface [estimate]:** 2 cm ≈ 16–41 GB, 5 cm ≈ 2.6–6.5 GB, 10 cm ≈ 0.66–1.6 GB, 25 cm ≈ 0.1–0.26 GB (brick structure, low to high Material cost). So the whole Planet can never be resident at full resolution. Generate it procedurally on demand and store only the Edits.
- **Structures compared:** sparse voxel DAGs give the best compression, but Edits are complex and Materials inflate them about 6× ([Molenaar & Eisemann 2024](https://diglib.eg.org/server/api/core/bitstreams/f0c8141c-e493-4993-809d-b2a47c3dfe3f/content)). Dense grids (Teardown) are the simplest but waste memory. Brickmaps and 64-trees sit in between: Edits are trivial or easy, ray tracing is fast, and they are simple enough for a beginner and AI agents to get right.

**Plain-language recommendation.** Store the Planet as many small 8×8×8 "bricks". Each brick has a 64-byte "is it solid?" bitmask plus palette-compressed Material IDs, and the bricks hang off a sparse lookup (a hash map or a shallow 64-tree) per region. Build LOD the way texture mipmaps work: each LOD level is the same brick structure with Voxels twice as large. Then keep a camera-centred set of rings, finest near the player and coarser outward, out to orbit. Do not keep the whole Planet: regenerate unedited terrain from the seed and save or replicate only Edits. Keep a sparse voxel DAG in reserve as a later optimisation for far, mostly unedited LOD levels or for disk, not as the first structure. On memory alone, **10 cm is comfortable, 5 cm is feasible, and 2 cm is only realistic if the full-resolution region around each player stays small** (see section 5).

---

## 1. What the structure has to do for BhayanakVoxel

- Hold a whole spherical Planet that is solid throughout ("voxel all the way through"; see `CONTEXT.md`).
- Absorb frequent Edits (digging and placing) quickly, on the client and on a dedicated server.
- Be ray-traced fast enough for path tracing at 60 fps on an RTX 3060 / RX 6700-class GPU with 8–12 GB VRAM.
- Provide LOD from about one pixel per Voxel near the player all the way out to an orbital view of the Planet (Zoom).
- Stream and replicate cheaply to dozens of players. Later, it also has to feed Debris (rigid bodies) and Fluid simulation.
- Be simple enough for a graphics beginner working with AI agents to implement and debug **[judgement]**.

## 2. Survey of structures and prior art

### 2.1 Dense grids (Teardown)

- Teardown stores every shape as "an uncompressed, regular 3D grid". That is "very fast to access", but empty space costs as much memory as filled space ([Gustafsson, Year summary 2024](https://blog.voxagon.se/2024/12/29/year-summary.html)).
- Each Voxel is a single byte that indexes an 8-bit palette of up to 255 Materials. The palette entry holds colour, roughness, emissiveness and so on ([Gustafsson, The Spraycan](https://blog.voxagon.se/2020/12/03/spraycan.html)).
- Each object is a 3D volume texture at one byte per Voxel. Two extra mip levels let the tracer skip 2×2×2 or 4×4×4 empty Voxels at a time ([Wittens, Teardown Frame Teardown](https://acko.net/blog/teardown-frame-teardown/)).
- The whole play area also lives in a 1-bit shadow volume. For the Marina level it is 1752×100×1500 texels (3504×200×3000 Voxels) and takes 262 MB ([Wittens](https://acko.net/blog/teardown-frame-teardown/)). An earlier prototype packed 2×2×2 octant bits per byte and added 20 cm and 40 cm mips, which took 292 Mb instead of 2 GB ([Gustafsson 2018](https://blog.voxagon.se/2018/10/17/from-screen-space-to-voxel-space.html)).
- Teardown's Voxel edge is commonly quoted as 10 cm. **[unverified]**: the only source found is a fan wiki ([Teardown wiki](https://teardown.fandom.com/wiki/Voxels)). The 2018 prototype post gives 5 cm for the shadow texture.
- **Gustafsson's own next engine moves to sparse storage.** It "splits each shape into 8x8x8 voxel 'chunks', which are tracked with a 3D bitmap", which "saves a lot of memory" while "updates are fast" ([Year summary 2024](https://blog.voxagon.se/2024/12/29/year-summary.html)).

**Takeaway:** dense storage is perfect for Edits and simplicity, but it does not scale to a Planet. Even its author is moving to 8³ bricks.

### 2.2 Brickmaps / two-level grids

- Van Wingerden's thesis uses a uniform "brickgrid" of pointers to 8×8×8 "brickmaps". A brick is a 512-bit occupancy mask, "exactly 71 bytes, not counting shading attributes". Colours are stored separately and DXT1-compressed to about 4 bits per Voxel. Each brick also stores an LOD colour ([van Wingerden 2015](https://web.archive.org/web/20200710155729/https://dspace.library.uu.nl/bitstream/handle/1874/315917/final.pdf;sequence=2), §3.1).
- The data stays in raw form, which makes it "directly editable". The thesis supports streaming from disk and basic LOD, rendering "potentially infinite landscapes while using a finite amount of memory" (abstract, same source).
- It measured 40–164 MRays/s on a Radeon HD 7970 and beat its ESVO comparison (43.8 and 22.5 MRays/s) on the same scenes (same source, Tables 3–4).
- The *dense* top-level grid became the largest cost: 64 MB of static grid against at most ~50 MB of bricks and colour (Table 1). **Lesson: on a Planet the top level must be sparse too.**
- The "Hybrid Voxel Formats" study benchmarked many combinations. "A good default format for most models is a format of the form R(N³) G(M)", meaning a raw grid on top and a DAG below. Raw grids that are too fine "can be detrimental" on sparse models ([Arbore et al. 2024](https://arxiv.org/pdf/2410.14128), §6.1).

### 2.3 Sparse voxel octrees (ESVO)

- Efficient Sparse Voxel Octrees (ESVO) need about 5 bytes per Voxel with colour, normal and contour, or 6.67 bytes per finest-level sample once the mip hierarchy is counted ([Laine & Karras 2010](https://research.nvidia.com/sites/default/files/pubs/2010-02_Efficient-Sparse-Voxel/laine2010tr1_paper.pdf), Table 1 and §3.1).
- The same paper makes the key scaling point: "if we only encode surfaces, the memory usage drops to O(N²)". It also notes that full resolution is needed only near the camera (§3.1, §4).
- Octree data sits in blocks with relative pointers, which "facilitates dynamic octree updates, for example when streaming". The hierarchy itself is built offline from meshes (§5, §7.1).
- Node layout is 4 bytes per node, with a 16-bit relative pointer plus far pointers. The 64-tree author found octree traversal "worse than hierarchical-grid methods by as much as 60%" ([dubiousconst282 2024](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/)).
- John Lin's view is that SVOs are good only for "storage and rendering", and "not even great" at those ([Lin, The Perfect Voxel Engine](https://voxely.net/blog/the-perfect-voxel-engine/)).

### 2.4 Sparse 64-trees (4×4×4 branching, "contree")

- A 64-tree node is 12 bytes: a 64-bit child mask, a leaf flag and a 31-bit child pointer. Only children that exist are stored ([dubiousconst282 2024](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/)).
- Memory is about 0.19 bytes per Voxel when fully populated, "nearly 3x less than SVOs". The Bistro scene at about 8K resolution took 224 MB, or 0.62 bytes per Voxel, against 368 MB for ESVO (same source).
- Traversal is fast because each level skips 4³ cells and the tree is half as deep as an octree. The guide reports cycles per ray on an unnamed integrated GPU, not MRays/s. One independent developer found it slower than their own tracer on an RTX 3050 ([itch.io comment](https://teknologicus.itch.io/vorxel)), so treat speed claims as scene- and implementation-dependent.
- Edits with compressed nodes are "quite a bit more complicated" and need a pool allocator. The author suggests "many smaller trees in a top-level grid" for large worlds and streaming (same source). An open implementation with hashing and modifications exists ([expenses/tree64](https://github.com/expenses/tree64)).
- It deduplicates less well than a DAG, because "our node layout cannot represent complete leaf paths" (same source).

### 2.5 Sparse voxel DAGs (SVDAG, SSVDAG, HashDAG)

- A DAG merges identical subtrees of an octree. It cut node counts by "one to three orders of magnitude". The Epic Citadel scene at 128K³ (19 billion Voxels) fits in 945 MB, where an SVO would need 5.1 GB. Ray tracing reached 170–240 MRays/s on a GTX 680 ([Kämpe et al. 2013](https://www.cse.chalmers.se/~uffe/HighResolutionSparseVoxelDAGs.pdf)).
- Geometry alone costs 0.08–2.24 bits per Voxel across their test scenes (Table 2). The paper stores **geometry only**: "Our focus has not been on dynamic scenes" (§3–5).
- SSVDAG also merges mirror-symmetric subtrees. At 256K³ it stores about 100 billion Voxels in under 575 MB (0.048 bits per Voxel), roughly 1.8× smaller than SVDAG ([Villanueva et al., JCGT 2017](https://jcgt.org/published/0006/02/01/)).
- Attributes such as colour or Material are decoupled from geometry and palette-compressed ([Dado et al. 2016](https://doi.org/10.1111/cgf.12841)).
- **Editing:** HashDAG stores the DAG in a hash table so nodes can be modified "without requiring de- and recompression" ([Careil et al. 2020](https://research.tudelft.nl/en/publications/interactively-modifying-compressed-sparse-voxel-representations/); [code](https://github.com/Phyronnaz/HashDAG), which needs CUDA and "at least 8GB of VRAM").
- GPU editing on an RTX 4080 places spheres of 1140–1820 Voxel radius in 15–18 ms, and copy edits take "a couple of milliseconds" ([Molenaar & Eisemann 2024](https://diglib.eg.org/server/api/core/bitstreams/f0c8141c-e493-4993-809d-b2a47c3dfe3f/content), §6).
- **Materials hurt DAGs a lot:** the Citadel at 128K³ grows from 980 MiB without Materials to 5997 MiB with 4-bit Material IDs. That is about 2.6 bits per Voxel against 0.43 **[estimate from their Table 1 and Kämpe's Voxel count]**. The reason is that "subtrees only match if shape and material are identical" (same source).
- **Streaming/LOD variant:** Aokana splits the world into many shallow DAGs of 256³ Voxels each, with octree-style LOD levels (LOD n is 8 children merged at the same 256³ resolution) and distance-based streaming. On an RTX 3060 Ti it renders a 64K scene of about 10 billion Voxels in about 6 ms. Only about 2–17% of the data is resident: 424 MB VRAM against 23 GB on disk for San Miguel at 64K ([Fang et al. 2025](https://arxiv.org/pdf/2505.02017), §3.1–3.4, Table 1).
- In Aokana's numbers, **colour dominates storage**: 21 GB of colour against 2.2 GB of geometry. The authors note that game attributes "cannot utilize complex lossy compression" (§4.2).

### 2.6 VDB / NanoVDB (film-industry sparse grids)

- VDB is a shallow B+tree: an unbounded root, internal nodes of 32³ and 16³, and 8³ leaves. A node entry is either a child or a constant "tile" value ([OpenVDB docs](https://www.openvdb.org/documentation/doxygen/overview.html)).
- It is the same idea as an extended brickmap and a useful design reference. NanoVDB ports it to the GPU for ray tracing ([NVIDIA](https://developer.nvidia.com/nanovdb)). It is aimed at simulation and film volumes, not game Edits **[judgement]**.

### 2.7 Run-length and palette-compressed bricks

- Voxlap's VXL format stores each 1024×1024 column as spans and keeps colour only for "surface voxels", meaning solid Voxels with an air neighbour. Interior solid is implicit ([Silverman, vxlform.txt](https://github.com/Ericson2314/Voxlap/blob/no-asm/share/documentation/vxlform.txt)). This is great for heightfield-like terrain, but RLE does not suit GPU random access and arbitrary 3D Edits **[judgement]**.
- Minecraft stores each 16×16×16 section as a palette plus packed indices of at least 4 bits (up to 12). A section with one block state stores no index array at all ([Minecraft Wiki, Chunk format](https://minecraft.wiki/w/Chunk_format)). This is the standard, easy way to compress Materials.

### 2.8 John Lin's engine (the visual reference)

- The reference video "New Voxel Engine Reveal – Crystal Islands Experiment" (John Lin, 2021-05-13) claims "an 8x (512 cubic) detail increase … high compression rates, and per-voxel material attributes", 5-bounce path-traced GI, an "effective world size" of **256K³**, and a 10× rendering speed-up over his previous engine ([video description](https://youtu.be/8ptH79R53c0)).
- His blog recommends using "whatever voxel format is best for the job". Volumes are converted between formats, and rendering uses Vulkan hardware ray tracing with intersection shaders written for each format ([The Perfect Voxel Engine](https://voxely.net/blog/the-perfect-voxel-engine/)).
- **[unverified]** Lin has not published his actual storage structure or his Voxel size in metres. Nothing in a primary source pins them down.

### 2.9 Douglas Dwyer's engine (Octo)

- Octo is written in Rust and runs natively and on WebGPU. It renders with "voxel ray marching with compute shaders" and supports editing, LODs, multiplayer and rigid bodies ([octo-release README](https://github.com/DouglasDwyer/octo-release)).
- His devlogs mention converting "voxel octrees to GPU data" with a custom GPU allocator ([Devlog #7](https://www.youtube.com/watch?v=IFUj53VwYvU)). They also mention DDA traversal, "bitwise masking", and a low-resolution depth prepass ([Devlog #18](https://www.youtube.com/watch?v=P2bGF6GPmfc)).
- **[unverified]** The exact current data structure is described only inside the videos. The transcripts could not be retrieved, so treat "octree-based plus bitmask DDA" as unconfirmed.

## 3. Memory arithmetic

### 3.1 Assumptions

- A km² of Planet surface, Voxel edge `s`, and 8³ bricks (brick edge `8s`).
- **Roughness factor k = 2.** Real terrain with slopes, overhangs, caves and vegetation has about 2× the surface of its flat projection, and a tilted surface crosses up to √3× more bricks. This is a guess; real k could be 1.5–4. Memory scales linearly with k.
- A sparse structure stores homogeneous interiors as one node, so it costs about the surface layer (ESVO §3.1, above).
- Per-brick costs:
  - **A: raw bytes.** 512 B (1 byte Material per Voxel, Teardown-style) + ~8 B header ≈ **520 B**.
  - **C: compact.** 64 B occupancy mask + ~2-bit palette indices on average (128 B) + a small palette and header (~18 B) ≈ **210 B**. Most bricks hold 1–4 Materials, with soil over rock. A 4-bit palette would be about 340 B.
- The brick count crossing a surface is about the surface Voxel count / 64, so cost per surface Voxel is ≈ 520/64 ≈ **8 B** (A) or 210/64 ≈ **3.3 B** (C).

### 3.2 Full-resolution memory per km² of surface **[estimate]**

| Voxel `s` | Voxels/km² (flat) | Surface Voxels/km² (k=2) | Brick edge | Bricks/km² (k=2) | **C: compact** | **A: raw bytes** | Dense grid, 10 m relief, 1 B/Voxel |
|---|---|---|---|---|---|---|---|
| 2 cm | 1e6/0.02² = 2.5e9 | 5.0e9 | 16 cm | 2·1e6/0.16² = 7.8e7 | 7.8e7·210 B = **16.4 GB** | 40.6 GB | 1e7 m³/8e-6 = 1250 GB |
| 5 cm | 4.0e8 | 8.0e8 | 40 cm | 1.25e7 | **2.6 GB** | 6.5 GB | 80 GB |
| 10 cm | 1.0e8 | 2.0e8 | 80 cm | 3.1e6 | **0.66 GB** | 1.6 GB | 10 GB |
| 25 cm | 1.6e7 | 3.2e7 | 2 m | 5.0e5 | **0.10 GB** | 0.26 GB | 0.64 GB |

For comparison, the same 10 cm km² (2e8 surface Voxels) in other structures, using the per-Voxel figures cited above **[estimate]**:

- 64-tree at 0.62 B/Voxel geometry: ~0.12 GB, plus Materials.
- SVDAG at 0.08–2.24 bits geometry: ~2–56 MB. With 4-bit Materials at about 2.6 bits/Voxel: ~65 MB, though highly scene-dependent.
- ESVO at 6.67 B/sample: ~1.3 GB.

The gap shows why DAGs win for storage and archives. It also shows that brick formats are already affordable within a bounded region.

### 3.3 Whole-Planet size, if stored at full resolution **[estimate]**

Surface area is 4πR², using the compact brick cost:

| Planet radius R | Surface | 10 cm | 5 cm | 2 cm |
|---|---|---|---|---|
| 1 km | 12.6 km² | 8 GB | 33 GB | 206 GB |
| 5 km | 314 km² | 206 GB | 823 GB | 5.2 TB |
| 10 km | 1257 km² | 824 GB | 3.3 TB | 20.6 TB |

**Conclusion:** full resolution everywhere is out of the question except for a tiny Planet at 10 cm or more. The unedited Planet must be **regenerated procedurally from a seed**, with only **Edits stored and replicated**. Generation cost scales the same way. One km² at 2 cm means 7.8e7 bricks × 512 = 4e10 Voxel evaluations, against 1.6e9 at 10 cm, so on-demand generation itself is 25× more work at 2 cm.

### 3.4 The region around the player, with LOD **[estimate]**

- **Pixel-sized LOD.** At 1440p with a 70° vertical FOV, one pixel spans 70°/1440 = 8.48e-4 rad (1.13e-3 rad at 1080p). A Voxel of size `s` covers one pixel at distance `d = s / 8.48e-4 ≈ 1180·s`, so full resolution is only *visible* out to: 2 cm → 24 m, 5 cm → 59 m, 10 cm → 118 m, 25 cm → 295 m (at 1080p: 18, 44, 88, 221 m).
- **Full-resolution disk.** Out to that radius, `R0 = 1180·s`, the surface holds `π·R0²/s² = π·1180² ≈ 4.4e6` flat Voxels. That is **independent of `s`**, or ~8.7e6 with k=2.
- **Clipmap rings.** Ring *i* spans `R0·2^(i-1)…R0·2^i` at Voxel size `s·2^i`. Its area is `3·4^(i-1)·πR0²` and its Voxel area is `4^i·s²`, so each ring holds `0.75 × N0`. The total over L rings is `N0·(1 + 0.75·L)`.
- **Levels needed** to reach 1–5 km from R0: 2 cm → 5–8, 5 cm → 4–6, 10 cm → 3–5, 25 cm → 2–4. Reaching across a whole Planet and out to orbit needs a few more levels; the count grows only logarithmically.

| LOD rings L | Surface Voxels resident | Compact (3.3 B/Voxel) | Raw (8 B/Voxel) |
|---|---|---|---|
| 6 | 4.8e7 | 160 MB | 390 MB |
| 8 | 6.1e7 | 200 MB | 500 MB |
| 12 | 8.7e7 | 290 MB | 710 MB |

- **Quality headroom.** At 2 Voxels per pixel for sharper path-traced detail, multiply by 4 (≈ 0.6–2.8 GB).
- **Fit on target GPUs.** Either way the voxel data fits in 8–12 GB of VRAM. That leaves room for path-tracing buffers, denoiser history and lighting caches, which [#5](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/5) must budget.
- **Prior art agrees.** Aokana keeps only ~2–17% of a scene resident with distance LOD ([Fang et al. 2025](https://arxiv.org/pdf/2505.02017), Table 1).
- **What still scales with `s`** is the region that must be **full resolution regardless of pixels**: where Edits land, Debris breaks off, Fluids flow and players collide. For a 64 m radius (0.0129 km²) per player, compact cost:

| Voxel `s` | Per player region | 30 players spread out (server) |
|---|---|---|
| 2 cm | 211 MB | 6.3 GB |
| 5 cm | 34 MB | 1.0 GB |
| 10 cm | 8 MB | 0.25 GB |
| 25 cm | 1.4 MB | 0.04 GB |

### 3.5 Coordinates and depth

- **Integer range.** A 32-bit integer Voxel coordinate at 2 cm spans 2³² × 0.02 m ≈ 85,900 km, which is ample.
- **Float precision.** A float32 position 5 km from its origin has a 0.49 mm step, so camera-relative coordinates are fine at all candidate sizes. Planet-scale precision belongs to [#4](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/4).
- **Hierarchy depth.** A 5 km-radius Planet is 2^18.9 Voxels across at 2 cm and 2^16.6 at 10 cm. That means 17–19 octree levels or 9–10 64-tree levels. For scale, John Lin's island world is 256K³ = 2^18 ([video description](https://youtu.be/8ptH79R53c0)).

## 4. Comparison

| Structure | Memory per km² at 10 cm **[estimate]** | Cost of an Edit | Ray traversal | Natural LOD / mips | Stream / replicate | Beginner + AI agents |
|---|---|---|---|---|---|---|
| Dense grid per object (Teardown) | Very high (~10 GB with 10 m relief) | Trivial: write bytes, re-upload a sub-box | Fast DDA with 2 mips ([acko](https://acko.net/blog/teardown-frame-teardown/)) | Mip textures | Simple, but large | Easiest, but does not scale |
| Brickmap: 8³ bricks + sparse top level | 0.66 GB compact, 1.6 GB raw | Trivial: rewrite affected bricks, mark dirty, upload ([van Wingerden](https://web.archive.org/web/20200710155729/https://dspace.library.uu.nl/bitstream/handle/1874/315917/final.pdf;sequence=2)) | Good: two-level DDA plus bitmask; 40–164 MRays/s on a 2012 GPU | Easy: mip bricks to 2× Voxel size, LOD colour per brick | Excellent: a brick is a natural network and disk unit | **Easy** |
| Sparse 64-tree | ~0.12 GB geometry plus Materials | Moderate: split/relink nodes, pool allocator ([dubiousconst282](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/)) | Very good: shallow, 64-bit mask skips | Inner nodes serve as LOD | Good if split into many small trees | Moderate |
| SVO / ESVO | ~1.3 GB with attributes | Hard: offline build ([Laine & Karras](https://research.nvidia.com/sites/default/files/pubs/2010-02_Efficient-Sparse-Voxel/laine2010tr1_paper.pdf)) | OK, deep and pointer-chasing; slower than grids ([dubiousconst282](https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/)) | Built in: every level is a mip | Block-based streaming possible | Moderate–hard |
| SVDAG / HashDAG | Smallest: ~2–65 MB, but ~6× more with Materials ([ME24](https://diglib.eg.org/server/api/core/bitstreams/f0c8141c-e493-4993-809d-b2a47c3dfe3f/content)) | Hard: hash-consing, garbage collection; CPU ([CBE20](https://github.com/Phyronnaz/HashDAG)) or CUDA GPU at ms scale ([ME24](https://diglib.eg.org/server/api/core/bitstreams/f0c8141c-e493-4993-809d-b2a47c3dfe3f/content)) | Good: 170–240 MRays/s on GTX 680 ([Kämpe](https://www.cse.chalmers.se/~uffe/HighResolutionSparseVoxelDAGs.pdf)); shallow chunks help ([Aokana](https://arxiv.org/pdf/2505.02017)) | Built in; Aokana-style chunk LOD | Shared nodes have machine-local IDs, so the subgraph must be re-serialised **[judgement]** | **Hard** |
| VDB-style B+tree | Similar to brickmap | Easy on CPU; NanoVDB GPU copy is static **[judgement]** | Good (HDDA) | Tiles act as coarse values | Good | Moderate; big dependency |
| RLE columns (Voxlap) / palette sections (Minecraft) | Low for heightfields | RLE: cheap locally, but columns get rewritten; palette: easy | RLE does not suit GPU random access | Not built in | Palette sections stream well | Easy, but RLE fits the GPU poorly |

## 5. Recommendation and implications

**Structure [judgement].**
1. Use 8³ bricks: a 64-byte occupancy mask plus palette-compressed Material IDs (1/2/4/8-bit indices, as in Minecraft). An all-solid or all-empty brick costs zero bytes because a flag in the parent marks it uniform.
2. Hang bricks off a sparse top level per Planet region: a hash map keyed by brick coordinate, or a shallow 64-tree. This matches Gustafsson's next engine ([2024](https://blog.voxagon.se/2024/12/29/year-summary.html)), van Wingerden's brickmaps without the dense top grid, the "R(N³) G(M)" finding ([Arbore et al.](https://arxiv.org/pdf/2410.14128)), and Aokana's many-shallow-structures result.
3. For LOD, build mip levels of bricks (each level has Voxels 2× larger) and keep camera-centred rings out to orbit (section 3.4). The per-ring brick count stays constant, so total memory is bounded.
4. Generate unedited bricks procedurally from a seed on demand. Persist and replicate Edits as operations (shape, Material, tick), plus compressed snapshots of edited bricks for late joiners. Lin's "format per job" idea also fits: Debris and Fluids can keep their own small dense grids and convert to and from bricks ([Lin](https://voxely.net/blog/the-perfect-voxel-engine/)).
5. Reconsider a DAG only for far LOD rings or on-disk archives, and only if measured memory is too high. Expect Materials to erode the gain.

**Voxel size, for [#8](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8):**
- **Memory on screen does not decide this.** With pixel-sized LOD the GPU holds about 0.2–0.7 GB of voxel data at any size.
- **What does decide it** is everything that runs at full resolution:
  - Edits/physics region: 2 cm ≈ 211 MB per player against 8 MB at 10 cm.
  - Procedural generation: 25× more work at 2 cm than at 10 cm.
  - Server memory: 6.3 GB against 0.25 GB for 30 spread-out players.
  - Fluid and Debris cell counts.
  - How many Voxels an Edit touches: a 1 m crater is ~65,000 Voxels at 2 cm but ~520 at 10 cm, about 125× more work and bandwidth.
- Suggested starting point: **10 cm, with 5 cm as the stretch target**. Keep `s` a single compile-time constant so a prototype can measure 5 cm and 2 cm directly. This is a recommendation, not a decision.

## 6. Open questions and doubts

- **Roughness factor k** (assumed 2) is a guess. Measure it on the real terrain generator, because every memory number scales with it.
- **Material bits per Voxel.** 8 bits (255 Materials, Teardown) or 16 bits? Are per-Voxel colour or normal variations needed on top, as Lin argues for small Voxels ([Lin](https://voxely.net/blog/the-perfect-voxel-engine/))? Per-Voxel colour multiplied storage in Aokana's tests ([Fang et al.](https://arxiv.org/pdf/2505.02017), Table 1).
- **The sphere mapping**, a cube-sphere or a global Cartesian grid, changes how bricks align with "down". It also changes LOD seams and whether bricks stay axis-aligned for ray tracing. That belongs to [#4](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/4).
- **Hardware RT (BLAS of brick AABBs plus intersection shaders, as Lin does) against compute-shader DDA** affects which layout is fastest. Decide with [#5](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/5). No 64-tree or brickmap benchmark on an RTX 3060-class GPU was found.
- **LOD downsampling rules** need defining: what a coarse Voxel's Material is (majority, or "solid if ≥ n of 8 children", as Aokana uses with density=2), and how Edits propagate up the mip chain.
- **Interior content.** If the generator puts caves, ores or strata deep inside, the interior stops being uniform and memory rises above the surface-only estimate.
- **Unverified:** Teardown's shipped Voxel size, John Lin's actual structure and Voxel size, and Douglas Dwyer's current structure.

## Sources

- Laine & Karras, *Efficient Sparse Voxel Octrees* (NVIDIA tech report, 2010): https://research.nvidia.com/sites/default/files/pubs/2010-02_Efficient-Sparse-Voxel/laine2010tr1_paper.pdf
- Kämpe, Sintorn & Assarsson, *High Resolution Sparse Voxel DAGs* (SIGGRAPH 2013): https://www.cse.chalmers.se/~uffe/HighResolutionSparseVoxelDAGs.pdf
- Villanueva, Marton & Gobbetti, *SSVDAGs* (JCGT 2017): https://jcgt.org/published/0006/02/01/
- Dado et al., *Geometry and Attribute Compression for Voxel Scenes* (EG 2016): https://doi.org/10.1111/cgf.12841
- Careil, Billeter & Eisemann, *Interactively Modifying Compressed Sparse Voxel Representations* (EG 2020): https://research.tudelft.nl/en/publications/interactively-modifying-compressed-sparse-voxel-representations/ and code: https://github.com/Phyronnaz/HashDAG
- Molenaar & Eisemann, *Editing Compact Voxel Representations on the GPU* (PG 2024): https://diglib.eg.org/server/api/core/bitstreams/f0c8141c-e493-4993-809d-b2a47c3dfe3f/content and code: https://github.com/mathijs727/GPU-SVDAG-Editing
- Fang, Wang & Wang, *Aokana: A GPU-Driven Voxel Rendering Framework for Open World Games* (2025): https://arxiv.org/pdf/2505.02017
- Arbore et al., *Hybrid Voxel Formats for Efficient Ray Tracing* (ISVC 2024): https://arxiv.org/pdf/2410.14128
- van Wingerden, *Real-time Ray Tracing and Editing of Large Voxel Scenes* (Utrecht MSc thesis, 2015): https://web.archive.org/web/20200710155729/https://dspace.library.uu.nl/bitstream/handle/1874/315917/final.pdf;sequence=2
- dubiousconst282, *A guide to fast voxel ray tracing using sparse 64-trees* (2024): https://dubiousconst282.github.io/2024/10/03/voxel-ray-tracing/ and https://github.com/dubiousconst282/VoxelRT
- OpenVDB overview: https://www.openvdb.org/documentation/doxygen/overview.html; NanoVDB: https://developer.nvidia.com/nanovdb
- Dennis Gustafsson (Teardown): https://blog.voxagon.se/2024/12/29/year-summary.html, https://blog.voxagon.se/2020/12/03/spraycan.html, https://blog.voxagon.se/2018/10/17/from-screen-space-to-voxel-space.html
- Steven Wittens, *Teardown Frame Teardown*: https://acko.net/blog/teardown-frame-teardown/
- John Lin: https://youtu.be/8ptH79R53c0 (video description) and https://voxely.net/blog/the-perfect-voxel-engine/
- Douglas Dwyer: https://github.com/DouglasDwyer/octo-release, https://www.youtube.com/watch?v=IFUj53VwYvU, https://www.youtube.com/watch?v=P2bGF6GPmfc
- Ken Silverman, Voxlap VXL format: https://github.com/Ericson2314/Voxlap/blob/no-asm/share/documentation/vxlform.txt
- Minecraft Wiki, Chunk format: https://minecraft.wiki/w/Chunk_format
