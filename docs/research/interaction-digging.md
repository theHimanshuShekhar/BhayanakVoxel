# Research: digging, mining and gathering in voxel-terrain games

Resolves issue [#276](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/276), part of the map [#275](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/275) "Player interaction: dig, gather, carry, craft, build and place". Researched 2026-11-28. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md) with the additions of PR #248 (Tool, Creative, Survival, Plant, Piece).

Every factual claim links to its source. Source labels:

- **[dev]**: a developer's own page, patch notes or talk.
- **[wiki]**: a game wiki. The Astroneer, Deep Rock Galactic and 7 Days to Die pages cited are on wiki.gg sites that call themselves "official", but they are community-edited, so treat them as community evidence, not a developer statement.
- **[community]**: a fan site, guide or reprint.
- **[secondary]**: press.
- **[unverified]**: I could not check it against a source that owns it.
- **[judgement]**: my own reading, not a source's claim.

Not done: no game was played, so "feel" below is only what the sources state (numbers, mechanics, patch rationale). Player praise and criticism is thin, because no review mining was done beyond one Steam score (§3.7). Reddit and Fandom block fetching from here.

## 1. What the owner has already decided (the yardstick)

From the map's Notes, the spec's M7 block and ADR 0011:

- **Tools:** axe, shovel, pickaxe. Each Material names its Tool (a `tool` field in `materials.toml`: axe, shovel, pickaxe or any).
- **Speed:** time comes from the Material's hardness, then whether the Tool is right, then its tier. A wrong Tool or bare hands is **about 5–10× slower, never blocked**. A low tier on very hard Materials is as slow as the wrong Tool.
- **Brushes by Tool:** pickaxe small and precise (up to about 0.5 m); shovel wide and shallow, plus smooth and flatten; axe a thin planar slab (the felling cut). **Placing needs no Tool.**
- **Creative** keeps free Brushes and Materials.
- **Breaking behaviour** (felling, collapse, placed things breaking) is the owner's later decision and works from the Voxels alone, with unsupported parts falling as Debris (M3, connectivity flood fill).
- **The seam for yield:** "an applied Edit reports the Voxels it removed and added of each Material" (spec, *For survival*).
- **Costs:** a Voxel is 5 cm, so 1 m³ is 8,000 Voxels. The Edit rate is 8 m³ a second, metric, and the Brush limit is 2 m in radius (ADR 0007). Edits are small deterministic operations of about 24 bytes that every client applies (ADR 0003). Undo restores bricks on the server (ADR 0010). Pieces and Plants are ordinary Voxels that the Planet forgets (ADR 0011, PR #248).

## 2. Comparison table

| | Dig unit and feel | What a dig gives | Resources in the ground | Tools and tiers | Wrong tool |
|---|---|---|---|---|---|
| **Enshrouded** | Continuous volumetric carve with the pickaxe or Construction Hammer; the rake smooths and fills. Rake radius adjustable since Update 7 [dev] | Material by volume: "the dug material drops by the amount of terrain removed" [community]; undo does not give resources back [community] | Ores are "Very hard" terrain Materials in a 106-Material list [community]; no dedicated veins found | Pickaxe tiers Pickaxe to Steel (damage 11 to 55 in the official list), plus Gilded and Mining Sieve [wiki]; damage falls per hardness grade, 60 at Soft to 5 at Very hard for the first pickaxe [community] | Not a rule; instead a tier × hardness damage matrix |
| **Astroneer** | Held Terrain Tool with Excavate, Elevate and Flatten modes; brush radius by Wide and Narrow mods [wiki] | Terrain goes into canisters 1:1; when full, "burned off" and lost [dev, wiki]. Deposits break into nuggets [wiki] | Surface deposits per planet; ten resources everywhere, rarer ores by planet [wiki] | No tool tiers found; mods (Boost, Wide, Narrow) [wiki] | n/a |
| **Deep Rock Galactic** | Discrete pickaxe swings; each terrain material has hardness 1–3, the number of hits to break a chunk [wiki]; a Power Attack mod destroys any terrain instantly [wiki] | Minerals in the rock; whether terrain itself gives anything is not stated on the pages read **[unverified]** | Hand-built cave layouts with minerals; hardness varies by biome [wiki] | One pickaxe with two mod tiers [wiki]; drills and explosives for volume | n/a |
| **7 Days to Die** | Per-hit block damage in a block-and-terrain mix; stamina cost [wiki] | Resources per block, not per time: "the speed at which a block is destroyed does not reduce the total resources obtained" [wiki]; 75% loss for clubs, hammers and spears [wiki] | Surface node plus 2–3 ore blocks beneath it, gravel veins as the cue, any ore in any biome since Alpha 18 [wiki] | Stone, Iron, Steel, Auger; "4 Stone Axe = 3 Iron Pickaxe = 2 Steel Pickaxe = 1 Auger" [wiki] | Shovels do 0.2× damage to stone and metal, 0.3× to wood, so 3.3–5× slower, never blocked [wiki] |
| **No Man's Sky** | Terrain Manipulator beam, three sphere sizes, flatten and restore modes [dev, community] | Terrain itself gives nothing; deposits give resources at a constant rate whatever the sphere size [community] | Resource Deposits marked by the Analysis Visor; "deposits without grids give nothing" [community] | One Tool; charge is the limit, refilled with ferrite [community] | n/a |
| **Teardown** | Instant carve by shape: `MakeHole` takes a radius per material class, soft, medium, hard [dev] | Nothing (no inventory) | None | Sledgehammer to rocket launcher; upgrades raise damage [community] | A hard class gets radius 0, so it is blocked outright [dev] |
| **Lay of the Land** | Sculpt tools raise and lower terrain; the world is "fully destructible" [dev (store page)] | Not described | Not described | Not described | Not described |
| **Minecraft** (reference only) | One block per break, time = hardness × 1.5 (or × 5) [community] | Block drop, tied to the tool tier for ores [community] | Not checked | Wood 2, Stone 4, Iron 6, Diamond 8 speed [community] | 5/1.5 = 3.3× slower, still possible; ore drops need the tier [community] |

## 3. Findings per game

### 3.1 Enshrouded (Keen Games)

- **Digging feel [dev, wiki, community].** The world is voxel terrain. The pickaxe and the Construction Hammer take terrain away; the hammer's Terrain section puts it back; the rake slopes, levels and spreads ground. Keen's Update 7 notes say "the rake tool's terraforming radius can now be adjusted manually", that "the tool can now be used to dig small ditches or irrigation canals" and that "the finetune mode has been made available for the rake" ([patch notes, reprinted by Hack the Minotaur](https://hacktheminotaur.com/enshrouded/enshrouded-full-patch-notes-update-7-v-0-9-0-0-new-biome-level-cap-increase-and-fishing/)). Patch #12 let the player use the pickaxe while crouched and fixed rake "ghost-voxels" (areas that looked empty but held voxel fragments), per a [secondary summary](https://hacktheminotaur.com/enshrouded/enshrouded-patch-12-stamina-regen-buffs-building-cost-reductions-holiday-qol/).
- **Tiers [wiki].** The [official wiki's Tools page](https://enshrouded.wiki.gg/wiki/Tools) lists pickaxe damage 11, 17, 25, 32, 40, 55, 70 (Pickaxe, Scrappy, Copper, Bronze, Iron, Steel, Gilded), stamina cost 6 (5 for Steel, 1 for Gilded), and a Rake at 500 durability. The Pickaxe recipe is one Stone, four Twigs and one String, made by hand.
- **Hardness [community].** [enshrouded.tools](https://www.enshrouded.tools/terraforming), an unofficial fan site with data from a game build, gives five hardness grades (Soft, Slightly hard, Moderately hard, Hard, Very hard) and a damage matrix. The first Pickaxe does 60, 50, 35, 20, 5; Iron does 100, 100, 95, 60, 50; Celestium does 100, 100, 100, 90, 80. The first pickaxe is 12× weaker on Very hard than on Soft. The site's text and table disagree on one Gilded value, so check any number before relying on it.
- **Yield [community].** Same site: "the dug material drops by the amount of terrain removed", scaled by a Mining Damage setting from 50% to 200% (Survival preset 75%). Another community guide says digging a basement fills the chests with dirt and stone ([enshroudedviki](https://enshroudedviki.com/building-guide), [unverified](#7-unverified-or-not-checked)).
- **What is kept [community].** Terraforming works anywhere, but only edits inside a Flame Altar's build area persist; outside they reset. 1.0 lets undo work with a full backpack, but the resources do not come back ([enshroudedviki](https://enshroudedviki.com/terraforming), secondary reports of 1.0 not checked against Keen's notes).
- **Multiplayer [dev].** Update 7 added a Visitor role that "prevents terraforming outside the base"; it is on by default for that role, and dedicated server admins can add or remove the setting.
- **Limits [community, unverified].** A community wiki cites a December 2024 AMA in which Keen said more organic voxel shapes were hard to fit into the build workflow. I did not find the AMA.

### 3.2 Astroneer (System Era)

- **Feel [wiki].** The [Terrain Tool](https://astroneer.wiki.gg/wiki/Terrain_Tool) is a held, two-handed tool with three modes: Excavate (dig), Elevate (spend stored soil to build) and Flatten (copies the slope under the middle dot across the area, fills gaps from stored soil and refills canisters from what it digs). Wide and Narrow mods change the radius; a Boost Mod raises speed and drains power. The wiki gives no radii or speeds. The tool can be used to build a column under your own feet.
- **Terrain as an item [dev].** Experimental Test 002 made terrain a resource ([notes](https://astroneer.wiki.gg/wiki/Experimental_Test_002)): "Adding or flattening terrain requires terrain 'ammo' in containers." "You can no longer add terrain indefinitely, even when tethered to your base." "Removing terrain is completely unrestricted." Removed terrain fills canisters on the backpack or tool, and "if all canisters are full (or no canisters are attached), collected terrain is burned off and permanently destroyed." [Patch 215](https://astroneer.wiki.gg/wiki/Patch_215) made soil use "a 1:1 relationship with changes made to terrain", added a soil gauge and a "burn off valve" so the loss is visible on the tool, and removed battery cost from deforming: "Deform forever!"
- **Resources [wiki].** [Soil](https://astroneer.wiki.gg/wiki/Resources) is "collected from the terrain" and fills the canister gradually as you dig. Ore deposits are "naturally formed deposits found scattered throughout the various planets"; "the deposits will break and will provide the player with nuggets". Ten resources are on every planet and the rarer ores are per planet. Collected pieces stack on the tool's rear port, with a spill to the ground if the inventory is full (Terrain Tool page).
- **Why it matters [judgement].** Astroneer is the one game where **placing** costs the dug material, one for one, with a visible gauge and a visible waste when full. It keeps removal free and only limits adding. The feedback is on the tool body, not in a menu.

### 3.3 Deep Rock Galactic (Ghost Ship Games)

- **Feel [wiki].** The [pickaxe](https://deeprockgalactic.wiki.gg/wiki/Equipment) is a swing. On the [Terrain page](https://deeprockgalactic.wiki.gg/wiki/Terrain), each material has a hardness of "typically 1 to 3", "the number of pickaxe hits needed to break a chunk". Bed Rock and Compressed Granite take five hits and break into smaller chunks. A Power Attack mod destroys any terrain type instantly. Hardness is per biome (Sandblasted Corridors 1; Salt Pits to Azure Weald 2; Crystalline Caverns, Radioactive Exclusion Zone, Magma Core 3). A drink lowers hardness by 2 (minimum 1), for the pickaxe only.
- **Chunks, not Voxels [wiki].** The unit of one dig is a chunk with a known hit count, which makes the feel readable: you count swings.
- **No structural integrity, but connectivity [wiki].** "The game does not calculate structural integrity or tensile strength... a pillar stands as long as it touches a floor, wall, or ceiling at any point." "Terrain fully cut off from the rest of the cave crumbles to dust." That is the same rule as the planned Debris flood fill, with the cut-off part disappearing instead of falling.
- **Where the design comes from [dev, secondary].** [PCGamesN](https://www.pcgamesn.com/deep-rock-galactic/deep-rock-galactic-unreal-engine-4) quotes lead programmer Jonas Møller: "it's the same tech as when we are building the level, technically. When we generate the levels we drill the tunnels - just not manually." The map is a hand-built layout the engine skins, after full procedural generation was tried and dropped.
- **Finding minerals.** The wiki's Equipment page has a Terrain Scanner (a 3-D map of the miner's surroundings) but nothing on how it shows minerals. Mineral amounts per swing are not on the pages I could read. **[unverified]**

### 3.4 7 Days to Die (The Fun Pimps)

- **Feel and speed [wiki].** Digging is per-hit block damage with a stamina cost. The [Harvesting page](https://7daystodie.wiki.gg/wiki/Harvesting) says a tool that does not suit the job "applies reduced block damage", and "the speed at which a block is destroyed does not reduce the total resources obtained". Wrong-Tool damage multipliers on the [Shovel page](https://7daystodie.wiki.gg/wiki/Stone_Shovel) are 0.2 for stone and metal and 0.3 for wood, so 3.3–5× slower and never blocked. Dirt is "200 hit points" and gives 22 Clay Soil per dig, which is more than the 16 needed to craft a Dirt block (the wiki calls it a renewable source). Clubs, hammers and spears lose 75% of the resources.
- **Tiers [wiki].** The [Mining page](https://7daystodie.wiki.gg/wiki/Mining) gives the progression as "4 Stone Axe = 3 Iron Pickaxe = 2 Steel Pickaxe = 1 Auger", so each tier is about 1.3–2× the last.
- **Ore sits in the ground [wiki].** "Surface nodes mark the easiest spots to dig. Each node is a full ore block, with 2-3 more ore blocks directly beneath it." Ore also occurs underground with no marker. The visual cue is gravel: "almost never find an ore block completely surrounded by stone", so players follow gravel veins. Since Alpha 18 any ore can appear in any biome.
- **Collapse [wiki].** The Mining page says "structural integrity matters": tunnels can weaken surface buildings and large caves can cave in and hurt you; players carry pillars and dig narrow tunnels in clay, sand and gravel.

### 3.5 No Man's Sky (Hello Games)

- **Feel [dev, community].** The Terrain Manipulator is a beam with three sphere sizes. The [Synthesis update notes](https://www.nomanssky.com/synthesis-update/) added "a flattening mode... to allow quick and easy flattening of a large area" and a "Restore" mode "allowing edits to be undone and the terrain restored to its previous state". Edits within a base are protected from regeneration, with a cap on protected edits that the update raised. The [Foundation notes](https://www.nomanssky.com/foundation-update/) say "improved calculation of resources received when mining terrain".
- **Yield [community].** The [Fandom wiki](https://nomanssky.fandom.com/wiki/Terrain_Manipulator) and [Miraheze wiki](https://nomanssky.miraheze.org/wiki/Resource_Deposit) agree that resources from a deposit arrive at a constant rate whatever the sphere size, so the smallest sphere is best: a bigger sphere costs more charge for the same yield. One 2019 Steam post disagrees (small beam 2–3× slower but about 4× the target material), **[unverified]** and contradicted by the newer guides. In NMS the ground itself gives nothing; the yield comes from marked deposits.
- **Finding deposits [community].** The Analysis Visor marks Resource Deposits with a diamond symbol; "mineable deposits have grids outlining the parts that give resources".
- **Charge as the limit [secondary].** [Twinfinite on update 1.33](https://twinfinite.net/ps4/no-mans-sky-update-1-33-notes/): refilling the Manipulator cost over 1,000 Plutonium while building a new one with full ammo cost 25 Plutonium and 25 Iron, an exploit the patch closed. Lesson: a refill cost has to be tied to the tool's own cost.

### 3.6 Teardown (Tuxedo Labs)

- **Hole by material class [dev].** The [modding API](https://teardowngame.com/modding/api.html) defines `MakeHole(position, r0, [r1], [r2], [silent])`: "Hole radius for soft materials", "for medium materials. May not be bigger than r0", "for hard materials. May not be bigger than r1". Soft is "glass, foliage, dirt, wood, plaster and plastic"; medium "concrete, brick and weak metal"; hard "hard metal and hard masonry". It returns the number of Voxels cut. The example `MakeHole(Vec(0,0,0), 5.0, 1.0)` cuts 5 m in soft and 1 m in medium, and leaves hard untouched.
- **Tools [community].** A [community docs page](https://get-teardown.readthedocs.io/en/latest/gameplay/tools-and-upgrades.html) lists the sledgehammer (wood, glass, weak masonry), a blowtorch and explosives; upgrades are bought with heist money and raise damage. It gives no numbers.
- **Voxel scale [dev].** [Our own Teardown research](inspiration-teardown-rt.md) records 10 Voxels per metre from the developers' talk, so Teardown Voxels are twice ours.
- **Cost of the carve [dev].** Teardown's [multiplayer post](https://blog.voxagon.se/2026/03/13/teardown-multiplayer.html) replicates "cut hole in this shape at voxel coord x,y,z" as a command, after a naive attempt to send voxel data "completely choked the connection". That is ADR 0003's choice, from a game that shipped it.
- **Yield.** Teardown has no inventory, so a dig gives nothing; the carve is the whole game.

### 3.7 Lay of the Land (Southern Cross Interactive, solo developer)

- [Steam page](https://store.steampowered.com/app/2776090/Lay_of_the_Land/) **[dev]**: "a sandbox adventure game set in a fantasy world where everything is physically simulated"; "fires spread and burn, sand collapses, water flows"; terrain raised or lowered "with terrain sculpting tools"; "voxel based building system", cylinder and cone tools, prefabs and your own prefabs; released 8 April 2026; "Very Positive", 80% of 1,306 reviews.
- **Gap:** no written devlog on the dig itself was found (the developer posts videos). Tool shapes, speed and yield are **[unverified]**. Its prefab tool is the nearest thing in this set to ADR 0011's Pieces.

### 3.8 Minecraft, as a yardstick only

Not on the ticket's list, but it is the one game whose break-time formula is written down ([community wiki](https://minecraft.wiki/w/Breaking), flagged there as a work in progress): time is hardness × 1.5 when the Tool can harvest the block and × 5 when it cannot (3.3× slower), with tool speed 1 (hand), 2, 4, 6, 8 for wood, stone, iron, diamond, and ore drops that need the right tier. So Minecraft's wrong-Tool penalty is milder than the owner's 5–10×, and it blocks drops, not digging, for ores.

## 4. Patterns across the games

1. **A tool × hardness matrix is the norm.** Enshrouded (11 to 80 per grade), 7 Days to Die (0.2 to 1 per material), Minecraft (speed per tier) and Teardown (radius per class) all compute from a Material property and a Tool property. The owner's rule (hardness, then right Tool, then tier) matches all of them.
2. **A wrong Tool is slower, not blocked, in everything but Teardown's hard class.** The 7 Days to Die multipliers (0.2 to 0.3) and Enshrouded's low-tier-on-hard cells (60 to 5, 12×) bracket the owner's 5–10×. The owner's rule that a low tier on a very hard Material is as slow as a wrong Tool is exactly Enshrouded's matrix.
3. **Yield is tied to what was removed, not to how long it took** (7 Days to Die; NMS's constant rate; Enshrouded's drop by terrain removed). This stops a slow wrong Tool from also giving less, and a fast Brush from giving more.
4. **Terrain has three roles: nothing, a stream of one soft Material, or a node of rare Material.** Teardown gives nothing; Astroneer, Enshrouded and 7 Days to Die turn removed volume into items; NMS and DRG make the player look for the valuable parts.
5. **Placing is limited by what you dug in the games that limit it at all.** Astroneer limits adding by stored terrain (1:1, with a gauge); the owner's rule says placing needs no Tool, and a Piece is an Edit.
6. **Ore is found by a cue, then a tool.** 7 Days to Die: node plus gravel; NMS: Analysis Visor; Astroneer: visible deposits. DRG's scanner is a 3-D map (mineral display unverified).
7. **Connectivity, not integrity, is the cheap collapse rule.** DRG's "cut off crumbles to dust" is the same input as our Debris flood fill; 7 Days to Die has an integrity system that we have said waits for the owner.

## 5. Options for BhayanakVoxel

The owner decides. Each option is judged against the rules in §1. The Edit rate (8 m³ a second) is the ceiling on all of them; per AGENTS.md, a floor on the low-end profile applies too, and any cost scales with the machine, not a constant.

### A. What counts as one dig

| Option | What it is | Fits | Against |
|---|---|---|---|
| **A1. Held, continuous carve** (Enshrouded, Astroneer, NMS) | Hold the button; the Brush removes Voxels at a rate set by hardness × Tool × tier, up to the Edit rate | Matches "digging time" in the M7 rule; works with Ghost; yield by volume is natural | A Brush with a time-based rate needs a rule for partial Voxels (the tick at which a Voxel goes) |
| **A2. Swings of a fixed hit count per chunk** (DRG) | A swing is one Edit; hardness is the hit count | Readable; each swing is one 24-byte operation, easy for the server to rate-limit | The count is an integer, so "5–10× slower" becomes 5–10 hits and a tier is a whole number of hits; 5 cm chunks would need to be much bigger than a Voxel to feel like a swing |
| **A3. Instant carve by Material class** (Teardown) | A Brush removes everything its Tool can, with a radius per class | Simple; no time model | Blocks the wrong Tool outright on the hard class, against the "never blocked" rule; not a fit |

**Cost note.** At the pickaxe's 0.5 m limit, a sphere of 0.5 m across is about 520 Voxels [estimate: (4/3)π(0.25 m)³ ≈ 0.065 m³ × 8,000]. At the 8 m³ a second Edit rate that is about 120 pickaxe Brushes a second, far more than a swing rate, so the Edit rate is not the limit for the pickaxe; it matters for a shovel or an axe slab and for Pieces.

### B. What a dig gives the player

| Option | What it is | Fits | Against |
|---|---|---|---|
| **B1. By volume, per Material, into a carried store** (Enshrouded, Astroneer's soil) | Each applied Edit reports removed Voxels by Material and the player gains that volume | Uses exactly the spec's seam; works from Voxels alone; the same number serves undo, Debris and sync | A real inventory model (units, stacks, a limit) is still open (#277–#284); 5 cm Voxels make the counts large, so the unit is not a Voxel |
| **B2. Items per dig, rounded from volume** (7 Days to Die: 22 Clay Soil per dirt block) | Convert volume into a count of items at a ratio set per Material | Readable numbers; a Material can be "worth more" than its volume | A second per-Material table; the ratio is a balance number for the owner |
| **B3. Nothing for terrain, items only from nodes** (NMS, Teardown) | Dirt and rock give nothing; marked deposits do | No carry problem for ordinary digging | Conflicts with the shovel and pickaxe Material lists, which name soil and stone for Tools |

Whichever is picked, three rules from the games are cheap to adopt and **[judgement]** worth a decision:
- **Yield by volume, not by time or Brush size** (7 Days to Die; NMS). Otherwise a bigger Brush or a slow wrong Tool changes the yield.
- **What a full carry does:** Astroneer burns the surplus and shows it; Enshrouded keeps it in chests. The 5 cm scale makes a hole of 1 m³ 8,000 Voxels, so the answer decides whether digging a basement is a chore.
- **Undo** (ADR 0010): Enshrouded does not return resources on undo. Whether an undone dig takes back its yield is a rule for the owner; the server already restores bricks, so it knows the Voxels it restores by Material.

### C. How resources sit in the ground

| Option | What it is | Fits | Against |
|---|---|---|---|
| **C1. Ores are ordinary Materials, placed by the generator** (7 Days to Die nodes, Enshrouded's ores as Very hard terrain Materials) | The generator puts ore Voxels in strata, veins and nodes from the seed, as it does Crystal clusters | ADR 0003: client-generated and deterministic; the Placement rule is the existing pattern; no extra save data | A Material list that grows; vein shape and the exact rule are generator work |
| **C2. A vein cue to read** (gravel by ore in 7 Days to Die; a surface node plus 2–3 below) | A surface Material or colour that tells the player where to dig | Costs nothing at run time; a Voxel scale lets a cue be seen | A visual-design job; the look rules (hero look) apply |
| **C3. A scanner** (NMS visor, DRG scanner) | A tool highlights deposits within a range | Helps at 5 cm, where a vein is small against a 320 km Planet | Reads the Voxels near the player every use, so needs a budget with a floor on the low-end profile; wallhack-style highlight is a design question |

**Cost note.** C3's cost is a search of the resident Voxels around the player; at the low-end reference it must degrade (shorter range, slower ping), not fail.

### D. Tools and tiers

| Option | What it is | Fits | Against |
|---|---|---|---|
| **D1. A tier × hardness table** (Enshrouded) | Per tier and per hardness grade, a speed; a low tier on Very hard is slow | The owner's rule as written; one data table in `materials.toml` and a tools file | Needs tuning numbers; 5 grades in Enshrouded, the Materials here have a continuous hardness |
| **D2. A speed multiplier per tier** (Minecraft, 7 Days to Die) | Each tier multiplies the speed (about 1.3–2× per tier) | Simplest; easy to balance | Cannot express "a low tier on a very hard Material is as slow as the wrong Tool" without an extra rule |
| **D3. A tier as a gate for a Material** (Minecraft's ore drops) | Below a tier, no yield | | Contradicts "never blocked" if it blocks the dig; if it blocks only the yield, it is a new rule for the owner |

Reference numbers: wrong Tool is 3.3× (Minecraft), 3.3–5× (7 Days to Die), up to 12× on the hardest grade for a low tier (Enshrouded). The owner's 5–10× sits inside these.

### E. Brush shapes and size by Tool

The sources agree on the idea: sizes are small and per Tool, and a big shape costs more. Enshrouded gives the rake a settable radius; Astroneer gives Wide and Narrow mods; NMS gives three sphere sizes with the charge as the cost; Teardown gives a radius per class. For this game the owner's rule already sets the shapes. Open options within it:

- **E1. Settable radius within a Tool's maximum** (Enshrouded's rake). Fits Ghost.
- **E2. A fixed set of sizes** (NMS's three). Fewer operations to define; matches a hotbar.
- **E3. Tier raises the maximum** (Astroneer's mods). Ties the Tool tiers to reach as well as speed; not in the owner's rule, so a new decision.

### F. Feedback

The primary sources say very little about particles, sound and camera shake, so this is mostly **[unverified]** and a design call for #280-style tickets. What is stated:
- Astroneer shows terrain use on the tool itself: a soil hose, a gauge and a burn-off valve (Patch 215).
- Astroneer shatters deposits into pieces that are vacuumed up (Terrain Tool page).
- Teardown's `MakeHole` has a `silent` flag, so break sounds are part of the carve [dev].
- NMS's Synthesis notes improved the terrain edit effects, without detail [dev].
- DRG's hits-to-break gives a count a player feels [judgement]; its wiki says nothing on particles or sound.

Option: put the cost and the limit **on the Tool's model** (Astroneer's way), so the HUD does not carry it. That fits the planned Edit HUD (#151) and the Ghost.

### G. Interaction with breaking and Debris

The owner has set breaking aside; the research only reports what exists:
- DRG's cut-off-crumbles rule is the nearest to the Debris flood fill; it needs no integrity.
- 7 Days to Die has an integrity system with cave-ins; Lay of the Land has collapsing sand and gravity; both are costlier than connectivity.
- A Tool that makes an axe-slab cut (felling) works on a Plant's Voxels in the same way as any other dig (PR #248).

## 6. Judged against the rules already decided

| Rule | Status |
|---|---|
| Each Material names its Tool | Supported by every game with tools (7 Days to Die's shovel, pickaxe, axe split is nearly the same list) |
| Wrong Tool 5–10× slower, never blocked | Inside the range of 7 Days to Die and Enshrouded; milder in Minecraft; Teardown blocks its hard class (A3 is the one option that breaks the rule) |
| Tiers set the speed; a low tier on a very hard Material is as slow as the wrong Tool | Enshrouded's matrix is the working example |
| Pickaxe small, shovel wide and shallow, axe a thin slab | No game here splits Brush shapes by Tool this way; the closest is the rake (shallow, wide) next to the pickaxe in Enshrouded. This is new design space, with no precedent to copy and none to warn against |
| Placing needs no Tool | Astroneer's placing costs stored soil; only a yield rule, not a Tool |
| Pieces and Plants are ordinary Voxels | A dig yields by Material from the Voxels alone, so a log Piece and a tree give the same wood Material |
| Edit rate 8 m³ a second | The ceiling on any held-dig option |

## 7. Unverified or not checked

- **Feel numbers:** no source gives Enshrouded, Astroneer, NMS or Teardown tool radii in metres. Enshrouded's pickaxe sphere, Astroneer's radius and NMS's three sizes were not found.
- **DRG:** mineral amounts per swing and the scanner's mineral display; the Terrain page was readable only through a fetch tool, and the Fandom copy was blocked.
- **Enshrouded basement chests** and **"1.0 undo with a full backpack"** are from community guides, not from Keen's notes. Keen's 1.0 notes were not found. The December 2024 AMA was not found.
- **Search-summary-only citations:** the 7 Days to Die shovel and dirt figures, the Enshrouded Patch #12 and Update 1.0 details and the NMS update 1.33 story were read from search summaries of those pages, not fetched in full.
- **Fandom pages** (7 Days to Die, Teardown, NMS) were blocked from here; the 7 Days to Die figures are from wiki.gg pages (community-edited), and the NMS ones from a search summary of the Fandom page plus two other wikis.
- **Teardown's per-tool hole sizes** are not published in the pages I could read; the page of tools is community-written and has no numbers.
- **Lay of the Land:** a solo developer posts devlog videos; none was watched. Only the Steam page was read.
- **Player praise and criticism** was not collected beyond the Steam score; Reddit is blocked from here. Cases not researched: Valheim (terrain tools, pickaxe tiers), Space Engineers (voxel hand drill, ore detector), Vintage Story, Minecraft's newer mining changes, and Dwarf Fortress. They are the next ones to read if the owner wants more.
- **Numbers marked [estimate]** are my arithmetic (§5 A).
