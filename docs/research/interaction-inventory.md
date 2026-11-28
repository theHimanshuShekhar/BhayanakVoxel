# Research: inventory, crafting and bulk Materials in voxel survival games

Resolves [#278](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/278), part of the map [#275](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/275) ("Player interaction: dig, gather, carry, craft, build and place"). Researched 2026-11-28. It feeds the grilling tickets [#280](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/280) (what a dig gives) and [#282](https://github.com/theHimanshuShekhar/BhayanakVoxel/issues/282) (inventory and crafting in Survival).

Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). **Tool**, **Creative**, **Survival**, **Plant** and **Piece** are in the glossary additions of PR #248 (branch `docs/terrain-glossary`), not yet on `main`; the M7 Tool rules are in the same branch's `docs/spec.md`.

Labels used below:
- **[official]** a first-party source: the developer's own site, patch notes, or source code.
- **[wiki]** a wiki. Where the page title calls it the game's "Official" wiki, I say so; I could not check who runs it. Treat wikis as good, not primary.
- **[community]** a fan guide or third-party site.
- **[calc]** my own arithmetic from this repo's numbers (shown).
- **[unverified]** I could not confirm it from a source I opened. Nothing marked so is used in the options as fact.

This is research, not a decision. It never proposes a rule the owner has not made.

## TL;DR

1. **No game counts single Voxels.** Every game turns matter into a coarser unit before the player sees it. Minecraft makes one block one item. 7 Days to Die turns a terrain block into 22 Clay Soil. Space Engineers and Dual Universe count litres. Astroneer counts "canister fill". At 5 cm, one cubic metre is 8,000 Voxels **[calc]**, so a per-Voxel count is not a usable unit either. The data to count by volume already exists: the Edit reports the Voxels it removed and added of each Material (spec, "For survival").
2. **Three inventory shapes cover all eight games:** slots and stacks (Minecraft, Vintage Story, Valheim, 7 Days to Die, Astroneer, Enshrouded), volume (Space Engineers, Dual Universe), and weight (Valheim, as a second limit on top of slots). Only Space Engineers and Dual Universe measure bulk matter in litres, and only Space Engineers is checked here against its own source code.
3. **Terrain matter is handled in three different ways.** It is given as a coarser resource that is not the same size as what was dug (7 Days to Die, Minecraft). It is stored as a volume that the same tool spends to fill terrain again (Astroneer's soil, Enshrouded's terrain material). Or it is given nothing at all (Valheim's Hoe: raising ground costs 2 Stone, while the page lists no drop from digging).
4. **Stations and containers are objects in every game here, with their own state.** A chest, a forge or a printer has contents, a queue or a level that the game stores and syncs. ADR 0011 makes a Piece a stamp of Voxels that the Planet forgets, and ADR 0002 rejects a second representation of matter. That is the largest mismatch between this game and the games researched, and it matters most for stations, containers and refunds (see "Options").
5. **Space Engineers is the only first-party source for the sync pattern, and it fits ADR 0003:** inventory changes are requests to the server, the server applies them, and only the server's volume and mass values are synced. Minecraft's wiki describes a server-managed sequence number to catch desync. For the other six I found no source.

## Comparison table

"Slot" means a discrete cell holding one stack. Facts without a mark are from the page named in the game's section.

| | Inventory shape | What one dig yields | Crafting | Bulk movement | Gathered → placed | Sync |
|---|---|---|---|---|---|---|
| **Minecraft** | 36 slots + 4 armour + off-hand, stacks of 64 (16 for some, 1 for durable items) | One block drops as items, if the Tool tier allows: a pickaxe is needed for stone to drop anything; no Tool still gives dirt. The Tool is the *drop gate* | 2×2 in the inventory, 3×3 at a crafting table; smithing, brewing and others are separate; recipes shaped or shapeless; recipe book | Chests (27 slots), hoppers (2.5 items/s), minecarts, shulker boxes (carry contents) | Item = block. Placing consumes one item; broken blocks come back as items | Server-managed state ID per window; server-side inventory since Beta 1.0 [wiki] |
| **Vintage Story** | Slots; bags in dedicated bag slots (3 to 14 slots); no weight found | Not found [unverified] | Grid, plus voxel work: knapping, clay forming, smithing; metal measured in units (100 per ingot, 5 per nugget) | Carry empty containers; chests 16 slots; crates hold one item type in bulk | Chisel splits a placed block into 16×16×16 voxels; in Survival it consumes supply up to 4,096 voxels per material [community] | Not found [unverified] |
| **Valheim** | 8×4 = 32 slots, up to 48; carry weight 300 base | None found for digging terrain; raising ground costs 2 Stone [wiki] | Hammer builds pieces for materials, near a Workbench; stations are placed objects whose upgrades attach to them; 20 m range | Carts and ships by slots [community] | Pieces cost materials; deconstruct refund not stated by the wiki | Not found [unverified] |
| **Astroneer** | Backpack: 8 inventory slots, 2 widget slots; resources in slots | Soil goes into canisters on the Terrain Tool; burned off if no canister has room | Backpack printer (small items); Small, Medium and Large printers for bigger; recipes unlocked with Bytes | Canisters on tools; rovers fill canisters with drills [wiki] | Soil is spent by the same tool to raise terrain; one Small Canister makes a Leveling Block | Not found [unverified] |
| **Enshrouded** | Slots (counts not found); stack sizes unified in the Nov 2025 update | The pickaxe "pays you in the material it digs"; amount not stated [community] | Stations crafted by survivors; each block recipe yields **100** blocks, each roof recipe 50 [official] | Magic chests: build from nearby chests without carrying [community] | Terrain material placed by the Construction Hammer or Rake; the world keeps terraforming only inside a Flame Altar's area [community] | Server role permission for terraforming [official]; inventory sync not found |
| **Space Engineers** | Volume (litres) and mass (kg); identical items stack; type constraints per inventory | Voxel material gives ore; ore ratio per material (Iron 5, Gold 1, Uranium 0.3) | Refinery (ore → ingot), Assembler (ingots → components), welding turns components into blocks | Conveyor system: instantaneous when powered, no stated rate; sorters; connectors | Ore → ingot → component → block | `[Event, Reliable, Server]` requests; volume and mass synced; server checks capacity [official, source code] |
| **Dual Universe** | Volume (litres) in containers by size; mass | Ore by litres and tier; territory "ore pools" limit it per hour [wiki, search excerpt] | Industry units, not opened here | Container XS 1,000 L to XL; hubs; link range [wiki, search excerpt] | Not opened here | Not found [unverified] |
| **7 Days to Die** | 45 slots, "Carrying Capacity" in slots (27 at start), no weight | A shovel on Forest Ground gives **22 Clay Soil**; Gravel gives 9 Small Stone and 8 Crushed Sand | Five stations (campfire, forge, workbench, cement mixer, chemistry); recipes unlocked by skill, books and schematics | Storage containers (capacity not found); Clay Soil stacks to 6,000 | Crafting 1 Dirt needs **16** Clay Soil, so 22 dug vs 16 spent: "renewable" | Not found [unverified] |

## Findings per game

### Minecraft

- **Inventory:** 27 storage + 9 hotbar + 4 armour + 1 off-hand slot **[wiki: [Inventory](https://minecraft.wiki/w/Inventory)]**. Most items stack to 64, some to 16, tools do not stack. Components can raise the cap to 99 (Java) **[wiki]**.
- **Yield per dig:** one block breaks into drops by its loot table. Stone, ore and similar give nothing, or a lesser item, if the Tool tier is too low. Snow gives no snowballs without a shovel. Ground blocks drop without any Tool **[wiki: [Breaking](https://minecraft.wiki/w/Breaking)]**. This is a drop gate. The ticket's decided rule (a wrong Tool is slower and "never blocked") is the opposite design.
- **Speed vs tier:** the Tool sets a speed multiplier (no Tool 1, wood 2, stone 4, iron 6, diamond 8, gold 12), and the base time is hardness × 1.5 with the right Tool or × 5 without **[wiki: Breaking]**. The decided "5–10× slower for a wrong Tool" is close to Minecraft's × 5.
- **Crafting:** 2×2 grid in the inventory, 3×3 at a crafting table, shapeless and shaped recipes, a recipe book **[wiki: [Crafting](https://minecraft.wiki/w/Crafting)]**. Progression runs through Tool tiers, not a tech tree **[wiki: Breaking]**.
- **Bulk movement:** a chest holds 27 slots (54 double). Broken, its items drop. A shulker box keeps its contents inside the dropped item, is not stackable, and cannot hold another shulker box or a bundle **[wiki: [Chest](https://minecraft.wiki/w/Chest), [Shulker Box](https://minecraft.wiki/w/Shulker_Box)]**. A hopper moves 1 item per 8 ticks, 2.5 per second **[wiki: [Hopper](https://minecraft.wiki/w/Hopper)]**.
- **Gathered → placed:** one block is one item; the unit is the block. There is no volume.
- **Sync:** the container packets carry a "server-managed sequence number used to avoid desynchronization", and the server resends contents "in response to state ID mismatches" **[wiki: [protocol](https://minecraft.wiki/w/Java_Edition_protocol/Packets)]**. The wiki's Inventory page says only that Beta 1.0 moved to "a proper server-side inventory" **[wiki]**. I did not find the click packet's field list.

### Vintage Story

- **Inventory:** bags go in dedicated bag slots, from a 3-slot hand basket to a 14-slot sturdy mining bag. A mining bag takes only stones, ores, minerals and metals. A bag keeps its contents when removed. Stationary containers hold 4 to 36 slots, a crate holds one item type in bulk, and a cabinet limits items by physical space rather than slot count **[wiki: [Container](https://wiki.vintagestory.at/Container), the page the Inventory link resolved to]**. The page gave no weight or per-item stack size, and no base slot count.
- **Voxel crafting:** this is the closest game to this one's detail scale.
  - **Chiseling** divides a placed block into a 16×16×16 grid **[wiki: [Chisel](https://wiki.vintagestory.at/Chisel)]**. Chisel tiers differ only in durability (copper 600 to steel 6,000) **[wiki]**.
  - A guide says "In Survival a normal block is used up until that material's supply reaches 4096 voxels", that a partial block gives only the voxels it holds, and that the chisel itself loses roughly 1 durability per third of successful cuts **[community: [guide](https://vintagestory.info/en/guides/the-chisel-in-vintage-story/)]**. The wiki's own Chisel page did not state the 4,096 rule, and neither source says whether removed voxels come back.
  - **Smithing** is a voxel job on an anvil: "Each ingot contains 42 voxels, and each plate contains 81 voxels." Hits move metal between voxels; only the Split mode removes it, and a mis-hit work item can be cut back to 20 bits **[wiki: [Smithing](https://wiki.vintagestory.at/Smithing)]**.
  - **Metal in units:** one ingot is 100 units, equal to 20 nuggets, so 5 units a nugget **[wiki: [Ingot](https://wiki.vintagestory.at/Ingot)]**. A mold takes 20 nuggets, 100 units **[wiki: [Crafting](https://wiki.vintagestory.at/Crafting)]**.
- **Progression:** knapped stone tools, then clay forming and casting (copper), then smithing on copper, bronze and iron anvils **[wiki: Crafting]**.
- **What this means for 5 cm:** a 16³ chiseled block is 4,096 voxels. If a Vintage Story block is 1 m [unverified: I did not open a source for its size], its voxel is 6.25 cm, close to this game's 5 cm, where 1 m is 20³ = 8,000 Voxels **[calc]**. The player there is charged in whole blocks of supply, not per voxel **[community]**.

### Valheim

- **Inventory:** an 8×4 grid of 32 slots (eight are the hotbar), up to 48 after two purchases from Haldor. Carry weight is 300 base. Over the limit gives Encumbered (no run, no block, no dodge; moving drains stamina; speed set to 2 m/s) **[wiki: [Inventory](https://valheim.weirdgloop.org/w/Inventory), [Encumbered](https://valheim.fandom.com/wiki/Encumbered) via search excerpt]**. Slots and weight are two separate limits. Cart and ship slot counts came only from a guide **[community, unverified]**.
- **Terrain:** the Hoe levels ground and raises it for 2 Stone per use near a Workbench, within about 15–20 units. The page lists no drop from digging **[wiki: [Hoe](https://valheim.weirdgloop.org/w/Hoe)]**. Terrain here is a height map edit, not removed matter [unverified: I did not find a first-party statement].
- **Crafting stations are placed objects, and they gate recipes.** A Workbench builds within 20 m (4 m more per upgrade, to 36 m). It reaches level 5 by attaching four add-ons within 5 m, and each level lets items take one more upgrade **[wiki: [Workbench](https://valheim.weirdgloop.org/w/Workbench)]**. The Forge, Stonecutter and Artisan Table gate metal, stone and late pieces **[wiki: [Building](https://valheim.weirdgloop.org/w/Building)]**.
- **Building:** pieces are built with the Hammer from inventory materials; the page gives no per-piece cost, and it does not say whether removal refunds **[wiki: Building]**.
- **Structure:** support is per material (wood and stone 16 m straight up, corewood 24 m, wood-iron 50 m) **[wiki: Building]**. That is the structural-integrity item still undecided on the map.
- **Sync:** not found. [unverified]

### Astroneer

- **Inventory:** the Backpack has 8 inventory slots and 2 widget slots; the Terrain Tool adds 3 slots. Collected resource nuggets and printed items go to a free slot, and fall to the ground if none is free **[wiki, titled "Official": [Backpack](https://astroneer.wiki.gg/wiki/Backpack)]**.
- **Terrain matter is a resource.** Soil goes into canisters on the Terrain Tool or Backpack, and "is burned off by the terrain tool or drills if there is no canister available". The reticle shows a circle for the soil available, split by canister. Soil is spent to raise terrain or pave **[wiki: [Soil](https://astroneer.wiki.gg/wiki/Soil), [Terrain Tool](https://astroneer.wiki.gg/wiki/Terrain_Tool) via search excerpt]**. The pages give no per-dig amount or canister size.
- **Crafting:** the Backpack printer makes Small items for 3 power units and holds its own input slot; bigger items need the Small, Medium or Large Printer. Some recipes need Bytes **[wiki: Backpack]**. Bytes come from a Research Chamber **[wiki: [Research](https://astroneer.wiki.gg/wiki/Research)]**; the page does not list costs.
- **Placing:** one Small Canister of soil makes a Leveling Block, and the canister is returned **[wiki: Soil]**.

### Enshrouded

- **Terrain:** the pickaxe digs any terrain and "pays you in the material it digs"; the Construction Hammer removes terrain and places terrain material back; the rake flattens, raises and digs ditches. The world keeps terraforming only inside a Flame Altar's area (40 m cube at level 1 up to 160 m), and servers have a role permission for terraforming **[community: [Terraforming](https://enshroudedviki.com/terraforming); official: [Wake of the Water notes](https://enshrouded.com/en-US/news/enshrouded-wake-of-the-water-update) for the permission]**.
- **Bulk crafting unit:** "All building block recipes now yield 100 blocks … all roof block recipes now yield 50", and stack sizes were unified, none smaller than before **[official: Wake of the Water, 2025-11-10]**. The block size is not given by the sources I read; a community wiki says half a metre [community, unverified].
- **Undo and a full backpack:** 1.0 notes say undo of terraforming "now also works when the backpack is completely full", and then "the resources are not granted back" **[community: Terraforming; I did not find the original 1.0 note]**.
- **Building from chests:** you can build from nearby magic chests without carrying the materials **[community: [Building guide](https://enshroudedviki.com/building-guide)]**.
- **Gap:** I found no number for how much one dig yields or for backpack slots.

### Space Engineers

- **Inventory:** limited by volume (litres) and mass (kg). Once volume is reached, nothing more goes in. Identical items stack; some inventories take only some item types **[wiki, titled "Official": [Inventory](https://spaceengineers.wiki.gg/wiki/Inventory)]**.
- **Volume density:** "all container types store 1000 L of cargo per cubic meter of block volume", and stored items add to the ship's mass **[wiki: [Large Cargo Container](https://spaceengineers.wiki.gg/wiki/Large_Cargo_Container) via search excerpt]**. This is the one game where a metre cube is exactly 1,000 litres of capacity, as here.
- **Per-item numbers:** Iron Ore is 1 kg and 0.37 L per item **[wiki: [Iron Ore](https://spaceengineers.wiki.gg/wiki/Iron_Ore)]**. A Refinery returns 70% of the ore's weight as ingots; Iron Ingots take 0.13 L per kg **[wiki]**.
- **Yield per dig:** a voxel material has a `minedoreratio` (Iron 5, Ice 5, Nickel, Silicon, Cobalt, Magnesium 3, Silver, Gold, Platinum 1, Uranium 0.3). Holding left-click drops ores, ice and stone as items; right-click digs tunnels and "destroys ores, ice, and stone" **[wiki: [Ore](https://spaceengineers.wiki.gg/wiki/Ore)]**. I could not confirm the unit of the ratio.
- **Crafting:** Refinery (ore → ingots) → Assembler (ingots → components) → welding (components → block). Assemblers have a production queue and run slower without power **[wiki: [Assembler](https://spaceengineers.wiki.gg/wiki/Assembler)]**.
- **Bulk movement:** the conveyor system transfers "instantaneously" when the grid is powered and ports connect; production blocks push into connected inventories with room and clog when all are full; a sorter filters by list; small-grid ports cannot carry large items **[wiki: [Conveyor system](https://spaceengineers.wiki.gg/wiki/Conveyor_system)]**. No throughput rate is stated.
- **Sync, from the source code:** `MyInventory` is an `IMyEventProxy`. Its volume and mass are `Sync<MyFixedPoint>` values with `ValidateNever()`, so clients cannot set them. Pickup, drop, transfer and consume are `[Event, Reliable, Server]` requests sent with `MyMultiplayer.RaiseEvent`, and mutating methods check `Sync.IsServer`. The server's `FixTransferAmount` enforces capacity and either spawns the overflow as a floating object or lowers the amount moved. The item-type constraint is checked by the client before the request; I did not find the server check on that path **[official: [`MyInventory.cs`](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/Entities/Inventory/MyInventory.cs), fetched through the raw file]**. The same repo's `MyVoxelBase` broadcasts voxel shape operations as reliable events, per this repo's [networking research](voxel-networking.md).

### Dual Universe

The Fandom page fetches were refused (HTTP 402), so these come from search excerpts of the game's "Official" wiki titles **[wiki, excerpt only]**. Treat them as leads.
- **Volume and mass:** Container XS holds 1,000 L, S 8,000 L, M 64,000 L, each with its own mass. Talents change volume, mass and link distance. One storage area tops out near 8.2 million litres.
- **Ore is limited per territory, per hour:** territories have an "ore pool" in litres per hour; at 100% target density a Tier 1 mining unit spawns 2,400 L, or 4,200 L with maximum skills.
- **Hand mining:** core mining collects ore while the active inventory has room.
- **Gap:** how much ore one voxel removal gives, and the crafting model.

### 7 Days to Die

- **Inventory:** a grid of 5×9 = 45 slots, 27 open at the start; "Encumbered" appears when slots in use exceed Carrying Capacity; there is no weight. A separate Toolbelt takes overflow **[wiki, titled "Official": [Inventory](https://7daystodie.wiki.gg/wiki/Inventory)]**.
- **Dig → resource conversion, with a deliberate loss:** a shovel on Forest Ground gives 22 Clay Soil (the Mother Lode perk raises it); Top Soil also gives 22; Gravel gives 9 Small Stone and 8 Crushed Sand **[wiki: [Forest Ground](https://7daystodie.wiki.gg/wiki/Forest_Ground)]**. Crafting 1 Dirt or Top Soil costs 16 Clay Soil, and the wiki calls both "renewable". Clay Soil stacks to 6,000 **[wiki: [Clay Soil](https://7daystodie.wiki.gg/wiki/Clay_Soil)]**. A higher-quality shovel increases the amount **[wiki]**. Forest Ground has 250 hit points and mass 10 **[wiki: Forest Ground]**.
- **Crafting:** five stations: campfire, forge (has its own material inventory), workbench, cement mixer, chemistry station. They are crafted at a skill level, found broken in places, or bought. Recipes unlock through crafting skills, perk books and schematics; a locked recipe shows a padlock **[wiki: [Crafting](https://7daystodie.wiki.gg/wiki/Crafting)]**. Bulk-stack recipes are themselves unlocked (a 6,000-size stack of wood and others from one skill book) **[wiki]**.
- **Gap:** I could not open the structural-stability page, so I did not use it.

## What the games agree on, and what this game differs in

- **The unit of "how much" is always bigger than the smallest piece of matter**, and **there is a conversion rate** between a dug thing and a built thing: Minecraft 1:1, 7 Days to Die 22 vs 16, Enshrouded 100 blocks per craft, Vintage Story 4,096 voxels per whole block.
- **A cap by slots, volume or weight is what stops the player.** Games with volume (Space Engineers) let the stack be exact; games with slots (the rest) hide the unit inside a stack size.
- **Stations and containers are objects.** In all eight games, a crafting station, chest, printer or conveyor port has its own state, record and sync. Here, ADR 0011 says the Planet forgets Pieces, ADR 0002 rejects a second representation of matter, and the spec lists vehicles and mechanisms as out of scope. Whatever holds items or a recipe queue is a new thing the Planet must store or the player must carry.
- **Dig behaviour here differs from Minecraft's:** the Tool decides speed and Brush shape, never permission (decided 2026-11-28). Minecraft's gate on drops (no pickaxe, no stone) would have to be re-expressed as speed. I found no source on whether the other games gate yield by Tool.

## Options for BhayanakVoxel

Judged against the decided rules in the map's Notes: the M7 Tool rules (a Tool per Material; wrong Tool or hands 5–10× slower, never blocked; tiers set the speed; each Tool has its own Brushes; placing needs no Tool), ADR 0011 (Pieces are stamped Voxels the Planet forgets), Creative keeping free Brushes and Materials, Undo (ADR 0010), Debris, the Edit rate (ADR 0007, 8 m³/s), ADR 0003 (client-generated terrain, server-replicated Edits), and "Use the whole machine" (AGENTS.md).

A set of numbers used throughout **[calc]**: a Voxel is 5 cm, so 1 Voxel = 0.05³ m³ = 0.000125 m³ = 0.125 L. 1 m³ = 8,000 Voxels = 1,000 L. A 10 cm cube is 8 Voxels = 1 L. A 0.5 m cube is 1,000 Voxels = 125 L, which is about a pickaxe dig as #280 describes it. The Edit rate of 8 m³/s is 64,000 Voxels/s. The 63 Materials in `crates/materials/materials.toml` run from 100 kg/m³ (the lightest) to 9,000 kg/m³ (the heaviest); rock is 2,700, so a 1 m³ block of rock is 2.7 t and one Voxel of it is 0.3375 kg.

### A. What the inventory counts (the shape)

| Option | How it works | Fits | Costs / conflicts |
|---|---|---|---|
| **A1. A ledger by Material, capped by volume** (Space Engineers, Dual Universe) | The player holds an integer Voxel count per Material, and one capacity in litres. No slots for matter | The Edit already reports Voxels removed and added per Material, so the dig yield is the Edit result. Exact at any Brush size. A record is 63 counts, so it is small and simple to sync and journal | Needs a different home for non-matter items (Tools, Pieces, keys). A plain litre cap says nothing about *which* Material fills it |
| **A2. Slots with stacks measured in a volume unit** (Minecraft, 7 Days to Die, Valheim) | Cells hold one Material each, up to a stack size in units | Familiar. Slot count is a simple limit that is cheap to tune. Fits Tools and Pieces in the same grid | A dig can cross many Materials, so one Edit touches many slots, and a mixed dig can fill a pack with scraps. Stack size needs a unit (see B) |
| **A3. Weight limit from density** (Valheim: 300 base; Space Engineers mass) | Capacity in kg, taken from each Material's `density` | The data exists already (it is for Debris). Realistic: light Materials (snow 300, leaves 120) carry far more than metal (iron 7,870) | At true density, 300 kg is under 0.12 m³ of rock (about 890 Voxels) **[calc]**. That is a pocketful, so any building loop needs a non-physical limit or a large number. Players will not read kg per Voxel |
| **A4. Hybrid: a Material ledger plus slots for items** | Matter in the ledger (A1); Tools, crafted Pieces and keys in a small slot grid (A2) | Matches the split in the glossary: Material vs Tool vs Piece. Each part has the right unit | Two models to teach and to sync. The line between "matter" and "item" needs a rule for things like a metal ingot or planks |

### B. The unit of matter the player sees (sized for 5 cm)

| Unit | Voxels | Litres | Rock mass | Notes |
|---|---|---|---|---|
| The Voxel | 1 | 0.125 | 0.34 kg | Exact. A pickaxe dig is ~1,000 of them; a 1 m³ Piece 8,000. Not readable. Keep as the *stored* integer even if shown differently |
| The litre (10 cm cube) | 8 | 1 | 2.7 kg | A whole-number display of 8 Voxels, as in Space Engineers/Dual Universe. A dig of ~1,000 Voxels reads "125 L" |
| The half-metre cube | 1,000 | 125 | 337 kg | One pickaxe dig is about one unit |
| The cubic metre | 8,000 | 1,000 | 2.7 t | Minecraft's block size. A dig is a fraction (0.125) |

Games show their unit in a way the player can feel: Minecraft by block, Space Engineers by litre. Storing the Voxel count and displaying litres (or m³ above some size) is an option that costs nothing at storage time, and it keeps the unit a UI choice. The same unit question sets the stack size in A2.

### C. How gathered matter becomes placed matter

| Option | How it works | Fits | Costs / conflicts |
|---|---|---|---|
| **C1. Place Material directly** (Astroneer soil, Enshrouded terraforming) | A Brush Edit spends Voxels from the ledger | Matches "placing needs no Tool". One path for terrain fill, and Creative simply does not spend | Does nothing for detail (a wall is still a flat slab), the reason ADR 0011 gives for Pieces |
| **C2. Craft a Piece first, then place it** (Valheim pieces, Enshrouded blocks, Minecraft blocks) | A recipe turns Material into a Piece item in the inventory; placing consumes it | Gives Pieces a cost. The Piece stays a stamp, so Debris, Undo and LOD are untouched | A solid 1 m Piece is 8,000 Voxels (1,000 L, 2.7 t if rock). Charging its true volume makes a house unaffordable for a small pack; charging less than its volume makes Pieces create matter. See the cost options below |
| **C3. Both** (Pieces for building, Materials for terrain) | C1 for Brush-like fill and C2 for Pieces | Covers terrain repair (the Tool-owned Brushes) and architecture | The most to build and test. Two costs to keep fair against each other |

How a Piece is charged (under C2 or C3):

- **Its solid Voxel volume.** Honest and exact; stairs and slabs cost less than cubes. A 1 m solid wall Piece is 1,000 L, and a pack must be that large to build, which is a non-physical pocket by any of the games' standards.
- **A fixed recipe cost per Piece** (Valheim style). Simple to tune; unrelated to the Piece's size, so a small Piece (a stair, a slab) costs the same as a full cube unless the recipe is sized to it.
- **A batch recipe** (Enshrouded: 100 blocks per craft; 7 Days to Die: 22 dug vs 16 spent). The conversion rate carries the economy, so you can tune generosity without changing the count.
- **No refund on removal, by default.** ADR 0011 says a Piece is "dug like anything else" and cannot be taken back whole. Digging one returns its Voxels by Material (exact under A1), not the Piece. A refund-on-deconstruct rule like the ones some of these games have (not confirmed for Valheim: its wiki page does not say) would need the record ADR 0011 left out.

A Piece the Planet forgets cannot be picked up whole, since the Planet has no record of which Voxels were one. ADR 0011 considered "stamped, with a record of which Piece it was" and left it for when survival is designed, which is now.

### D. Crafting: recipes, stations, progression

| Option | How it works | Fits | Costs / conflicts |
|---|---|---|---|
| **D1. Craft anywhere, from the inventory** (Minecraft's 2×2, Astroneer's Backpack printer) | A recipe list in the UI; needs only the ledger | No placed objects to store, replicate or snap. Fits ADR 0011 | No station-driven progression; tiers must come from elsewhere |
| **D2. Stations as placed objects** (Valheim Workbench, 7 Days to Die forge, Space Engineers Assembler) | A station near the player unlocks recipes; some hold a queue or fuel | The strongest sense of place and progression. Valheim's add-ons show upgrade by building around a station | A station has state (a queue, fuel, an upgrade level). A Piece is forgotten Voxels. So a station is a second kind of placed thing with its own record, save and sync |
| **D3. Progression through Tool tier only** (the decided tiers: wood, stone, iron, steel, for example) | Better Tools, crafted from gathered Materials, are the tech tree. Recipes unlock by having found or dug a Material | Matches the decided rule that tiers set speed and "a low tier on very hard Materials is as slow as the wrong Tool". Minecraft and Vintage Story work this way | A tier gate on *crafting* is new; digging itself is never gated. Needs a crafting model for the first Tool (Vintage Story's knapping: two stones, no station) |
| **D4. Unlocks by research or discovery** (Astroneer Bytes, 7 Days to Die books and schematics, Enshrouded finding blocks) | Recipes are earned | Pacing and goals beyond Tool tiers | Extra systems (currency, found items). Not covered by the owner's rules; likely a later ticket |

Vintage Story's voxel crafts (knapping, clay forming, smithing) show voxel-level crafting being fun at 16³. They are the only voxel-resolution crafts in the research, and each is a separate mini-game with its own UI, not a recipe. That is a large addition for M7.

### E. Moving bulk Materials

| Option | How it works | Fits | Costs / conflicts |
|---|---|---|---|
| **E1. Carry only** | The pack is the whole system | Nothing new to store or sync. Fits the out-of-scope list (vehicles, mechanisms) | Large builds are limited by trips, so capacity (A) does all the work |
| **E2. A pile is a world object** (spill or heap as Voxels) | Dumped Material goes back into the Planet as Voxels (Space Engineers' boulders; Minecraft's item drops) | No new representation: a pile is Voxels, so Debris, LOD and Undo already apply | Not a *container*: it is open to the weather, to other players and to Fluids; nothing keeps it as an inventory |
| **E3. A container object** (chests, crates, storage vessels) | A placed object holds a ledger or slots | Familiar. Shulker-style "carries its contents" gives a cheap bulk carry | Same new-record problem as D2: its contents are state the Planet must save, replicate and bound. Space Engineers sets the lower bound: inventory changes are server requests |
| **E4. Vehicles and conveyors** (Minecraft hoppers, Space Engineers conveyors, Valheim carts) | A machine moves matter for the player | The most automation | The spec already lists vehicles and mechanisms as out of scope; conveyors are a serial cost for the server, and AGENTS.md calls a serial hot path a defect. Later, if ever |

Rates: a Minecraft hopper moves 2.5 items a second and a Space Engineers conveyor has no stated rate, so neither gives a number to size against. The number that matters here is the Edit rate, 8 m³/s = 64,000 Voxels/s. A conveyor would compete with it for the server's tick, and ADR 0007 leaves that tick budget undecided.

### F. What happens when the pack is full

| Option | Seen in | Trade-off |
|---|---|---|
| **The dig still happens; the overflow is lost** | Astroneer ("burned off") | Never stalls, never blocked. Wasteful, so the player needs a clear signal |
| **The dig still happens; the overflow spills as a pile** | Space Engineers `FixTransferAmount` spawns floating objects; Minecraft drops items | Nothing lost, but a pile is Voxels (E2) or entities, and either costs server work per spill |
| **The dig is refused** | Not seen in the researched sources | Conflicts with "never blocked" if read as including the pack |
| **Undo restores terrain but not the resources** | Enshrouded 1.0, as relayed by a community page | Keeps undo working at any fill level (ADR 0010); the refund is a rule to decide |

### G. Sync, saves, undo and the Edit rate

This part follows the decided ADRs and shows what changes. It makes no new decision.

- **Server-authoritative.** Space Engineers validates and applies every inventory change on the server, and clients cannot set volume or mass **[official, source code]**. Minecraft has a server-managed sequence number for container windows **[wiki]**. That matches ADR 0003, where the server owns every Edit.
- **A ledger update can ride on the Edit result.** The spec already requires an applied Edit to report the Voxels it removed and added per Material. The server adds that to the player's ledger and sends the new totals (63 Materials × 4 bytes ≈ 250 bytes **[calc]**, or only the changed entries). The client can predict its own dig the way it predicts the Edit, and reconcile on the ack.
- **Deterministic cost.** Counting Voxels per Material across a large Edit is a sum of integers, which AGENTS.md allows per thread (`par_fold` for integer sums), reduced in piece order. It adds no random state. At the 8 m³/s Edit cap it is at most 64,000 Voxel reads a second, which the Edit already does **[calc]**. It must be tested at 1, 4 and all threads like the rest. On the low-end profile (4 threads) it is part of the existing Edit pass.
- **Journal and replay (ADR 0010).** The journal records accepted Edits. If the ledger change is derived from an Edit, replaying the journal rebuilds the ledger (it needs the same Material pass); if the ledger is saved at each checkpoint, there is a possible gap of up to a second after a crash. That is a choice for the saves ticket, not this one.
- **Undo (ADR 0010).** Undo restores overwritten bricks. Whether it also gives back or takes the Materials is open (see F). A pile or a Piece placed after a dug Voxel would also change the ledger a second time.
- **Creative.** Creative needs no ledger: Edits spend and give nothing. The seam is the same Edit result, ignored.
- **Cheating and griefing** stay out of scope (protection is M2's separate effort).

## Gaps in this research

- **Dual Universe:** all facts are from search excerpts; no first-party page, and nothing on its crafting.
- **Valheim, Astroneer, Enshrouded, Vintage Story, 7 Days to Die, Dual Universe:** no first-party statement on inventory sync was found.
- **Enshrouded:** no figure for a dig's yield, backpack slots or block size. The "pays you in the material it digs" line is from a community wiki.
- **Valheim:** the Hammer's per-piece costs, the deconstruct refund and whether the pickaxe drops anything were not stated by the pages I read.
- **Space Engineers:** the unit of `minedoreratio` is not stated by the wiki page, and I did not read `MyVoxelBase` for how much ore a removed Voxel gives.
- **Vintage Story:** whether chiseled-away voxels come back is not stated by either source; the 4,096-voxel supply rule is from a community guide.
- **Minecraft:** the click-container packet's fields and its resync flow were not reached in the page I could read.
- **Nothing in this document was measured.** All numbers about this game are arithmetic from the repo's 5 cm Voxel and the data file.

## Sources

Everything cited above, by game. Apart from Enshrouded's patch notes and Keen's source code, these are wikis and community pages; I found no developer blog or talk on these questions.

- Minecraft Wiki: [Inventory](https://minecraft.wiki/w/Inventory), [Crafting](https://minecraft.wiki/w/Crafting), [Breaking](https://minecraft.wiki/w/Breaking), [Chest](https://minecraft.wiki/w/Chest), [Shulker Box](https://minecraft.wiki/w/Shulker_Box), [Hopper](https://minecraft.wiki/w/Hopper), [Java Edition protocol](https://minecraft.wiki/w/Java_Edition_protocol/Packets).
- Vintage Story wiki: [Chisel](https://wiki.vintagestory.at/Chisel), [Ingot](https://wiki.vintagestory.at/Ingot), [Crafting](https://wiki.vintagestory.at/Crafting), [Smithing](https://wiki.vintagestory.at/Smithing), [Container](https://wiki.vintagestory.at/Container). Community: [The chisel in Vintage Story](https://vintagestory.info/en/guides/the-chisel-in-vintage-story/).
- Valheim: [Inventory](https://valheim.weirdgloop.org/w/Inventory), [Hoe](https://valheim.weirdgloop.org/w/Hoe), [Workbench](https://valheim.weirdgloop.org/w/Workbench), [Building](https://valheim.weirdgloop.org/w/Building) (a community wiki); [Encumbered](https://valheim.fandom.com/wiki/Encumbered) (search excerpt only).
- Astroneer wiki ("Official" per its title): [Backpack](https://astroneer.wiki.gg/wiki/Backpack), [Soil](https://astroneer.wiki.gg/wiki/Soil), [Terrain Tool](https://astroneer.wiki.gg/wiki/Terrain_Tool) (search excerpt), [Research](https://astroneer.wiki.gg/wiki/Research).
- Enshrouded: first-party [Wake of the Water update](https://enshrouded.com/en-US/news/enshrouded-wake-of-the-water-update); community [Terraforming](https://enshroudedviki.com/terraforming) and [Building guide](https://enshroudedviki.com/building-guide).
- Space Engineers wiki ("Official" per its title): [Inventory](https://spaceengineers.wiki.gg/wiki/Inventory), [Ore](https://spaceengineers.wiki.gg/wiki/Ore), [Iron Ore](https://spaceengineers.wiki.gg/wiki/Iron_Ore), [Assembler](https://spaceengineers.wiki.gg/wiki/Assembler), [Conveyor system](https://spaceengineers.wiki.gg/wiki/Conveyor_system), [Large Cargo Container](https://spaceengineers.wiki.gg/wiki/Large_Cargo_Container) (search excerpt). First-party code: [`MyInventory.cs`](https://github.com/KeenSoftwareHouse/SpaceEngineers/blob/master/Sources/Sandbox.Game/Game/Entities/Inventory/MyInventory.cs).
- Dual Universe wiki (Fandom, "Official" per its title), search excerpts only: [Mining Units](https://dualuniverse.fandom.com/wiki/Mining_Units), [Item Containers](https://dualuniverse.fandom.com/wiki/Item_Containers), [Mining](https://dualuniverse.fandom.com/wiki/Mining).
- 7 Days to Die wiki ("Official" per its title): [Inventory](https://7daystodie.wiki.gg/wiki/Inventory), [Forest Ground](https://7daystodie.wiki.gg/wiki/Forest_Ground), [Clay Soil](https://7daystodie.wiki.gg/wiki/Clay_Soil), [Crafting](https://7daystodie.wiki.gg/wiki/Crafting).
- This repo: [ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md), [ADR 0007](../adr/0007-5-cm-voxels.md), [ADR 0010](../adr/0010-edit-journal-and-undo.md), [ADR 0011](../adr/0011-pieces-are-stamped-voxels.md), [spec](../spec.md), [`materials.toml`](../../crates/materials/materials.toml), [networking research](voxel-networking.md), [AGENTS.md](../../AGENTS.md).
