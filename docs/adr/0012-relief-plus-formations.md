# Terrain is a Relief plus Formations

The Planet's shape is a **Relief** (`CONTEXT.md`): one height per place, made of the world map's elevation and per-brick detail. **Formations** add to the Relief or carve from it where a Landform or a landmark asks for one: leaning cliffs, spires, arches, sky islands, and the Caves. It was decided on 2026-11-25 in "Terrain shape model" (#244), on the terrain map (#238), so that the Planet can have Tectonic- and Terralith-like shapes while most bricks are still decided without sampling a Voxel.

We chose it because nearly all of a 320 km Planet's surface is one height per place. A height bound decides a brick as sky, sea, deep rock or one stratum in a few lookups. A full 3D density everywhere would have to sample every brick near the surface and rebuild that bound in three dimensions, for overhangs that only a few Landforms need.

- **A place is solid when it is under the Relief or inside an added Formation, and not inside a carved one. Carving wins**, so a Cave can cut through an arch.
- **The world map says where; bricks say what.** The map stores a per-texel allowance for each Formation kind, from the Landform, Biome and landmark fields. A brick finds Formation sites on a hashed lattice (as crystal clusters do) and evaluates their shapes. Placement depends on the seed alone, so every brick, at any level, and the server agree. This amends ADR 0008: the map carries the allowances, and the shapes are among the things made per brick. Large one-off landmarks may later be placed at map build.
- **Every shape keeps a proven per-brick bound.** A Formation is a distance-like field with a known maximum slope: an analytic stamp (cone, capsule, polytope, ellipsoid, with noise displacement) or 3D density noise inside a stamp's envelope. A brick's bound is the value at its centre plus or minus the slope times its half-diagonal. Each kind declares its largest reach from the Relief, and a brick widens its Relief bound by that reach only where the map allows the kind. The Relief's own noise is held to the same rule: gradient noise goes in only after its centre-plus-gradient bound is built and tested. Each shape ships with a test of its bound against sampling.
- **LOD (ADR 0002):** a Formation agrees Voxel for Voxel with the pyramid at the levels where it is about 4 Voxels across or more, and in volume (within 1.5×) below that, as crystal clusters do.
- **Caves become a carving Formation kind**, with depth measured from the Relief. Further cave kinds (chambers, noise caves) plug in the same way.
- **Placement rules (#204) read the full shape**: the Relief, Formations and Caves, through one solid field. The sunlit rule's open sky still comes from the world map.
- **The sea:** added Formations may rise from the sea floor or the shore (sea stacks, coastal arches). Carving Formations never carve below sea level under water, so there are no flooded voids before Fluids.
- **Materials:** an added Formation takes its Biome's strata by depth below its own top, so a sky island has topsoil on top and rock below.

## Considered Options

- **A full 3D density everywhere** (Minecraft 1.18 and later): the most freedom, but every brick near the surface is sampled and the bounds must be rebuilt in 3D.
- **Adding wins over carving:** Caves would stop at an arch or a sky island.
- **Formations stored as instances on the world map:** exact control, but millions of entries on a 1.3 million km² Planet and a slower map build.
- **Formations from hash and noise alone, with no map field:** cheap, but the shapes could not follow the Landform and Biome.
- **Bounds from margins measured over many seeds:** cheaper to build, but a rare miss is a hole or a wrong brick, against ADR 0002 and ADR 0003.

## Consequences

- A thin spire or arch can vanish or break up at coarse levels. Keeping such features readable from far away is a separate question on the terrain map.
- The highest point a Formation reaches (about 2 km above the Relief for a sky island) raises the sky shortcuts' and the horizon cap's highest ground where Formations are allowed.
- Today's Caves code moves behind the Formation interface. The generator version and the goldens change when the terrain build lands; there are no saves yet, so no migration is needed.
