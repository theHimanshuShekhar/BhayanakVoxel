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

**Material**:
The kind of matter a Voxel is made of (e.g. rock, sand, water, glass), carrying properties like hardness or emissiveness.
_Avoid_: Block type, voxel type

**Edit**:
A player-caused change to the Planet's Voxels: digging (removing) or placing.
_Avoid_: Modification, build

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

### Planning

**Vertical slice**:
The first playable build that proves the engine's hardest claims; specified as ordered Milestones.
_Avoid_: MVP, demo

**Milestone**:
One ordered stage of the Vertical slice: M1 Planet + Edits + path-traced lighting + Zoom; then multiplayer; then Debris and Fluids.
_Avoid_: Phase, sprint
