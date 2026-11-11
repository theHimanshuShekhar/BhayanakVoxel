# Vertical-slice spec: BhayanakVoxel

This spec is where the [wayfinder map](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/1) ends. It lives in this file and changes only by PR; [#21](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/21) points here. It was last reshaped by the plan review of 2026-11-10 (ADRs 0008–0010).

- **Vocabulary:** [`CONTEXT.md`](../CONTEXT.md)
- **Architecture decisions:** [`docs/adr/`](adr/)
- **Detail behind each decision:** the linked tickets.

Break this spec into build tickets with `/to-tickets`, one Milestone at a time.

## What we're building

BhayanakVoxel is a hobby / tech-demo voxel game. It has one fully voxel, editable, spherical **Planet**, **320 km in radius** (ADR 0008, was 16 km), made of **5 cm Voxels** (ADR 0007, was 10 cm). Lighting is path-traced, inspired by John Lin's *Crystal Islands*. The **Zoom** runs continuously from first person out to the whole Planet as a small disc. The game targets native desktop (Windows and Linux, Vulkan). It ends up as dedicated-server multiplayer for dozens of players, with **Debris** and **Fluids**.

**What the game is** (decided 2026-11-11). The Vertical slice is a shared creative sandbox: shape the Planet with Brushes and build on it with **Pieces**, with friends, with free Materials and no rules. The headline is the Planet itself: its scale, its Zoom and its look. After the slice it grows, in this order, into moving Pieces, survival with Tools, and a solar system of Planets (see *After the Vertical slice*).

- **Who builds it:** AI agents write most of the code. The dev, a graphics beginner, steers, reviews, playtests and merges.
- **Reference hardware:** M0–M4 target an **RTX 4090** at 1440p60. M5 brings the game down to RTX 3060 / RX 6700-class cards.

## Architecture at a glance

| Area | Decision | Detail |
|---|---|---|
| Engine | Custom **Rust + wgpu** engine, wgpu version pinned, **Vulkan only**, WGSL shaders. A Cargo workspace with the crates `planet-gen`, `sim`, `net` and `render`, plus `client` and `server` binaries (the server never links wgpu). Plain structs, no ECS. | [ADR 0001](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/main/docs/adr/0001-custom-engine-rust-wgpu.md), [Choose engine foundation & language](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/7) |
| Planet | 5 cm Voxels (ADR 0007, #147; was 10 cm), a **320 km radius** (ADR 0008; was ~16 km), diggable to the core, fixed 9.8 m/s² gravity toward the centre. The Zoom reaches ~20 R (6,400 km). One seeded world spawn where every character starts; no travel aid beyond walking and flying. Integer Voxel coordinates, f64 simulation positions, camera-relative f32 on the GPU. | [Choose Voxel size & Planet radius](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8), [Research: spherical voxel Planets](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/4) |
| Storage and LOD | Sparse **8³ bricks**: an occupancy mask plus palette-compressed 16-bit Materials. Unedited bricks are a regenerable cache; edited bricks persist. **All-voxel LOD rings to orbit** using majority downsampling, with Edits propagated to every level. The LOD band distance shrinks with camera speed, so a fast camera draws coarser levels (ADR 0009). | [ADR 0002](adr/0002-brick-store-all-voxel-lod.md), [ADR 0009](adr/0009-speed-scaled-lod.md), [Choose voxel data structure & LOD scheme](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/9) |
| Rendering | **Compute DDA** behind one `trace()` function; every lighting pass is built on it first. Hardware RT (wgpu ray queries over brick AABBs) is a time-boxed spike after GI, adopted only if it is faster on real secondary rays (ADR 0001). 1-bounce GI plus a world-space radiance cache. Emissives, glossy reflections, glass, volumetrics, and a Hillaire atmosphere. Tracing at half resolution per axis, followed by our own WGSL denoiser (growing toward A-SVGF) and TAAU. A reference path-tracer mode. **≈ 8.6 ms of GPU work on the 4090.** | [Choose lighting & rendering pipeline](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/10), [Research: voxel path tracing](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/5) |
| Terrain | Earth-like with fantasy accents. ~50% ocean, ±2 km relief, 4 biomes, dense vegetation, rivers and lakes, caves to ~300 m, a molten core. A **precomputed per-seed cube-sphere world map** that is coarse (~240 m a texel: continents, climate, biomes, large drainage), with everything finer made by deterministic functions at generation, then per-brick generation at any LOD. Relief, caves and strata are in metres and continents have a fixed size in km, so a bigger Planet has more of them (ADR 0008). Deterministic on the CPU. | [Choose terrain generation for M1](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/14), [Research: bit-identical Planet generation](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/12) |
| Materials and Edits | A creative palette of ~55 data-defined Materials with PBR and physical properties. Sphere, cube and cylinder Brushes up to ~4 m, plus smooth/flatten. Local-tangent snapping. Hardness-timed digging. **Pieces**: 1 m ready-made Voxel models placed in one Edit, which become ordinary Voxels and line up with their neighbours (ADR 0011). | [ADR 0011](adr/0011-pieces-are-stamped-voxels.md), [Choose M1 Materials & Edit tools](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/15) |
| Player | Walk, run, crouch, jump, swim, stamina-limited climbing, and fly up to 50 m/s. The capsule sweeps against Voxels, with the same code on client and server. Over-shoulder third person. Zoom by scroll or a cinematic key. A voxel humanoid avatar. | [Choose character controller, camera & Zoom control](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/16) |
| Network | **Clients generate terrain; the server replicates only ~24 B Edit operations Planet-wide.** The server is authoritative at 60 Hz, using renet. Join and brick-hash checks. Single-player runs the real server in-process over a loopback. | [ADR 0003](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/main/docs/adr/0003-client-generated-terrain-edit-ops.md), [Choose network architecture](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/11) |
| Persistence | One SQLite `world.db` per world. Every accepted Edit is appended to a journal as it is applied; a checkpoint of the diverged bricks every ~1 min lets the journal be truncated; 3 backups kept (ADR 0010). A generator-version guard. A ~2 GB client cache. | [Choose save format & world persistence](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/17) |
| Debris | Connectivity flood fill. rapier3d with parry3d `Voxels` colliders, simulated on the server. Fracture by hardness. Debris merges back after ~3 s at rest. ≤ ~1,000 live pieces, each ≤ 500 m³. | [Choose Debris detection & physics](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/18) |
| Fluids | A server-side cellular automaton (fill levels, granular sand) with 26-direction radial down. Rivers and the ocean simulate near players. 20 Hz, ≤ 2M active Voxels, ≤ 4 ms. | [Choose Fluid simulation model](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19) |
| Dev loop | A PR per ticket; the orchestrating agent merges on green CI and review, and the dev playtests. The public repo runs fmt/clippy/tests, a Linux+Windows determinism hash matrix, lavapipe golden images, and a Windows `.exe` artifact. A `needs-benchmark` label triggers `client bench` on the 4090 (run by the agent over SSH), and a stage over budget blocks the merge. The streaming bench also gates frame time, frame-thread CPU time, holes and soft misses on the full-size Planet (see M1a). | [Choose build, test & playtest loop](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/13) |

## Milestones

Every Milestone is **done** only when all of the following hold:

1. All automated gates are green: CI, the determinism matrix, lavapipe goldens, and a benchmark within budget.
2. The dev has run the Milestone's playtest checklist on the 4090 and ticked it off in that Milestone's tracking issue.

### M0: Skeleton

**Scope**
- The Cargo workspace with all crates and both binaries.
- All CI gates wired up: fmt, clippy, test, the determinism matrix, lavapipe goldens, and the Windows artifact.
- A winit window with a wgpu Vulkan device.
- The in-process server with the message loopback and a fixed 60 Hz tick.
- The brick store and an `apply(EditOp)` stub.
- A `planet-gen` stub: a sphere with noise, `GENERATOR_VERSION`, and golden hashes.
- The compute DDA `trace()` and the **reference progressive path tracer** on a test scene.
- Per-pass GPU timers and the debug overlay (egui).
- The `client bench` harness.

**Acceptance**
- A test sphere Planet is rendered by the reference path tracer and converges while the camera is still.
- The brick-hash test matches across Linux and Windows × debug and release.
- A lavapipe golden image of the test scene passes.
- `client bench` emits JSON with per-pass timings.
- A Windows artifact launches on the dev's PC.

**Playtest checklist:** launch the artifact, fly a free camera around the test sphere, toggle the reference mode, and read the timers.

### M1a: Walk the Planet

**Scope**
- **Terrain**
  - The world-map pass: elevation, erosion, biomes and sea level.
  - Core terrain: 4 biomes, caves and overhangs, strata and the core, oceans as static water.
  - **Coarse LOD generation to orbit.**
  - The 320 km Planet: coarse world map, detail at generation, fixed-size continents (ADR 0008).
- **Rendering**
  - The brick LOD rings, the 360° shell and the Guard (ADR 0004), and speed-scaled LOD (ADR 0009).
  - A hardware RT `trace()` spike after GI; compute is the path every pass is built on.
  - Primary visibility, sun shadows, radiance-cache GI, and emissive Voxels.
  - Denoise (temporal → A-SVGF), TAAU, and the Hillaire atmosphere.
- **Player**
  - The character controller: walk, run, crouch, jump, swim, climb with stamina, fly.
  - The camera modes and the full **Zoom** to ~20 R (scroll plus the cinematic key, with an orbit look-around).
  - The voxel avatar.
- **Editing:** the Material set loaded from a data file, the palette and hotbar UI, Brushes (dig, place, smooth/flatten), hardness-timed digging, and local-tangent snapping.
- **Saves:** SQLite, the Edit journal, checkpoints, backups, named worlds, the start menu, and the generator-version guard.

**Acceptance**
- **60 fps at 1440p output on the 4090**, with core stages within their budgets:

| Stage | Budget |
|---|---|
| Primary visibility | 1.0 ms |
| Sun shadows | 0.4 ms |
| GI | 1.5 ms |
| Denoise | 1.0 ms |
| Atmosphere | 0.3 ms |
| Upscale | 0.5 ms |
| Edit and BVH updates | 1.0 ms |

- Generation and streaming (restated 2026-11-10, measured by the streaming bench on the full-size Planet on the 4090):
  - The player has control in ≤ ~2 s, on a 20 m full-detail ring; loading continues nearest first behind it.
  - **No holes, ever**, and **zero soft misses at the speed-scaled level** on the 50 m/s flight paths, the vertical take-off and the 180° turn.
  - Full detail returns within a settle time after the camera slows or stops. The time is reported first and becomes a gate once measured.
  - The Guard (the whole Planet, coarse) is ready ≤ ~5 s after start, and the world map takes ≤ ~1 min.
  - Frame time p99 ≤ 16.7 ms and frame-thread CPU p99 ≤ 8 ms; memory within budget.
- Edits are visible from orbit (majority propagation).
- Edits persist across a restart.
- Determinism and golden gates stay green.

**M1a-2 order (2026-11-10).** The tracking issue is [#41](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/41).
1. The streaming bench (#162 with #64's measurement part).
2. The 320 km Planet: the generator, then the renderer's levels, Guard and sizes, measured again.
3. Speed-scaled LOD and the 20 m spawn.
4. #62 saves with the journal (#143 margins and prefetch are done), #144 Edit epochs, #145 and #163 uploads, #146 horizon cap.
5. The server bot soak.
6. The reference view's decisions (#100–#103) and ADR 0005, before any lighting ticket.
7. #52 sun shadows, #53 atmosphere, #54 GI, #55 denoiser, then the #50 hardware RT spike.
8. #57 swim, climb and fly, #58 camera and Zoom, #59 avatar, #150 and #151 bindings and HUD, Pieces (the system and about 12 Pieces, ADR 0011), #149 heatmap, #63 start menu, #64 final budgets and soak.

**Playtest checklist**
1. Create a world and spawn.
2. Walk, climb and swim.
3. Dig a tunnel, and build a wall of Pieces that stands upright on the curve.
4. Fly at 50 m/s with no holes in the terrain.
5. Zoom from first person to the Planet disc (~20 R) and back without hitches.
6. See the crater from orbit.
7. Watch a sunset from the ground and from orbit.
8. Quit and reload the world.

### M1b: Make it gorgeous

M1b is built in three ordered parts (2026-11-10; this replaces the single M1b of #138). Each has its own tracking issue, acceptance and playtest. The decisions behind the items are in [#135](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/135)–[#138](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/138), [#152](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/152) and [#153](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/153); [#139](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/139) records their detail here.

**M1b-1: Look**
- The art direction: tone curve, auto-exposure, palette (#135, tuned in the look-dev prototype #148).
- **Dense vegetation**: tree species, undergrowth, vines and logs, with placement rules (#136).
- **Fantasy accents**: glowing crystal caves, floating islands with static waterfalls, giant arches and spires, bioluminescent flora.
- **Rivers and lakes** as static water. With the coarse world map (ADR 0008) rivers need a new design: the map gives large drainage only.
- The **day/night cycle**: a world clock that can be paused and set, moon light and a starfield (#138).
- **Undo** of a player's own last ~32 Edits (#138, ADR 0010).
- The **Piece catalogue** grows to 50 or more as data: walls, slabs, stairs, posts, fences, roofs, window and door frames, lamps (ADR 0011).

During M1b a throwaway **Debris and Fluids prototype** at 5 cm (CPU only, on the real brick store) measures active Voxels per millisecond, so that M3 and M4 are known to be feasible as specified, or re-specified, before M2 shapes the server.

**M1b-2: Light transport**
- Glossy reflections, glass (one layer, #136) and transparency, transparent water, and volumetric fog with god rays.
- Deep water stops being proven solid for the renderer when water becomes transparent (ADR 0007).
- Vegetation **wind sway**, in the tracer only (#152).

**M1b-3: Audio**
- Positional audio: footsteps by Material, dig and place, biome ambience, water and wind.
- Ray-traced occlusion and reverb behind a toggle, if it fits its CPU budget (#153).

**Acceptance**
- 60 fps at 1440p on the 4090 within the **full ≈ 8.6 ms GPU budget**. The added stages are reflections 0.7 ms, glass 0.7 ms and volumetrics 1.0 ms.
- Generation and streaming targets still hold with dense vegetation.
- The image-difference metric against the reference path tracer stays under its threshold on fixed camera paths. The metric and threshold are ADR 0005's (#102).

**Playtest checklist:** visit each biome and each fantasy feature, see a crystal cave lit by emissive GI, look through glass and water, see god rays in a forest, follow a river to the sea, watch a day pass into night, undo a dig, and repeat the full M1a checklist.

### M2: Multiplayer

**Scope**
- A dedicated headless server with renet transport replacing the loopback.
- The join flow: version and probe hashes, then edited-brick snapshots.
- Prediction and reconciliation, and interpolation of other players.
- Prioritised snapshots, Edit-op broadcast, and brick-hash checks near Edits.
- Server config (one world per process) and the ~2 GB client cache.
- Player identity from a persistent id the client generates.
- **Friends-only servers**: a password or an allow-list, and an admin who can kick a player and restore a backup. Everyone on a server is trusted; there are no claims or roles.
- One seeded world spawn where every player starts (ADR 0008).
- The server owns the world clock (#138), and undo is tested with two players.

**Acceptance**
- The server holds **60 Hz with 32 scripted bot clients** (walking, flying and digging at the rate limits, spread over the Planet) **plus 2 human players**.
- ≤ ~1 Mbit/s per client.
- **Zero brick-hash mismatches over a 1-hour soak.**
- Server memory stays within ~0.25 GB of voxel working set for 30 spread-out players, plus overhead.

A headless bot soak against the in-process server runs from M1a on (tick time and memory per player), so the server's per-tick Edit budget and the memory target are measured before this Milestone.

**Playtest checklist:** two humans on separate PCs see each other move smoothly, dig and build together, see each other's Edits from orbit, and rejoin to a cached world.

### M3: Debris

**Scope**
- The connectivity flood fill (1M-Voxel budget).
- rapier3d with parry3d `Voxels` and temporary terrain colliders.
- Fracture by hardness.
- Merging back after ~3 s at rest.
- One client-only particle system: dust and Voxel-sized rubble, never replicated (#138).
- Players are pushed by and can ride Debris.
- TLAS instances for Debris.
- Server spawns plus snapshot replication.

**Acceptance**
- With **1,000 live Debris** the server step stays ≤ ~8 ms and client BLAS/TLAS updates stay within 1 ms.
- Over budget, the oldest and farthest Debris merge early.
- Debris state is consistent across clients.

**Playtest checklist:** undercut an overhang and watch it fall and fracture, ride a falling slab, watch rubble merge and dig into it, and do the same with a second player watching.

### M4: Fluids

**Scope**
- The cellular automaton: fill levels for water and lava, granular sand, 26-direction down.
- Rivers and the ocean surface simulate near players (~500 m), sleeping elsewhere; the ocean is an infinite source.
- Interactions: lava + water → rock, sand sinking, currents, and Debris buoyancy and pushing.
- Path-traced water and lava surfaces.
- Delta replication.

**Acceptance**
- 20 Hz CA, ≤ 2M active Voxels, ≤ ~4 ms per tick, and deltas ≤ 20 Hz.
- Over budget, the farthest regions sleep first.
- M2 network and perf gates still hold.

**Playtest checklist:** dig a channel from the sea and watch it flood, dam a river, pour lava into water, drop sand into a lake, and get carried by a current.

### M5: Mid-range GPUs

**Scope:** a quality-preset system, plus profiling and optimisation on the RTX 3060 / RX 6700 class, using a borrowed or cloud mid-range GPU for verification.

**Acceptance**
- **60 fps at 1080p output** (~960×540 internal) on an **RTX 3060 / RX 6700** with all features at a "medium" preset.
- The 4090 targets still hold at "high".
- Compute DDA and hardware RT are each benchmarked on AMD and NVIDIA, and the faster one is chosen per vendor.

**Playtest checklist:** the full M1a/M1b checklist on a 3060-class GPU at 1080p.

## After the Vertical slice

Decided 2026-11-11. These are ordered but not designed; each is grilled when it is near.

- **M6: Moving Pieces.** Doors and hatches, on Debris's moving bodies.
- **M7: Survival and Tools.** Gathering, an inventory, health. A Tool is what a character holds; it owns the Brushes its holder can use, and replaces free Brush choice.
- **M8: Solar system.** More than one Planet, and travel between them.

Two seams are kept open for them now, and nothing else is built:

- **For more Planets:** code takes the Planet as a value (radius, seed), not as global constants, and saves and the Edit protocol carry a Planet id.
- **For survival:** an applied Edit reports the Voxels it removed and added of each Material, and a save holds a record for each player that can grow.

## Explicitly out of scope

Of the Vertical slice; the first two items are M7 and M8's.

- Gameplay loop, progression, crafting, survival, inventory, health and fall damage.
- Multiple planets, moons or space travel.
- MMO scale and sharding.
- Web, console and mobile.
- Commercial concerns.
- Paint, line/box and copy-paste tools, and landscape-scale area sculpting.
- Travel aids: teleport and fast travel. The Planet is crossed by walking and flying, and seen whole from orbit.
- Ambient creatures, vehicles, fire and explosives, mechanisms (moving Pieces are M6's), and recreating real places (#138).
- Public servers, claims and per-player rollback.
- Structural-stress collapse.
- NRD and DLSS integration.

## Known risks

- **Snapping a Piece to a build whose Voxels do not remember being Pieces** (ADR 0011). The Piece ticket must show it working on a build picked up again after a reload.
- **Debris and Fluids at 5 cm are unproven.** The M1b prototype measures them.
- **Generation is 7–8× short of strict full detail at 50 m/s at 5 cm** (about 150 K generated bricks/s against 1.1–1.3 M). Mitigated by speed-scaled LOD (ADR 0009), the Guard, and the streaming bench's soft-miss gate.
- **Terrain below the world map's ~240 m texels has no precomputed erosion** (ADR 0008). Mitigated by the look-dev prototype and a landscape golden; rivers need a new design in M1b-1.
- **Voxel-count budgets for Debris and Fluids cover an eighth of the volume at 5 cm** (ADR 0007). M3 and M4 re-tune them.

- **wgpu's ray queries are experimental.** Mitigated by pinning wgpu and keeping the compute `trace()` fallback.
- **Every lighting feature is in scope**, and glass complicates denoising. Mitigated by splitting M1a from M1b and comparing against the reference path tracer.
- **`planet-gen` is the heaviest CPU workload**: dense vegetation, rivers and fantasy features. Mitigated by throughput targets in CI.
- **"Bigger" Debris limits and ocean simulation near players** stress the server. Mitigated by graceful degradation and per-Milestone budgets.
- **Bit-identical generation is a hard requirement.** Mitigated by the determinism matrix plus runtime hash checks.
- **The dev is a beginner and agents work blind on visuals.** Mitigated by goldens, the reference mode, per-pass timers and benchmark gates.



