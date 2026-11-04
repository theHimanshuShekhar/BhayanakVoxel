# Research: networking an editable voxel Planet for dozens of players

Issue: [#6](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/6). Researched 2026-10-27.
Vocabulary follows [`CONTEXT.md`](../../CONTEXT.md): Planet, Voxel, Material, Edit, Debris, Fluid, Zoom, Milestone.

Labels used below:
- **[source]** means a primary source states it, and the link follows.
- **[calc]** is my own arithmetic. Every step is shown, so the assumptions can be swapped out.
- **[unverified]** means I could not confirm it from a primary source.
- **[recommendation]** is a judgement call, not a fact.

## TL;DR

1. **Do not stream voxel terrain from the server.** Each client should generate the procedural base Planet itself from `(seed, generator version)`. The server should send only the Edits. If the server streamed terrain at the scale and LOD range this game wants, it would cost about **12 Mbit/s per walking player at 10 cm Voxels** and **hundreds of Mbit/s at 2 cm** (see the terrain-streaming section). That is roughly 0.8 Gbit/s for 64 players, even at 10 cm. Space Engineers planets work this way: a planet is saved as `(version, seed, generator)` plus an octree of modifications ([source](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Planet/MyPlanetStorageProvider.cs)).
2. **Replicate Edits as small operations, not as voxel deltas.** An operation looks like "dig sphere r = 1 m at (x, y, z) with tool T". Space Engineers broadcasts voxel shape operations as reliable events ([source](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/MyVoxelBase.cs)). Teardown sends "cut hole in this shape at voxel coord x,y,z" commands, because sending the changed voxels "used enormous amounts of bandwidth" ([source](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html)). Each operation is about 24 bytes. Sending every Edit by every player to everyone costs about **61 kbit/s per client** for 64 busy players. A per-voxel delta for one 1 m dig at 2 cm Voxels is about **2 MB**.
3. **The server is authoritative over everything**: Edits, player movement, Debris and Fluids. Clients predict their own movement and Edits and then reconcile with the server, which is the standard approach ([source](https://gafferongames.com/post/what_every_programmer_needs_to_know_about_game_networking/)). Players and Debris use state synchronization with interest management and priority, not lockstep. Fluids start as server-simulated and are sent as per-chunk deltas.
4. **The biggest technical risk is determinism.** Client-side generation and operation replay only work if every machine produces bit-identical Voxels. That rules out compiler fast-math defaults and GPU-generated authoritative terrain. It calls for integer or fixed-point Edit rasterization, plus a chunk-hash check that falls back to sending the real chunk.
5. **Voxel size is also a networking decision.** At 2 cm, the server's full-resolution working set for 64 scattered players is about 165 GB raw (about 20 GB palette-compressed). At 10 cm it is about 4 GB raw (about 0.5 GB compressed). See the memory section.

## Recommended architecture (plain language)

```
                ┌──────────────────────── Dedicated headless server ───────────────────────┐
                │  Same "world core" library as the client: generator + Edit applier.     │
                │  No rendering, no LOD.                                                  │
                │  - Loads full-resolution chunks only near players (simulation radius).  │
                │  - Validates Edit requests, applies them, numbers them (global seq #).  │
                │  - Simulates players, Debris (physics) and Fluids. Fixed tick, ~20-30 Hz.│
                │  - Stores: seed + generator version + per-chunk edited data + Edit log. │
                └────────▲───────────────────────────────┬────────────────────────────────┘
                         │ inputs, Edit requests          │ reliable-ordered: Edit ops, chunk fixes,
                         │ (with client sequence ids)     │   Debris spawns, Fluid deltas, acks
                         │                                │ unreliable: player/Debris state snapshots
                ┌────────┴───────────────────────────────▼────────────────────────────────┐
                │  Client                                                                  │
                │  - Generates the base Planet locally at every LOD from (seed, version).  │
                │  - Applies the Edit-op stream on top (all ops, Planet-wide: it's cheap). │
                │  - Predicts own movement + own Edits; reconciles on server ack.          │
                │  - Interpolates other players and Debris.                                │
                │  - Caches edited chunks on disk keyed by (chunk id, version).            │
                └──────────────────────────────────────────────────────────────────────────┘
```

In words:

- **One world-core library, two programs.** The procedural generator, Voxel storage and the code that applies an Edit live in one library. The headless server and the client both link it. This is what makes "client generates, server only sends Edits" safe.
- **An Edit is a command.** The client says "I want to dig sphere r = 0.5 m at voxel (x, y, z), sequence 812". The server checks reach, tool and rate limits. It then applies the Edit and gives it a global number. Next it broadcasts the command to every client, which all apply it in the same order. Finally it tells the requester "812 accepted" or "812 rejected, here is the true chunk". Minecraft uses the same predict-then-acknowledge pattern for block changes ([source](https://minecraft.wiki/w/Java_Edition_protocol/Packets#Acknowledge_Block_Change)).
- **Joining or reconnecting.** The client gets the seed and generator version. For each edited chunk it is about to load, it also gets the chunk's version and the edited data, sent as a compressed difference from the generated base. Unedited chunks cost nothing. Space Engineers sends a voxel map's saved storage only when `contentChanged` is true. Otherwise the client builds the map from its definition ([source](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/Replication/MyVoxelReplicable.cs)).
- **Moving things (players, Debris)** are sent as state snapshots over an unreliable channel. They use a per-client priority budget, favouring nearby and visible objects. Teardown and Gaffer's state synchronization both work this way ([Teardown](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html), [Gaffer](https://gafferongames.com/post/state_synchronization/)).
- **Fluids** run only on the server, and only in active regions near players. Changed chunks are sent as compact deltas at a reduced rate. Settled Fluid becomes ordinary Voxels.
- **Transport** is UDP with a reliable-ordered channel and an unreliable channel. One option is Valve's GameNetworkingSockets (BSD-3). It provides "both reliable and unreliable message types", fragmentation and AES-GCM encryption ([source](https://github.com/ValveSoftware/GameNetworkingSockets)). Use whatever equivalent the chosen engine has.

What M1 needs so multiplayer can be added later, even though multiplayer itself ships after M1 **[recommendation]**:

1. Every Edit goes through `apply(EditOp)`, even in single-player. The UI never writes Voxels directly.
2. The generator and `apply(EditOp)` are deterministic and covered by a hash test (see Risks).
3. World state and simulation do not depend on the renderer, so they can be compiled headless.
4. A fixed simulation tick, separate from the render frame rate.
5. Integer voxel coordinates for all world addressing.

---

## 1. Replicating Edits: operation logs versus chunk deltas

### What prior art does

| Game | How a world change goes over the wire | Source |
|---|---|---|
| Minecraft (Java) | Per-block deltas. **Block Update** carries a position and a block-state VarInt. **Update Section Blocks** is "fired whenever 2 or more blocks are changed within the same chunk section on the same tick". Each entry packs the block state id, shifted left by 12, with a 4+4+4-bit position inside the 16³ section, as a VarLong. **Acknowledge Block Change** confirms a client-predicted change by sequence id. | [Packets wiki](https://minecraft.wiki/w/Java_Edition_protocol/Packets) |
| Luanti (Minetest) | Per-node deltas `TOCLIENT_ADDNODE` (position + serialized node) and `TOCLIENT_REMOVENODE`. Whole 16³ MapBlocks go via `TOCLIENT_BLOCKDATA`. To save bandwidth, "block transfers are slowed down when a player is building something" (`full_block_send_enable_min_time_from_building`, default 2.0 s). | [networkprotocol.h](https://github.com/luanti-org/luanti/blob/master/src/network/networkprotocol.h), [settingtypes.txt](https://github.com/luanti-org/luanti/blob/master/builtin/settingtypes.txt) |
| Space Engineers | Operations. A client calls `RequestVoxelOperationSphere/Box/Capsule/Ramp/Elipsoid(...)`, which raises a `[Event, Reliable, Server]` RPC. The server then raises `PerformVoxelOperation*_Implementation` as `[Event, Reliable, Broadcast]` with the shape parameters, material and operation type. Explosions use `VoxelCutoutSphere_Implementation(center, radius, …)`, also reliable broadcast. | [MyVoxelBase.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/MyVoxelBase.cs) |
| Teardown (MP, 2026) | Operations, run deterministically. Destruction was rewritten "in fixed-point integer math". Each break becomes commands such as "cut hole in this shape at voxel coord x,y,z", delivered reliably and in order. Bandwidth stays small because commands don't scale with object size. The first attempt sent changed voxel data and "completely choked the connection when large objects were destroyed". | [Voxagon blog](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html) |

### Why operations win for micro-Voxels [calc]

Take one Edit: dig a sphere of radius 1 m. The number of Voxels touched is (4/3)·π·(1 m / s)³, where s is the Voxel size. Chunks here are 32³ Voxels, so a chunk edge is 32·s.

| Voxel size s | Voxels touched | Per-voxel delta at ~4 B each | Chunks touched ≈ (2 m / chunk edge + 1)³ | Re-send touched chunks at ~4 KB each | Operation message |
|---|---|---|---|---|---|
| 25 cm | 268 | 1.1 KB | 2 | 8 KB | ~24 B |
| 10 cm | 4,189 | 16.8 KB | 4 | 17 KB | ~24 B |
| 2 cm | 523,599 | **2.1 MB** | 70 | **281 KB** | ~24 B |

These figures assume the following:
- **Per-voxel delta at 4 B** is a rough figure, about half a Minecraft VarLong entry.
- **4 KB per compressed 32³ surface chunk** comes from Minecraft's 4-bits-per-entry palette. That is 2,048 B of data per 16³ section ([chunk format](https://minecraft.wiki/w/Java_Edition_protocol/Chunk_format)). So 32³ = 8 sections ≈ 16 KB before general compression. Zstd or deflate typically reduces that by a few times. **[unverified for our data: measure it]**
- **The 24-byte operation** breaks down as follows:
  - op + shape: 2 B
  - centre as 3 × i32 voxel coordinates: 12 B
  - radius: 2 B
  - Material: 2 B
  - global sequence number: 4 B
  - player id: 2 B

**The operation stream, worst case [calc]:**
- Setup: 64 players, each doing 5 Edits/s, and every Edit is sent to all 64 clients.
- Server out: 64 × 5 × 24 B × 64 = 491,520 B/s ≈ **3.9 Mbit/s**.
- Per client: 3.9 Mbit/s / 64 ≈ **61 kbit/s**.
- Overhead: if Edits are batched once per 50 ms tick, UDP/IP headers add about 64 × 20 × ~40 B ≈ 51 KB/s total.

So it is affordable to **broadcast every Edit to every client Planet-wide**. Each client can then show far-away craters in its coarse LOD (for the Zoom) without any extra "LOD of Edits" protocol. This is simpler than per-region interest management for Edits **[recommendation]**.

**Costs of the operation approach:**
- **Determinism.** Every client must rasterize the shape into exactly the same Voxels. Use integer or fixed-point maths for the shape test, as Teardown did ([source](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html)).
- **Order.** Operations do not commute (dig-then-place ≠ place-then-dig). Apply them in the server's global order over a reliable-ordered channel.
- **CPU.** A large brush applied on 64 clients plus the server multiplies CPU work, and a 10 m sphere at 2 cm is about 5×10⁸ Voxels. Cap brush size.
- **Non-Edit changes need a second path.** Fluids, Debris detachment and server scripts don't come from a player operation. Keep a generic, reliable "chunk delta" message for them (see section 3).
- **Late joiners.** Replaying the whole history is a bad idea. Teardown replays its command buffer, but has to disable joining once the buffer fills ([source](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html)). Instead, keep **per-chunk materialized edited data plus a version number** on the server. Send that to joiners, then switch them to the live operation stream.

## 2. Streaming terrain: send voxel data, or let clients generate?

### Prior art

- **Minecraft: the server sends terrain.** **Chunk Data and Update Light** is "sent when a chunk comes into the client's view distance". Sections are 16³ paletted containers: 0 bits per entry when the section is a single block type, 4–8 bits with a local palette, 15 bits direct ([chunk format](https://minecraft.wiki/w/Java_Edition_protocol/Chunk_format)).
  - Packets above `network-compression-threshold` (default 256 bytes) are zlib-compressed ([server.properties](https://minecraft.wiki/w/Server.properties), [Packets](https://minecraft.wiki/w/Java_Edition_protocol/Packets)).
  - Rate control is client-driven. The client answers each batch with **Chunk Batch Received** carrying "desired chunks per tick". The vanilla server "will stop sending further chunk data until the client acknowledges the sent chunk batch", then allows "up to 10 unacknowledged batches" ([Packets](https://minecraft.wiki/w/Java_Edition_protocol/Packets#Chunk_Batch_Received)).
  - `view-distance` defaults to 10 chunks ([server.properties](https://minecraft.wiki/w/Server.properties)).
- **Luanti: the server sends terrain.** 16×16×16 MapBlocks are serialized and zstd-compressed since map format 29 ([world_format.md](https://github.com/luanti-org/luanti/blob/master/doc/world_format.md)). The network settings are in [settingtypes.txt](https://github.com/luanti-org/luanti/blob/master/builtin/settingtypes.txt):
  - `max_block_send_distance` defaults to 13 MapBlocks.
  - `max_simultaneous_block_sends_per_client` defaults to 50.
  - `server_side_occlusion_culling` "can reduce the number of blocks sent to the client by 50-80%".
- **Veloren: the server sends terrain.**
  - Chunks are "32x32xZ" and "tend to be a few hundred kilobytes each" uncompressed. LZ4 reaches "around 25%", deflate "around 17%", and the image-based lossless format for gameplay data "around 1-2%" ([devblog 117](https://veloren.net/blog/devblog-117/)).
  - Earlier, network output hit "10 Mbit/s per player even on idle". LZ4 had to be re-enabled "to serve more than 4 players at a time on the official server" ([devblog 75](https://veloren.net/blog/devblog-75/)).
  - The world map on disk is a coarse heightmap of "only 16 bytes per chunk" ([devblog 117](https://veloren.net/blog/devblog-117/)).
- **Vintage Story:** chunks are 32 blocks, and `MaxChunkRadius` is "How many chunks in each direction should be loaded at most for each player" ([API docs](https://apidocs.vintagestory.at/api/Vintagestory.API.Server.IServerConfig.html)). **[unverified]** That the server streams chunk contents rather than clients generating them. This is likely but I found no primary source on the wire format.
- **Space Engineers: the client generates, and the server sends only modified storage.**
  - The planet storage provider serializes just `Version`, `Seed` and the generator name ([MyPlanetStorageProvider.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Planet/MyPlanetStorageProvider.cs)).
  - The voxel replicable sends `Voxel.Storage.Save(...)` bytes only if `contentChanged`. Otherwise the client initializes the map from its object builder ([MyVoxelReplicable.cs](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/Replication/MyVoxelReplicable.cs)).
  - Note: this is the public source snapshot (last pushed 2022), not necessarily what ships today.
- **Dual Universe:** the founder said "a planet is basically a seed number… They only store on the disk the difference between the procedurally generated planet and whatever modification you have done". This is an interview quote, not a protocol document **[secondary source]** ([Koalition interview, 2016](https://thekoalition.com/2016/dual-universe-gameplay-debut-qa-with-novaquarks-founder-jean-christophe-baillie)).

### Why this Planet can't be server-streamed [calc]

**How big is the Planet?** Volume Voxels = (4/3)·π·R³ / s³. Assume radius R = 1 km, since the Planet size is not chosen yet.

| s | Voxels in the whole Planet | Raw at 2 B/Voxel |
|---|---|---|
| 25 cm | 2.7×10¹¹ | 0.5 TB |
| 10 cm | 4.2×10¹² | 8.4 TB |
| 2 cm | 5.2×10¹⁴ | 1,047 TB |

Nobody, server or client, can store this. The base Planet *must* be a function, `generate(seed, version, chunk, LOD)`, with only Edits stored. This holds regardless of networking.

**Streaming cost if the server sent chunks.**
- Assume full-resolution (LOD0) radius r = 64 m, about 3 chunks thick at the surface, and 4 KB per compressed chunk.
- At s = 10 cm, the chunk edge is 3.2 m:
  - LOD0 columns = π·64² / 3.2² = 1,257, so about 3,770 chunks, or about 15 MB.
- Each coarser LOD ring with a constant chunk radius has about the same chunk count. Getting from a 3.2 m chunk to a 2 km planet diameter takes ⌈log₂(2000/3.2)⌉+1 ≈ 11 levels. A full initial load is therefore 11 × 3,770 × 4 KB ≈ **166 MB per client**.
- While walking at 5 m/s, new LOD0 chunks per second = (leading edge 2r × speed / chunk area) × thickness = (128 × 5 / 10.24) × 3 ≈ 187 chunks/s ≈ 750 KB/s.
- Coarser rings each add about half of the previous ring. Their chunk edge doubles, so the chunks crossed per second halve, and the total is about 2× LOD0 ≈ 1.5 MB/s ≈ **12 Mbit/s per player**, or **768 Mbit/s server upload for 64 players**.
- Flying at 50 m/s is 10× that.
- At s = 2 cm, the same radius is 94k chunks, and walking costs about **300 Mbit/s per player**.

Zooming to orbit with server-streamed terrain would mean shipping a whole-Planet LOD pyramid to every client, which is not viable. Client-side generation reduces terrain bandwidth to the edited chunks only. Each one is sent as a compressed diff from the generated base, at a small cost that has not been measured.

**The catch is that client generation must be bit-identical to the server's.** Mitigations **[recommendation]**:
- The generator lives in shared world-core code, and the server and client run the *same* algorithm on the CPU. A GPU may generate **visual-only far LOD**. GPU floating-point results are not guaranteed to match the server, so they must never feed collision or Edits.
- Avoid compiler floating-point contraction and fast-math in that code. For example, GCC's default is "`-ffp-contract=fast`" except in strict ISO C mode, which can fuse multiply-adds and change results between builds ([GCC docs](https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html)). Better still, write noise and Edit rasterization in integer or fixed-point arithmetic.
- Version the generator. The Planet save stores `(seed, generator_version)` and clients refuse to join on a mismatch, as Space Engineers stores `Version` + `Seed` ([source](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Engine/Voxels/Planet/MyPlanetStorageProvider.cs)).
- **Hash check with fallback.** The client periodically sends hashes of the LOD0 chunks around it. On a mismatch, the server sends the real chunk, so the game self-heals instead of desyncing. Also log the mismatch: it is a bug.
- **CI determinism test.** Generate N fixed chunks on every target OS and compiler and compare hashes.

## 3. Server authority: movement, Debris, Fluids

### Techniques (primary: Glenn Fiedler, Gaffer On Games)

- **Deterministic lockstep** sends only inputs, so "bandwidth is proportional to the size of the input, not the number of objects". It needs determinism "exact down to the bit-level". Fiedler warns that floating-point determinism across compilers, operating systems or debug/release builds is not guaranteed ([source](https://gafferongames.com/post/deterministic_lockstep/)).
- **Snapshot interpolation** runs no simulation on the receiver. Its cost: 900 cubes at 60 Hz took 11.6 Mbit/s, dropping to about 2 Mbit/s at 10 Hz. A 10 Hz rate needs about 300–350 ms of interpolation delay to survive two lost packets; at 30 Hz, 150 ms ([source](https://gafferongames.com/post/snapshot_interpolation/)).
- **State synchronization** sends inputs plus state for a prioritized subset each packet, under a fixed budget such as "256kbit/sec". It uses a per-object **priority accumulator**, so objects that didn't fit "are first in line to be included in the next packet". It is "an approximate and lossy synchronization strategy" ([source](https://gafferongames.com/post/state_synchronization/)).
- **Client-side prediction with rewind-and-replay.** "The server is the man": the server is authoritative over each player character, and the client predicts locally and replays inputs when a correction arrives ([source](https://gafferongames.com/post/what_every_programmer_needs_to_know_about_game_networking/)).
- **Space Engineers** (GDC 2023, "Predicted Physics Based Multiplayer in Space Engineers", Jan Hlousek): clients send controls and the server sends delta state updates. **[unverified]** The slides PDF is image-only and I could not read it directly. This summary comes from a search-engine excerpt. Slides: [PDF](https://media.gdcvault.com/gdc2023/Slides/Predicted+Physics+Based_Hlousek_Jan.pdf).
- **Teardown:** deterministic commands handle scene changes. State sync with "eventual consistency" over an unreliable stream handles physics and players, with a per-client priority queue favouring visible objects. The budget is "around one Mbit per client". It is listen-server only, with no dedicated servers ([source](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html)).

### Applied to BhayanakVoxel [recommendation]

| Thing | Model | Why |
|---|---|---|
| Player movement | Server-authoritative. The client predicts with rewind-and-replay, and others are interpolated. | Standard approach; it resists cheating and needs no physics determinism. |
| Edits | Deterministic operations, reliably ordered (section 1). The client predicts its own Edit and the server acknowledges or corrects it. | Bandwidth is independent of Voxel size. |
| Debris | Server-simulated rigid bodies, sent with state sync and a priority accumulator. On spawn, send the Debris' Voxel content compressed (reliable) together with an id. After that, send only its transform and velocity. | Full lockstep physics is hard. Jolt claims cross-platform determinism only with a CMake option, about 8% slower, the same source and defines everywhere, and the broadphase queries "are NOT deterministic" ([Jolt docs](https://jrouwe.github.io/JoltPhysics/index.html)). Rapier needs the `enhanced-determinism` feature and strict IEEE-754 targets ([Rapier docs](https://rapier.rs/docs/user_guides/rust/determinism/)). Teardown's author calls their hybrid "a lot of hard work and a lot of code". |
| Fluids | Server-simulated in active regions near players. Changed chunks go out as compact deltas (changed-cell bitmask + Materials, compressed) at a reduced rate, for example 5–10 Hz, to clients whose interest radius covers them. Settled Fluid becomes normal Voxels. | Simplest correct approach. A deterministic integer cellular automaton run on clients is a possible later optimization. It needs tick-aligned inputs and a state snapshot for joiners **[open question]**. |
| Interest management | Edit operations go Planet-wide (cheap). Entities, Debris and Fluid deltas only go within a radius, as Minecraft does with `simulation-distance` and Luanti with `active_block_range` / `active_object_send_range_blocks` ([server.properties](https://minecraft.wiki/w/Server.properties), [settingtypes.txt](https://github.com/luanti-org/luanti/blob/master/builtin/settingtypes.txt)). | Keeps per-client bandwidth bounded. |
| Tick rate | Fixed server tick of about 20–30 Hz. Minecraft runs 20 ticks/s ([Tick](https://minecraft.wiki/w/Tick)) and Luanti's `dedicated_server_step` defaults to 0.09 s ([settingtypes.txt](https://github.com/luanti-org/luanti/blob/master/builtin/settingtypes.txt)). | Proven sufficient for building games. |

**Player-state bandwidth [calc]:**
- Setup: 64 players, each about 20 B of quantized state (position, orientation, velocity), at 20 Hz, sent to the 63 others.
- Server out: 64 × 63 × 20 × 20 B ≈ 1.6 MB/s ≈ **12.9 Mbit/s**.
- Per client: about **200 kbit/s**.
- Interest radius and send-rate falloff with distance reduce this further.

**Debris bandwidth:** give it a fixed per-client budget, for example 256 kbit/s as in Fiedler's example ([source](https://gafferongames.com/post/state_synchronization/)). The priority accumulator degrades gracefully when there is too much Debris.

**Fluid bandwidth example [calc]:** a flood touching 100 chunks, each with a change set of ~200 B, at 10 Hz costs 100 × 200 × 10 = 200 KB/s ≈ **1.6 Mbit/s** for each client watching. That is too much as a steady state, so cap it: lower the rate for distant chunks and merge consecutive deltas. **[unverified: the 200 B per-chunk delta is a guess and needs measuring]**

**Per-client total, steady state [calc]:** Edits 61 kbit/s + players 200 kbit/s + Debris ≤ 256 kbit/s + Fluids (capped). That comes to roughly **0.5–1 Mbit/s per client**, the same order as Teardown's "around one Mbit per client" budget. For 64 clients, the server needs roughly **32–64 Mbit/s of upload** in steady state, plus bursts when players join (edited chunks only).

## 4. Server memory and CPU for one Planet [calc]

The server renders nothing and keeps no LOD. It needs full-resolution chunks only within a *simulation radius* around each player, for collision, Edit application, Fluids and Debris. It also needs the edited-chunk store.

**Working set**, with simulation radius 32 m, about 3 chunks thick (more for small chunks), 32³ chunks at 2 B/Voxel = 64 KiB raw, and 64 players spread out so there is no overlap:

| s | Chunks per player | Raw per player | 64 players raw | Assuming palette compression to ~1/8 |
|---|---|---|---|---|
| 25 cm | 151 | 10 MB | ~1 GB | ~0.1 GB |
| 10 cm | 942 | 62 MB | ~4 GB | ~0.5 GB |
| 2 cm | 39,270 (5 thick) | 2.6 GB | ~165 GB | ~21 GB |

The 1/8 palette ratio is an assumption, based on Minecraft's 4-bit palettes versus 15-bit direct ([chunk format](https://minecraft.wiki/w/Java_Edition_protocol/Chunk_format)). **[Unverified for our data.]** At 2 cm, the server must shrink the simulation radius, use a sparser structure such as an octree or run-length encoding, or both. This should feed into the Voxel-size decision.

**Edit storage growth:**
- Assume 64 players each dig 1 m³/min, which is 3,840 m³/h.
- At 10 cm: 3.84×10⁶ Voxels/h ≈ 8 MB/h raw.
- At 2 cm: 4.8×10⁸ Voxels/h ≈ 960 MB/h raw. That is before compression or diff-from-base, which should shrink it a lot because dug Voxels are mostly "air".

**CPU, terrain generation:**
- At 10 cm, a walking player crosses into (2·32 m × 5 m/s / 10.24 m²) × 3 ≈ 94 new chunks/s of the simulation radius.
- 64 players → ~6,000 chunks/s.
- At an assumed 1 ms per chunk per core, that is ~6 cores of generation alone.
- **[Unverified: generator cost is unknown, so measure "chunks generated per second per core" in M1.]**
- Caching generated chunks and players clustering together both help.

**CPU, simulation:** Edits are cheap if brush sizes are capped. The costs of Fluids and Debris physics are open-ended, so bound them by active-region caps.

## 5. Prior art not fully covered

- **Avorion:** I found no primary source on its networking internals. Hosting guides only mention settings like `broadcastInterval` and `aliveSectorsPerPlayer` ([Avorion wiki](https://avorion.fandom.com/wiki/Setting_up_a_server)). **[unverified, low value]**
- **Vintage Story:** wire format not found (see section 2).
- **No Man's Sky / Astroneer** (procedural planets with terrain edits in multiplayer): no primary technical source found. Not researched further.
- **Valve's Source Multiplayer Networking** article (lag compensation, interpolation) was behind a bot wall during this research, so I used Gaffer On Games instead.

## Risks for a beginner-led, agent-built project

1. **Determinism bugs are silent and late.** Generator or Edit-op desyncs show up as "terrain looks different for my friend" weeks later. Mitigate with the hash-check fallback from day one, plus a CI test that hashes generated and edited chunks across OS and compiler builds. Agents can write these tests well when asked explicitly.
2. **Building the generator twice.** If the client generates on the GPU for path tracing or LOD while the server uses the CPU, results diverge. Decide early that authoritative near-field generation is shared CPU code.
3. **Networking retrofits are expensive.** Teardown's author wrote that adding multiplayer was "far more complicated than making the entire engine for single player" and spanned "3+ years and dozens of developers" ([post on X](https://x.com/voxagonlabs/status/2027632578958623140); **[unverified: read via a search excerpt, not fetched directly]**). His blog calls the result "a lot of hard work and a lot of code" ([blog](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html)). The five M1 rules in the architecture section are the cheap insurance.
4. **Debris and Fluids are where bandwidth explodes.** Budget and prioritize them from the start, and never send them as "full chunk every tick".
5. **The 2 cm Voxel size multiplies every cost** by (10/2)³ = 125× per volume relative to 10 cm, for server memory, Edit CPU and Fluid deltas. Networking is the one area it barely affects, because Edits travel as operations.
6. **Scope creep in the transport.** Use an existing reliable-plus-unreliable UDP library rather than letting agents write a custom reliability layer. Veloren needed several iterations of its own network stack ([devblog 75](https://veloren.net/blog/devblog-75/)).

## Open questions for downstream decisions

- **Voxel size** (2–25 cm) and **Planet radius** are needed to finalize the memory and CPU numbers. Every [calc] above is parameterized so it can be redone.
- **How long does generating one chunk take?** This determines whether the server can keep up with 64 moving players, and whether the client can generate LOD fast enough during a Zoom.
- **Fluid model:** server deltas (recommended first) or a deterministic client-side cellular automaton?
- **Debris spawn:** send the Voxel content, or have clients deterministically recompute the detached group? (The recommendation is to send it.)
- Whether the chosen engine or language has a mature reliable-plus-unreliable UDP layer, and whether it makes the world core easy to build headless.
