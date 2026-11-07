# Research: John Lin's Crystal Islands engine reveal

Resolves issue [#134](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/134), part of the inspirations map [#130](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/130). Researched November 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). This builds on the John Lin section of [voxel-path-tracing.md](voxel-path-tracing.md) §1 and on [inspiration-john-lin-water.md](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-water/docs/research/inspiration-john-lin-water.md) (his older engine), and does not repeat them.

## TL;DR

- **The creator abandoned this look three weeks after showing it.** On 6 June 2021 Lin wrote that he had "decided to shelf the micro-voxel goal". The optimisations would go to "the 'pixel art' style that so many people said they preferred" instead. His reasons were "gameplay possibilities and end-user experience", and that maintaining "two separate art styles is also overly ambitious" (S3, and his pinned comment on the video, S2). Our [vertical slice](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/21) names this video as its visual reference. Take it as a reference for **mood, palette and lighting**, not for detail.
- **His Voxels are probably 3–10× finer than ours.** Thin branches are about three Voxels thick [0:28], and flower florets are a few Voxels across [4:36]. That implies roughly 1–3 cm Voxels, if the plants are life-sized (low confidence; the metric size is never stated). Our decided Voxel is 10 cm ([#8](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8)). His fallback "pixel art" engine is 8× coarser than this one (S1), so **our 10 cm sits between his two styles**. The voxelised-mesh vegetation (single leaves, florets, palm fronds) will not resolve at 10 cm. Large forms will: rock, crystals, tree silhouettes.
- **What the video is.** One uncut 5:05 flythrough of a procedurally generated floating island. It has no narration, no captions, no HUD, no player and no Edits. The shot list: forest floor, canyon, a dark descent, about two minutes of emissive crystal caves, the sunlit cliffs, a flower meadow, then a pull-out to the whole island [0:00–5:04].
- **Stated technique** (S1 description; S2):
  - 5-bounce path-traced GI "from the sun, atmosphere and all emissive objects", with a temporal-only denoiser.
  - Per-voxel Material attributes; the blog (S6) names albedo and normal.
  - A pipeline that **voxelises polygon assets**: Quixel Megascans rock, PlantFactory vegetation and crystals.
  - "Ray-traced world generation" that places assets "in sunlight, on cave walls, in big open areas".
  - An island generated once at startup, taking about a minute, in an "effective world size" of 256K³.
  - Rendering 10× faster than the old engine, and **3 GB of VRAM** for this scene.
  - **No frame rate, GPU or resolution is ever given.**
- **Look, judged from frames:**
  - Very saturated lime and emerald foliage on beige, photo-textured boulders.
  - A clear sky, deep blue overhead fading to peach at the horizon, with warm aerial haze and no clouds.
  - Crystals look **opaque and glossy** and come in many hues. They read pastel in daylight and neon in the caves, where they light the rock in their own colour.
  - Rock is smooth-shaded inside stair-stepped silhouettes.
  - No bloom, no depth of field, no motion blur. Highlights clip to flat white.
- **Best candidates to take:**
  1. **Exposure and tone mapping with eye adaptation.** We have no decision on it, and the reference's look depends on it.
  2. **A family of opaque, glossy, emissive crystal Materials in several hues**, coloured per chamber.
  3. **A crystal-cluster generator**: radiating hexagonal prisms aligned to the surface normal.
  4. **His placement rules** (sunlit, cave wall, open area), done as deterministic per-brick tests.
  5. **Moss and flower caps** on upward-facing rock.
  6. **A look-dev flythrough** that copies this video's shot list.

  Rejected: per-voxel albedo and normal storage, a mesh-asset voxelisation pipeline with third-party libraries, and generating the whole world at startup.

## Sources and how they were used

| Key | Source | Type | Used for |
|---|---|---|---|
| **S1** | John Lin, ["New Voxel Engine Reveal - Crystal Islands Experiment"](https://www.youtube.com/watch?v=8ptH79R53c0), YouTube, 13 May 2021, 5:05, 1080p30 (format 137) | **First-party** video and description | Description: all stated technique. Frames: 1 fps contact sheets of all 305 s. 47 full-resolution stills. Nearest-neighbour crops at 0:17, 0:28, 0:37, 1:53, 2:50, 4:36. An ffmpeg scene-change scan (threshold 0.25) found **no cuts**. **There is no subtitle or auto-caption track.** The audio track exists but **was not analysed** |
| **S2** | Lin's pinned comment on S1, by the uploader account, extracted with yt-dlp (1,127 comments, top-sorted) | **First-party** | The micro-voxel decision, "3GB of VRAM", "10x faster at the high resolution", and the new C/C++ + C#/.NET codebase. YouTube shows only "5 years ago" for its date; the text matches S3 |
| **S3** | [@ProgrammerLin thread, 6 Jun 2021](https://x.com/ProgrammerLin/status/1401347278212714497) (posts …278212714497, …278963576832, …279773003777, …284332257286, …286429442048) | **First-party**, read through Twitter's public syndication endpoint (`cdn.syndication.twimg.com/tweet-result`) | The decision to "shelf the micro-voxel goal", its reasons, and the new engine's languages |
| **S4** | [@ProgrammerLin, 12 May 2021](https://x.com/ProgrammerLin/status/1392334836820451329) | First-party (syndication endpoint) | "path traced global illumination finally making a triumphant return" |
| **S5** | [@ProgrammerLin, 12 Mar 2021](https://x.com/ProgrammerLin/status/1370405170392092674) | First-party (syndication endpoint) | "3rd revision to my engine, which shrinks the #voxels … 8x (512 cubic) detail increase … world gen and view distance" |
| **S6** | J. Lin, ["The Perfect Voxel Engine"](https://voxely.net/blog/the-perfect-voxel-engine/), voxely.net, 18 Sep 2021 | First-party blog | "unique per-voxel attributes in addition to a material identifier, like albedo and normal"; "Importing meshes and voxelizing them"; "Converting CSG operations to voxels (AKA a building system)" |
| S7 | Web-search snippets of other @ProgrammerLin posts | First-party, **search snippet only, unverified** | "Rendering is now 10x faster on average … proper ray traced soft shadows, AO and specular highlights". Also "Unrealistically strong god rays" (around 9 May 2021) and "Gentle breeze animations and leaf palettes … higher res #voxel tree (and vegetation) models". The status IDs were not found, so none could be read |
| S8 | YouTube comments on S1 (S2's extraction) | Viewer reception | Some viewers preferred the old engine's "charm". Not used for any technical claim |

Timestamps `[m:ss]` are S1. They come from 1 fps contact sheets and are accurate to about ±1 s. Telling frames, with direct links:

- [0:08](https://www.youtube.com/watch?v=8ptH79R53c0&t=8s): forest-floor palette.
- [0:28](https://www.youtube.com/watch?v=8ptH79R53c0&t=28s): Voxel scale on branches.
- [1:01](https://www.youtube.com/watch?v=8ptH79R53c0&t=61s): canyon, haze and peach sky.
- [1:44](https://www.youtube.com/watch?v=8ptH79R53c0&t=104s): glossy blue crystals.
- [1:53](https://www.youtube.com/watch?v=8ptH79R53c0&t=113s): blocky reflections.
- [2:17](https://www.youtube.com/watch?v=8ptH79R53c0&t=137s): emissive light pools.
- [2:34](https://www.youtube.com/watch?v=8ptH79R53c0&t=154s): crystal chamber.
- [3:29](https://www.youtube.com/watch?v=8ptH79R53c0&t=209s): magenta chamber.
- [3:56](https://www.youtube.com/watch?v=8ptH79R53c0&t=236s): sunlit cliffs.
- [4:36](https://www.youtube.com/watch?v=8ptH79R53c0&t=276s): florets up close.
- [5:04](https://www.youtube.com/watch?v=8ptH79R53c0&t=304s): the whole island.

## 1. What it is

- **A tech reveal of Lin's third engine revision.** It was announced in March 2021 as shrinking the Voxels for "an 8x (512 cubic) detail increase" (S5), and shown in a 12 May post (S4) and this video (S1). The description calls it "more a feature test than anything else". It says "Things won't look this chaotic in whatever the final version is" (S1).
- **Shown versus stated.** The description lists animation support, fast collision for physics and walking, "full fracture and mutation of objects", and the asset pipeline. The video shows **only the static generated world and its lighting**. Nothing is edited, fractured, simulated or walked on.
- **What happened next** (S2, S3):
  - On 6 June 2021 Lin shelved micro-voxels. He moved the optimisations to the coarser "pixel art" style and started a new C/C++ plus C#/.NET 5.0 codebase "designed around these realigned goals".
  - He gave his reasons: gameplay possibilities, end-user experience, and that "prioritizing one style dictates the features that are possible in the engine".
  - The faster engine makes it "significantly less costly to store and trace individual voxels". He lists "procedural snow build-up, mossy vegetation to grow on user builds … seasonal vegetation with growth stages" as what that enables.
  - The channel has posted nothing since S1 (the [water research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-water/docs/research/inspiration-john-lin-water.md) §1).
- **Viewer reception is split** (S8). The most-liked comments praise the detail, while a visible minority preferred the old engine's "charm", which is the feedback Lin acted on.

## 2. Techniques

Each item is labelled **Stated** (Lin's own words) or **Inferred** (judged from frames, with a confidence).

### Lighting and rendering

- **Stated:**
  - "path traced global illumination that features 5 bounces from the sun, atmosphere and all emissive objects".
  - "the path traced denoiser is lacking a spatial filter".
  - Tuning knobs include "level-of-detail threshold, lighting refresh rates, and the world size" (S1).
  - Vulkan hardware RT with per-format intersection shaders (S6, already in [voxel-path-tracing.md](voxel-path-tracing.md) §1).
- **Inferred, high confidence: emissives are real light sources in the GI, not glow sprites.**
  - Rock next to a red or magenta crystal bed is washed red or magenta [2:14–2:19, 2:58–3:02, 3:20].
  - Rock behind a pink crystal carries a soft pink pool [3:33].
  - A blue crystal casts blue onto the boulder behind it [1:44].
  - The light pools sit on the surfaces, with no screen-space halo.
- **Inferred, medium confidence: daylight is sky-dominated and soft.**
  - Sunlit scenes show little directional shading and almost no long cast shadows [3:56, 4:12, 4:43].
  - Small crisp shadows do appear: branch stubs shadow a trunk [0:17].
  - No sun disk is ever in frame.
  - Lin's "proper ray traced soft shadows" (S7) is unverified.
- **Inferred, medium confidence: the temporal-only denoiser shows as grain in dim areas.** The dark descent shows fine noise on lit rock [1:22–1:31]. There is no visible ghosting at 1 fps sampling.
- **Inferred, medium confidence: glossy crystals reflect a blocky, coarser-than-Voxel image.**
  - On the big blue crystal, the reflected sky-to-rock boundary is stair-stepped in blocks of about 8–10 px. The crystal's own silhouette steps are about 3 px [1:48, 1:53, 2:50 crops].
  - The likely cause is reflection rays reading a coarser LOD, or per-voxel normals quantising the reflection direction. "Lighting refresh rates" (S1) hints at cached lighting as a third option.
  - Which one it is cannot be told. Either way, **the creator shipped this artefact in his hero shots**.
- **Inferred, high confidence: smooth shading inside stepped silhouettes.**
  - Boulders, trunks and crystal faces shade smoothly. Their outlines stair-step at the Voxel scale [0:17, 0:28, 2:50].
  - That matches the stored per-voxel normal of S6, presumably baked from the source meshes (inference).
  - Our renderer uses per-face DDA normals (`crates/render/src/reference.rs`, `face()`).
- **Inferred, medium confidence: no refraction or transparency on crystals.** Even the large blue crystal at 1:48 shows reflections only, never the rock behind it through the crystal.
- **Inferred, medium confidence: LOD holds to the whole-island view.** The pull-out from canopy height to the whole island [4:45–5:04] shows no hole or pop at 1 fps. A faint cross-shaped seam across the canopy at 5:02–5:04 may be a region or LOD boundary (low confidence).

### World generation and data

- **Stated (S1):**
  - The island "is generated once upon startup (like Terraria …), which takes about a minute".
  - Assets are placed by "ray-traced world generation … in sunlight, on cave walls, in big open areas".
  - Placement still misbehaves: "object intersections occurring or being placed in odd places".
- **Stated:**
  - Assets are converted from Megascans, PlantFactory/PlantCatalog and "other highly detailed polygon models" (S1).
  - Per-voxel attributes beyond a Material ID: "albedo and normal" (S6).
  - Compression rates are "high"; no format is given.
- **Inferred, high confidence: everything visible is stamped from a small set of source assets.**
  - The same broadleaf tree, lupin, cycad, conifer and crystal-cluster shapes repeat throughout [0:00–0:50, 3:56–4:58].
  - Crystals stick out of rock along the local surface direction: horizontally from cliffs, down from ceilings [0:52, 1:07, 2:34].
  - Some vegetation floats or intersects rock, as Lin admits [3:29, 4:52].

### Performance

| Claim | Value | Source |
|---|---|---|
| VRAM for this scene | "3GB" | S2 |
| Speed vs old engine | "10x faster at the high resolution than the engine did previously at the low resolution"; "A 10x speedup in rendering" | S2, S1 |
| Target hardware | "reserved for RTX graphics cards"; a low-detail mode for older cards (later dropped by S3) | S1 |
| Generation | about a minute at startup, once | S1 |
| GPU, fps, resolution, frame-time breakdown | **Not stated** anywhere first-party | — |

## 3. Art style

Judged from the frames named.

- **Voxel scale on screen** (inferred, low confidence on the metric value):
  - Large surfaces (boulders, trunks, crystal faces) read as continuous and smooth at every distance shown.
  - Voxels become visible only on thin things close to the camera:
    - Branches step in blocks of about 10 px at 1080p, about three Voxels thick [0:28].
    - Lupin florets are chunky cubes several pixels wide [4:36].
    - Crystal silhouettes step by about 3 px [2:50].
  - At mid distance the image reads as photographic-cartoon, not as Voxels [1:01, 4:43].
- **Palette:**
  - Saturated lime and emerald greens dominate the foliage [0:28, 4:43, 4:52].
  - Rock is warm beige and tan with photographic cracks [1:01, 3:56].
  - Flower accents: cobalt and teal lupins [0:08], violet lupins [0:46–0:52], magenta and crimson spikes [0:55–0:58], red carpets on the meadow tops [4:33–4:45].
  - Daylight crystals are candy pastels: white, pale pink, lilac, mint, sky blue [0:52, 1:07, 3:56, 4:12].
  - Cave crystals are neon: electric blue, cyan, green, magenta, red, amber [1:37–3:47].
- **Material looks:**
  - **Rock:** big chaotic stacked boulders, smooth-shaded, with photo-texture albedo. They read as Megascans because they are (S1).
  - **Foliage:** individual leaves on broadleaf trees, palms and cycads, and conifers [0:28–0:41]. Grass is a dense fur of fine blades capping rock tops [0:46, 4:36]. Moss and grass caps follow the upward-facing edges of boulders with clean borders [3:56, 4:36].
  - **Crystals:** clusters of hexagonal prisms with pointed tips, radiating from a base. They range from a few Voxels to several metres, opaque, glossy to mirror-like (the blue ones at 1:44–1:55), and emissive. Each cave chamber has a dominant hue:
    - blue and cyan [1:37–2:05];
    - cyan and green [2:20–2:55];
    - pastel mixed [3:22–3:28];
    - pink, magenta and violet [3:25–3:40];
    - blue again [3:40–3:47].

    The cave floors hold beds of red and magenta [2:14–2:19, 2:58–3:02].
  - **Emissives:** flat, fully saturated colour with no visible texture. They are bright enough to light the rock, and edges stay crisp.
  - **Water and glass:** none in the scene.
- **Lighting mood:**
  - One fixed time of day for all five minutes, likely a low sun. The horizon glows peach in one direction and the zenith is blue [0:52, 1:01, 3:58].
  - Daylight is high-key and soft.
  - Caves are dim but readable, never black. Sky light comes through openings and the emissives fill the rest [1:44, 2:34, 3:00]. Only the descent passage [1:24–1:28] goes near-black.
  - The cave rock is brighter than a fixed exposure would plausibly give next to the sunlit canyon, and the same crystals turn from pastel to neon. Both suggest **automatic exposure adaptation** (inferred, medium confidence).
- **Sky and atmosphere:**
  - A clear gradient from blue zenith to peach horizon. No clouds, stars or sun disk are visible.
  - Warm aerial haze fades distant rock toward the horizon colour [0:52, 1:01, 4:02].
  - Seen from above, the void below the island is near-white [5:00–5:04].
  - **No particles**: no falling leaves, dust or fireflies anywhere. **No god rays** are visible, although Lin posted about strong god rays that same week (S7, unverified).
- **Post-processing:**
  - No bloom (emissive edges are crisp [2:17, 3:33]).
  - No depth of field, no motion blur, no vignette, no lens flare.
  - Highlights clip to flat white on sunlit pale crystals [1:07–1:09, 3:29], which suggests a simple tone curve.
  - Thin silhouettes stay stair-stepped and sharp, which suggests little or no edge-smoothing AA.
  - The overall feel is vivid, toy-like and slightly overexposed.
- **UI and HUD:** none in any frame.

## 4. Controls

- **Camera: a free-flying 6-DOF camera with roll** (inferred, high confidence).
  - It rolls the horizon to near-vertical [4:10–4:16].
  - It flies through foliage and through crystals without collision. A crystal fills the screen at 1:08–1:09 and 1:18–1:19.
  - It drifts at a slow, steady, drone-like pace for one continuous take, ending in a vertical pull-out to the whole island [4:45–5:04].
  - Whether it is hand-flown or a recorded path cannot be told.
- **No Edit controls, no Brush, no Ghost, no crosshair, no cursor, no avatar.** The video shows nothing about how Edits are driven. For his Edit UI, see the old engine's Brush panel in the [water research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-water/docs/research/inspiration-john-lin-water.md) §3.

## 5. Features, and what is demo versus planned

Nothing shipped, and the micro-voxel engine itself was shelved (S3).

| Feature | Status | Source |
|---|---|---|
| Procedurally generated floating island, generated once at startup | **Demo** | S1; [0:00–5:04] |
| Voxelised Megascans rock, PlantFactory vegetation, crystals | **Demo** | S1 |
| Ray-traced asset placement (sunlit, cave walls, open areas) | **Demo**, with admitted placement bugs | S1 |
| 5-bounce path-traced GI with emissive crystals | **Demo** | S1; [1:37–3:47] |
| Whole-island view (LOD) | **Demo** | [4:45–5:04] |
| Animation of voxel models (wind) | **Stated**, not verifiable in the footage | S1; S7 (unverified) |
| Collision for physics and walking; fracture and mutation | **Stated**, not shown | S1 |
| Rigid bodies, fluids | **Planned** ("will make a return … eventually") | S1 |
| Low-detail mode for non-RTX cards | **Planned**, then superseded: micro-voxels shelved | S1, S3 |
| Snow build-up, moss growing on builds, seasonal growth stages | **Planned** for the coarser engine | S2 |

## 6. What this video adds to the map's fog

- **Shadow softness:** the reference leans **soft and sky-dominated**. Long sun shadows are hard to find, though small contact shadows are crisp [0:17]. It does not support a crisp, Teardown-style sun.
- **Day/night:** one fixed late-day light for the whole video. Nothing about a cycle. The peach horizon is the mood to aim for at one time of day.
- **Smooth vs per-face normals:** this is the strongest evidence yet for smooth shading inside blocky silhouettes. It is first-party design (S6, per-voxel normals) and visible in every close-up. But at our 10 cm the silhouettes are 3–10× coarser, so the combination will look different. The question still needs a look-dev comparison.
- **Particles:** the reference has **none**. Cosmetic particles are not needed for this look.
- **Volumetric clouds:** the reference has **no clouds**, only a clear gradient sky and aerial haze. Clouds come from #131, not from this reference.
- **Audio:** nothing; no captions or speech track, and the audio was not analysed.
- **A new fog item:** exposure and tone mapping. There is no decision on auto-exposure, the tone curve or bloom, and the reference's day-to-cave contrast depends on them (§3).

## 7. What this changes in our earlier research

- **[voxel-path-tracing.md](voxel-path-tracing.md) §1 should add:**
  - The creator **shelved** the micro-voxel engine on 6 June 2021 (S3, verified).
  - The scene used **3 GB of VRAM** (S2).
  - Per-voxel **albedo and normal** are stored (S6).
- **Still unverified:** the "Reported but not verified" May 2021 post ("10x faster on average … soft shadows, AO and specular"); its status ID was not found (S7).
- **Two other posts are now verified,** the March and 12 May 2021 ones (S4, S5). Twitter's public syndication endpoint (`cdn.syndication.twimg.com/tweet-result?id=…`) returns post text and reply parents where x.com pages could not be read. Later research can use it.
- **The vertical-slice spec's line "inspired by John Lin's *Crystal Islands*"** should be read as lighting, palette and mood. The detail level is a Voxel size we did not choose, and its author abandoned it.

## 8. Cross-check against what is already decided

Sources:

- [Vertical-slice map #1](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/1) Decisions so far (#2–#20) and the [spec #21](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/21) M1b scope.
- ADRs 0001–0004 and 0006 in [`docs/adr/`](../adr/).
- [`CONTEXT.md`](../../CONTEXT.md).
- The current code: `crates/materials/materials.toml` has 55 Materials, one `crystal` (id 17: blue, roughness 0.05, transparency 0.3, IOR 1.55, emissive 4.0) and per-Material `noise_scale`/`hue_jitter`/`value_jitter`; `reference.rs` uses face normals; the client has a noclip mode and bench camera paths with keyframes; there is no exposure or tone-mapping code.

| # | Finding | Dimension | Verdict | Against |
|---|---|---|---|---|
| 1 | Creator shelved micro-voxels for a coarser style (gameplay, UX, two styles too costly) | Planning | **New** (calibration of the reference) | #21 ("inspired by John Lin's *Crystal Islands*") |
| 2 | Voxels roughly 1–3 cm (inferred, low confidence) | Art | **Conflicts** | #8 (10 cm Voxels) |
| 3 | 5-bounce path-traced GI from sun, sky and emissives | Technique | **Already decided** (by other means) | #10 (1 bounce + world-space radiance cache) |
| 4 | Temporal-only denoiser; grain in dim areas | Technique | **Already decided** (ours adds a spatial filter) | #10 (A-SVGF-style) |
| 5 | Emissive crystals light caves in their own colour | Art / technique | **Already decided** | #10 (emissives in M1), #14 (glowing crystal caves), #21 M1b checklist |
| 6 | Crystals opaque and glossy, no refraction | Art | **New** (differs from our crystal Material: transparency 0.3, IOR 1.55) | `materials.toml` id 17; #10 (glass) |
| 7 | Many crystal hues, one dominant hue per chamber | Art | **New** | `materials.toml` (one crystal) |
| 8 | Crystal clusters: radiating hexagonal prisms along the surface normal, a few Voxels to metres | Art / feature | **New** (shape spec) | #14 (decoration pass) |
| 9 | Blocky reflections coarser than the Voxel, accepted in hero shots | Technique | **New** (a cost-saving licence) | #10 (glossy reflections, 0.7 ms) |
| 10 | Smooth shading inside stepped silhouettes, from per-voxel normals | Art / technique | **New** | `reference.rs` face normals; #130 fog (normals) |
| 11 | Per-voxel albedo and normal attributes | Technique | **Conflicts** | #9, ADR 0002 (palette Materials, no per-Voxel colour) |
| 12 | Photo-textured rock (per-voxel albedo from Megascans) | Art | **Conflicts** / approximated | #9; `materials.toml` hue/value jitter |
| 13 | Voxelised polygon assets (Megascans, PlantFactory) | Technique | **Conflicts** | #14 (procedural Voxel tree species); #130 out-of-scope rule on others' assets |
| 14 | Ray-traced placement: sunlit, cave walls, open areas | Technique | **Conflicts** (method) / **New** (rules) | #14 (per-brick at any LOD), #12 (deterministic CPU) |
| 15 | World generated once at startup (~1 min) | Technique | **Conflicts** | #14 (per-brick on demand), ADR 0003 |
| 16 | Dense vegetation: broadleaf, palms, conifers, lupins, grass fur | Art | **Already decided** | #14 (dense and lush), #21 M1b |
| 17 | Moss and grass caps on upward-facing rock; flower carpets on tops | Art | **New** (placement rule) | #14 |
| 18 | Floating island(s) | Feature | **Already decided** | #14 (floating islands) |
| 19 | Saturated lime greens, beige rock, pastel and neon crystals | Art | **New** (look-dev input) | #130 fog (look-dev) |
| 20 | Clear sky, blue zenith to peach horizon, aerial haze, no clouds | Art | **Already decided** (atmosphere) / **New** (no clouds) | #10 (Hillaire atmosphere, volumetrics); #131 (clouds) |
| 21 | One fixed late-day light | Art | **New** (input to the day/night fog) | #21 (M1a checklist: watch a sunset) |
| 22 | Soft, sky-dominated daylight shadows | Art | **New** (input to the shadow-softness fog) | #130 fog (our ~2.3° sun) |
| 23 | Auto-exposure, pastel-by-day and neon-in-cave (inferred) | Art / technique | **New** (no decision exists) | #10, #21 |
| 24 | No bloom, DoF or motion blur; clipped highlights | Art | **New** (post-processing direction) | — |
| 25 | No particles | Art | **New** (input to the particle fog) | #131, #133 |
| 26 | Free 6-DOF camera with roll, no collision | Controls | **Already decided** (noclip exists) / **New** (roll) | #16 (fly mode), client `N: walk / noclip` |
| 27 | Pull-out from canopy to the whole island in one move | Controls / feature | **Already decided** | CONTEXT **Zoom**, #16 |
| 28 | No HUD, Edits or avatar shown | Controls | **Not applicable** | #15, #16 |
| 29 | 3 GB VRAM; 10× faster than the old engine; RTX only | Technique | **New** (data point; no GPU/fps) | #9 (~0.2–0.7 GB residency), #10 (4090, 8.6 ms) |
| 30 | Low-detail mode per GPU (later dropped) | Feature | **Conflicts** | #8 (Voxel size is the world's), #21 M5 (3060 at 1080p) |
| 31 | Animation, collision, fracture: stated, not shown | Feature | **Not applicable** (unverifiable) | #18 (Debris) |
| 32 | Snow build-up, moss on builds, growth stages (his later plans) | Feature | **New** (out of scope for the slice) | — |

## 9. Candidates to adopt

Costs assume a beginner developer with AI agents writing the code: **Low** = a ticket of days, **Medium** = one to two weeks, **High** = several weeks or a spike first.

| # | Candidate | Dimension | Rough cost | Recommendation |
|---|---|---|---|---|
| A | **Exposure and tone mapping with eye adaptation.** A luminance-histogram auto-exposure with a slow adaptation rate, plus a filmic tone curve that rolls highlights off instead of clipping (an improvement on S1's flat whites). This is what lets one crystal read pastel at noon and neon in a cave. | Art / technique | Low–Medium (one compute pass plus a curve; must be identical in reference mode, or the image metric drifts) | **Adopt for M1b.** It is a gap: no decision exists. Raise it as fog on #130. |
| B | **A crystal Material family**: six to eight hues (blue, cyan, green, magenta, red, amber, white, violet), **opaque**, roughness about 0.05–0.15, emissive. The current `crystal` Material is transparent, IOR 1.55. The reference gets its look without glass, which also avoids glass denoising. | Art | Low (rows in `materials.toml`) | **Adopt for M1b**, and make crystal opaque by default. Keep a transparent "quartz" as a separate, rarer Material if wanted. |
| C | **A per-chamber hue rule.** Each crystal cave picks one dominant hue (seeded from its location) with a few accents, and floors get dense beds of a second hue. | Art / feature | Low | **Adopt** in the #14 decoration pass. |
| D | **A crystal-cluster generator**: radiating hexagonal prisms with pointed tips from a base point, aligned to the density gradient (the surface normal), sizes from 0.3 m to several metres. | Feature / art | Medium (an SDF stamp in `planet-gen`, deterministic, at every LOD) | **Adopt for M1b.** At 10 cm a 0.3 m prism is 3 Voxels, so set the minimum size by readability, not by S1. |
| E | **His placement rules without the ray tracing**: "sunlit" from the world map's slope and aspect and a sky-openness estimate, "cave wall" from the 3D density gradient inside Caves, "open area" from local density. All are deterministic per-brick tests. | Technique | Medium | **Adopt the rules; reject tracing at generation time.** Tracing needs neighbouring bricks and breaks per-brick generation at any LOD (#14, #12). |
| F | **Moss and flower caps**: upward-facing rock surfaces near the surface get a moss or grass Voxel layer with a clean edge; meadow tops get flower carpets by biome. | Art | Low–Medium | **Adopt** as part of #14's dense vegetation. |
| G | **A look-dev flythrough modelled on S1's shot list**: forest floor close-up, canyon with haze, dark descent, two crystal chambers of different hue, sunlit cliff, meadow, pull-out. Built as a bench camera path (keyframes already exist), with roll added to the free camera. | Controls / art | Low | **Adopt** as the scene for the map's look-dev question. |
| H | **Coarser-LOD secondary rays**: reflection (and possibly GI) rays read one or two LOD levels coarser than primary rays near the camera. S1 shows the blocky result is tolerable even in hero shots. | Technique | Medium (needs the reference-metric check) | **Spike in M1b** against the 0.7 ms reflection budget. Adopt only if the image metric stays under threshold. |
| I | **Smooth normals from neighbouring occupancy** (computed, not stored) inside blocky silhouettes. | Art / technique | Medium | **Defer to M1b look-dev**, already open as fog. Try it in scene G next to face normals; S1 is the case for it. |
| J | **Look-dev palette references**: saturated lime foliage on warm beige rock, pastel-by-day and neon-in-cave crystals, blue-to-peach sky with warm haze. Principles only, no assets. | Art | Low | **Adopt as input** to the look-dev question. |
| K | **No bloom, DoF or motion blur at first**; add a subtle bloom only if emissives look flat after A. | Art | — | **Adopt as the default** for M1b. |
| L | **Per-voxel albedo and normal storage.** | Technique | High (doubles or triples brick size; conflicts with palette compression) | **Reject.** It conflicts with #9 and ADR 0002. Material jitter plus computed normals (I) get most of the look. |
| M | **Voxelising third-party mesh libraries** (Megascans, PlantFactory). | Technique | High | **Reject.** It conflicts with #14's procedural species, and #130 rules out others' assets (their licences were not checked for this use). A voxeliser for our own models could be **deferred**, if procedural trees fall short. |
| N | **Generating the whole world at startup.** | Technique | — | **Reject.** It conflicts with per-brick on-demand generation (#14) and a 16 km Planet. |
| O | **A low-detail mode with a different Voxel size per GPU.** | Feature | — | **Reject.** Voxel size is world and gameplay state (#8); Lin dropped it too. M5's 3060 target is met by scaling the render, not the world. |

## Open questions this research raises

- **Which detail level is the reference for?** Lin's micro-voxels are 3–10× finer than ours, and he left them for a style 8× coarser. Does the dev want the reference to set mood, palette and lighting only, and accept a chunkier vegetation look? Or does it argue for the 5 cm stretch from #8?
- **Exposure and tone mapping** (A): who owns it, and does it run in reference mode so the image metric stays comparable?
- **Should crystals be opaque?** The current Material is transparent, and the reference is not.
- **Are coarse-LOD reflections acceptable** to the dev's eye, and to the image-difference metric (H)?
- **Smooth versus face normals** at 10 cm: only scene G will show it.

## Unverified or not checked

- **No speech or captions exist** in S1 (no subtitle or auto-caption track). Every technical statement comes from the description, the pinned comment, the posts and the blog. The audio track was not analysed.
- **Not stated anywhere first-party:** GPU, frame rate, resolution, frame-time breakdown, metric Voxel size, compression format, how "lighting refresh rates" works.
- **Unverified (search snippets only, S7):** "Rendering is now 10x faster on average … soft shadows, AO and specular highlights", "Unrealistically strong god rays", "Gentle breeze animations". Their status IDs were not found.
- **Not reached:** x.com pages (posts were read through the syndication endpoint instead); nitter.net ("offline"); voxely.net/blog's index (HTTP 406 to curl; the post itself was read through WebFetch); Reddit (not attempted, given the sibling tickets' failures).
- **Not checked:** Lin's July 2021 blog post on the new codebase's design (an ECS post, judged off-topic). A commenter's claim about a "Sunheart" trademark (secondary, unverified).
- **Inference, low confidence:**
  - the metric Voxel size (it assumes life-sized plants, which may be exaggerated);
  - the cause of the blocky reflections;
  - auto-exposure;
  - the cross-shaped seam at 5:02;
  - whether the camera was hand-flown.
- **Not verifiable from footage:** wind animation of vegetation (the camera never holds still).
