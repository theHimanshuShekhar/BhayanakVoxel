# BhayanakVoxel

A detailed voxel game set on a single spherical planet, where players reshape a finely voxelized world and can zoom continuously from their character out to orbit.

## Language

### World

**Planet**:
The single spherical world the game takes place on; voxel all the way through and editable at every point.
_Avoid_: World, map, level

**Voxel**:
The smallest unit of matter on the Planet, much smaller than a Minecraft block.
_Avoid_: Block, cube

**Exposed**:
Said of a solid Voxel with air against one of its faces. The Planet's surface is made of its exposed Voxels.
_Avoid_: Visible, surface Voxel

**Material**:
The kind of matter a Voxel is made of (e.g. rock, sand, water, glass), carrying properties like hardness or emissiveness.
_Avoid_: Block type, voxel type

**Edit**:
A player-caused change to the Planet's Voxels: digging (removing) or placing.
_Avoid_: Modification, build

**Brush**:
The shape and size an Edit applies to the Planet (e.g. a 1 m sphere).
_Avoid_: Tool size, cursor

**Debris**:
A group of Voxels detached from the Planet by Edits that moves as a physical rigid body.
_Avoid_: Fragment, chunk (reserved for storage, if used at all)

**Fluid**:
A Material whose Voxels flow and settle over time (water, sand, lava).
_Avoid_: Liquid (sand is a Fluid too)

### Experience

**Zoom**:
The continuous camera transition from a player's first/third-person view out to an orbital view of the whole Planet and back.
_Avoid_: Map view, space view

### Walking

**Ground**:
The level-0 bricks around one character, generated from the seed by both the client's predictor and the server, so that a collision query reads them and never generates one. Evicted behind the character. Not the Resident set, which is the camera's, and which collision never reads.
_Avoid_: Terrain cache, collision set

**Leash**:
The ball of Voxels that a character may be in, which the server moves as the Ground around the character becomes ready, and tells the client from which input it holds. Space beyond it is solid.
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
The always-resident coarse copy of the whole Planet, kept so a late fine brick is a coarser frame rather than a hole. Never the target LOD.
_Avoid_: Fallback LOD, backup

**Soft miss**:
A frame where a brick inside the resident set is drawn at a coarser level than the LOD rule asks for. Not a hole, but a visible pop; a soft miss inside the resident set is a bug.
_Avoid_: LOD miss, pop

**Diverged brick**:
A brick whose content differs from what the generator would produce for its coordinate — an Edit, settled Fluid, or merged Debris. The only bricks that must be persisted.
_Avoid_: Dirty brick, edited brick

**Prefetch**:
Work issued early because the camera is predicted to need it. It hides latency; it is not what makes residency correct.
_Avoid_: Speculative loading, preload

**Horizon cap**:
The spherical cap of the Planet visible from a camera at height h, with half-angle arccos(R/(R+h)). The exact global occluder.
_Avoid_: Horizon cull, planet cull

**LOD band**:
The distance interval that selects one level: from 2^L·d0 to 2^(L+1)·d0, where d0 is the distance at which a level-0 Voxel covers one output pixel.
_Avoid_: Ring (the ring is the residency; the band is the selection rule)

### Planning

**Vertical slice**:
The first playable build that proves the engine's hardest claims; specified as ordered Milestones.
_Avoid_: MVP, demo

**Milestone**:
One ordered stage of the Vertical slice: M1 Planet + Edits + path-traced lighting + Zoom; then multiplayer; then Debris and Fluids.
_Avoid_: Phase, sprint
