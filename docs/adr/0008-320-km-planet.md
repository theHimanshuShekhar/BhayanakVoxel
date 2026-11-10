# A 320 km Planet with a coarse world map

The Planet's radius is 320 km, twenty times the 16 km of [Choose Voxel size & Planet radius](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/8). This supersedes that radius, and the sentence in ADR 0007 that let it stand; the Voxel stays 5 cm. It was decided in the plan review of 2026-11-10. A 16 km Planet is crossed in half an hour of flying and shows a horizon 250 m away from the ground; at 320 km the horizon is about 1.1 km away, the surface is 1.3 million km², and the view from orbit is of a world and not of a hill.

The switch is made early in M1a-2, right after the streaming bench exists and before the rest of the streaming work (#143–#146), so that every later budget is measured at the real size.

What makes a Planet this size affordable is that nothing the player pays for scales with its area:

- **The world map stays one global cube map of 6 × 2048² texels**, which is about 240 m a texel at this radius (it was about 12 m). It carries only what is large: continents, climate, biomes and the large drainage. It still takes well under a minute.
- **Everything finer is made at generation**, by deterministic functions evaluated per brick: relief, small valleys, caves, strata, placement. Nothing is precomputed below the map's texel, so there are no seams and the result is bit-identical on every machine, as ADR 0003 needs.
- **Features stay metric.** Peaks are 2 km, caves and strata keep their sizes (ADR 0007). Continent and biome noise gets a fixed size in km (continents of about 30–60 km), so a bigger Planet has more continents, not bigger ones. The core scales with the radius.
- **The Guard gets coarser, not bigger.** It is still the whole Planet in about 74 MiB and about 5 s, which puts it about four levels coarser than levels 9–13. The prefetch simulator, run again at 320 km, sets the exact levels.
- **The Zoom reaches about 20 R** (6,400 km), not the 20–50 R of #8. The pyramid grows from 14 levels to 18 (level 17's Voxels are 6.6 km, under a pixel at 20 R).
- **There is one seeded world spawn**, chosen by the server from the seed, where every character starts.
- **There is no travel aid.** The Planet is crossed by walking and by flying at 50 m/s (11 hours around), and seen whole from the Zoom. The Zoom does not move the character.

## Considered Options

- **Stay at 16 km:** everything measured stays valid. Rejected: the horizon and the orbital view are too small for the game the art direction describes.
- **240 km (15×):** the same work as 320 km for a smaller result.
- **A finer world map** (keep 12 m texels): 20× the resolution a side, and erosion scales with size³. Minutes to hours a seed.
- **Regional maps, made on demand:** erosion does not tile, so seams, or a blend that is no longer the eroded result.
- **A Zoom to 50 R:** 19–20 levels and an orbital view of a Planet a few dozen pixels across. 20 R already shows the disc whole.
- **Travel by Zoom** (zoom out, pick a place, zoom in): cheap to build, but it makes the Planet's size a menu. Left out of scope; it can be added without changing anything here.

## Consequences

- `GENERATOR_VERSION` goes up; every existing world and golden is remade.
- The radius is one constant. Today `planet-gen` holds it twice (`WORLD_RADIUS` and `NOISE_RADIUS` in `world_map.rs`).
- **Terrain between 240 m and a few metres has no precomputed erosion.** It is noise shaped by the map's fields, and how good that looks is the main risk of this decision. The look-dev prototype (#148) and a landscape golden judge it.
- **Rivers need a new design** in M1b-1: the map gives drainage at 240 m, and a river is narrower than that.
- **The renderer cannot hold the scene in Voxels in `f32`.** The Zoom at 20 R is 128 million Voxels from the centre, past the 2^24 that a float holds exactly (ADR 0007 relied on 16 million). The scene bound (`MAX_BOUND_RADIUS`) and far ray lengths are stored in coarse units; #58 carries that with the Zoom. Coordinates stay inside `i32`, and an Edit's coordinates inside `sim`'s 2^24 (838 km).
- `prefetch-sim` keys a brick in 20 bits an axis, which a 320 km Planet overflows; it is widened with the re-run.
- The shell reaches a horizon four and a half times as far, so it has more rings. The bands are in Voxels, so each ring holds about as many bricks as before; the scene and store sizes are measured again and the 512 MiB limit re-decided on the numbers.
- Everything measured at 16 km is stale: the Guard's levels and bytes, the shell sizes of ADR 0004, the world map's time, and the constants ADR 0007 lists.
