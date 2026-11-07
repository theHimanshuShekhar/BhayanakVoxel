# Research: 4J Studios' Elements Engine reveal

Resolves issue [#131](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/131), part of the inspirations map [#130](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/130). Researched November 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md).

## TL;DR

- **What it is.** The Elements Engine is 4J Studios' in-house voxel sandbox engine. 4J is the Dundee studio that ported Minecraft to consoles from 2012 to 2019. The engine was revealed on 22 May 2024 with a 74-second trailer of "real-time engine footage with example builds" (S1). Its first and only game is **Reforj**, a survival/creative sandbox announced in September 2024. Reforj has been in pre-alpha on Xbox Series X|S since April 2025, a PC closed alpha is due in Q1 2027, and PS5 is planned ([4J news](https://4jstudios.com/news/); [Wikipedia: Reforj](https://en.wikipedia.org/wiki/Reforj)). The engine is "in-house only for now" (S2 [0:28]).
- **Its design rules are about frame rate and input, not looks.** As told to GamesIndustry.biz, CEO Paddy Burns set two rules in 2019: run at 60 fps "no matter what ridiculous levels of destruction were happening on screen", and design around a controller, running natively on current consoles ([GI.biz, 15 Jul 2026](https://www.gamesindustry.biz/we-thought-well-how-would-we-reinvent-this-wheel-minecraft-specialists-4j-studios-on-building-a-new-sandbox-crafting-game-from-first-principles)).
- **Its answer to blockiness is shape, not resolution.** Its voxels ("Elements") are about Minecraft-sized (each Reforj world is 4 km across, reportedly 4096 blocks). Each Element can take one of **660 shapes**: slopes, stairs, wedges and rounded corners. On terrain the shape is picked automatically from the neighbours, so mountains read as faceted low-poly rather than stair-stepped [0:30], [0:36]. BhayanakVoxel answers the same problem the other way: **Voxels** 10× smaller per axis.
- **Rendering is almost certainly rasterised meshes, not ray tracing (inferred, high confidence).** Shaped Elements carry 512×512 textures with normal detail. 4J later replaced those textures with patented vector "Rixels" ([GI.biz, 11 Aug 2026](https://www.gamesindustry.biz/reforj-developer-4j-studios-unveils-rixels-modified-pixels-that-reduce-texture-memory-requirements-by-98)). The footage also shows a directional sun, fog, fly-through clouds, transparent refracting water, and lava lighting its cave. Nothing first-party mentions ray tracing or GI. Bloom only arrived in July 2026, as "the first of several planned post-processing effects" ([4J, Jul 2026](https://4jstudios.com/4j-studios-debuts-pioneering-new-graphics-technology-for-upcoming-title-reforj/)).
- **Features shown:** large-scale terrain shaping [0:32–0:33], copy & paste of regions, per-Element shape sculpting, destructible terrain with explosions, a **revert tool** that undoes destruction, and spline camera tools. Most of these come from 4J's own follow-up clips (S2, S3). The most striking moment is a mountain top bursting into thousands of tumbling Elements, each apparently its own rigid body [0:21–0:23].
- **Worth taking (recommendations):**
  1. A **revert/undo of recent Edits**, cheap because the `sim` crate already keeps an `Undo` per Edit.
  2. A **long-session frame-time soak** with an on-screen fps + uptime overlay, as in 4J's "consistently smooth" clip.
  3. **Fly-through volumetric clouds**, plus the trailer's strong aerial perspective and biome palettes, as look-dev references for M1b.
  4. **Cosmetic rubble** on big Edits, separate from simulated **Debris**.
  5. A **copy & paste stamp** and an **outline-style Ghost for large Brushes**, both deferred.
  Reject the 660-shape vocabulary and authored decorative meshes: both conflict with ADR 0002's all-voxel bricks.

## Sources and how they were used

| Key | Source | Type | Used for |
|---|---|---|---|
| **S1** | 4J Studios, ["Introducing... the Elements Engine"](https://www.youtube.com/watch?v=1wa7rTm56bw), YouTube, 22 May 2024, 1:14, chapters Introduction / Engine capabilities / Element engine / Call to action | First-party video | 37 frames at 2 s intervals, 21 more at odd seconds 13–53, 42 at 2 fps for 0:13–0:34, 16 at 4 fps for 0:30.5–0:34.5; English captions (en-GB) and description |
| **S2** | 4J Studios, ["Elements Engine FAQ"](https://www.youtube.com/watch?v=dbyMke2sHT4), 5 Jul 2024, 1:49, CTO Richard Reavy | First-party video | Auto-captions plus 18 frames (on-screen questions, tool footage) |
| **S3** | 4J Studios, ["Elements Engine: Tools & Features"](https://www.youtube.com/watch?v=Rw-v4VARrUc), 22 Aug 2024, 0:36, no speech | First-party video | 24 frames; the on-screen captions name each tool |
| **S4** | 4J Studios, ["Elements Engine: Consistently Smooth Experience"](https://www.youtube.com/watch?v=40ObfvzoSHM), 18 Jul 2024, 0:16 | First-party video | 8 frames (fps overlay) |
| W1 | 4J Studios [news index](https://4jstudios.com/news/) and posts ([reveal](https://4jstudios.com/introducing-the-elements-engine/), [Rixels update](https://4jstudios.com/4j-studios-debuts-pioneering-new-graphics-technology-for-upcoming-title-reforj/)) | First-party | Purpose, dates, bloom/Rixels |
| W2 | Lewis Packwood, ["We thought, well, how would we reinvent this wheel?"](https://www.gamesindustry.biz/we-thought-well-how-would-we-reinvent-this-wheel-minecraft-specialists-4j-studios-on-building-a-new-sandbox-crafting-game-from-first-principles), GamesIndustry.biz, 15 Jul 2026 | Interview (creative director Brian Gomez) | Design rules, 660 shapes, world size, sculpting |
| W3 | Sophie McEvoy, ["Reforj developer 4J Studios unveils Rixels"](https://www.gamesindustry.biz/reforj-developer-4j-studios-unveils-rixels-modified-pixels-that-reduce-texture-memory-requirements-by-98), GamesIndustry.biz, 11 Aug 2026 | Interview (chairman Chris van der Kuyl) | Texture pipeline, 60 fps intent |
| W4 | [Scottish Games Network, 30 May 2024](https://scottishgames.net/2024/05/30/4j-studios-teases-new-voxel-based-elements-engine/); [PCGamesN, Jul 2024](https://www.pcgamesn.com/elements-engine/in-development) | Secondary | Dundee build, reception |
| W5 | [Wikipedia: Reforj](https://en.wikipedia.org/wiki/Reforj), [Wikipedia: 4J Studios](https://en.wikipedia.org/wiki/4J_Studios), [Reforj wiki](https://reforj.wiki.gg/wiki/Reforj) | Tertiary | Release status; game mechanics (marked where the wiki gives no source) |

Timestamps like `[0:21]` are S1 unless prefixed (`[S2 1:05]`, `[S3 0:13]`, `[S4 0:06]`). S1's captions are a voiceover of rhetorical questions with no technical content. Every technical statement below comes from S2–S4 or W1–W3, or is inferred from frames and labelled that way.

## 1. What the engine is (first-party)

- **Stated purpose.** "A proprietary voxel sandbox engine that harnesses the incredible power of the newest generation of consoles and graphics cards", and "a first look at the powerful technology that will be used to create 4J Studios' next game" (S1 description; [W1 reveal post](https://4jstudios.com/introducing-the-elements-engine/)). The voiceover promises worlds "explored at blistering speeds" [0:27–0:31] where "players can reach incredible highs and mine the deepest depths" [0:31–0:37], made "of more than just blocks" [0:37–0:45].
- **Why build it.** "We wanted to have full control of the engine so that we can make it do exactly what we wanted it to do … and also so that we could target the hardware that we want the game to run on" (Reavy, [S2 0:40–0:54]). Gomez places the start in 2019, when Microsoft was bringing Minecraft work in-house. The engine was "stable enough … to begin work on designing a game" around 2023/24 (W2).
- **Platforms.** "Xbox Series X and S, PlayStation 5 and PC" [S2 1:20–1:34]. Reforj pre-alpha launched on Xbox Series X|S on 30 Apr 2025, opened via Xbox Insider on 11 Feb 2026, and has a PC closed alpha due Q1 2027 (W5, citing PCGamesN, Xbox Wire and GI.biz).
- **The game.** Reforj: survival/creative sandbox; worlds are "fully destructible, four kilometre by four kilometre planetoid[s] that loop around", linked by portals. GI.biz says the size limit "helps to keep the performance high" (W2, journalist's paraphrase of Gomez).
- **The footage is example builds, not a game.** Every gameplay shot carries the caption "REAL-TIME ENGINE FOOTAGE WITH EXAMPLE BUILDS" [0:14–0:52]. The city is the studio's home town: "we use several engine based tools to assist with editing Dundee" [S2 0:56–1:01]. That matches the V&A Dundee's stepped façade and RRS Discovery's masts at [0:48–0:51] and [S3 0:25–0:28]. The castle, sphinx and pyramids are further example builds.

## 2. Techniques

Each item is labelled **Stated** (a first-party source says it) or **Inferred** (judged from frames), with a confidence.

### Rendering

- **Rasterised, textured meshes of shaped Elements.** *Inferred, high.* Evidence:
  - Surfaces are flat polygons with crisp edges, bevels and texture/normal detail: sand ripples, scale-pattern stone and rounded sphinx paws [0:36–0:37].
  - The final image is free of noise even in fast motion.
  - The first-party texture story is a raster one: "We were using 512x512 normal textures, so a pixel grid with square pixels in it" (van der Kuyl, W3), later replaced by vector "Rixels" with 23 shapes, 72 KB instead of 4 MB per grid (W3).
  - No first-party source mentions ray tracing, path tracing or GI, and a targeted search found none.
- **Sun lighting and shadows.**
  - *Inferred, medium:* one directional sun with soft-edged cast shadows, for example the sphinx's shadow on the sand [0:38–0:41]. A shadow-map technique is likely; frames cannot tell which.
  - *Inferred, low:* bounce light. Shadowed faces in the city read flat and grey [0:42–0:47], unlike the coloured bounce in a path-traced image.
- **Emissives.** *Inferred, medium.* Lava visibly lights the cave walls orange, and pink crystals and glowing mushrooms read as self-lit [0:34–0:35]. The scene sits in coloured volumetric-looking haze [0:34]. That the lava's light reaches nearby surfaces suggests local lights or a light-propagation scheme, not just unlit emissive textures.
- **Sky, atmosphere, fog.**
  - *Inferred, high:* a scattering sky with a pink/violet sunrise band and a bright sun disc [0:14–0:17], [0:48–0:51].
  - *Inferred, high:* strong distance (aerial-perspective) fog that turns far ridges blue-white [0:30], [0:32].
  - *Inferred, high:* a separate underwater fog in deep teal [0:18–0:20].
- **Clouds.** *Inferred, medium-high: volumetric.* The opening shot flies the camera *through* a cloud mass, with the sun glowing through it [0:14–0:15]. Elsewhere clouds are puffy cumulus with lit tops and darker bases, and cast no visible shadows [0:21], [0:26].
- **Water.** *Inferred, high.*
  - Transparent with depth-tinted colour: the sea floor is visible through the harbour [0:44–0:47].
  - Normal-mapped waves and a bright sun glint [S3 0:28]; a split over/under-water view with a refracting surface [0:17.5].
  - Foamy white waterfalls [0:28]. Nothing shows the water *flowing* as a simulation, so treat waterfalls as authored or static (low confidence either way).
- **Post-processing.**
  - *Stated:* bloom was added in the Reforj "Rixels Update" in July 2026 as "the first of several planned post-processing effects" (W1). By implication the 2024 reveal had no bloom.
  - *Inferred, medium:* no depth of field or motion blur in the reveal, and gentle grading.
  - Unknown: the anti-aliasing method. Edges are clean at 1080p.
- **Upscaling, denoising.** Nothing stated. Nothing to denoise if the renderer is rasterised (see above).

### Data structures and LOD

- **The Element: a voxel with a shape.**
  - *Stated:* "Ours has got 660 different shapes" (Gomez, W2). Players build with cubes and then "pick a staircase shape … point the cursor at that block, and then they can transform it" (W2). Engine tools let "us … sculpt shapes so that we can support more than just cubic voxels" (Reavy, [S2 1:12–1:18]; the auto-captions read "ships"/"boxels", but the footage at [S2 1:12] shows a curved tube sculpted from shaped Elements).
  - *Reported, unsourced (W5 wiki):* terrain Elements "take different sculpting shapes depending on the shape of the local terrain".
  - *Inferred, high:* this matches the faceted mountains [0:30], [S3 0:03].
- **Element size ≈ 1 m.** *Inferred, medium.* The world is 4 km across (W2) and "4096 blocks in width" (W5 wiki, unsourced). Castle walls, trees and people-scale doors also read at Minecraft scale [0:26].
- **Thin parts.** *Inferred, medium.* Lamp posts, railings and masts are far thinner than one Element [0:42–0:51], so some shapes are sub-Element profiles (posts, panes).
- **Decorations are separate meshes.** *Inferred, medium-high.* Kelp, coral, crabs, birds and crystals are smooth, curved, animated and un-voxel-like [0:18–0:20], [0:26], [0:34]. They are authored meshes or sprites, not Elements.
- **World topology.**
  - *Stated (journalist, W2):* a finite 4 km × 4 km planetoid that "loops around".
  - *Wiki, unsourced:* "each side of the world joins back up to the other side … to effectively become a sphere".
  - *Inferred, medium:* a wrapping flat map (a torus), not a geometric sphere. There is no radial gravity or visible curvature. The dark curved band in the opening shot [0:14–0:17] is more likely a cloud/fog volume edge than planet curvature (low confidence).
- **LOD.** Nothing stated. Far forests stay detailed out to the fog [0:30], [0:32]; how is unknown.

### Simulation

- **Destruction and physics.**
  - *Stated:* "destructible terrain" [S3 0:13] and explosions [S3 0:15]; W5/Wikipedia cite "advanced physics simulations" (secondary).
  - *Stated:* GI.biz saw a demo where "exploding a mountain with a vast column of Boomblocks didn't cause the frame rate to blink" (W2, journalist observation).
  - *Inferred, medium-high:* detached Elements become **individual** tumbling rigid bodies (cubes, wedges and pyramids, thousands at once) rather than connected chunks [0:21–0:23], [S3 0:12].
  - Unknown: whether they settle back into terrain, despawn or persist.
- **Revert.** *Stated:* "Revert tool" [S3 0:16]. The frame shows a crater and debris field on a snow peak being restored (pink-tinted region). *Inferred:* the engine keeps per-Edit history or a regenerable base world to restore from.
- **Fluids.** Nothing stated or shown as simulated (see Water above).
- **Creatures.** Crabs walk the reef floor [0:18–0:19] and birds fly near the castle [0:26]. Simple ambient life.

### Streaming and performance

- **Frame rate is the headline claim.**
  - *Stated:* the 60-fps-no-matter-what rule (W2).
  - *Stated:* "could we build an engine that gave you persistent 60-frames-per-second in this kind of game? … for a player it would transform their experience" (van der Kuyl, W3).
  - *Shown:* S4 overlays "60 FPS" with a session timer reading 2 h 21 m 44 s [S4 0:06] and 3 h 59 m 32 s [S4 0:10] while flying over terrain and Dundee. That is long-session stability, though on unstated hardware.
- **Memory.** *Stated:* Rixels cut texture storage by 98.2% ([W1 Rixels post](https://4jstudios.com/4j-studios-debuts-pioneering-new-graphics-technology-for-upcoming-title-reforj/); W3). The saved memory is said to help "avoid performance dips" (W3).
- **Streaming.**
  - *Stated (VO):* "explored at blistering speeds" [0:27–0:31].
  - *Inferred, low-medium:* a finite 4 km world can be largely resident, which sidesteps the streaming problem BhayanakVoxel faces at 16 km radius with Zoom to orbit.
  - *Secondary:* "faster loading times" (Wikipedia citing savingcontent.com).
- **Hardware for the footage:** not stated. Platforms are current consoles and PC (S2).

## 3. Art style

Judged from the S1 frames unless noted.

- **Voxel scale and how it reads.** Element-scale (~1 m) geometry with bevels and slopes gives a **faceted, low-poly, toy-like** read: chunky but never stair-stepped on slopes [0:30], [0:32]. Builds read like Minecraft scale with smoother silhouettes: castle crenellations [0:26], sphinx [0:38–0:41], pitched roofs [0:42–0:45]. PCGamesN called it "Minecraft but better"; the Scottish Games Network said it shows world building "without the distinctive Minecraft retro 'block' aesthetic" (W4).
- **Palette.** Saturated but not neon:
  - Grass greens with rust-brown dirt patches, cool grey faceted rock and pure white snow [0:30–0:32].
  - Ochre sand with terracotta and cream strata in the desert [0:38–0:41].
  - Muted greys with teal accents in the city [0:42–0:47].
  - Deep teal underwater with pink and violet coral [0:18–0:20].
  - Saturated orange lava, magenta crystals and acid-yellow fungus in caves [0:34–0:35].
- **Material looks.**
  - *Rock:* faceted, flat-shaded planes with light texture.
  - *Snow:* soft and pillowy, with rounded tops [0:21], [S3 0:18].
  - *Sand:* fine wind-ripple normal detail [0:36].
  - *Grass:* painted tufts [S2 1:12].
  - *Water:* glassy teal with sun glints and foam.
  - *Foliage:* stylised trees in two families, tiered disc pines [0:28], [S4 0:06] and blobby canopies built of chunky leaf clusters [S4 0:08].
  - *Emissives:* lava, crystals and mushrooms in caves.
  - No glass is shown.
- **Lighting mood and time of day.** Mostly bright, clear midday [0:26–0:41]. Bookended by a pink sunrise through cloud [0:14–0:17] and a lilac dusk over the waterfront [0:48–0:51]. Underwater and cave scenes go moody and dark.
- **Atmosphere and fog.** Heavy aerial perspective: distance turns blue-white within a few hundred metres [0:30], [0:32]. Low cloud wraps the peaks [0:32–0:33]. Volumetric-looking cumulus fills every sky.
- **Post-processing.** Clean image, no visible bloom (bloom came in 2026, W1), no DoF, gentle grading. The logo cards use grain and vignette [0:56–1:04], but gameplay does not.
- **UI and HUD.** There is no game HUD in the reveal. What appears is editor UI:
  - Spline camera paths: white curves, green and red nodes, a cyan look-at line, a blue axis gizmo [0:24–0:25], [S3 0:27].
  - A **yellow-and-black hazard-striped outline** draped over the terrain for the shaping tool [0:32–0:33].
  - A **red translucent box** for copy/paste regions [S3 0:09].
  - A small white circle as the sculpt cursor [S2 1:12].
  - A green "60 FPS" + uptime chip [S4 0:06].
  - The brand mark is an "E" made of square, quarter-round and triangular tiles [0:52–0:54]. It is the shape vocabulary as a logo.

## 4. Controls

- **Camera in the reveal.** Every gameplay shot is a free-flying or spline-driven camera. There is no character on screen. Moves are fast swoops: a dive into the sea [0:17–0:18], an orbit of the castle along a visible spline [0:24–0:25], a low skim over the desert [0:36–0:41].
- **Edits driven by a mouse cursor (editor build).** A mouse pointer is visible while the terrain-shaping outline is drawn and applied [0:32–0:33]. The outline grows as a stroke, then closes into a ring. One frame later the enclosed ground has risen into a cliff tens of metres high (between 32.75 s and 33.0 s). *Inferred, medium:* a freehand lasso or area Brush with a raise operation, at landscape scale.
- **Sculpting.** *Stated:* point at an Element and pick a shape (W2). Joe Garrett ("Stampy") "was very much at the core of getting the sculpting to feel right" (Gomez, W2).
- **Controller-first.** *Stated:* designed "around a controller rather than a mouse" (W2). The reveal footage itself does not show controller input.
- **In Reforj (game, not engine).** Per the Reforj wiki (unsourced): first- and third-person views, gliding with a wingsuit, and free flight in creative mode.
- **Input feel:** not judgeable from trailer footage beyond the camera being smooth with no visible hitches.

## 5. Features, and what is demo versus shipped

| Feature | Evidence | Status |
|---|---|---|
| Terrain shaping (area raise/sculpt) | [0:32–0:33], [S3 0:03–0:04], "terrain sculpting tools" [S2 1:09] | Engine tool used to build the demo worlds |
| Copy & paste regions | [S3 0:07–0:09]; "cut and paste regions, which we use for … place buildings into the world" [S2 1:01–1:09] | Engine tool |
| Shape sculpting of Elements | [S2 1:12–1:18]; 660 shapes (W2) | Shipped in Reforj pre-alpha (W2) |
| Destructible terrain, explosions | [0:21–0:23], [S3 0:12–0:15]; Boomblocks demo (W2) | Shipped in Reforj ("fully destructible" worlds, W2) |
| Revert tool | [S3 0:16] | Engine tool; not known in the game |
| Camera spline tools | [0:24–0:25], [S3 0:24–0:28] | Engine/dev tool |
| Biomes: temperate forest, snowy peaks, desert, ocean and reef, lava caves | [0:14–0:41] | Demo builds; Reforj has procedural biomes and caves (W5 wiki) |
| Ambient creatures (crabs, birds) | [0:18], [0:26] | Demo |
| Real places recreated (Dundee waterfront) | [0:42–0:51]; S2 | Demo only |
| Multiplayer | Reforj: peer-to-peer, up to 4 per world (W5 wiki, unsourced) | Game; not shown in the reveal |
| Scale | 4 km wrapping planetoids linked by portals (W2) | Game |
| Fluid simulation | Not shown or stated | Unknown |

## 6. Cross-check against what is already decided

Sources: the [vertical-slice map #1](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/1) Decisions so far (#7–#20), ADRs 0001–0004 and 0006 in [`docs/adr/`](../adr/), and [`CONTEXT.md`](../../CONTEXT.md).

| # | Finding | Dimension | Verdict | Against |
|---|---|---|---|---|
| 1 | ~1 m Elements with 660 shapes as the cure for blockiness | Art / technique | **Conflicts** | #8 (10 cm **Voxels**), ADR 0002 (occupancy mask + Material only; no per-Voxel extra data) |
| 2 | Rasterised textured meshes, no ray tracing | Technique | **Conflicts** | #10 (path-traced lighting, our own denoiser, TAAU), ADR 0001 |
| 3 | Finite 4 km world that wraps, no radial gravity | Technique | **Conflicts** | #8 (~16 km-radius spherical **Planet**, radial gravity), CONTEXT **Planet** |
| 4 | Decorative props as authored meshes (kelp, crabs, crystals) | Art | **Conflicts** | ADR 0002 (all-voxel, no mesh fallback); #16 (voxel humanoid avatar) |
| 5 | 60 fps as a hard rule, even under heavy destruction | Technique | **Already decided** | #20 (M1 60 fps on 4090, M5 1080p60 on 3060); #13 `needs-benchmark` gate |
| 6 | Long-session soak with an on-screen fps + uptime overlay [S4] | Technique | **New** | #13 bench times passes on a scripted flight; no multi-hour stability run |
| 7 | Volumetric clouds the camera flies through | Art / technique | **New** | #10 has volumetrics and a Hillaire atmosphere; clouds are not specified |
| 8 | Strong aerial perspective and per-biome palettes (green/grey/white highlands, ochre desert, teal sea, lava-orange caves) | Art | **New** (palette direction) / already decided (atmosphere) | #10 atmosphere; #14 biomes; no art direction recorded yet |
| 9 | Lava, crystal and fungus emissives lighting caves | Art | **Already decided** | #10 (emissives in M1), #14 (crystal caves, glowing flora, molten core), #15 (light Materials) |
| 10 | Transparent refracting water with sun glints and underwater fog | Art / technique | **Already decided** | #10 (glass, glossy reflections), #19 (path-traced water) |
| 11 | Waterfalls with foam | Art / feature | **New** | #14 rivers static until Fluids; #19 Fluids are M4 |
| 12 | Reef flora and fauna (kelp, coral, crabs) | Feature | **New** | #14 covers land vegetation; nothing underwater |
| 13 | Each detached Element tumbles as its own rigid body, thousands at once | Feature / technique | **Conflicts** (as simulation) / **New** (as cosmetic) | #18 (connected-group **Debris** via flood fill, ≤ ~1,000 live, server-simulated) |
| 14 | Explosions as an Edit source | Feature | **New** | #15 Edit tools are dig/place/smooth/flatten Brushes; gameplay loop is out of scope on #1 |
| 15 | Revert tool restoring destroyed terrain | Feature | **New** | ADR 0003 (Edits are ordered ops), #17 (recent Edit log), `sim::Undo` per Edit (used for prediction rollback) |
| 16 | Copy & paste of regions | Feature | **New**, partly **conflicts** | ADR 0003 (~24 B ops: a paste payload is far larger) |
| 17 | Landscape-scale terrain shaping (tens of metres per stroke) | Feature / controls | **Conflicts** for players | #15 (Brushes ≤ ~4 m, ~8 m³/s, 8 m reach) |
| 18 | Outline-only area preview (hazard-stripe ring draped on terrain) | Controls / art | **New** variant | CONTEXT **Ghost** (shows exactly the Voxels the Edit would change), #15 |
| 19 | Spline camera tool with editable keyframes | Controls | **Already decided** (bench) / **New** (interactive) | #13 bench; `bins/client/src/bench/path.rs` keyframed path; #16 cinematic Zoom key |
| 20 | Controller-first design | Controls | **New** | #16 drives Zoom by mouse scroll plus a key; no gamepad decision; console targets are out of scope on #1 |
| 21 | Free fly at high speed | Controls | **Already decided** | #16 fly 10–50 m/s |
| 22 | First/third-person switch, wingsuit (Reforj) | Controls | **Already decided** (third person) / **New** (glide) | #16 over-shoulder third person |
| 23 | Texture-memory cut via vector Rixels | Technique | **Conflicts** (does not apply to us) | #9 / ADR 0002: Materials have no textures, no per-Voxel colour |
| 24 | Peer-to-peer, 4 players (Reforj, unsourced) | Feature | **Conflicts** | #11 / ADR 0003 (authoritative server, dozens of players) |
| 25 | Bloom as a later post effect | Art | **New** | #10 lists TAAU and denoiser, not bloom/grading |

## 7. Candidates to adopt

Costs assume a beginner developer with AI agents writing the code: **Low** = a ticket of days, **Medium** = one to two weeks, **High** = several weeks or a spike first.

| # | Candidate | Dimension | Rough cost | Recommendation |
|---|---|---|---|---|
| A | **Revert / undo of a player's recent Edits** (creative tool). The `sim` crate already returns an `Undo` with the overwritten bricks for each Edit, and #17 keeps a recent Edit log. A revert is a new Edit op that restores them, so it replicates under ADR 0003. | Feature | Low–Medium | **Adopt**, after M1a. Cheap given what exists, and makes Edit-heavy testing and play forgiving. Needs a rule for reverting over someone else's later Edit (reject or clip). |
| B | **Long-session soak**: run the client for hours on the 4090 with an fps + uptime chip, and record worst frame and memory over time. | Technique | Low | **Adopt** as a bench mode. 4J's whole pitch is *sustained* 60 fps; our bench is a short scripted flight, so leaks and slow drifts would go unseen. |
| C | **Volumetric cumulus clouds** the camera can fly through, lit by the same sun and atmosphere. They have to work from ground level out to orbital **Zoom**. | Art / technique | High (needs a GPU-time spike against the 8.6 ms budget) | **Defer to M1b look-dev**. Strongest single "gorgeous" cue in the trailer. Measure cost before committing. |
| D | **Look-dev references from the trailer**: heavy aerial perspective, distinct per-biome palettes, sunrise/dusk grading, pillowy snow vs faceted rock. Principles only, no assets. | Art | Low (records in the art direction) | **Adopt as input** to the map's look-dev question. Pairs with the John Lin references. |
| E | **Cosmetic rubble**: short-lived, client-only, unreplicated voxel particles spawned by big Edits and explosions. Kept separate from simulated **Debris** (#18), so "thousands of tumbling bits" costs nothing on the server or network. | Feature / art | Medium | **Defer to M3** (Debris). Decide then whether rubble is particles or tiny Debris. |
| F | **Copy & paste stamp**: copy a brick-aligned region and paste it as one Edit whose payload is a stored blob, sent once and referenced by hash. | Feature | Medium–High (protocol, persistence, Edit limits) | **Defer** past M2. Valuable for building, but bends ADR 0003's tiny-op model. |
| G | **Outline Ghost for large Brushes**: when a Brush is wider than a few metres, draw the boundary draped on the surface instead of a full volume preview. | Controls | Low–Medium | **Defer** until Brushes over ~4 m exist (they don't under #15). Note it in the Edit-tool UX. |
| H | **Interactive spline camera** for cinematics and trailer capture, built on the bench's keyframe `CameraPath`. | Controls | Low–Medium | **Defer**. Nice for showing off M1b; the bench already covers reproducibility. |
| I | **Gamepad support** | Controls | Medium | **Defer**. Not needed for PC/4090 M1; becomes fog if consoles or Steam Deck ever enter scope. |
| J | **Bloom + colour grading pass** | Art | Low–Medium | **Fold into M1b** look-dev alongside C/D (it is cheap after TAAU). |
| K | **660-shape Element vocabulary** | Art / technique | High | **Reject**. 10 cm Voxels already give smooth-reading slopes. Shapes need a per-Voxel shape ID (breaks ADR 0002's Material-only bricks) and per-shape ray intersection in the tracer. |
| L | **Authored mesh props** (kelp, creatures, crystals) | Art | Medium | **Reject for now**. Conflicts with the all-voxel Planet and the tracer's single structure. Build reef flora from Voxels in the generator if wanted (#12 in the table: new, defer). |

## Open questions this research raises

- **Volumetric clouds' fit with Zoom and the budget.** Clouds are the trailer's biggest look win, but they must look right from the ground, inside them, and from orbit. Their GPU cost is unknown.
- **Undo semantics in multiplayer.** Can a player revert only their own Edits? What happens when a later Edit by someone else overlaps the region?
- **Cosmetic rubble versus Debris.** Where is the line between visual-only voxel particles and simulated Debris, and does the client need its own particle system before M3?
- **Gamepad.** Keyboard and mouse only, or a controller path too? It affects how the Brush and Ghost are aimed.

## Unverified or not checked

- **Not verified:** the reveal's audio track (only video was downloaded); the Scottish Games Network says it is set to Big Country's "In A Big Country" (W4).
- **Not verified:** the Courier article on the Dundee recreation could not be fetched (a bot wall). The Dundee identification rests on S2 and the frames.
- **Not checked:** 4J's Discord-exclusive Elements Engine video series (S4's description says the series is posted "exclusively on the 4J Studios Discord server"). Rendering details, if 4J published any, may be there.
- **Unsourced:** the Reforj wiki claims (4096-block width, auto-shaped terrain, P2P up to 4 players, first/third person) cite no source. They are marked as such above.
