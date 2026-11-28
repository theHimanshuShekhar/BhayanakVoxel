# Research: player edits in fine-voxel engines

Resolves issue [#279](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/279), part of the interaction map [#275](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/275). Researched 2026-11-28. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md) (with the Tool, Creative, Survival, Plant and Piece additions of PR #248). This builds on [inspiration-teardown-rt.md](inspiration-teardown-rt.md), [inspiration-john-lin-water.md](inspiration-john-lin-water.md), [inspiration-john-lin-crystal-islands.md](inspiration-john-lin-crystal-islands.md), [inspiration-elements-engine.md](inspiration-elements-engine.md) and [voxel-networking.md](voxel-networking.md), and does not repeat them. It lists options with trade-offs; it decides nothing.

## TL;DR

- **No studied engine has published a numeric Edit-rate budget.** Teardown caps the size of one shape (an explosion is 0.5 to 4.0, paint is 0 to 5 m) and runs the cut on the server. Enshrouded and Lay of the Land pace digging with swings, stamina and tool durability. Our 8 m³/s (ADR 0007) is a server-protection ceiling, not a feel control. A hand Tool will sit well under it: a 0.25 m pickaxe sphere is 524 Voxels, about 0.8% of one second's 64,000 (arithmetic below).
- **Teardown confirms ADR 0003's shape, from the developers themselves.** The shipped multiplayer sends deterministic commands ("cut hole in this shape at voxel coord x,y,z") over a reliable ordered stream and never voxel data. It rewrote destruction in fixed-point integer maths for this. It also caps the command buffer for late joiners and disables join-in-progress when it fills, which ADR 0003 avoids with edited-brick snapshots.
- **Enshrouded is the closest shipped analogue of Survival terraforming, and it has what the spec does not yet say.** Different Tools shape terrain differently (pickaxe digs; hammer removes or puts back terrain; rake flattens, raises and cuts ditches, with adjustable radius, shiftable depth and a fine-tune mode). Terraforming is client-predicted (patch note, May 2025) and permission-gated by role. **Undo of terraforming exists and is tied to the inventory**: since 1.0 it still works with a full backpack but then does not return the resources.
- **Two engines give shape-by-material and shape-by-mode evidence for M7.** Teardown's `MakeHole` takes three radii, one each for soft, medium and hard Materials, so a hard Material is cut by a smaller hole. Lay of the Land's tools have a selectable dig mode ("normal", "box", "cube") with patch-note bugs about it resetting.
- **Far LOD of Edits is undocumented everywhere.** No source says how an Edit shows beyond the near field. Atomontage claims levels of detail "inherently always stay in sync" (a marketing quote with no mechanism). This is one place where ADR 0003 (Planet-wide ops) and [dynamic-content.md](dynamic-content.md) are ahead of the sources.
- **Chunky particles and edit snapping have almost no primary source.** The only first-party particle reference is Teardown's modding API, which splits the world edit (server only) from particles (client only). Snapping evidence is patch notes: a one-Voxel placement tolerance (Lay of the Land) and a snapping toggle (Enshrouded, community wiki).
- **Atomontage, Voxel Farm and John Lin's engine publish little on player edits.** What they do publish is listed, and marked where it is a search summary or marketing.

## Sources and how they were used

Quality labels: **First-party** (developer's own post, talk, docs or patch notes), **Press** (an interview or article quoting developers), **Community** (a fan wiki, forum or player post). Steam posts are the developers' own, read from Steam's public news API (`ISteamNews/GetNewsForApp`) and linked to their Steam announcement pages.

| Key | Source | Quality | Used for |
|---|---|---|---|
| T1 | Gustafsson, ["The unlikely story of Teardown Multiplayer"](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html), 13 Mar 2026 | First-party | Edit replication, determinism, late join |
| T2 | Tuxedo Labs, [Teardown modding API](https://teardowngame.com/modding/api.html) (`MakeHole`, `Explosion`, `Paint`, `PaintRGBA`, `SpawnParticle`, `ParticleCollide`, `ParticleSticky`) | First-party docs | Edit shapes by material class, size caps, server and client split |
| T3 | Gustafsson, ["The Spraycan"](https://blog.voxagon.se/2020/12/03/spraycan.html), 3 Dec 2020 | First-party | Material painting on palette voxels |
| T4 | Gustafsson, ["Teardown quicksave"](https://blog.voxagon.se/2020/11/18/teardown-quicksave.html), 18 Nov 2020 | First-party | Saving a changed voxel world |
| T5 | Teardown [0.7.2 notes](https://steamcommunity.com/games/1167630/announcements/detail/4072920612519599496), 11 May 2021 | First-party | Disconnected buildings falling; particle rework |
| T6 | Gustafsson and Rundlett, ["Raytracing Voxels in Teardown and Beyond"](https://www.youtube.com/watch?v=IM1Dr98f3xU), GPC 2025; and the 80 Level interview ([W3 in the Teardown note](inspiration-teardown-rt.md)) | First-party talk; press | 10 voxels per metre; commands, buffer cap, no voxel data on the wire. Not re-read here; taken from the existing note, and the interview was re-fetched |
| E1 | Keen Games, Steam posts for Enshrouded (app 1203620): [0.7.0.1](https://steamcommunity.com/games/1203620/announcements/detail/5582843074958482080) (6 Feb 2024), [Hollow Halls](https://steamcommunity.com/games/1203620/announcements/detail/5686430301736314574) (26 Mar 2024), [Melodies of the Mire](https://steamcommunity.com/games/1203620/announcements/detail/5746109972545485693) (5 Jun 2024), [Patch 4](https://steamcommunity.com/games/1203620/announcements/detail/6890025005348315491) (13 Jun 2024), [Souls of the Frozen Frontier](https://steamcommunity.com/games/1203620/announcements/detail/6146943657174062772) (5 Nov 2024), [World Design AMA](https://steamcommunity.com/games/1203620/announcements/detail/1786409468884723) (19 Dec 2024), [Thralls of Twilight](https://steamcommunity.com/games/1203620/announcements/detail/1799088287989513) (13 May 2025), [Patch 9](https://steamcommunity.com/games/1203620/announcements/detail/1801617199390408) (5 Jun 2025), [Wake of the Water](https://steamcommunity.com/games/1203620/announcements/detail/1815580768392038) (10 Nov 2025), [Patch 12](https://steamcommunity.com/games/1203620/announcements/detail/1818752592135385) (16 Dec 2025), [1.0 changelog](https://steamcommunity.com/games/1203620/announcements/detail/1846651706674432) (14 Oct 2026) | First-party | Tools, undo, prediction, permissions, bugs |
| E2 | [enshroudedviki.com/terraforming](https://enshroudedviki.com/terraforming) | **Community** (a fan wiki that cites E1) | Tool costs, the Flame Altar persistence rule, the foundation trick. Every claim from it is marked |
| E3 | Enshrouded [Steam store page](https://store.steampowered.com/app/1203620/Enshrouded/) | First-party | "fully terraformable land", co-op up to 16 |
| L1 | Tooley1998 (the solo developer), Steam posts for Lay of the Land (app 2776090): [Launch and What's Coming Next](https://steamcommunity.com/games/2776090/announcements/detail/1829528821311450) (11 Apr 2026), patch notes [1.0.5](https://steamcommunity.com/games/2776090/announcements/detail/1829528821305585), [1.0.6](https://steamcommunity.com/games/2776090/announcements/detail/1829528821305802), [1.0.8](https://steamcommunity.com/games/2776090/announcements/detail/1829528821309051), [1.0.11](https://steamcommunity.com/games/2776090/announcements/detail/1830163047258918), [1.0.12](https://steamcommunity.com/games/2776090/announcements/detail/1830797770231865), [1.0.16](https://steamcommunity.com/games/2776090/announcements/detail/1830797770244147), [1.0.23](https://steamcommunity.com/games/2776090/announcements/detail/1831432155573038), [Improved Voxel Simulations](https://steamcommunity.com/games/2776090/announcements/detail/1811772772202709) (24 Sep 2025), [Creative Mode](https://steamcommunity.com/games/2776090/announcements/detail/1811772772431939) (1 Oct 2025), [Development Update 1.1.15](https://steamcommunity.com/games/2776090/announcements/detail/1839041357034867) (27 Jul 2026), [1.3.0](https://steamcommunity.com/games/2776090/announcements/detail/1846651706673052) (14 Oct 2026) | First-party | Dig modes, Ghost, snapping tolerance, Creative, simulation cost |
| L2 | Steam [store page](https://store.steampowered.com/app/2776090/Lay_of_the_Land/) | First-party | Terrain sculpting tools, cylinder and cone tools, prefabs, no online multiplayer |
| L3 | Steam [community discussion "Why system requirements so high?"](https://steamcommunity.com/app/2776090/discussions/0/800091475572429083/) | **Community** | The 10 cm voxel figure only. **Unverified** |
| V1 | Cepero, Procedural World: ["Voxel Physics"](http://procworld.blogspot.com/2013/12/voxel-physics.html) (Dec 2013) and ["Destroy the Block"](http://procworld.blogspot.com/2017/03/destroy-block.html) (Mar 2017) | First-party blog | Detachment by flood fill, mass from voxels, 30 cm demo Voxel, "Godzilla" scale |
| V2 | [voxelfarm.com/technology.html](https://www.voxelfarm.com/technology.html) | First-party product page | "Realtime"/"Runtime Voxel Edition" and the Sandbox module exist; no detail |
| V3 | Procedural World's 2013 "Network Update" and "Storage Matters" posts, the May 2017 "Plugin Status" post (comment on clipmapview calls: paint, smooth, grow, forget), and two Voxel Farm forum threads ("real time CSG operations", "Smooth Terrain Sculpting?") | First-party blog and forum | **Read only through a search engine's summaries; the pages were not opened.** Treat as unverified |
| A1 | GamesBeat, [Atomontage 2024 edition](https://gamesbeat.com/atomontage-launches-2024-edition-of-3d-art-virtual-matter-platform/), quoting Tabar and Siles | Press | Claims on edit sync, server simulation. The `archive.atomontage.com` page failed to load (TLS error) |
| J1 | John Lin, ["The Perfect Voxel Engine"](https://voxely.net/blog/the-perfect-voxel-engine/), 18 Sep 2021 | First-party blog | A feature checklist only |
| J2 | Lin's Brush panel, Ghost and hotbar, and his decision to shelve the micro-Voxel goal | First-party, already read | From [inspiration-john-lin-water.md](inspiration-john-lin-water.md) §3–4 and [inspiration-john-lin-crystal-islands.md](inspiration-john-lin-crystal-islands.md); not repeated |
| C1 | Rijsdijk, ["Large-Scale Smooth Voxel Terrain"](https://jorisar.nl/blog/SmoothVoxelTerrain/) | **Community** blog | An Edit as an SDF, an operation and a box |
| X1 | This repo: [`crates/sim/src/edit.rs`](../../crates/sim/src/edit.rs), [`crates/net/src`](../../crates/net/src) (`EditMode`, `Shape`) | Code | Today's limits and what an Edit can be |

## What this engine has today (for the comparison)

- An Edit is one operation of about 24 bytes: a mode (**Dig**, **Place**, **Smooth** or **Flatten**), a shape (**Sphere**, **Cube** or **Cylinder**), a radius, a centre and a Material. Everything is integer maths in a fixed order (ADR 0003, `sim::apply`).
- The limits in `sim/src/edit.rs`: 20 Edits a second, 8 m³ a second (64,000 Voxels at 5 cm), a burst of nine seconds' worth (576,000 Voxels), reach 8 m, a Brush radius of up to 2 m (1 m for a Smooth or a Flatten). A Smooth is a 3×3×3 majority over a snapshot. It costs six times a Dig per Voxel examined.
- Undo (ADR 0010) is the server restoring the bricks an Edit overwrote, sent through the snapshot path, for about 32 Edits a player. A **Piece** (ADR 0011) is one Edit of about 8,000 Voxels (a metre cube), an eighth of a second of the rate.

Arithmetic used below (5 cm Voxel = 1.25 × 10⁻⁴ m³; 8,000 Voxels per m³; 64,000 Voxels a second):

| Edit | Volume | Voxels | Share of one second's budget |
|---|---|---|---|
| Sphere, radius 0.25 m (a 0.5 m pickaxe, read as a diameter) | 0.0654 m³ | 524 | 0.8% |
| Sphere, radius 0.5 m (a 0.5 m pickaxe, read as a radius) | 0.524 m³ | 4,189 | 6.5% |
| Disc, radius 1 m, 0.1 m deep (a wide shallow shovel scoop; one possible shape) | 0.314 m³ | 2,513 | 3.9% |
| A Piece, 1 m cube | 1 m³ | 8,000 | 12.5% |
| Largest Brush today, sphere of radius 2 m | 33.5 m³ | 268,083 | 4.2 s of rate (inside the 9 s burst) |

## Comparison

`—` means no primary source found, not that the feature is absent.

| | Teardown | Enshrouded | Lay of the Land | Voxel Farm | Atomontage | John Lin |
|---|---|---|---|---|---|---|
| **Voxel size** | 10 per metre (T6, existing note) | Not stated by Keen in anything read | "10 cm" (community only, **unverified**) | 30 cm in the 2013 demo (V1); smaller by default in 2017 (V1) | Not stated; "so small that you don't really see them" (A1) | ~1–3 cm in Crystal Islands (inferred, existing note); then shelved |
| **Edit shapes** | `MakeHole(pos, r0, r1, r2)`: a sphere with one radius per Material class; `Explosion` size 0.5–4.0 (T2). Shipped Tools are impact, cutting and blasting, not Brushes (existing note) | Pickaxe; construction hammer removes or puts back terrain and cuts by building-shape outline; rake flattens, raises, cuts ditches (E1, E2) | Tools with a selectable dig mode "normal", "box", "cube" (L1); cylinder and cone building tools (L2) | Paint, smooth, grow, forget calls (V3, unverified) | "Photoshop for 3D" (A1); no shapes given | A carousel of five lumpy Brush shapes with a size hotbar; Ghost outline (existing note) |
| **Smoothing** | — | Rake flattens and raises (E1). "Ghost-voxels" bug fixed Dec 2025 (E1) | Terrain sculpting tools (L2); "raise/lower" | "smooth" call (V3) | — | — |
| **Material painting** | `Paint`, `PaintRGBA` radius 0–5 m, dithered by a probability (T2). Palette-index swap through precomputed tint tables (T3) | — | — | "paint" call (V3) | Per-voxel colour, normals, shaders (A1) | Material tabs in the Brush panel |
| **Rate or throttle** | Shape size caps (T2). No rate documented | Swing cadence; stamina cost per use (rake: 2 stamina, **community**, E2) | Tool durability; "tool breaking … unable to get out" was a reported soft-lock (L1) | "Updating 1000³ voxels at once in under 10 ms isn't realistic" (V3, unverified) | — | — |
| **Network** | Deterministic commands, reliable ordered, fixed-point integers (T1). ~1 Mbit/client for bodies | Client-side prediction of terraforming (May 2025); role permissions (E1) | None online; split-screen up to 4 (L1) | 2013 server tested with ~100 clients (V3, unverified) | Server simulates, sends results (A1, press) | — |
| **Late join** | Replay the command stream; buffer capped; join-in-progress disabled when full (T1) | — | — | — | — | — |
| **Undo** | Not found. Quicksave and quickload (T4) | Yes: building and terraforming undo (E1). Refunds resources; with a full backpack it works but refunds nothing (1.0, E1) | Not found | — | — | — |
| **Far LOD of edits** | Not described (levels are small) | — | LOD fading added Sep 2025; nothing on edits | — | "LODs inherently always stay in sync" (A1) | — |
| **Detach and fall** | "Large buildings fall properly when disconnected" (T5) | — | Voxel physics objects (L1) | Flood fill from supported voxels; mass from density (V1) | Voxel bodies simulated on the server (A1) | — |
| **Cost warnings** | "Massively improved destruction and physics performance" (T5) | Water into cavities; CPU for hosts optimised (E1) | Voxel simulation and world creation are "the two main performance pain points" (L1) | CPU generation, then triangles (V3) | CPU-only servers (A1) | — |

## Findings per engine

### Teardown (Tuxedo Labs)

**Facts, first-party.**

- **The Edit is a shape with a radius per Material class.** `MakeHole(position, r0, [r1], [r2], [silent])` is server only. The radii are in metres. `r0` is for soft Materials (glass, foliage, dirt, wood, plaster, plastic), `r1` for medium (concrete, brick, weak metal), `r2` for hard (hard metal, hard masonry). It returns the number of Voxels cut out, and zero if nothing changed (T2). So the hole is bigger in soft Materials and smaller in hard ones, as one Edit, with no extra time model.
- **Size caps are per shape.** `Explosion(pos, size)` takes a size from 0.5 to 4.0. `Paint` and `PaintRGBA` take a radius of 0.0 to 5.0 and a dithering probability (T2). No rate limit is documented.
- **Painting is a Material change, not a colour per Voxel.** A Voxel stores one byte, an index into a palette of up to 255 Materials. At load, the game precomputes tinted variants of each used Material into spare palette slots, and painting swaps the Voxel's index through a translation table (T3). The cost is palette slots: black over yellow needs variants of variants, and when the palette fills, similar colours are merged. Broken fragments inherit the parent's palette, so palettes do not multiply (T3). This fits our Material-only bricks (ADR 0002) and is what the Teardown note's item L already suggested ("scorched rock" as a Material).
- **Edits replicate as commands.** "Each breakage event is split into a stream of deterministic commands that are replicated on all clients", for example "cut hole in this shape at voxel coord x,y,z". They travel on a reliable ordered stream. Voxel data was ruled out: the 2021 prototype "used enormous amounts of bandwidth and completely choked the connection when large objects were destroyed". Destruction was rewritten in fixed-point integer maths. Hierarchy splits, new objects and joints still involve floating point, so they too are commands (T1). The command size is independent of the size of the object.
- **Late join replays the command log, with a cap.** Three options were weighed: serialise the whole scene (30–50 MB common), send changed objects only, or replay the commands. They chose replay. "We still limit the buffer size and disable join-in-progress when it fills up" (T1). The 80 Level interview says the same.
- **Saving is the whole state, run-length encoded.** 15–20% of original size, about 80 MB for a large level. Physics and acceleration structures are rebuilt on load (T4).
- **Detach is real.** 0.7.2 added "new algorithms [that] also allow large buildings to fall properly when disconnected" (T5).
- **Particles are client-side and separate.** `SpawnParticle` and the `Particle*` functions are called from client code (the API's own examples are in `client.tick`), with `ParticleCollide` and `ParticleSticky` to control collision and friction. `MakeHole` is server only (T2). The docs do not say that break dust is made this way, only that the API splits world edits from particles.

**Not found.** Undo or rewind of an Edit; an Edit rate; how an Edit shows at a distance (levels are small).

**For us.** It is the strongest confirmation of ADR 0003: ops, determinism by integers, reliable ordered, and never voxel data. It differs on late join. The three-radius `MakeHole` is the one new Edit idea here (see option A3).

### Enshrouded (Keen Games)

**Facts, first-party (patch notes and posts).**

- **Terraforming is part of the product.** The store page says "fully terraformable land" and "carve homes into mountainsides" (E3).
- **Tools differ.** From the notes: the pickaxe (1.0 added higher damage in upper tiers; a crouch fix in Dec 2025), the construction hammer (it "puts terrain material back"; a 0.7.0.1 fix says a single terrain voxel placement "could add a larger terrain volume than intended"), and the rake (its recipe unlocked in 0.7.0.1; the radius can be set by hand and the cursor depth shifted to dig ditches and canals since 10 Nov 2025; a fine-tune mode was added then; it works in water since Dec 2025) (E1).
- **Undo exists and is tied to resources.** "Allow undo function while building hammer is selected but building menu is closed" (Mar 2024). In 1.0: "Undo of terraforming now also works when the backpack is completely full. Please note that in this case the resources are not granted back to the backpack" (E1). So an undo that refunds is a design decision with an inventory edge case.
- **Prediction and permissions.** "Improved the client-side prediction for terraforming terrain" (13 May 2025). "Terraforming in the player base" is a role permission (5 Jun 2024). Patch 4 adds a warning when you try to chop wood or terraform without permission. A "Visitor" role that forbids terraforming outside the base arrived 10 Nov 2025 (E1).
- **The rake and the player.** Fixed: "the rake could modify terrain too close to the player, making them fall through the world" (5 Nov 2024). This is the Edit-near-the-character case.
- **A flatten bug class.** Patch 12: "an issue with the rake tool that would create ghost-voxels: areas that appeared empty but still had fragments of voxel information. This could lead to incorrect behavior for further building" (E1). It suggests Enshrouded's terrain keeps more than a Material per Voxel (not stated).
- **Lighting under edits.** Patch 9 fixed "flashing lighting while terraforming" in the dynamic global illumination (E1). Edits invalidating cached light is a live problem there too.
- **Building voxels and terrain voxels are different things.** "Improved the visuals at locations where terrain voxels and building voxels intersect" (Nov 2024). In the Dec 2024 AMA a designer says they tried "voxels that look like building materials but are voxeled the same way as terrain materials (so they can be more organic)" and found them hard to fit into the in-game building workflow (E1). The same split is what ADR 0011 avoids: Pieces are stamped as ordinary Voxels.
- **Cursor preview.** "Improved the cursor preview of the rake tool. Its position should now be more readable, especially in bright areas" (Feb 2025) (E1). A Ghost needs to stay legible in sunlight.
- **World scale.** 10 × 10 km (AMA). The voxel size is not stated in anything read.

**Community only (E2, unverified).** The pickaxe is "the slow method" and returns the dug Material. The rake costs 2 stamina a use and 500 durability (the advanced rake 1 and 1,000). The hammer's "Remove" with a foundation placed part-way underground clears the soil inside the outline "as one clean cube", used for square basements. Terraforming works anywhere but only changes inside a Flame Altar's area are kept (40 m a side at level 1, 160 m at level 4). I could not confirm the Flame Altar rule in Keen's own posts, so it should not be relied on.

**Not found.** An edit-rate figure; how far edits are sent; the voxel size; an interview on the terrain engine (the Eurogamer.de interview returned 403; a search summary says its technical director said water has to flow into dug cavities in real time, which is not verified).

**For us.** It is the nearest Survival precedent: Tools with their own terrain shapes, resource-coupled undo, role permissions (our map puts protection out of scope), and client prediction. Its ghost-voxel and "edit beside the character" bugs are the failures to test for.

### Lay of the Land (Tooley1998, Southern Cross Interactive)

**Facts, first-party.**

- **Tools with dig modes.** The patch notes mention "tool dig mode", "box dig mode" and "cube mode" against a "normal mode". 1.0.5: "Fixed box dig mode not resetting on death". 1.0.8: "Fixed tool dig mode resetting when equipping an empty inventory slot". 1.0.12: "Tools will now edit voxel physics objects when in box or cube mode as if the tool was in normal mode to remove the need to switch back to normal mode when harvesting" (L1). So each Tool has a per-Tool shape mode that the player toggles, and a mode that gets in the way of harvesting is a reported annoyance.
- **Ghost shows what will really change.** 1.0.16: "Building previews no longer render on top of voxels better representing what voxels will actually be placed" (L1). The same idea as our Ghost.
- **Snapping tolerance.** 1.0.23: "Made it easier to place useables, decorations and furniture etc on slightly uneven surfaces by checking if the object can be placed 1 voxel higher if it cannot be placed at the targeted level" (L1). A one-Voxel try-above rule.
- **Shapes and prefabs.** The store page lists cylinder and cone tools and prefabs (windows, fences, slopes), and a rotate-and-flip gizmo came in 2025 (L1, L2).
- **Soft-lock from digging.** The developer's launch post lists "digging a hole, your tool breaking causing you to be unable to get out" as "better ways to get yourself out of soft locks … under investigation" (L1). 1.0.6 "Increased maximum durability of all tools". Tool wear and digging collided for the developer.
- **Yield from Plants.** "Reduced the number of hits with an axe required to remove leaves from a fallen tree"; "tutorial informing the player that they can pick up entire branches of trees" (L1, 1.0.12).
- **Creative.** Announced 1 Oct 2025: "unlimited resources, flying and new world options", including a Flat World type "for those who want to build without having to flatten or raise the terrain". It is Creative as a separate way to play, as in the glossary.
- **Simulation is the cost.** The July 2026 update: "the two main performance pain points" were voxel simulation and world creation; "Voxel simulation performance has also been improved a lot recently … It still is not as good as I'd like it to be". Sand, gravel, snow and water are simulated voxels (L1). Terrain digging itself is not named as a cost.
- **No online network.** Split-screen up to 4 only, Steam Remote Play, "no tethering" (L1, L2).

**Community only.** 10 cm Voxels (L3).

**Not found.** Edit shapes or sizes; throughput; undo; far LOD.

**For us.** Its useful evidence is on Tool modes, a truthful Ghost, a one-Voxel placement tolerance and the digging soft-lock. It says nothing on networking.

### Voxel Farm (Procedural World, Cepero)

**Facts, first-party.**

- **Detachment by flood fill.** "You can think of it as a flood-fill." Voxels reached from supported places like the ground count as held; unreached sections are floating. Mass is the sum of the Voxels' density, and a physics mesh envelope is fed to the solver and re-contoured after a further cut. In the demo the Voxel was 30 cm, and the smallest detachable piece was about a Voxel (V1, Dec 2013). This is the Debris plan (M3) described in 2013, and it needs no object memory.
- **Edit calls.** A comment on the May 2017 plugin post says the Voxel Studio tools map to calls on the clipmap view: paint, smooth, grow and "forget" (V3, unverified summary). "Forget" suggests an Edit that returns a region to the generated state. That is something this engine's diverged-brick rule makes cheap (a brick that equals the generator's output is not diverged).
- **Scale.** The 2017 demo needed rescaling because "Voxel Farm's voxels are much smaller, the default configuration felt closer to a Godzilla/Kaiju simulator" (V1). Fine Voxels change how big a "normal" hole should be.
- **Product.** "Voxel Farm allows you to give control to your users. Let them build and destroy" and "Realtime/Runtime Voxel Edition" (V2). No shapes, speeds or networking on that page.

**From search summaries only (V3, not opened).** A forum answer that smooth sculpting may need custom code and that high-resolution Voxels with sharp features need a terrain layer that "reacts properly to blob-like edits"; a staff reply that updating 1000³ Voxels in under 10 ms is not realistic, with all generation on the CPU and then converted to triangles; a 2013 server test with nearly 100 clients making changes "many times higher than humans would produce".

**Not found.** Edit networking beyond the 2013 test; undo; far LOD.

**For us.** The 2013 flood-fill description supports Debris from Voxels alone. The "forget" idea is worth a Creative-only reset Brush.

### Atomontage

**Facts, press.** "It's like Photoshop for 3D"; "each voxel can contain color, but also normals, shaders, and other physical properties"; "the LODs inherently always stay in sync"; "We don't need a GPU at all on the server side"; "dozens and dozens of clients" in one space (Tabar and Siles, A1). A search summary of another interview says the physics is simulated on the server and results sent to clients; neither was a primary post.

**Not found.** No edit shape, rate, undo or format is given. The archive site did not load. These are product claims without numbers, and are not evidence for any decision.

### John Lin (Voxely)

Already covered in the two existing notes: the Brush panel, a carousel of five lumpy rock-like shapes, a size hotbar and a Ghost drawn as an outline or a translucent solid; Crystal Islands has no Edits at all; and the micro-Voxel goal was shelved on 6 Jun 2021 for "gameplay possibilities and end-user experience". New here: his 2021 checklist ("The Perfect Voxel Engine") names "Creating anything", "Destroying everything", "Converting CSG operations to voxels (AKA a building system)" and "State serialization and synchronization across a network", with no design for any of them (J1). It is a list of intent and not evidence of a mechanism.

### Community technique note (C1)

One community implementation defines an Edit as "an SDF, an operation, and an axis-aligned bounding box around the edit" (union or difference), applied by finding the octree leaves in the box and re-meshing the chunks it touches. Large edits are called slow. It is the same shape as our op (a Brush plus a mode plus an extent) but on a smooth distance field; our bricks hold Materials, so this is a comparison and not a method to take.

## Findings against the ADRs and the spec

**ADR 0003 (deterministic ops).** Teardown's shipped multiplayer is the same design: commands, reliable and ordered, integer maths, no voxel data. Two differences to keep in mind: they cap the log and close late join, and we send edited-brick snapshots instead; and Enshrouded's prediction note shows the client has to predict terraforming to feel right. A Survival Edit needs two server-side outputs that the op alone does not carry: whether it is allowed (Tool, reach, rate, permission) and the **yield**. Teardown's `MakeHole` returning the number of Voxels cut shows the server can read the removed count from its own result. Proposed here, not found in a source: the server derives the yield from the Voxels actually removed, by Material, and sends it to the digging player as an inventory change, not as part of the broadcast op.

**ADR 0007 (Edit rate).** No source documents a volume meter. The practical limits there are caps on shape size, swing cadence, stamina and durability. Today's 64,000 Voxels a second is a ceiling for the server and for cheaters; hand Tools will use a few percent of it (table above). The cost the sources do report is elsewhere: Enshrouded's water and Lay of the Land's voxel simulation (sand, gravel, snow, water), which are Fluids and Debris here (M3, M4), and would fall under "Use the whole machine" with a floor on the low-end profile.

**ADR 0010 (Undo).** Only Enshrouded has a documented undo, and it is coupled to the inventory. ADR 0010 restores bricks, which does not refund anything. In Survival, restoring bricks without touching the inventory would let a player dig, collect, and undo for free; refunding on a Place needs a rule for a full inventory, which Enshrouded had to add in 1.0.

**Spec: Brushes and M7 Tool Brushes.** The spec's pickaxe (small, precise, up to about 0.5 m), shovel (wide and shallow, plus smooth and flatten) and axe (thin planar slab) have the following support. The pickaxe maps onto our Sphere. The shovel's Smooth and Flatten are what Enshrouded's rake does, with a radius and a depth the player sets. The axe's slab has **no source in any engine read**: the nearest is Enshrouded's community-reported trick of using a placed foundation's footprint as the cut. A slab needs an extent per axis, and today's `Shape` carries one radius, so the op's size should be checked against the 24 bytes of ADR 0003 before it is promised.

**Far LOD.** No source. Planet-wide ops (ADR 0003) and per-child LOD composition ([dynamic-content.md](dynamic-content.md)) are this engine's own answer.

## Options for BhayanakVoxel

Judged against the map's decided rules. None of these is a recommendation to decide; each lists what it costs.

### A. Brush shapes for the M7 Tools

Fixed by the spec: the three Tools, each with its own shapes; wrong Tool or bare hands 5–10× slower, never blocked; placing needs no Tool.

| Option | What it is | For | Against | Evidence |
|---|---|---|---|---|
| **A1. One fixed shape set per Tool** (the spec as written) | Pickaxe sphere, shovel disc plus Smooth and Flatten, axe slab. No player-set parameters | Simplest op, easiest to test at 1, 4 and all threads; no mode UI | The slab needs an extent per axis; shovel size is a guess until the dig yield is decided | Spec only |
| **A2. A1 plus a player-set size within the Tool's range** | Radius, and for the shovel depth, set by the player inside a per-Tool maximum (Enshrouded's rake: adjustable radius, shiftable depth, fine-tune) | Matches the shipped precedent; "fine-tune" gives precision on 5 cm Voxels | Needs a size control on the Edit HUD (#151); every size must stay inside the rate bound | E1 |
| **A3. A1 plus a per-Tool mode toggle** (normal, box, cube) | A mode that changes the shape, not only the size | Lets a pickaxe cut square tunnels and a shovel level a floor | The only precedent reports mode-reset bugs and a harvesting annoyance; the spec gives each Tool its own shapes already | L1 |
| **A4. Radius by Material hardness** | One Edit takes a radius for soft, medium and hard Materials, so a pickaxe hole is smaller in hard rock | One Edit, no second time model; gives hard rock its own feel | The spec already times digging by hardness and Tool; two hardness effects must not stack to feel unusable. Needs a hardness class on each Material | T2 |

The options combine; A2 and A4 are independent of each other.

### B. Edit rate in Survival

| Option | What it is | For | Against |
|---|---|---|---|
| **B1. Keep ADR 0007 as the only meter** | Tool speed shows up as dig time (hardness, Tool right or wrong, tier), and volume per Edit is bounded by the Tool's Brush | No change; hand Tools use 1–7% of the second's budget | Nothing stops a player chaining Edits at 20 a second within the budget, so time per Voxel (not volume) has to be what a Survival player waits on |
| **B2. A per-Tool cadence or stamina** | A swing takes time and optionally stamina (Enshrouded: stamina per use, **community**) | Familiar feel; self-limiting | Stamina belongs to the survival-needs ground that #275 puts out of scope |
| **B3. A lower per-player Survival budget than Creative** | Survival uses a share of the Creative figure | Protects a server with many diggers (the soak, #169, measures it) | A second set of limits to test at the low-end profile and the high end; no source supports a number |

### C. Edit yield and networking

| Option | What it is | For | Against |
|---|---|---|---|
| **C1. Server derives the yield from its own result** (proposed here) | The op broadcast stays as in ADR 0003; the server counts removed Voxels by Material and sends an inventory change to the digger | No change to the op; Teardown's `MakeHole` returning a count is the same idea | The Edit result has to be returned out of `apply`, and it must stay deterministic and cheap |
| **C2. Client-predicted yield, confirmed by the server** | The digger sees the count at once | Feel, as with Enshrouded's prediction work | A mispredicted count must be corrected without flicker; more protocol |

### D. Undo in Survival

| Option | What it is | For | Against |
|---|---|---|---|
| **D1. Creative only** | Undo stays as ADR 0010 | No inventory edge case; no precedent either way | Survival players lose forgiveness |
| **D2. Survival undo limited to Places, with a refund** | Undo of a Place refunds the Material; a Dig cannot be undone | Closest to Enshrouded; no free duplication from a Dig | Needs a full-inventory rule (Enshrouded: undo works, nothing is refunded) |
| **D3. Survival undo of both, with the inventory rolled back** | The undo also reverses the inventory change | Symmetric and simple to say | Interacts with other players taking the dropped items; ADR 0010 already clips what others changed |

### E. Smoothing and Material painting

- **Smoothing.** Today's Smooth is a 3×3×3 majority that never changes a Material, and a Flatten is against the tangent plane. Both are in the shovel's set already. Enshrouded's ghost-voxel bug and the rake-beside-the-player bug are the cases to test here: the Edit at a brick edge and at a LOD seam, and an Edit whose Brush contains the character (the leash and Ground already exist). Cost: more tests, no new design.
- **Material painting.** The sources point one way: a paint Edit swaps a Voxel's Material for a variant Material (Teardown), which fits ADR 0002. It costs Material slots for each tint (Teardown ran out). The spec defers scorch and paint marks, so this is a pointer for later.

### F. Feel on 5 cm Voxels

| Technique | Evidence | Cost or risk for us |
|---|---|---|
| **Truthful Ghost**: show exactly the Voxels an Edit will change | Lay of the Land 1.0.16; the glossary's **Ghost** already says it | Legibility in bright light (Enshrouded Feb 2025); large Brushes need an outline instead (the Elements note, item G) |
| **One-Voxel placement tolerance** for a Piece or a placed item | Lay of the Land 1.0.23 | ADR 0011 already specifies snapping to faces; this is a tolerance on top, not a second system |
| **Cosmetic client-only particles for an Edit**, separate from Debris | Teardown's API splits the server edit from client particles; Elements note, item E | Keeps cost off the server and the wire; no first-party source for the look (size, count, colour from the dug Material). Needs the Debris-versus-particle line drawn in M3 |
| **A cut shape from a Piece footprint** (a "clean" cut) | Enshrouded foundation trick, **community only** | Not a Tool shape; a possible Creative aid, unverified |
| **An exclusion near the character** | Enshrouded Nov 2024 | Cheap; the Edit reach and Ground already exist |
| **Tool wear must never trap a player** | Lay of the Land's launch post | This is why the owner's "Tool wear and repair" decision (still open in #275) has to include a way out of a hole |

### G. What this research does not settle

- **The edit-per-second budget in Survival and any number for the pickaxe's size** need the owner's dig-yield and feel decisions (#275's not-yet-specified prototype).
- **Far-LOD display of Edits** has no outside evidence; it rests on this engine's own design and measurements.
- **Chunky particle look** and **snap tolerance values** have no primary numbers.

## What was not verified

- Enshrouded's voxel size, and the Flame Altar rule (community wiki only).
- Lay of the Land's 10 cm Voxel (community only).
- Everything marked V3 (Voxel Farm forum and the 2013 posts) and the Atomontage archive site: not opened. The Teardown talk (T6) was not re-watched for this note; its figures come from the existing Teardown note.
- Every engine's real rate of editing: none is published.
