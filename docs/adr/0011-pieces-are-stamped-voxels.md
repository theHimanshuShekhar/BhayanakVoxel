# Pieces are stamped Voxels, not objects

Building is done with **Pieces** (`CONTEXT.md`): ready-made Voxel models one metre across, such as a brick wall, a log or a stair, placed whole in one Edit. It was decided in the game-direction review of 2026-11-11. A placed Piece is ordinary Voxels. The Planet does not remember that it was a Piece: it can be carved by any Brush, it shows at every LOD, and it will fall as Debris, with no storage, replication or rendering of its own.

We chose it because 5 cm Voxels make free-hand architecture impractical (a small house is thousands of careful Edits), while a remembered object would be a second representation of matter, which ADR 0002 rejects.

- **A Piece is data**: a shape and a Material pattern, in a data file like the Materials. The pattern is a function of position in the Piece's own space, so mortar lines and tree rings stay straight whatever way the Piece is turned.
- **A Piece is an Edit operation of the usual size** (ADR 0003): which Piece, where, which way. Every client stamps the same Voxels.
- **A Piece lines up with the Piece it is placed against.** The first Piece of a build stands on the Planet's local up and faces the way the camera looks; later ones take their frame from their neighbour, so a build has one straight grid of its own. Two builds begun apart do not line up.
- **Voxels are on one lattice for the whole Planet**, so a build that is tilted against it has faces stepped at 5 cm, as a cube Brush has today. Accepted.
- **Pieces are static.** A doorway is a gap. Pieces that move (doors, hatches) come after the Vertical slice, built on Debris's moving bodies.

## Considered Options

- **Pieces as remembered objects** (Minecraft's blocks): they break whole and can be picked up, which survival would like. A second store beside the bricks, with its own saves, replication and LOD, and rules for a Piece half dug away.
- **Stamped, with a record of which Piece it was:** lets survival give a whole Piece back later. A record per placed Piece in every save, for a rule not yet designed; it can be added when survival is.
- **A Planet-wide building grid:** every build lines up, but builds lean against local up away from a few places, and seams meet where the grids do.
- **Builds stored in their own turned frame:** crisp faces everywhere; a second representation again.
- **Construction Brushes only** (line, box, wall): no ready-made detail, so every wall is a flat slab of one Material.

## Consequences

- A one-metre Piece is 8,000 Voxels, an eighth of a second of the Edit rate (ADR 0007): about eight Pieces a second at the most.
- A Piece cannot be taken back whole; it is dug like anything else. Undo (ADR 0010) covers a mistake.
- A long build drifts from the ground's curve: its grid is flat and the Planet is not. At 320 km that is about 1.5 cm in 100 m.
- Snapping must find the neighbouring build's frame from Voxels that do not remember it: the client keeps the frame of the build it is working on, and a build picked up again later is matched from its faces. How well that works is the main risk, and the Piece ticket must show it.
- Pieces need glass and emissive Materials to look right (panes, lamps): they are opaque or plain until M1b.
