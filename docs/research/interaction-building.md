# Research: placing, snapping, building helpers and structural integrity in building games

Resolves issue [#277](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/277), part of the map [#275](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/275) "Player interaction: dig, gather, carry, craft, build and place". Researched 28 November 2026. Terms follow [`CONTEXT.md`](../../CONTEXT.md) (**Planet**, **Voxel**, **Material**, **Edit**, **Brush**, **Tool**, **Piece**, **Debris**, **Creative**, **Survival**), with the Tool, Creative, Survival and snapping additions from PR #248 (branch `docs/terrain-glossary`).

How to read the labels:

- **[official]** means the developer's own post, docs or wiki, or the engine's API docs.
- **[wiki]** means a community-run wiki (Minecraft Wiki, Valheim wikis, Enshrouded wiki). They are detailed and usually right, but they are not the developer.
- **[community]** means a guide, forum post, mod page or search-engine summary of one.
- **[estimate]** is my own arithmetic or inference. **[unverified]** is a claim I could not check against a source that owns it.

No developer source was found for Valheim's integrity rules, Dual Universe's current build costs, or Space Engineers' grinding refund. Those rows say so. Several fetches failed (Fandom returned HTTP 402, one board an expired certificate), so some Dual Universe and Hytale survival facts come from search-engine summaries only and are marked.

## TL;DR

- **Every game here places a ready-made, snapped thing against a face of what is already built, with a modifier to turn snapping off.** Minecraft's cell is one block. Valheim, Enshrouded, Medieval Engineers and Starbase snap to attach points. The one voxel-detail system, Vintage Story, lets a block be chiselled to 16×16×16 sub-voxels, and a chiselled block cannot be re-oriented by picking it up and placing it again (S10, S11). That matches ADR 0011's rule: orientation comes from the neighbour, not from the placed thing.
- **Building helpers split into two kinds.** One kind needs no memory of what was built: fill, line, box, hollow, mirror, masked replace, copy and paste of a Voxel region (Minecraft `/fill`, `/clone`, Space Engineers, Dual Universe, Hytale). The other kind needs an identity: Dual Universe's core blueprints and Valheim's per-piece support values. Only the second kind clashes with ADR 0011.
- **Structural integrity comes in three shapes.**
  - *None* (Minecraft blocks, Enshrouded as far as I can tell).
  - *A distance rule*: Minecraft's scaffolding (7), leaf decay (6), Vintage Story's `maxSupportDistance` (default 2).
  - *A support-propagation graph* (Valheim, Medieval Engineers, Starbase). In every case it is computed from what is there (pieces, joints, beams), not from a remembered build.
- **Survival cost is almost always "the thing's recipe, paid at placement, refunded at removal".** Minecraft pays one item per block. Space Engineers pays components while welding. Valheim refunds 100% on hammer removal (community). Vintage Story's chisel charges a whole block per Material used, however few Voxels (community search summary).
- **ADR 0011 already fits the cheapest, most common options.** Creative helpers that only emit Edits fit as they are. Survival cost and refund by **Material** fit, because the spec's M7 text (PR #248, `docs/terrain-glossary`) already says an applied Edit reports the Voxels it removed and added of each Material. Cost and refund by **Piece** need either a record, which ADR 0011 sets aside, or a fixed price per Piece type paid at placement and never refunded.

## Comparison table

"Needs Planet to remember a Piece?" is judged against ADR 0011 (stamped Voxels, no record). Cells say what the game does, with the source number.

| Game | Placement and snapping | Helpers | Structural integrity | Survival cost and refund | Multiplayer | Needs a remembered Piece? |
|---|---|---|---|---|---|---|
| **Minecraft** | One block against a face. Slabs and stairs take their half and facing from where they are placed (S10 for Vintage Story's equivalent; Minecraft's own page was not readable, [unverified]). Reach 4.5 survival, 5 creative (S7). | `/fill` with `replace`, `hollow`, `outline`, `keep`, `destroy`; `/clone` with `masked`, `filtered`, `move`; structure blocks save and load regions up to 48³ with rotation and mirror (S3, S4, S5). All are operator-only. | None for ordinary blocks. Gravity blocks fall when unsupported (sand, gravel, concrete powder, anvils). Scaffolding falls at distance 7. Leaves decay beyond 6 from a log; player-placed leaves never decay (S1, S2, S6). | Blocks are items. Scaffolding drops itself (S1). General refund rule [unverified here]. | Server-side commands and structure blocks are permission-gated (S3, S4). | Yes: a block is a stored id. |
| **Vintage Story** | Slab mode per item (Auto, Horizontal, Vertical); stairs flip by which half is aimed at. Chisel splits a block into 16×16×16 sub-voxels (S9, S10). | Chisel modes: add, remove, rotate, flip, grid size (S9). Mods add push/pull and paint (community). | Optional `caveIns`: unstable rock falls unless a support is within `maxSupportDistance` (default 2), with `collapseChance` 0.25 (S11). Community calls it unfinished (S12). | A chiselled block uses a whole block per Material (community, S9 does not say it). Blocks above resistance 100 cannot be chiselled. | Land claims: Traverse, Use and All levels; each box at least 5×5×5 (S13). | A chiselled block is stored as its own block entity (S9 does not describe storage, [unverified]). |
| **Hytale** | Prefabs placed with a Paste Tool, or as a brush. Arrow keys nudge a multiblock placement (S14). | Selection tool (copy, cut, paste, rotate, extrude, save prefab), scripted brushes, masked brushes (S14). | Not mentioned in the Creative post (S14). | Builder's workbench turns logs and stone into building blocks; placed blocks can be recovered with a pickaxe (community only). | Not covered by the official post. Permissions are server config (community). | A prefab is a saved selection pasted as ordinary blocks (my reading of S14, [unverified]). |
| **Valheim** | Pieces snap to attach points. Hold L-Shift for free placement. Scroll rotates (S15). | None for copy and paste. Hammer builds one piece at a time (S15). | Each piece inherits a fraction of the stability of the piece it hangs from, by material. Ground and trees count as full support. A wood or stone pole stands 16 m, corewood 24 m, wood-iron 50 m (S15). Colours show it. Community says it recalculates about every 0.5 s (community, [unverified]). | Hammer within range of a Workbench (S15). Removal refunds 100% (community search summary). | No Iron Gate source found on integrity or multiplayer. | Per-material values only, so no. Per-piece values would. |
| **Enshrouded** | Snap points toggled with X, 90° steps for building blocks, free rotation for furniture (community). Pieces come in 1, 2 and 4 m sizes (S16). | Hammer plus rake terraforming and overgrowth and decay brushes with small, medium and big shapes (S16). No blueprints found. | Sources conflict: one guide says a stability system exists, a newer one says none (community, [unverified]). | Not on the wiki pages read. | Flame Altar protects a 40 m cube (level 1) to 160 m (level 4); edits outside it revert after 30 minutes (S17). | The altar's area is remembered state. Voxel terrain edits are not objects. |
| **Space Engineers** | Blocks snap to a grid (large 2.5 m, small 0.5 m [unverified here]). Ctrl-drag places a line; Ctrl-Shift-drag a plane (S18). | Copy, cut and paste of whole grids, blueprints and symmetry in Creative (S18). Survival uses a projector that shows a hologram that welders build (S19). | Original game: none (a forum report, community). Splitting a grid is possible, and Merge Blocks repair it (S20). | Placing a block makes an unwelded frame. Components are used up while welding (S20). Grinding returns components; the refund rate is not on the pages read (S20, S21). | Ownership checks after pasting (S18). | Yes: blocks and grids are stored objects. |
| **Medieval Engineers** | Blocks snap to nearby ones; small blocks can sit on large ones. 0.25 m and 2.5 m blocks (S22). | Structural Integrity View in Creative (S22). | Each block is held by those under and beside it. Stone is stronger but heavier. Damaged blocks stop supporting. Broken parts become dynamic fractures. Only large static constructions are checked (S22). | Wood, stone and iron blocks. Refund not found. | Land claims control building and mining (S22). | Yes. |
| **Dual Universe** | Core Unit makes a build zone. Voxels are added or removed with tools. Elements attach at set anchors (search summary of the devblog and wiki, S23). | Shape tools, smooth, line, symmetry plane, copy and paste (S23). Core blueprints copy a whole construct (S23). | Not found. | Voxels and elements come from inventory in the tutorial (S24, community). A blueprint copy needs the same resources (S23). | Not read. | Blueprints do. Voxel tools do not. |
| **Starbase** | Free placement unless parts overlap; Easy Build snapping toggles with a key; snap point lights green when valid (S25, S26). | Integrity view in the build toolbar; blueprints through a station designer (S25, S26). | Capital ship frame: every beam must attach to another beam to pass an integrity check. Superstructures can snap in half under thrust (S25, S26). | Parts are crafted. A purple outline shows a hologram when out of stock (S25). | Station lot owners decide access (S26). | Yes. |

## Findings per game

### Minecraft

- **Placement:** one block per use, against a face. Block interaction range is 4.5 in survival and 5 in creative; the same attribute covers placing (S7).
- **Helpers (all operator-only):**
  - `/fill` has modes `replace` (the default, with an optional filter), `keep` (air only), `hollow` (shell and clear the inside), `outline` (shell only) and `destroy` (drops the old blocks). The old fixed limit of 32,768 blocks became a gamerule in 1.19.4 (S4).
  - `/clone` copies a region with `replace`, `masked` (non-air only) or `filtered`, and `move` clears the source (S5).
  - Structure blocks save a region (48³ on Java) and load it rotated 0, 90, 180 or 270 degrees and optionally mirrored. Only Creative with permission level 2 opens them (S3).
- **Structural integrity:** none for ordinary blocks. What exists is local, rule-based and cheap:
  - Gravity blocks fall one cell at a time when the cell below is replaceable, and turn back into a block on landing (S2).
  - Scaffolding stores a distance 0 to 7 from a supported scaffold and falls at 7 (S1).
  - Leaves decay when no log is within 6 (Java) and player-placed leaves never decay (S6).
- **Collapse feel:** blocks become falling entities that time out after 600 ticks (30 s) and drop as an item (S2).

### Vintage Story

- **Placement:** orientable blocks follow where they are aimed: stairs go upside down on a ceiling or upper half of a wall; slabs have an item-level mode of Auto, Horizontal or Vertical (S10).
- **Sub-block building:** the chisel turns a placed block into a 16×16×16 grid of voxels (4,096). Chiselled blocks cannot be reoriented by collecting and placing them again, and cannot be copied in survival, only in creative (S9, S10). A material above resistance 100 is "too strong to chisel" (community search summary of the wiki).
- **Cost:** a chiselled block consumes a whole block of each Material it contains, however little is used (community search summary; the wiki page read does not say this). A voxel-per-item cost needs a mod.
- **Structural integrity:** an optional world setting. `UnstableRock` makes a block fall unless a support beam is within `maxSupportDistance` (default 2). A block judged unstable falls with probability `collapseChance` (0.25). A block with no connection always falls (S11). Community notes say fences and slabs do not stop a cave-in, full blocks do, and collapses are shallow (S12). One mod author calls the system "half-baked and unfinished" (community).
- **Multiplayer:** land claims with Traverse, Use and All levels; All covers placing and breaking blocks. Each box is at least 5×5×5 (S13). Claims are out of scope for #275 (map: "Multiplayer protection").

### Hytale

- **Source:** the Creative Mode post by Hypixel Studios (S14) is the only official source I read.
- **Helpers:** a Selection Tool (copy, cut, paste, rotate, extrude, save as prefab; a magic wand and free-shape selections are planned), a Paste Tool that places a prefab precisely or "like a brush" with arrow-key nudging, scripted brushes that anyone can author, and a masked shape brush that replaces instead of adding (S14).
- **Not covered by that post:** undo, snapping, physics or collapse, prefab size limits, multiplayer sharing (S14).
- **Survival costs (community only):** a Builder's Workbench makes building blocks from raw logs and stone, and placed blocks are recovered with a pickaxe. Official detail was not found. [unverified]

### Valheim

- **No Iron Gate source on integrity was found.** The wiki (S15) and guides describe it:
  - Each piece inherits a fraction of the stability of the piece it is anchored to, depending on material and position.
  - Terrain, boulders and trees count as ground.
  - A pole straight up from the ground stands 16 m in wood or stone, 24 m in corewood, 50 m in wood-iron.
  - Colours run blue (as stable as the material allows), green, then red. Placing on a fully red piece can make the new part break and drop its resources.
  - A community guide gives percent bands and a half-second recalculation (community, [unverified]).
- **Placement:** most pieces snap, L-Shift places freely, scroll rotates, middle mouse removes (S15). Building needs the Hammer within a Workbench's area. Repairs cost stamina, not Materials (S15).
- **Refund:** removing a piece returns 100% of its Materials, and pieces that fall to a collapse drop their Materials as items (community search summary; the wiki page did not state it).
- **Feel:** the colour overlay is the design's main aid. The player sees where it will fail before building.

### Enshrouded

- **Pieces:** 1 m, 2 m and 4 m building pieces, plus terrain cubes, discs and ramps, an Overgrowth and a Decay tool with three brush sizes (S16).
- **Snapping (community):** snap points toggle with X; building blocks turn in 90° steps; furniture turns freely; disabling snap lets roof pieces shift by half a block.
- **Persistence:** a Flame Altar protects a 40 m cube at level 1, up to 160 m at level 4. Edits outside revert after 30 minutes or a restart. Deactivating an altar leaves buildings until the reset (S17).
- **Integrity:** guides conflict on whether it exists (community, [unverified]). Treat as no integrity until checked in game.

### Space Engineers and Medieval Engineers

- **Space Engineers, Creative:** blocks are placed fully welded at no cost; Ctrl-drag places a line, Ctrl-Shift-drag a plane; copy, cut and paste work on whole grids; symmetry mirrors what is built (S18).
- **Space Engineers, Survival:** a placed block is an unwelded frame with almost no hit points, and welding uses up components. A block becomes functional at a marked progress line (S20). The projector shows a hologram of a blueprint that welders turn into blocks; each projected block costs 1 PCU, only blocks touching the projector's grid or welded blocks can be welded (S19). There is no undo; players quick-save (S20).
- **Space Engineers, integrity:** a community report says the original game has none; the developers reportedly declined for performance (community, [unverified]). Keen put it in Medieval Engineers first.
- **Medieval Engineers:** blocks are held up by the blocks under and beside them; stone is stronger and heavier; damaged blocks stop supporting; when enough fail, the structure falls in real time as dynamic fractures. Only large static constructions are checked. A Creative-only Structural Integrity View lets builders test before Survival (S22). Land claims control building (S22).

### Dual Universe and Starbase

- **Dual Universe (from search summaries of the wiki and the devblog; the pages would not fetch):** a Core Unit sets a build zone. Tools add or remove cubes, spheres and prisms, smooth, and mirror around a plane. Copy and paste works inside the zone. Elements (cockpits, engines) attach at anchors. A core blueprint copies a construct, needs the same resources, and a later edit needs a new master (S23). Voxel placement uses inventory in the construction tutorial (S24, community).
- **Starbase:** the capital ship frame is built from beams, and every beam must attach to another beam to pass an integrity check. An integrity view in the build toolbar shows failures (S25). A developer FAQ answer says part placement is free if parts do not overlap and snapping can be turned off (S26, via search summary). The wiki page is flagged out of date (S26).

## What this means for BhayanakVoxel

Fixed by the map's Notes and ADR 0011: Pieces are stamped Voxels; they snap to any face at half and quarter steps and turn square within a build; with nothing to snap to a Piece starts a new build; a Piece costs one Edit of 8,000 Voxels; Creative keeps free Brushes and Materials; placing needs no Tool; breaking behaviour is the owner's to decide later and works from the Voxels alone.

Two things in the evidence to carry into the options:

1. **Anything the games compute from what is there fits.** Gravity falling, support distance, a support graph and an integrity overlay all read the structure, not a record. Every game that stores objects (Minecraft, Space Engineers, Valheim's per-piece types) pays for it in saves and replication, which is what ADR 0011 declined.
2. **Edit size is the constraint, not the idea.** An Edit is about 24 bytes (ADR 0003) and a full Piece about an eighth of a second of the Edit rate (ADR 0007), at most about 8 Pieces a second. A helper that stamps many Pieces runs at that rate unless it is cheaper per Voxel.

## Options for BhayanakVoxel

These are options with trade-offs. The owner decides. "Record?" says whether it needs the Planet to remember a Piece.

### A. Creative helpers

| # | Option | Examples | Fits ADR 0011? | Trade-offs |
|---|---|---|---|---|
| A1 | **Line and box of Pieces** (drag a row or a plane of the same Piece) | Space Engineers Ctrl-drag and Ctrl-Shift-drag (S18) | Yes, no record. Many Edits from one gesture, each one grid cell on the build's frame. | Needs the build's frame (#183). Rate-limited by the Edit rate, so a long wall takes seconds. |
| A2 | **Fill, hollow and outline of a region with one Material** | `/fill` modes (S4) | Yes. A Brush mode, not a Piece. | Cheap to add beside the cube Brush. Rooms and shells in one action; flat Material, no pattern. |
| A3 | **Mirror and symmetry** across a plane | Space Engineers, Dual Universe (S18, S23) | Yes. Replays each Edit reflected. | The plane is in the build's own frame. On a sphere, mirrored builds drift apart with distance (ADR 0011: about 1.5 cm in 100 m). Needs UI to place the plane. |
| A4 | **Copy and paste a Voxel region** (selection, rotate in 90° steps, paste) | `/clone`, structure blocks, Hytale paste tool (S3, S5, S14) | Reading Voxels needs no record. Writing is the problem: an arbitrary region is not a 24-byte Edit. | Options: (a) send the clipboard to the server as an upload and replicate it like a snapshot, (b) paste as a run of Piece Edits, which only works for builds made of Pieces. Paste rate is limited by the Edit rate. |
| A5 | **Saved blueprint as a personal clipboard library** | Hytale prefabs, Space Engineers blueprints (S14, S18) | Yes, if it lives in the player's data, not on the Planet. | Needs player storage and, in multiplayer, an upload path. Shares with A4. |
| A6 | **Blueprint tied to a build** (Dual Universe core blueprint) | S23 | **No.** It needs to know which Voxels are one construct. | Would need a record or a user-drawn selection each time. The selection version is A4/A5. |
| A7 | **Masked replace Brush** (swap Material A for B inside the Brush) | Hytale masked brush, `/fill replace` with a filter (S4, S14) | Yes. | Cheap. Good for re-skinning a build. |
| A8 | **Projector or ghost of a blueprint** (hologram that stays after you place it) | Space Engineers projector (S19) | Yes as a Ghost. The Planet stores nothing. | Mostly a Survival aid; in Creative it is paste. Client must keep the Ghost's frame across a reload. |
| A9 | **Snap switch** | Valheim L-Shift, Enshrouded X, Starbase C (S15) | Yes. ADR 0011 says a Piece with nothing to snap to starts a new build. | A hold-to-ignore key is the common answer. Any "free" placement still has to say which frame it uses. |

### B. Survival cost of placing and refund of breaking

Placing needs no Tool (map). Cost is the open part.

| # | Option | Examples | Needs a record? | Trade-offs |
|---|---|---|---|---|
| B1 | **Cost by Material, counted from the stamped Voxels** (a full cube Piece of rock costs the Voxels of rock it stamps, in whatever unit inventory counts) | Spec M7 text on PR #248: an Edit reports the Voxels removed and added of each Material | No. | Accurate for any shape, so a stair costs half a cube. No Piece identity needed. Needs the inventory unit decided (#278 and later tickets). 8,000 Voxels per cube is a large number for a player to read; a coarser unit helps. |
| B2 | **Fixed price per Piece type, paid at placement, refund by Material when dug** | Minecraft: one item per block; Valheim: a recipe per piece (S15) | No, if the refund is by Material and not by Piece. | Simple recipes ("a stair costs 4 planks"). Break and refund no longer match: dig a Piece and get Materials, not the Piece. Players can place and dig for a loss or a gain; set the conversion so a round trip never gains. |
| B3 | **Whole-block rounding up** (any use of a Material costs a unit) | Vintage Story's chisel (community, S9) | No. | Easy to explain. Punishes small pieces and detail. |
| B4 | **Progressive building** (placement makes a frame; gathering and working complete it) | Space Engineers welding (S20) | No, but the frame state is a second Voxel state. | Adds time and feel to Survival. Needs a state in the Voxel or a pending-build record, so it is the closest to a second representation. |
| B5 | **Full refund of the Piece when removed** | Valheim hammer removal (community) | **Yes.** It needs to know which Voxels were one Piece (the "stamped, with a record" option ADR 0011 set aside). | Best feel, and what survival players expect. Cost is a record per Piece in every save, plus the half-dug Piece rules. |
| B6 | **Refund by Material only, with a loss** | Spec M7 yield from digging | No. | Matches digging a natural Voxel. Players recover most Materials but not the shape. |
| B7 | **Place free, dig costs nothing back** (Creative-like, for early Survival) | Space Engineers Creative (S18) | No. | A staging step only. |

### C. Structural integrity

Debris already covers "connected to the Planet or not" (M3, flood fill). These are in addition, and the map defers the decision to the owner.

| # | Option | Examples | Needs a record? | Trade-offs |
|---|---|---|---|---|
| C1 | **Debris connectivity only** | Minecraft blocks; Enshrouded as far as known | No. | Free, already planned. Buildings can overhang forever, and a one-Voxel strut holds a house. |
| C2 | **Gravity Materials** (sand, gravel fall when unsupported) | Minecraft gravity blocks (S2) | No. | Matches the Fluid idea (sand is a Fluid, CONTEXT.md). Local, cheap, and looks right. |
| C3 | **Distance rule per Material** (a Voxel survives within N Voxels of a support, N from the Material) | Minecraft scaffolding 7, leaf decay 6 (S1, S6); Vintage Story `maxSupportDistance` 2 (S11) | No. | One number per Material, easy to explain. At 5 cm, a distance in Voxels is tiny, so N must be in metres; the flood fill is over many Voxels. [estimate] |
| C4 | **Support graph with a stability value per Material** (a Valheim-like model) | Valheim, Medieval Engineers, Starbase beams (S15, S22, S25) | Per-Material values need none. Per-Piece values (Valheim's different walls and poles) would. | Strongest builder feel and an overlay (Valheim colours, the Medieval Engineers view) shows it. Highest cost: it runs over Voxels, not pieces, and must be deterministic (ADR 0003) and scale both ways (AGENTS.md). Valheim runs on a few thousand pieces, a 5 cm build is millions of Voxels, so the graph would run on a coarser grid. [estimate] |
| C5 | **Probabilistic collapse near unsupported rock** | Vintage Story `collapseChance` (S11) | No. | A random draw must come from the seed, not a shared RNG (AGENTS.md determinism). |
| C6 | **Integrity overlay in Creative only** | Medieval Engineers Structural Integrity View (S22) | No. | Lets builders learn the rule before Survival. Only meaningful if C3 or C4 exists. |

Combinations that follow the evidence: C1 plus C2 is the cheapest start. C3 or C4 is added with the breaking-behaviour decision. C6 goes with whichever is picked. A rule that depends on which Piece a Voxel came from (Valheim's per-piece values, Starbase's beams) is the one family that asks the Planet to remember a Piece.

## What was not found

- Iron Gate's own description of Valheim's stability model, and the exact decay numbers.
- The Space Engineers grinding refund rate (the Grinder pages give speeds only).
- Dual Universe's current voxel costs and any integrity rule (pages would not fetch).
- Hytale's survival building rules, undo, and any collapse model. The official Creative post is silent on all three.
- Minecraft's slab and stair placement rules (the wiki pages were truncated). Vintage Story's equivalent is sourced instead (S10).
- Enshrouded's stability: sources conflict.

## Sources

Fetched in full or in part by this ticket. "Search summary" means only the search engine's summary was read.

- S1 [Minecraft Wiki: Scaffolding](https://minecraft.wiki/w/Scaffolding) [wiki]
- S2 [Minecraft Wiki: Falling block](https://minecraft.wiki/w/Falling_Block) [wiki]
- S3 [Minecraft Wiki: Structure Block](https://minecraft.wiki/w/Structure_Block) [wiki]
- S4 [Minecraft Wiki: /fill](https://minecraft.wiki/w/Commands/fill) [wiki]
- S5 [Minecraft Wiki: /clone](https://minecraft.wiki/w/Commands/clone) [wiki]
- S6 [Minecraft Wiki: Leaves](https://minecraft.wiki/w/Leaves) [wiki]
- S7 [Minecraft Wiki: Interaction range](https://minecraft.wiki/w/Interaction_range) [wiki]
- S8 [Wikipedia: Minecraft](https://en.wikipedia.org/wiki/Minecraft)
- S9 [Vintage Story Wiki: Chisel](https://wiki.vintagestory.at/Chisel) [wiki]
- S10 [Vintage Story Wiki: Building](https://wiki.vintagestory.at/Building) [wiki]
- S11 [Vintage Story API docs: BlockBehaviorUnstableRock](https://apidocs.vintagestory.at/json-docs/jsondocs/Vintagestory.GameContent.BlockBehaviorUnstableRock.html) [official]
- S12 [Vintage Story forum: Shoring for cave-ins](https://www.vintagestory.at/forums/topic/14859-shoring-for-cave-ins/) [community, search summary]; [Cave-in Fix mod](https://mods.vintagestory.at/caveinfix) [community, search summary]
- S13 [Vintage Story Wiki: Land claiming](https://wiki.vintagestory.at/Land_claiming) [wiki]
- S14 [Hytale: Creative Mode](https://hytale.com/news/2025/11/hytale-creative-mode) [official]; survival costs from [Switchblade Gaming's building guide](https://www.switchbladegaming.com/hytale/building-guide/) [community, search summary]
- S15 [Valheim Wiki: Building](https://valheim.weirdgloop.org/w/Building) [wiki]; refund and recalculation from community guides (search summaries): [Valheim Hammer guide](https://valheim-guides.com/building/hammer), [Guides Factory](https://guides-factory.com/guides/valheim-building-stability-guide)
- S16 [Enshrouded wiki: Building](https://enshrouded.wiki.gg/wiki/Building) [wiki]; snapping and stability from community guides (search summary), for example [Enshrouded Building Guide](https://enshrouded-wiki.wiki/en/base-building/enshrouded-building-guide/)
- S17 [Enshrouded wiki: Flame Altar](https://enshrouded.wiki.gg/wiki/Flame_Altar) [wiki]
- S18 [Space Engineers Wiki: Creative Mode](https://spaceengineers.wiki.gg/wiki/Creative_Mode) [wiki]
- S19 [Space Engineers Wiki: Projector](https://spaceengineers.wiki.gg/wiki/Projector) [wiki]
- S20 [Space Engineers Wiki: Building](https://spaceengineers.wiki.gg/wiki/Building) [wiki]
- S21 [Space Engineers Wiki: Grinder (Tool)](https://spaceengineers.wiki.gg/wiki/Grinder_(Tool)) [wiki]; no-integrity report from [Keen forum, Structural Integrity Mode](https://forum.keenswh.com/threads/structural-integrity-mode.7291878/) [community, search summary]
- S22 [Medieval Engineers: Features](https://www.medievalengineers.com/features/) [official]
- S23 [Dual Universe devblog: Builder gameplay, voxel tools and elements](https://board.dualthegame.com/index.php?/topic/378-devblog-builder-gameplay-voxel-tools-elements/) [official; search summary only, the page would not load]; [Dual Universe wiki: Blueprints](https://dualuniverse.fandom.com/wiki/Blueprints) [wiki; search summary only]
- S24 [Dual Universe support: Construction Tutorial](https://support.dualthegame.com/hc/en-us/articles/360021173940-Construction-Tutorial) [official; search summary only]
- S25 [Starbase wiki: Capital Ships](https://wiki.starbasegame.com/index.php/Capital_Ships) [wiki]
- S26 [Starbase wiki: Frequently Asked Questions](https://wiki.starbasegame.com/index.php/Frequently_Asked_Questions) [wiki; flagged out of date]
