# BhayanakVoxel

A detailed voxel game set on a spherical planet, where players reshape a finely voxelized world and can zoom continuously from their character out to orbit.

## Language

### World

**Planet**:
A spherical world the game takes place on; voxel all the way through and editable at every point. The Vertical slice has one.
_Avoid_: World, map, level

**World map**:
The Planet's coarse description, made once from the seed: its continents, Climate, Biomes and large rivers, one texel to about 240 m. Everything finer is made when a brick is generated.
_Avoid_: Map (alone)

**Voxel**:
The smallest unit of matter on the Planet, 5 cm across (ADR 0007), much smaller than a Minecraft block. Sizes in the game are in metres; the Voxel is the unit of the Planet's coordinates.
_Avoid_: Block, cube

**Exposed**:
Said of a solid Voxel with air against one of its faces. The Planet's surface is made of its exposed Voxels.
_Avoid_: Visible, surface Voxel

**Material**:
The kind of matter a Voxel is made of (e.g. rock, sand, water, glass), carrying properties like hardness, emissiveness, or whether a character can walk through it (leaves and grass can).
_Avoid_: Block type, voxel type

**Spawn**:
The one place on a Planet where every character starts, chosen from the seed.
_Avoid_: Spawn point, start position

**Debris**:
A group of Voxels detached from the Planet by Edits that moves as a physical rigid body.
_Avoid_: Fragment, chunk (reserved for storage, if used at all)

**Fluid**:
A Material whose Voxels flow and settle over time (water, sand, lava).
_Avoid_: Liquid (sand is a Fluid too)

### Terrain

**Cave**:
Air that the generator carves under the Planet's land, in tunnels, chambers and ravines, some of which open to the surface as cave mouths. Each Cave has its Cave Biome. A part of the Planet's generated terrain, not an Edit.
_Avoid_: Dungeon, hole (a hole is dug)

**Climate**:
The conditions at a point on the Planet: its temperature and its moisture. It chooses the Biome.
_Avoid_: Weather, environment

**Landform**:
The kind of shape the land takes at a place, such as a range, plateau, canyon, valley, plains, coast, volcanic land or ocean.
_Avoid_: Terrain type, relief

**Biome**:
A named kind of land with its own surface and life, chosen by the Climate from those its Landform allows (a canyon in a hot, dry Climate is badlands), or, for a Cave Biome, by the land above it and its depth.
_Avoid_: Zone, region

**Cave Biome**:
The Biome of a Cave, chosen by the surface Biome's moisture and rock above it and by its depth, such as lush caves under wet land or dripstone in limestone. The surface above keeps its own Biome.
_Avoid_: Cave type, underground biome

**Relief**:
The height of the Planet's solid surface at each place, the sea floor included: one height per place, so it has no overhangs. Most of the terrain is the Relief alone.
_Avoid_: Heightfield, ground (the Ground is what is generated around a character), elevation (the world map's coarse share of it)

**Formation**:
A 3D shape the generator adds to the Relief or carves from it, only where a Biome or a Landmark asks for one: a leaning cliff, a spire, an arch, a sky island. A place is solid when it is under the Relief or inside an added Formation, and not inside a carved one; carving wins. Caves are carved Formations. Part of the generated terrain, not an Edit.
_Avoid_: Overhang (one kind), stamp, 3D layer

**Landmark**:
One of the few giant one-off Formations placed across the Planet from the seed, such as a 500 m arch or a sky archipelago, each where its Biome allows its kind. The everyday spires and arches are just Formations.
_Avoid_: POI, wonder, structure (structures are buildings and villages)

**Ore**:
A Material dug to be turned into metal, fuel or gems, such as iron ore, coal or Skyrite. Ore lies in the ground as ordinary Voxels.
_Avoid_: Resource (too broad), mineral

**Vein**:
One body of an Ore in the ground, whatever its shape: a seam along the strata, a winding streak, a pocket, or flecks in sand. Part of the generated terrain, not an Edit.
_Avoid_: Deposit, node, lode

**Plant**:
One generated growth of Voxels that a Biome places: a tree, shrub, grass tuft, flower, fern, vine or fallen log, each grown differently from the seed. Its Voxels are ordinary Voxels, so digging cuts it like anything else, and the Planet does not remember which Voxels belong to which Plant. Part of the generated terrain, not an Edit.
_Avoid_: Species, foliage, flora, vegetation (the informal name for Plants together)

**Plant kind**:
A template that grows Plants, such as a conifer, a broadleaf tree or a fern, with its shape and Materials. Each Biome lists the Plant kinds it grows and how densely.
_Avoid_: Species, tree type

**Crystal cluster**:
A few radiating hexagonal prisms with pointed tips, of one of eight emissive crystal Materials, that the generator grows from a sunlit slope or the wall of a Cave, along the surface's normal. Part of the generated terrain (it fills air only), not an Edit. Each Cave has a dominant hue.
_Avoid_: Crystal (the Material of one prism), geode

**Placement rule**:
A test of a place that depends on the seed alone (the world map and the Caves' density, never the order bricks are made in), so that every brick, at any level, and the server agree on what is there: sunlit (slope, aspect, open sky), Cave wall (the density's gradient) and room (the local density). Content is placed by them.

### Building and playing

**Edit**:
A player-caused change to the Planet's Voxels: digging (removing) or placing.
_Avoid_: Modification, build

**Brush**:
The shape and size an Edit applies to the Planet (e.g. a 1 m sphere).
_Avoid_: Tool size (a Tool is what a character holds), cursor

**Piece**:
A ready-made Voxel model, such as a brick Piece or a log, about a metre across, that a player places whole in a single Edit. Once placed it is ordinary Voxels and the Planet does not remember it. A Piece snaps to any face of the Piece it is placed against, in line or staggered, and turned square to it; with nothing to snap to it starts a new build.
_Avoid_: Block, prefab, tile, brick (a brick is the 8×8×8 Voxel storage unit; say "brick Piece")

**Tool**:
What a character holds to dig in Survival: an axe, a shovel or a pickaxe, each of a tier. Every Material names the Tool that digs it. A wrong Tool or bare hands dig it far more slowly but never not at all, and a better tier digs faster. A Tool decides which Brushes its holder can dig with. Placing needs no Tool. Not in Creative.
_Avoid_: Item, weapon

**Yield**:
What a dig gives a player in Survival: the volume of each Material it removed, shown in litres.
_Avoid_: Drop, loot, resources

**Ledger**:
The litres of each Material that a player or a Container holds in Survival. Yield fills it; crafting spends from it, and so does pouring loose ground with a shovel.
_Avoid_: Pack (the thing that sets its size), inventory (informal: a player's Ledger plus slots of Tools and crafted Pieces)

**Container**:
A placed chest or crate that holds a Ledger and stacks of crafted Pieces: the one kind of placed thing the Planet remembers. Digging into it breaks it and spills what it held as Materials.
_Avoid_: Storage block, inventory

**Clipboard**:
The region of Voxels a player has copied in Creative and not saved, ready to Paste.
_Avoid_: Selection (what is outlined before it is copied), buffer

**Blueprint**:
A saved region of Voxels in a player's library, kept by the server and exportable as a file. In Creative it is Pasted; in Survival it shows as a Ghost that the player builds into with Pieces.
_Avoid_: Prefab, schematic, design

**Paste**:
The Edit that stamps a Clipboard or Blueprint onto the Planet at a position, turned in 90° steps. Afterwards it is ordinary Voxels.
_Avoid_: Clone, place (placing is one Piece)

**Creative**:
The way to play with every Brush and Material free and no Tools, as in the Vertical slice.
_Avoid_: Sandbox mode, build mode

**Survival**:
The way to play where a character gathers, carries an inventory, has health, and digs with Tools.
_Avoid_: Adventure mode

**Ghost**:
The preview of a Brush where the next click would apply it, showing exactly the Voxels the Edit would change. In Survival a Blueprint also shows as a Ghost, marking the Voxels still missing.
_Avoid_: Cursor, outline

**Journal**:
The durable, ordered record of the Edits the server has accepted since the last checkpoint of the diverged bricks. Replaying it on top of that checkpoint gives the Planet as it was (ADR 0010).
_Avoid_: Edit log, write-ahead log, history (the undo history is another thing)

### Experience

**Zoom**:
The continuous camera transition from a player's first/third-person view out to an orbital view of the whole Planet and back. It moves the camera, never the character.
_Avoid_: Map view, space view

**Primary view**:
The view of the Planet, drawn every frame, that a player plays in.
_Avoid_: Real-time view, game view

**Reference view**:
The unbiased view of the same Planet that the Primary view is measured against. It is shown when the view is Still and the scene has caught up with the camera (or the catch-up cap has passed, ADR 0005), and gets cleaner the longer it stays so.
_Avoid_: Ground truth, path-traced mode, reference mode

**World clock**:
The time of day of the Planet: a fraction of the day, how long the day is (30 minutes by default) and whether it is paused. It moves the sun and the Moon, and in M2 the server holds it and sends it. It always runs, whatever the camera does; what is shown is held only while the Reference view is accumulating a Still view, and catches up with the clock when the view moves again.
_Avoid_: Game time, sun position

**Moon**:
A dim, cool directional light and a soft disc in the sky at night, opposite the sun. It is the one shadowed **key light** when it is stronger than the sun at the camera, and each light fades to nothing before the other takes over, so the shadows turn while there is no key light. Not physical: the hero look over physical blue (#203).

**Still**:
The camera, the sun and the window have not changed for 2 frames and no Edit has happened. The Reference view is shown only while the view is Still, and after a flight only once the scene has caught up with the camera; any change cuts to the Primary view at once and the Reference view starts again from sample 0.
_Avoid_: Idle, paused

### Walking

**Ground**:
The part of the Planet around a character that both client and server have generated, which the character stands on and touches. It follows the character; what lies beyond it is not yet known.
_Avoid_: Terrain cache, collision set

**Leash**:
The region a character is confined to. The server keeps it within the Ground and moves it as the character walks; beyond it the character is blocked.
_Avoid_: Tether, bounds

### Visibility and streaming

**Visible set**:
The bricks that could contribute a pixel this frame: inside the camera frustum, inside the horizon cap, and not provably occluded. A property of the camera, not of what is loaded.
_Avoid_: Render set, draw list

**Resident set**:
The bricks the client actually holds at their selected LOD. It must contain the visible set, and is normally a complete 360° shell around the camera.
_Avoid_: Loaded set, cache

**Shell**:
The complete 360° resident set around the camera, made of one ring per LOD.
_Avoid_: Ring set, sphere

**Ring**:
One level's part of the shell.
_Avoid_: LOD level (the level is the data; the ring is the residency)

**Guard**:
The always-resident coarse copy of the whole Planet, kept so a late fine brick is a coarser frame rather than a hole. Never the target LOD. At the start only the part that a camera near the Spawn could see is made before play begins, and the rest follows in the background; until it is whole a flying camera is held near the Spawn.
_Avoid_: Fallback LOD, backup

**Soft miss**:
A frame where a brick inside the resident set is drawn at a coarser level than the LOD band asks for at the camera's speed. A coarser level that the band itself selects for a fast camera is not a soft miss. Not a hole, but a visible pop; a soft miss inside the resident set is a bug.
_Avoid_: LOD miss, pop

**Diverged brick**:
A brick whose content differs from what the generator would produce for its coordinate — an Edit, settled Fluid, or merged Debris. The only bricks that must be persisted, and the only ones eviction never drops (with the units over them and beside them).
_Avoid_: Dirty brick, edited brick

**Unit**:
A brick together with its eight children: the store keeps the children of a brick resident, or evicts them, together. Eviction takes whole Units, and never one that is the Guard's, wanted this frame, or is over or beside a diverged brick.
_Avoid_: Chunk, node (the selection's nodes are the bricks drawn and split)

**Prefetch**:
Work issued early because the camera is predicted to need it. It hides latency; it is not what makes residency correct.
_Avoid_: Speculative loading, preload

**Horizon cap**:
The spherical cap of the Planet visible from a camera at height h, with half-angle arccos(R/(R+h)). The exact global occluder.
_Avoid_: Horizon cull, planet cull

**LOD band**:
The distance interval that selects one level: from 2^L·d0 to 2^(L+1)·d0, where d0 is the distance at which a level-0 Voxel covers one output pixel while the camera is slow, and shrinks as the camera moves faster, so a fast camera selects coarser levels.
_Avoid_: Ring (the ring is the residency; the band is the selection rule)

### Planning

**Vertical slice**:
The first playable build that proves the engine's hardest claims; specified as ordered Milestones.
_Avoid_: MVP, demo

**Milestone**:
One ordered stage of the plan in `docs/spec.md`: M0 to M5 make the Vertical slice (the skeleton; Planet + Edits + path-traced lighting + Zoom; multiplayer; Debris; Fluids; mid-range GPUs), and M6 to M8 follow it.
_Avoid_: Phase, sprint
