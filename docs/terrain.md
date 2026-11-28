# Terrain spec: a Tectonic- and Terralith-like Planet

This is where the wayfinder map "Terrain: a Tectonic- and Terralith-like Planet" ([#238](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/238)) ends. It gathers the owner's decisions from #238's children into one buildable spec. The ordered build tickets are in [`terrain-tickets.md`](terrain-tickets.md). It changes only by PR.

- **Vocabulary:** [`CONTEXT.md`](../CONTEXT.md). Relief, Formation, Landmark, Landform, Climate, Biome, Cave Biome, Plant, Ore and Vein are defined there (#248, merged 2026-11-28). "World" and "map" are on its Avoid list: this spec says Planet, and "world map" only for the 245 m texel map.
- **Architecture decisions:** ADR 0002 (the LOD pyramid), ADR 0003 (determinism), ADR 0007 (5 cm Voxels), ADR 0008 (the 320 km Planet and its world map), ADR 0009 (LOD bands) and ADR 0012 (a Relief plus Formations).
- **Whole-machine rules:** `AGENTS.md`, "Use the whole machine" (#264), and [`perf/resource-audit.md`](perf/resource-audit.md).

## Summary

The Planet keeps its 320 km radius, its 5 cm Voxels and its 6 × 2048² world map of about 245 m a texel. What changes is everything that the world map holds and everything a brick makes from it.

- **Scale** (#243). There are 4–8 continents of 200–600 km, with island arcs and archipelagos between them. About 55% of the Planet is ocean. Typical ranges reach 2–5 km, with valleys 1–3 km below their ridgelines. Only the greatest range or two reach 6–8 km. Open ocean is 1.5–3 km deep, and trenches reach 4 km. The ground column is about 12 km tall, 4× today's 3 km.
- **Shape model** (#246, C2). Plates set the continents and the lines of the big ranges. Splines and ridge noise shape the land within the plates. Coasts are warped by noise. Stream-power erosion carves the valleys.
- **Relief plus Formations** (#244, ADR 0012). The Relief, one height per place, covers the whole Planet. Formations add to it or carve from it only where a Landform, Biome or Landmark asks. Carving wins. Every shape has a proven per-brick bound and a declared reach.
- **Landform, then Biome** (#245, #249). The shape fields choose a Landform family. The Climate then chooses a Biome from those the family allows. There are 42 Biomes in two waves, plus about six Cave Biomes, all in one data table.
- **Rivers and lakes** (#206, #266). Rivers are routed by multiple-flow routing (MFD) on the world map and carved per brick. Lakes fill their basins to the spill height. Rivers and canyons narrower than a texel still read from orbit.
- **The build** lands in place on main, ticket by ticket (owner, 2026-11-27). Every ticket that changes what is generated bumps `GENERATOR_VERSION`, remakes the goldens and determinism hashes, and posts images. No saves exist yet, so nothing migrates.
- **Order** (owner, 2026-11-28). The build lands as a show-friends cut, aimed at about 2026-12-08:
  1. Group A, wave-1 terrain.
  2. Trees and fallen logs (#205).
  3. Group B, the rest of the terrain.
  4. Ground cover (#273), then rare Plants (#274).
- **Cost** (#247). The new terrain may cost one total of extra generation per brick, about 25–35 µs over today's terrain at the tall relief. The exact number is set in #247. There are no fixed shares between features.

## The pipeline

### World-map stages, in order

The world map is built once per seed and `GENERATOR_VERSION`, on all threads, and cached on disk (#264). The whole build takes about 2 minutes or less at full size (owner, #246; it was about a minute in ADR 0008). Each stage is parallel and gives the same result at any thread count (see "Determinism and the whole machine").

1. **Plates** (#246). Hashed plates on the sphere, each with a motion. Each plate edge is converging, separating or subducting, from the two plates' motions.
2. **Shape fields** (#246, #240). These are continentalness, erosion and ridge fields, made from the plates and from gradient noise:
   - **Continents** come from the plates, with the coasts warped by noise so that they do not follow plate edges.
   - **Archipelagos and island arcs** lie between the continents.
   - **Ranges** follow colliding plate edges. **Rifts** follow separating edges. **Island arcs and trenches** follow subducting edges.
   - **Ridgelines** use the slope-warped ridge noise of #240, under a spline mask that keeps ridges in the mountain belts.
3. **Base elevation** (#246, #240). Monotone splines map the shape fields to elevation, Tectonic's way.
   - The highest summits are capped at the map's highest elevation (8 km).
   - The prototype ran this stage and erosion at 1024² a face (about 490 m a texel) and upsampled to 2048². The build may do the same, if the result fits the 2-minute build.
4. **Volcano stamps** (#257, group B). The cones, shields and calderas are added to the elevation before erosion.
5. **Ocean floor profiles** (#258, group B). Each ocean texel measures its distance from the coast and from the nearest plate edge, along with that edge's kind. Monotone splines then give the shelf, slope, ridge and trench profiles. Seamounts are stamped as volcanoes are.
   - **Until then (group A),** the ocean's depth comes from the splines of stage 3.
6. **Normalise.** The elevation is scaled so that about 55% of the area is ocean (#243). Plateaus are about 10% of the land, and one or two massifs rise above 6 km (#246's aims, which depend on the seed and are not gates).
7. **Stream-power erosion** (#241, #246). It runs on land only (#258).
   - **Method:** the stream power law with n = 1 and m = 1/2, solved by Braun & Willett's implicit FastScape solver on one graph over the whole cube. The neighbours come from `world_map::step`, so there are no seams at the faces' edges.
   - **Order:** the passes run level by level in a fixed order (Barnes 2019).
   - **Mode:** either "amplify" (10–40 steps on the shaped heights) or "coarse to fine" (steady state at 512², then a few steps at 2048²). Each costs about a minute or less. The ticket measures both and keeps the one that fits the build's time.
   - **Uplift:** from the range belts of the plates (#241's recommendation, decided; see "Decided with the owner").
   - **Erodibility:** active volcanoes are less erodible, so they stay fresh (#257).
   - **Thermal pass:** today's thermal pass runs afterwards as the slope limit.
8. **Lakes and routing** (#206, #241).
   - **Lakes:** every closed basin fills to its spill height. A basin deeper than about 1 m and larger than a few texels is a lake (the prototype used 1 m and 6 texels). Active craters are skipped, because they hold lava (#257). A caldera fills to the notch in its rim, and a river spills out.
   - **Routing:** MFD (Holmgren) splits the drainage area among all lower neighbours by slope to the fourth power.
   - **Stored per texel:** the dominant receiver (one of 8), the mean flow direction, the drainage area and the lake level.
   - **The water surface** of a river falls downhill: no texel's is above an upstream texel's.
9. **Climate** (#245). It runs once, after erosion.
   - **Temperature** is Earth-like, from latitude and altitude.
   - **Moisture** is carried in from the oceans by wind bands, rains out on the windward side of ranges, and leaves rain shadows behind them.
   - **Noise** shifts both by up to about 15%.
10. **Landform family, variant and Biome** (#245, #249).
    - **The family** comes from the same shape values that made the land (Terralith's rule). The families are range, plateau, canyon, valley, plains, coast, volcanic and ocean.
    - **The variant field** is a low-frequency noise, Terralith's "weirdness".
    - **The Biome** of each texel's centre is the nearest Climate point within its family, with the rare Biomes at the variant field's extremes. It is stored for the readers of the world map: the Spawn, the tour, the Landmarks and the Cave Biomes.
11. **Formation allowances** (#244, #246, #249, group B). The allowances of each Formation kind come from the family, steepness, crest and dryness, and from the Biome's allowances. Regions are retired (#249).
12. **Landmarks** (#256, group B). About 30–60 one-off Landmarks are placed from the seed and kept as a short list on the world map.
13. **The Spawn** (#262). The Spawn search runs within the build's time (see "Spawn").

### Per-brick stages, in order

A brick is still a function of the seed, the world map and its coordinate alone (ADR 0003, ADR 0008), at every level (ADR 0002). Most bricks are still decided by bounds without sampling a Voxel (ADR 0012).

1. **Fields.** The brick reads the world map's fields, interpolated between texel centres so that they are continuous. River and Biome work reads the 3×3 texels around the brick (#241, #245).
2. **Relief.** The Relief is the map's elevation plus the detail. The detail is:
   - today's octaves, moved to gradient noise once its centre-plus-gradient bound is built and tested (#240, #244);
   - **the detail layer chosen in #267** (see "Detail below the texel");
   - cinder cones and crater rims on volcanoes (#257, group B);
   - shelf-only detail at sea (#258, group B).
3. **Rivers and lakes** (#206, #266). River channels and lake water, with the river tint and canyon thickening at coarse levels.
4. **Biome blend** (#245). The weights of the nearest Biomes come from the 3×3 texels. Landform settings blend smoothly, and surface Materials mix over a few hundred metres with noise.
5. **Surface Materials** (#249). These are the surface, the subsoil, the strata bands and the bedrock by Landform, with the shared rules for steep slopes, the snowline and river banks.
6. **Formations** (#244, #256, #259, group B). Added kinds come first, then carved kinds, so carving wins. Caves are a carving kind.
7. **Content** that fills air only: Crystal clusters (#204), and later Plants (#205, #273, #274).
8. **Coarse-level rules** (ADR 0002). An exposed coarse Voxel takes the Material at the top of the land within it. A coarse Voxel that a river crosses takes the water Material (#266). Thin Formations and canyons thicken (#256, #266).

## Features

### Scale and storage (#243, #246)

- `PEAK` becomes 8 km and `TRENCH` 4 km. That makes a ground column about 12 km tall, which reaches `Planet::max_radius`, `interior_radius`, the Guard, the LOD bands (ADR 0009) and the `ground_range` shortcuts.
- **Elevation storage.** Today's `i16` in 10 cm units stops at 3.27 km. The spec uses an `i16` in **25 cm** units, which holds ±8.19 km, as the prototype did (#246). See "Decided with the owner".
- **The noise lattice keeps its 10 cm spacing.** `ELEVATION_UNITS_PER_METRE` names both the storage unit and the lattice today (`detail.rs`'s `VOXELS_PER_POINT`). The two become separate constants, so the noise does not move when the storage unit does.
- **The texel record** grows from today's 8 bytes (201 MB on disk at 2048²). The target is at most **16 bytes a texel** (about 400 MB). Anything that can be derived cheaply from stored fields per brick is derived, not stored: the Biome blend weights, and the Formation allowances where the family, slope and Biome give them. The low-end profile's memory is measured with the full map (see "Determinism and the whole machine").

| Field | Size | From |
|---|---|---|
| elevation, 25 cm units | 2 B | stages 3–7 |
| erosion, 25 cm units | 2 B | stage 7 |
| lake level, 25 cm units (none outside lakes) | 2 B | stage 8 |
| drainage area, 16 × log₂ texels, as today | 1 B | stage 8 |
| dominant receiver (3 bits) and mean flow direction | 2 B | stage 8 |
| temperature, moisture | 2 B | stage 9 |
| Landform family, variant | 2 B | stage 10 |
| Biome id at the centre | 1 B | stage 10 |
| rough, as today | 1 B | stages 7, 10 |
| `regions` until group B, then the allowances the Biome does not give | 1 B | stage 11 |

The table is a plan. The tickets may move a field if a measurement says so, and say so on the PR.

### Landform model (#246)

- **C2: plates plus splines, with noise-warped coasts.**
- **Plates** are hashed, each with a motion. Converging edges raise ranges. Separating edges open rifts on land and ridges at sea. Subducting edges make island arcs, trenches and arc volcanoes.
- **Splines and ridge noise** shape the land within and along the plates, Tectonic's way: continentalness, erosion and ridge fields through monotone splines. The same values choose the Landform family, so the shape seen from orbit matches the family on the ground.
- **Coasts** are noise-warped, with archipelagos and island arcs between continents.
- **The prototype's figures** are aims, not gates, and they depend on the seed. They are 5 continents, about 55% ocean, the greatest range at about 8.1 km, one or two massifs above 6 km, and plateaus at about 10% of the land.
- **Sizes stay metric at any radius** (ADR 0008). The number of plates follows the Planet's area, with a floor of two, so a bigger Planet has more continents, not bigger ones. The 40 km test Planet (`TEST_RADIUS`, which 22 test files use) gets one or two land masses and sea, still has a Spawn, and still has about 55% ocean after the normalise stage.
- **The prototype's code** is on the throwaway branch `prototype/landform-model` at 330ff37 (`crates/planet-gen/src/landform_proto/`). The build ports it, made deterministic and parallel. It is not merged as it is.

### Noise toolkit (#240)

- **Gradient noise.** Improved Perlin noise with hashed corners and no period. It uses a quintic fade, `f64`, today's integer lattice with power-of-two cells, a per-cell gradient cache, and analytic derivatives.
  - Cached, it costs 1.5× today's value noise; uncached, 0.6×.
  - Where its axis bias shows, each octave takes a fixed rotation.
  - OpenSimplex2 with radius² 0.5 is the fallback. The reference code, with radius² 0.6, is discontinuous.
- **Domain warp** on the world map only, at about 1.3 µs a sample for two levels (about +4 s on 8 cores). It is too dear per brick. On the sphere, warp by the tangent part of the warp vector.
- **Ridgelines.** A folded low-frequency base, with detail dragged along the base's slope, Tectonic's `base + 0.6·detail(p + 3000·|∇base|)`. A jaggedness spline masks it. With analytic gradients it costs 4 noise evaluations, not 8.
- **Splines.** Monotone cubic Hermite in vanilla's form, with linear ends and nesting. The slopes come from PCHIP, or are checked against Fritsch–Carlson's `0 ≤ α, β ≤ 3`. 8 knots cost about 13 ns.
- **Sphere.** 3D noise is sampled at the 3D point, which has no seams or poles.
- **Bounds.** `Detail::bounds` relies on value noise having its extremes at a box's corners (#166's exact-corners lever), which gradient noise does not have. A **centre-plus-gradient bound** (the value at the centre ± the greatest gradient × the half-diagonal) is built and tested against sampling before any per-brick octave switches (#240, #244).
- **Arithmetic.** Everything is `+ - * /`, `sqrt`, `abs`, comparisons and the integer hash. Hash and salt arithmetic uses `wrapping_*`; a plain `*` panicked in the debug build (#240).

### Rivers and lakes (#206, #241, #266)

- **Routing: MFD** (owner, 2026-11-28). From 20 km the main stems take the same routes as D8. Up close, MFD's rivers curve and branch, with about 3.5× as many tributary heads.
  - **Fallback:** if MFD does not fit the cost budget, D8 channels are drawn on MFD's flow.
- **Channels per brick.**
  - **Shape:** each river edge between two texels is a Hermite spline between their hash-jittered points. Its tangents are the mean flow directions at its ends, with a noise meander. So a river is smooth through every texel, and tributaries join along the main stem.
  - **Size:** width and depth follow the drainage area.
  - **Inputs:** a brick reads them from its 3×3 texels, with nothing stored below the world map (ADR 0008).
- **Width: about twice the prototype's at every flow.** The prototype gave about 6–8 m at the eye-height river and 46 m at the biggest mouth. The target is about 100 m at the biggest mouths (#206). See "Decided with the owner": #266 says about 200 m.
- **Lakes** are water up to their basin's stored spill level. Their shores take the bank Material (see "Biomes").
- **Mouths.**
  - Rivers end at the sea at the mouth.
  - In group B, rivers above a drainage threshold build a delta or a sediment fan, and the biggest few cut a submarine canyon across the shelf and down the slope (#258).
- **Caves.**
  - Caves meet rivers under the bed.
  - A river never pours into a Cave except at a karst sink (#259, group B).
  - Waterfalls where a river leaves a cliff belong to "Sky-island waterfalls and glowing Plants" (#207).
- **Water is static** until Fluids (M4).
- **Replaced:** today's `detail.rs` valley (`VALLEY`, `valley()`, up to 3 m where much drains) is replaced by the channels.

**Narrower than a texel, seen from orbit (#266).** A coarse Voxel is about one pixel (ADR 0002, ADR 0009).

- **The river tint.**
  - A coarse Voxel that a river crosses takes the water Material, and its shape does not change. So from far away a river reads as a blue line one or two pixels wide.
  - A river shows at a level only while it is at least **1/8 of that level's Voxel** wide. As the camera climbs, tributaries drop out smallest first, and the line is never more than 8× the river's true width.
  - The same rule holds at every level, so a river never pops in or jumps as the level changes. It only ends at a distance ring.
- **Canyon thickening.**
  - A canyon narrower than a texel is carved at least about one coarse Voxel wide, and narrows smoothly to its true width closer in, so its shadow line holds from far away.
  - It is thickened at a level only while it is at least 1/8 of that level's Voxel wide. Below that it fills in.
  - In group A the sub-texel canyons are the gorges the channels cut where a channel is deeper than it is wide; the canyon Landform of wave 2 uses the same rule.
- **Bounds and tuning.** Both rules stay inside each kind's per-brick bound and are tested with it. The 1/8 cutoff is tuned on the 4090 look-dev tour.
- **Records.** ADR 0002's generation rule for coarse Voxels gains the river tint, which changes the Material only, never the shape. ADR 0012's thickening exception gains canyons.

### Climate (#245)

- **Temperature.**
  - About 30 °C at the equator and −30 °C at the poles, with a lapse rate of 6.5 °C per km.
  - The snowline is about 5 km at the equator and drops toward the poles, so the 8 km giants are snow-capped even in the tropics.
- **Moisture.**
  - Earth-like wind bands (trade winds, westerlies and polar easterlies) carry moisture in off the oceans.
  - It rains out on the windward side of ranges. That gives rain shadows behind them, dry continental interiors, a wet equator and dry subtropics (about 25–35°).
- **Noise** shifts temperature and moisture by up to about 15%.
- **Storage:** temperature and moisture are 8 bits each per texel. The Biome and the blend weights are derived per brick from the 3×3 texels around it.
- **Arithmetic.** Latitude enters as the sine of latitude (the unit direction's component along the axis), never as an angle. Bands and temperatures are splines over it, so no `asin` or `cos` is needed (ADR 0003).

### Biomes (#245, #249, #259)

- **How a Biome is chosen** (#249).
  - The Landform comes first. Within its family, a place takes the Biome nearest its temperature and moisture point. There are no gaps or overlaps, and the distances give the blend weights at borders.
  - The rare Biomes (★) sit at the extremes of the variant field, in big patches covering about 10–15% of land.
  - Altitude follows from the lapse rate, so one range can run from jungle mountains at its foot to snow at its top.
- **Blended borders** (#245). Landform settings blend smoothly across Biome borders, so the land never steps. Surface Materials mix over a few hundred metres with noise. The prototype's sawtooth Material edges at the 245 m texels (#246) are what this removes.
- **The ground under a Biome** (#249).
  - A surface Material.
  - A subsoil of a set depth.
  - Optional strata bands by depth, such as the badlands stripes.
  - Bedrock by Landform: granite in ranges, basalt in volcanic land, and sandstone or limestone in sediment.
- **Shared rules** (#249).
  - Steep slopes show the bedrock.
  - Land above the snowline is snow.
  - River banks and lake shores take each Biome's bank Material: sand in deserts, gravel in cold country, mud in wetlands.
- **Strata depth.** Today's `STRATA_DEPTH` (48 m) and deep rock below stay unless a Biome's row says otherwise.
- **Added Formations** take their Biome's strata by depth below their own top, so a sky island has topsoil on top and rock below (#244).
- **Ocean Biomes** split at the shelf break, about 150 m. The shelf Biomes cover the shelf, and the deep Biomes start at the slope. Their borders blend, as on land (#258).
  - **Group A:** the split is a depth of 150 m. **Group B:** it is the profile's shelf break.
  - Reefs grow only on the warm shelf, shallower than about 40 m, including atolls (#258).
  - Today's 60 cm ice on polar seas becomes the Frozen sea Biome's surface.
- **Cave Biomes** (#259). These are about six rows in the same table, as their own family, chosen by the surface Biome's moisture, its rock and the depth:
  - lush caves under wet land;
  - dripstone in limestone and dry land;
  - ice caves under frozen land;
  - basalt and lava caves under volcanic land;
  - crystal caves under the amethyst canyon, and rarely elsewhere;
  - plain rock otherwise.

  A Cave takes the Cave Biome of where it is, and the surface keeps its own Biome.
- **Plants.** Each Biome lists its Plant kinds and a density, as data. #205, #273 and #274 build them.
- **New Materials** are added as Biomes need them. Expected: terracotta tints, ash, pumice, podzol, chalk, travertine, packed ice, pink leaves and reef. The palette's limit is 1023, and `materials.toml` has 63 Materials today.

**The catalogue (#249).** ① is wave 1, ② is wave 2, and ★ is rare. Brackets give the Formations a Biome allows; sea stacks are added to the cliff coasts (#256). Look-dev tunes the names, colours, exact Climate points and variant thresholds.

| Family | Wave 1 (26) | Wave 2 (16) |
|---|---|---|
| Range | Glacial peaks (cold; snow, ice, granite) [leaning cliffs]; Alpine highlands (cool and wet; grassy soil, granite outcrops); Rocky mountains (temperate; conifers, granite, gravel scree); Jungle mountains (hot and wet; moss, leaves on steep slopes); Arid peaks (hot or temperate, dry; rock, gravel, dry grass) | ★ Emerald peaks (temperate and wet; vivid moss) [spires, sky islands]; ★ Painted mountains (hot and dry; banded terracotta) [spires] |
| Valley | Lush valley (temperate and wet; grass, flowers, mud on banks); Boreal valley (cold; podzol, conifers); Wetlands (warm, wet and flat; mud, peat, pools; glowing flora); Tundra valley (cold and dry; gravel, moss, snow patches); Sandstone valley (hot and dry; sandstone, red sand) | ★ Cherry valley (temperate; pink blossom) [sky islands] |
| Plains | Meadow (temperate); Temperate forest (temperate and wet); Jungle (hot and wet; glowing flora); Savanna (hot, medium); Desert dunes (hot and dry); Cold steppe (cold and dry); Snowy plains (frozen) | ★ Desert oasis (hot and dry; palms, pools) |
| Coast | Sandy beach (warm or temperate); Rocky cliff coast (temperate; granite) [leaning cliffs, arches, sea stacks]; Frozen coast (cold; gravel, ice) | ★ White cliffs (temperate; chalk or limestone) [arches, sea stacks] |
| Ocean | Warm shelf (pale sand, reef); Temperate shelf (sand, gravel); Frozen sea (sea ice); Warm deep; Temperate deep; Polar deep | |
| Canyon | | Badlands canyon (hot and dry; terracotta bands) [spires]; Desert canyon (hot and very dry; sandstone) [arches]; Glacial chasm (cold; ice walls) [leaning cliffs]; ★ Amethyst canyon (crystal) [crystal spires], crystal caves |
| Plateau | | Mesa (hot and dry; terracotta, flat tops) [spires]; Blooming plateau (temperate and wet; flowers); Frozen plateau (cold; ice cap) |
| Volcanic | | Volcanic peaks (basalt, ash, obsidian, lava vents); Ashen savanna (hot; ash, dry grass); ★ Caldera (a crater lake); ★ Geothermal fields (travertine, hot pools) |

The lists give 26 Biomes in wave 1 and 16 in wave 2, 42 in all; #249's text says 25 and 17. The lists are what is built (see "Decided with the owner"). Trenches are a Landform shape, not a Biome.

### Detail below the texel (#246, #267)

From 5 km and on the ground, #246's shots read as smooth swells, because today's detail has no ridge or erosion detail below about 490 m. **The detail layer chosen in #267** fills that slot. #267 is open. A prototype on the throwaway branch `prototype/ridge-detail` (from `prototype/landform-model` 330ff37) is being built: one look-dev sheet per candidate from the 4090, each with its µs per brick from a one-thread measure, and the owner picks from them side by side. The candidates are:

- **Ridged noise along the range.** A ridged octave whose ridgelines run along the range's axis (the ridge field's direction on the world map), folded and slope-warped as #240's ridges are, with its amplitude from the range's height and steepness.
- **Gullies on the stored receivers.** Small V-shaped channels cut along hash-jittered lines that follow the world map's stored receivers below the river threshold, deeper where more drains. They make the dendritic texture of eroded slopes.
- **Talus and scree on steep ground.** Smooth aprons at the foot of slopes steeper than about the angle of repose, in a scree Material, with blocky noise above them.

Whatever is chosen keeps a proven per-brick bound and declares its reach (ADR 0012), and its cost counts against #247's total. Today's `detail.rs` rules (`factor` rising to full roughness at 600 m, `scale` from the `rough` field) are retuned for the 12 km column with it.

### Volcanoes (#257, group B)

- **Three kinds**, plus small cinder cones as detail:
  - **Cone** (stratovolcano): steep, with a summit crater, along subduction arcs. It is 10–30 km across and 1.5–4 km above the land around it (rarely 6 km), with a crater 0.3–1.5 km wide.
  - **Shield:** a broad, gentle dome with 3–8° slopes, in hotspot chains. It is 30–120 km across and 1–3 km tall.
  - **Caldera:** a collapsed crater, from either kind. It is 5–20 km wide, with a rim 200–800 m above the floor.
  - **Cinder cones:** 100–300 m cones on the flanks, made by the per-brick detail, not by map stamps.
  - **The summit cap:** a summit never exceeds the map's highest elevation (8 km). Where it would, the cone is lowered.
- **Placement: about 50–100 volcanoes a Planet.**
  - **Arcs:** one volcano every 30–60 km along each subduction arc, on land or as islands.
  - **Hotspots:** 3–6 a Planet. Each is a chain of 3–8 shields that age along the plate's motion, with the tallest at the active end.
  - **At sea:** a volcano whose top stays under the sea is a seamount (#258). Most continents have some volcanoes, but not all do.
- **Shape: a map stamp before erosion.** Each volcano's radial profile is added to the elevation, with its outline warped by noise. Erosion then cuts radial gullies down the flanks. The per-brick detail makes the sharp crater rim and everything below a texel.
- **Active volcanoes: about 1 in 5,** mostly at the active ends of hotspot chains and among the youngest arc cones. Their rock is fresher and darker. Dormant volcanoes are more eroded and greener.
- **Lava.**
  - Some active craters hold a lava lake of static, emissive `lava`. Fluids come in M4.
  - Volcanic peaks have small glowing vents and fissures.
  - Old flows down the flanks are cooled basalt and obsidian tongues, not lava.
- **Lakes.**
  - The lake fill skips active craters. Dormant craters may hold water.
  - The caldera stamp leaves one low notch in its rim, and the fill floods the floor up to it.
  - Some calderas have a central island, a resurgent dome.
  - A caldera is dry where its floor is cut open to the lowland.
- **Biomes by place on the volcano.**
  - Volcanic peaks on cones and crater rims.
  - Ashen savanna on the lower flanks and the apron, in hot and medium Climates. Elsewhere the surrounding Biome continues there, with patches of ash.
  - Caldera on caldera floors.
  - Geothermal fields on some dormant calderas and hotspot plains, through the variant field.
  - A cold volcano is snow-capped above the snowline, by the shared rule.

### Ocean floor (#258, group B)

- **Depths** are #243's: shelves 50–200 m deep, open ocean 1.5–3 km, and trenches to 4 km.
- **How it's made.** The world map's build measures each ocean texel's distance from the coast and from the nearest plate edge, with the edge's kind. Monotone splines turn those distances into the profiles, with noise on top.
- **Shelves are set by the plate edge.**
  - They are 50–200 km wide on coasts far from any plate edge, and 0–20 km wide where a trench or island arc is close.
  - The shelf break is at about 150 m, then a 3–6° slope runs down to about 3 km, then a gentle rise to the deep plain.
- **Mid-ocean ridges** run along separating plate edges and rise 1–2 km above the deep plain, with a central rift valley. Fracture zones break them into segments every 50–150 km.
- **The deep plain.** Flat sediment plains lie near the continents, with abyssal hills (100–300 m of noise) toward the ridges. Seamounts come from the volcano placement, and scattered extinct ones are added, some with flat tops (guyots).
- **Trenches** are long and asymmetric.
  - They are 30–80 km wide and follow the arc's curve for its whole length.
  - The side toward the arc is steep and the ocean side is gentle, with an outer rise 200–500 m high beyond.
  - They are deepest near the middle of the arc, down to 4 km.
  - The arc's volcanoes stand on a raised ridge behind the trench.
- **Detail: full on shelves, coarse in the deep.**
  - Shelves and the upper slope, down to about 200 m, get the full per-brick detail: rocks, sandbars, reefs and ripples. The floor shows through the static water there, whose transparency is 0.9.
  - The deep floor gets only the map's shape plus cheap noise. Deep detail comes with Fluids (M4), when players can dive.
- **Rivers:** deltas, fans and submarine canyons (see "Rivers and lakes").

### Caves (#259, group B)

Caves become a carving Formation kind, with depth measured from the Relief (#244). Today's tubes (`caves.rs`) keep their look.

| Kind | Shape | Depth below the Relief | How common |
|---|---|---|---|
| Tube system (today's random walk) | passages up to 7 m wide | 15–400 m | most land cells of 1 km, as today |
| Big chamber | a cathedral hall 20–150 m across | 50–600 m | about 1 tube system in 4 |
| Noise cave | spaghetti tunnels and cheese caverns: 3D noise inside a stamp's envelope | 15–400 m | patches over about 20% of land |
| Ravine | a slot 5–30 m wide that breaks the surface | the surface down to 200 m | a few per 100 km²; more in canyon and plateau land |
| Lava tunnel | a tube 5–15 m wide that runs downslope | 5–60 m | under most volcanic flanks |
| Crystal cave | a chamber lined with Crystal clusters | 50–600 m | always under the amethyst canyon; about 1% of cells elsewhere |

- **Depth and reach.**
  - The deepest carving is about 600 m below the Relief, up from 300 m (`caves::MAX_DEPTH`).
  - A chamber of up to 150 m keeps every Cave within today's 1 km reach (`caves::CELL`, `SPAN`), so the horizon cap's bound (#81) holds.
  - Chambers and ravines are wider than today's 7.2 m tubes, so Caves appear in coarser levels than today's 0–7. The claims in `Planet::coarse_brick` and the horizon cap that levels 8 and up never show a Cave are re-proved or changed.
- **Underground rivers: karst sinks and springs.** In limestone and dry Biomes, some surface rivers sink into a Cave. They flow underground along the world map's drainage path and come out downstream as a spring. The cave stream is static water on the tunnel floor, stepped as surface rivers are, and the surface river carries on from the spring.
- **Mouths** open mostly on steep ground: in cliff faces, valley sides and canyon walls, about one per km² in hilly land, and rarely on flat plains. Ravines and karst sinks are open by nature. Ravine sites skip river channels and lakes.
- **Light.** Lush caves have glowing Plants, crystal caves glow, and lava caves hold lava. Dripstone, ice and plain rock caves stay dark except near their mouths.
- **The sea.** Carving never goes below sea level under water (ADR 0012), so there are no Caves under the sea floor yet. Coastal Caves above sea level are allowed.

### Formations and Landmarks (#244, #256, group B)

- **The model** (ADR 0012).
  - A place is solid when it is under the Relief or inside an added Formation, and not inside a carved one.
  - The world map says where, and bricks say what. The map holds each kind's allowance, and a brick finds sites on a hashed lattice, as Crystal clusters do, and evaluates their shapes.
  - Every Formation is a distance-like field with a known greatest slope. A brick's bound is the value at its centre ± the slope × its half-diagonal.
  - A brick widens its Relief bound by a kind's reach only where the world map allows that kind. The sky shortcuts and the horizon cap's highest point (`Planet::max_radius`, which `loader.rs` reads) take the largest reach into account.
- **LOD.** A Formation agrees Voxel for Voxel at the levels where it is about 4 or more Voxels across, and in volume (within 1.5×) below that.
- **The kinds** (#256):
  - **Spire:** a banded column, tapered and roughened by noise, 15–120 m tall (rarely 250 m) and 4–8× taller than it is wide. Its sides step along the Biome's strata bands. Spires stand alone or in groups of 3–10.
  - **Arch:** either a free-standing arch (a bent, flattened band spanning 30–150 m, with its feet on the Relief or in the sea), or a window (an elliptic hole carved through a fin or headland that the same Formation adds). Carving wins, so the hole cuts through the fin.
  - **Leaning cliff:** a tilted slab grown out of a slope steeper than about 45°. It is 30–200 m tall and leans downhill by up to 30 m, so the top overhangs the foot.
  - **Sky island:** a domed top with the Biome's soil, over a jagged rocky underside that tapers downward. It is 40–400 m across and floats 150–1,500 m above the Relief. Islands come in groups of 2–6, with small fragments. A site is skipped if the underside would come within 100 m of the Relief anywhere beneath it.
  - **Crystal spire** (amethyst canyon): the Crystal cluster prism of #204, scaled up to 5–40 m long. Crystal spires grow in leaning clusters and glow.
  - **Sea stack:** a spire in the shallows just off the rocky cliff coast and the white cliffs.

| Kind | Reach above the Relief |
|---|---|
| Spire | 250 m |
| Arch | 150 m |
| Leaning cliff | 200 m (its height; the Relief under the overhang is the cliff's foot) |
| Sky island | 2 km |
| Crystal spire | 40 m |
| Landmark | its own, up to 3 km |

- **Density: rare enough to be landmarks.** From most viewpoints in the Biome you see one or two, not a forest. Look-dev tunes the numbers, and #247 bounds them. Where a Biome allows the kind, there is:
  - a spire group every 1–2 km;
  - about one arch per 10 km²;
  - a leaning slab along most steep faces;
  - a sky-island group every 5–10 km.
- **Water.**
  - Spires, leaning slabs and crystal spires skip sites on river channels and lakes.
  - Arches may span a river, and sky islands may float over water.
  - Added Formations may rise from the sea.
- **Far away.** Spires, sea stacks, arch bands and crystal spires stay at least one Voxel of each level across, until their height drops under about 4 Voxels of that level. The thickening is part of each kind's bound and is tested with it. Sky islands and leaning cliffs keep the normal rule.
- **Landmarks.**
  - The world map's build places about 30–60 a Planet from the seed, such as a 500 m arch, a sky archipelago or a 600 m spire.
  - Each is a scaled-up kind, placed only where its Biome allows that kind.
  - They are at least about 50 km apart, on land the tour can reach. A seed with too little fitting land gets fewer.
  - The list is kept on the world map, so the look-dev tour and the Spawn can find them.
- **Regions are retired** (#249). The world map's `regions` field becomes per-Biome allowances. Crystal clusters, today in `Regions::CRYSTAL_CAVES`, follow the Biome and Cave Biome allowances.

### Ores and Veins (#281, #285, group B)

Ore is ordinary Voxels of ore Materials, placed by the generator from the seed, as Crystal clusters are (#281). #285 is the build ticket; #281's resolution is its spec.

- **Ores.**
  - Coal, copper, tin, iron ore, silver and gold. Gems are the crystal Materials, in pockets.
  - Skyrite on sky islands, Emberite beside active lava, Lumenite (it glows) in crystal and lush caves, and Abyssal ore in trenches and the deepest rock.
  - A few stained-rock Materials (rusty, green).
- **Veins in four shapes,** each with a proven per-brick bound and a declared reach:
  - seams along the strata;
  - streaks (thin, winding sheets) through hard rock;
  - pockets;
  - flecks in river and beach sand downstream of gold Veins.
- **Where, by geology and depth.**
  - Coal is in the sediment under plains and valleys, and iron in sedimentary bands.
  - Copper, tin, silver and gold are in range granite and volcanic land.
  - Each fantasy ore is only in its place, and ore is richer deeper down.
- **Cues.** Veins outcrop on cliffs, cave walls and canyon sides, with stained rock round them.
- **Depends on** the bedrock by Landform, the Cave Biomes, sky islands, active lava and trenches, so it comes last in group B.

### The Spawn (#262)

The Spawn moves when this lands, so the bench paths that start there and the goldens are remade with its `GENERATOR_VERSION` bump.

- **Where: a valley with a view of a range.**
  - Valley or plains land.
  - Meadow, temperate forest or lush valley, all wave-1 Biomes.
  - 20–300 m above the sea.
  - A range of 2 km or more in sight within about 30 km.
- **Safety: every Spawn passes all of these.**
  - The land across the spawn ring varies by at most 40 m.
  - No water, river channel or lake is in the ring.
  - No Cave, cave mouth or ravine is within the ring, or a brick or two below it.
  - No Formation may be allowed overhead: no sky island, overhang or arch.
  - It is at least 30 km from an active volcano.

  The Formation and volcano rules hold trivially in group A. They become live as group B adds those things, and each group B ticket that adds one keeps the Spawn's test of it.
- **Search: the best of a few hundred seeded tries.**
  - The tries that pass the safety rules are kept, and each is scored: a range in sight, a river or lake within a few km, and a Landmark in view. The best one is taken.
  - "In sight" is a line-of-sight test over the world map's texels. The character starts facing the best view.
  - The search is a pure function of the seed and the world map (ADR 0003). It runs in the world map's build, within its time.
- **Fallback: the rules relax step by step.**
  1. Drop the view first.
  2. Then the Biome.
  3. Then the height band.
  4. The safety rules are kept to the last.

  The final fallback is today's "land that varies least", so every world map has a Spawn. The report says which step was used.
- **Replaced:** today's `Planet::spawn` and `spawn_where` (temperate land 20–100 m above the sea within the first of 4096 seeded tries).

## Data files

- **The Biome table:** `crates/planet-gen/biomes.toml`, one `[[biome]]` table a row, in the style of `materials.toml`. Every property is required, and a missing or unknown one is a load error. A row has:
  - its name and family (one of the seven Landform families, ocean, or cave);
  - its Climate point (temperature and moisture);
  - for a rare row, its variant band;
  - its surface Material, subsoil Material and depth, its strata bands by depth, and its bank Material;
  - its bedrock rule, or the family's bedrock;
  - its Formation allowances (kind and density scale);
  - its flags (glowing flora, crystal caves);
  - its Plant kinds and densities, which #205 fills.
- **Materials** are named by their `materials.toml` names and resolved to ids when the table is loaded. `materials` becomes a normal dependency of `planet-gen`, not a dev-dependency. Ids are stable (`materials.toml`), so the output depends on the ids and `GENERATOR_VERSION`, as it does now.
- **Loading.** The table is compiled in with `include_str!` and parsed once, never read from disk at run time, so every machine has the same table.
- **The table is generator input** (#249). A test pins a hash of the parsed table beside `GENERATOR_VERSION`, so an edit to the table without a version bump fails CI.
- **`materials.toml`** gains the new Materials, each with every property.
- **The Landmark list and the Spawn** are on the world map and in its cache file. Nothing else is a data file: the splines' knots, the plate counts and the feature sizes are constants in code, each with its issue number.

## Determinism and the whole machine

- **ADR 0003.**
  - Only `+ - * /`, `sqrt`, `abs`, comparisons, conversions and the integer hash, with `wrapping_*` in hash and salt arithmetic.
  - No transcendentals: no `sin`, `cos`, `asin`, `exp`, `ln` or `powf`. MFD's slope to the fourth power is two products.
  - No `mul_add`. #240 found the `target-cpu=native` build used no FMA, and it stays that way.
  - Directions come from normalised random vectors, not angles, as the caves do.
- **The same output at any thread count.** The world map's hash, the cave hash, the brick hashes and the Spawn are the same at 1, 4 and all threads. Every new stage gets the test `world_map` already has (`the_map_is_the_same_at_1_4_and_all_threads...`).
- **Parallel, never serial** (`AGENTS.md`, #264).
  - Every world-map stage is cut into fixed pieces (`PIECE`, or a fixed number of rows) with `machine::par_fill`, `par_pieces`, `par_map` or `par_chunks`.
  - Reductions are integer sums, least and greatest, or in piece order. Drainage area is an integer.
  - **FastScape** runs level by level (Barnes 2019), never as one serial stack walk.
  - **The lake fill** must not be one serial priority flood over the cube (7.5 s a step, #241). It is a basin graph (Cordonnier, Bovy & Braun 2019): pits found per piece, then a small graph of basins, with the same lakes at their spill heights (see "Decided with the owner").
  - **The Spawn's tries** run with `par_map` in try order, and ties are broken by try index.
  - **Landmarks** are placed in a fixed order of hashed candidates.
  - No shared RNG, no atomic counters handing out ids, and no `HashMap` iteration order in output.
- **Threads** come from `machine::threads()`, `bulk_threads()` and `loader_workers()`. Nothing calls `available_parallelism`.
- **Budgets scale both ways.** Every new cache or budget takes `machine::profile()` and has a floor and a ceiling, tested at both. That covers the per-thread Formation site cache, the per-thread river texel cache, and any per-thread cache of Cave cells (today's `caves::KEPT` is a fixed 48 and goes on the audit's list).
- **The low end** is 4 threads, 8 GB RAM (about 5 GiB free), 4 GB VRAM and no ray queries (`--profile low-end`).
  - It must build the full world map, cache it and start without failing, stalling or running out of memory.
  - The world map's peak RAM during the build is measured on the low-end profile and reported on the PR.
- **Each ticket's PR** carries per-thread CPU samples from before and after for the stages it changes, as `AGENTS.md` asks. It adds rows to `docs/perf/resource-audit.md` for anything not cheap, with its measured cost.

## Cost budget and the measure (#247)

- **One total.** The terrain build may add about **25–35 µs of generation per brick, over today's terrain, at the tall relief** (`--peak-m 8000 --trench-m 4000`). The exact number is set in #247, from the 4090 runs on PR #272. It is the lowest of three limits:
  - 4090 start-up at 5 s, about 45 µs;
  - low-end start-up at 15 s, about 35 µs;
  - the extra cost at which flying first misses on the 4090, 25–50 µs.

  It replaces #166's +10% rule for this effort.
- **No fixed shares.** Each ticket measures its own cost and posts it, with what is left of the total.
- **The measure** is a generator bench: the spawn ring's bricks at the tall relief, generated on one thread, in µs per brick (#247). It arrives in the first build ticket with a baseline of today's terrain, and every later ticket reports against that baseline. It runs on the dev box, which is what a ticket's merge is judged by (#238's loop).
- **The start-up gates** change in the first build ticket (owner, 2026-11-28):
  - on the default profile, time to control ≤ **5 s** (from 2 s, #220) and spawn ring ≤ **4.5 s** (from 2 s);
  - on the low-end profile, time to control ≤ **15 s**, a new limit. It must never fail, stall or run out of memory.
  - Flying keeps the zero camera-soft-miss gate. The frame gates, holes, scene memory and settle are unchanged.
- **When a ticket goes over:**
  1. Optimise first: proven bounds that skip bricks, cheaper noise, caching per column.
  2. Then give up far detail, so shapes stop at a coarser level.
  3. Then the owner decides.
- **The loop** (#238).
  - Each ticket merges on green CI plus its dev-box generator cost staying within the total.
  - The 4090's full-size real-time streaming run (`client bench --streaming --real-time` at the tall relief, default and `--profile low-end`) is once per group of 3–4 tickets, and at the end of wave 1.
  - There is one optimisation pass per group.
  - No further terrain ticket merges while a group's streaming run has a gate failing; the group's optimisation pass comes first.
  - A ticket that changes shaders, the loader, uploads or streaming code gets its own 4090 run (`docs/agents/benchmarking.md`).
- **The world map's build time** is reported per stage (`BuildTimes`, `client map-build`) on every ticket that changes it, and is held to about 2 minutes or less at full size on the dev box (#246).

## The look-dev tour (#239)

The tour is the yardstick every terrain step is judged by on the 4090, against the reference board of 66 shots (kept on the dev box at `target/refboard/board.html`; the images are third-party and stay out of the repository). It is built first, so every later step is judged by it.

- **Shots find their own feature.** Each shot searches the world map for its feature and frames it, so the tour survives terrain changes. A shot reports when its feature does not exist yet, on stderr and on the sheet. The prototype's `client lookdev --landform` (`bins/client/src/bench/lookdev/landform.rs` on `prototype/landform-model`) is the model.
- **Render settings:** 1280×720 at noon (the sun 60° up), with the existing look-dev tooling (`lookdev-pr.sh` on the 4090).
- **The shots:**
  1. orbit, the whole Planet;
  2. 30 km up, over a continent;
  3. a range from 5 km, plus a dusk variant;
  4. a valley floor, looking up at peaks;
  5. a river mouth;
  6. a canyon rim;
  7. a cliff coast;
  8. a plateau edge;
  9. a volcano;
  10. a sky island;
  11. an underground river;
  12. a crystal cave;
  13. a lake from 500 m above the shore, looking across (#206).
- **Added later:** #205 adds four shots (the forest floor at eye height, snowy conifers from 50 m, a jungle canopy from 500 m, and a forest edge from 20 km), and #285 adds two (an ore-streaked cliff from 10 m, and a Lumenite pocket in a Cave).
- **When each shot finds its feature:**
  - shots 1–4 and 6–8 from the landform model and its erosion (#246's prototype found a canyon rim 1.26 km deep);
  - shots 5 and 13 from the rivers and lakes;
  - shot 9 from the volcanoes, 10 from the sky islands, and 11 from the karst sinks;
  - shot 12 finds today's crystal caves from the start.
- **The haze.** #246's ground shots had a heavy brown haze from the atmosphere and exposure settings. It is a look setting for the tour (see "Decided with the owner").
- **Every ticket that changes generation** posts the tour's sheet from the 4090 on its PR, with the world map's debug images (`examples/world_map.rs`: elevation, family, Climate, Biome, rivers and lakes, and later allowances).

## What changes in today's code

| Where | Today | Becomes |
|---|---|---|
| `crates/planet-gen/src/lib.rs` | `GENERATOR_VERSION` 7; trilinear `value_noise` only | Bumped by every generating ticket; a `noise` module beside `value_noise` (the caves and crystals keep theirs) |
| `world_map.rs` constants | `PEAK` 2 km, `TRENCH` 1 km, `i16` elevation in 10 cm units, `ELEVATION_UNITS_PER_METRE` as both storage unit and lattice | `PEAK` 8 km, `TRENCH` 4 km, 25 cm storage units, a separate lattice constant; `Relief::DEFAULT` is the tall relief and `with_relief` stays for the bench |
| `world_map.rs` build (`generate_on`) | `raw_elevation` (jittered continent balls and 7 octaves), `normalise`, thermal `erode`, D4 `drainage`, `biome`, `regions`, `roughness` | The stages of "World-map stages": plates, shape fields, splines, stamps, ocean profiles, normalise, stream-power erosion, lakes and MFD, Climate, family, Biome, allowances, Landmarks, Spawn; `BuildTimes` gains a field a stage |
| `Texel`, `Fields`, `FieldRange`, `ElevationCell` | elevation, erosion, `Biome` (4 kinds), `Regions`, rough, drainage | The fields of the texel table; `Biome` becomes an id into the Biome table; `Regions` is retired in group B |
| `world_map/cache.rs` | `BVXWMAP1`, 8 bytes a texel | A new magic and encoding for the new texel; the Landmark list and Spawn in the file |
| `detail.rs` | 7 value-noise octaves (3 ridged), `REACH` ±23 m, `VALLEY` cut, exact-corner `bounds` | Gradient-noise octaves with the centre-plus-gradient bound, the #267 layer, no `VALLEY` (the channels replace it), a new `REACH` |
| `planet.rs` strata | `strata()` by 4 `Biome`s, `SEABED`, `ICE_THICKNESS` on snowy seas | The Biome table's surface, subsoil, bands and bedrock, blended at borders, with the shared rules |
| `planet.rs` `terrain()` shortcuts | `single_texel` + `one_layer`; `ground_range` from the map and detail | The same shortcuts, made aware of blended borders, river channels and the Formations' reach |
| `planet.rs` Spawn | `spawn`, `spawn_where`, `SPAWN_TRIES`, `SPAWN_ELEVATIONS`, `GENTLE` | A `planet/spawn.rs` with #262's rules, a facing, and the fallback report |
| `caves.rs` | Random-walk tubes, `MAX_DEPTH` 300 m, levels 0–7 | A carving Formation kind; new kinds beside it; 600 m |
| `planet/crystals.rs` | `Regions::CRYSTAL_CAVES` | The Biome and Cave Biome allowances |
| `tests/golden.rs` | Hashes of version 7 | Remade with every bump |
| `examples/ring_throughput.rs`, `map_build.rs`, `world_map.rs` | Timings and debug images | The generator bench (µs per brick on one thread on the spawn-ring set) and the new debug images |
| `bins/client/bench-budgets.toml`, `bench/budget.rs`, `bench/realtime.rs` | time to control and spawn ring 2 s; no low-end limit | 5 s and 4.5 s; a low-end time-to-control limit of 15 s |
| `bins/client/src/bench/lookdev/` | #148's recorded shots near the Spawn (`shots.rs`, `finder.rs`) that use `Biome` and `Regions` | The tour (a new `tour.rs`); the recorded shots move to Biome ids and are re-recorded when the Spawn moves |
| `bins/client` start-up | starts at `Planet::spawn` | starts at the Spawn, facing its view |

**Records to update as the build lands:**
- ADR 0008: the world map's build of about 2 minutes, 8 km peaks, rivers made per brick from stored routing.
- ADR 0002: the river tint in the generation rule for coarse Voxels.
- ADR 0012: canyons in the thickening exception.
- `docs/spec.md`: the Terrain row (it still says ±2 km relief, 4 biomes and caves to about 300 m).
- `docs/agents/benchmarking.md`: the new gates and the generator bench.
- #201's acceptance: "reported against #166's numbers" becomes "against #247's budget".

## Build tickets (filed 2026-11-28)

They land in this order, in place on main, each bumping `GENERATOR_VERSION`. Each merges on green CI with its generator cost inside the budget. The 4090 streaming run comes once per group of three or four tickets (owner, 2026-11-28).

**Group A, wave 1 (the show-friends cut):**

- #287 The start-up gates move to 5 s, a generator bench measures µs per brick, and the ground column grows to 12 km
- #288 The look-dev tour finds its own shots on the Planet
- #289 Gradient noise, warps, ridges and splines join the generator with proven bounds
- #290 Plates and splines shape the continents, ranges and coasts on the world map
- #291 Stream-power erosion carves the valleys, lakes fill to their spill height, and MFD routes the rivers
- #292 Rivers and lakes are carved per brick, and narrow rivers and canyons still read from orbit
- *4090 group run and an optimisation pass*
- #293 Climate and the Biome table choose the wave-1 Biomes
- #294 Biome borders blend, and the land takes each Biome's surface, strata and bedrock
- #295 The detail layer chosen in #267 makes ranges crisp from 5 km and on the ground
- #296 The Spawn is a temperate valley with a range in view
- *End of wave 1: 4090 run, the whole tour, an optimisation pass*

**Then** #205 Trees and fallen logs.

**Group B:**

- #297 Formations become one interface, Caves become a carving kind, and Biome allowances replace Regions
- #298 Big chambers, noise caves and ravines open the land to 600 m
- #299 Volcanoes rise on the arcs and hotspots, with calderas, lava and the volcanic Biomes
- *4090 group run and an optimisation pass*
- #300 Cave Biomes, lava tunnels, crystal caves and karst rivers fill the underground
- #301 The ocean floor gets shelves, ridges, trenches, seamounts and reefs
- #302 Spires, arches, leaning cliffs and sea stacks stand where their Biomes allow them
- *4090 group run and an optimisation pass*
- #303 Sky islands, crystal spires and the Landmarks are placed
- #304 The wave-2 Biomes: canyons, plateaus and the rare Biomes
- #285 Ores and Veins in the ground
- *4090 group run and an optimisation pass*

**Then** #273 Ground cover and undergrowth, and #274 Rare and hanging Plants.

## Decided with the owner (2026-11-28)

1. **Elevation storage.** #246 left the real storage to the spec. **Decided (owner, 2026-11-28):** an `i16` in 25 cm units (±8.19 km), as the prototype had, with the 10 cm noise lattice kept as its own constant. It holds 8 km peaks and 4 km trenches at 25 cm, far finer than a 245 m texel needs, and keeps the texel small. An `i32` would double the field for nothing seen.
2. **The biggest river's width.** #206 says about 100 m at the biggest mouths; #266 says the biggest, about 200 m across, stay visible to Voxels of about 1.6 km. At 100 m and the 1/8 cutoff, a river shows up to level 13 (410 m Voxels), not high orbit. **Decided (owner, 2026-11-28):** build #206's 100 m, and tune the cutoff on the tour so that the main stems still read from high orbit, as #266 allows ("the build ticket tunes the 1/8 cutoff").
3. **Plateau and canyon land before wave 2.** The landform model makes plateaus and canyons in group A, but their Biomes are wave 2. **Decided (owner, 2026-11-28):** until wave 2, a plateau takes the nearest plains Biome and a canyon the nearest valley Biome by Climate, with its shape as built.
4. **Biome counts.** #249's lists give 26 Biomes in wave 1 and 16 in wave 2; its text says 25 and 17. **Decided (owner, 2026-11-28):** build the lists as written, 42 in all.
5. **Uplift for the erosion.** #241 left the uplift source open, and suggested the range belts. **Decided (owner, 2026-11-28):** uplift from the plates' collision belts, so the erosion keeps the ranges where the plates put them.
6. **The lake fill.** The prototype filled lakes with one priority flood over the cube, which is serial (7.5 s a step at full size, #241) and was not deterministic. **Decided (owner, 2026-11-28):** #241's basin graph, which is parallel and gives the same lakes at their spill heights.
7. **The world map's build on the low end.** No gate covers it: about 2 minutes is the dev box's figure, and a 4-thread machine takes longer once, then hits the cache. **Decided (owner, 2026-11-28):** report it on `--profile low-end` from the landform ticket on, and set a limit (about 5 minutes) only if the number calls for one.
8. **What the generator bench measures.** #247 names the spawn ring's bricks. The Spawn moves to a gentle valley in group A, which costs less than the ranges a flight crosses. **Decided (owner, 2026-11-28):** keep the spawn ring as the gated figure, and report two fixed sets beside it, not gated: a range and a coast.
9. **The tour's haze.** **Decided (owner, 2026-11-28):** the tour ticket sets the tour's exposure and atmosphere settings so the ground shots read clear, with no shader change. A shader change, if one is needed, is its own ticket with `needs-benchmark`.
10. **The determinism job and the full world map.** `tests/golden.rs` builds the full 2048² world map, and the `determinism` job runs it in a debug leg as well as release. That debug leg builds `planet-gen` at `opt-level = 0` on purpose (`.github/workflows/ci.yml`), so that unoptimised code is checked against the same hashes. At about 2 minutes in release, the full map could take an hour or more unoptimised. **Decided (owner, 2026-11-28):** keep the debug leg unoptimised. Have it check a full-radius world map at 512² a face, which runs every stage, plus the brick and cave hashes. The full 2048² map's hash is checked in the release leg only. The landform ticket (#290) measures the debug leg first, and makes the change only if the leg goes past about 20 minutes.
11. **Issue records.** These need editing to match this order: #205's "Blocked by" becomes the last group A ticket, not the last terrain ticket. #285's "Blocked by #274" becomes the wave-2 Biome ticket, since it now lands in group B (owner, 2026-11-28) and #281's resolution placed it after the vegetation. **Decided (owner, 2026-11-28):** edit both when the tickets are filed.
