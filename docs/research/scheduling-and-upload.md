# Research: scheduling and upload hitches

Resolves [#75](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/75). Feeds the map [#71](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/71).
Refines the contracts of [#48](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/48), [#49](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/49) and [#64](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/64); touches [#47](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/47), [#50](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/50) and [#51](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/51).
Checked on 2026-10-31.

Markers used below: **[measured]** is a number this ticket measured on the code in `main` (8-vCPU Linux VM, release build, 8-core generation); **[estimate]** is arithmetic from a measured number, with the working shown; **[judgement]** is engineering opinion; **[unverified]** could not be checked against a primary source.

## TL;DR

- **The frame thread must never do work proportional to the Planet.** The two scene hitches both come from O(all bricks) work landing on the winit thread: `Scene::new` costs **1.25–1.37 s [measured]** for 2.63 M bricks, and re-creating the scene's GPU buffers re-uploads **136 MB [measured]**. The third, generation, is per-brick but bursty. Everything else on the edit path is already cheap: a 1 m dig's scene patch is **20–90 µs [measured]** and uploads **≈ 5 KB [measured]**.
- **The rebuild is not really compaction, it is bookkeeping.** 10 % of it is sorting 2.6 M brick coordinates (139 ms [measured]); the rest is grouping bricks by region into a `BTreeMap` and packing every brick again at ≈ 0.5 µs/brick [measured]. Compaction proper — copying live data down — never happens, because the code rebuilds from the `BrickStore` instead.
- **Compaction can be made unnecessary rather than fast.** The garbage comes from two accounting choices: a patch rewrites a whole region's 512 refs (≈ 1.1 K words per 1 m dig [measured]) and appends the changed bricks' words. Give each region a *run* of refs from a size-class free list, and reuse brick-data runs the same way, and a dig that does not change a region's occupancy rewrites in place: no garbage, no compaction, no rebuild trigger.
- **The missing-region rebuild is the blocker for streaming.** `Scene::update` rebuilds everything whenever a changed brick lands in a region the scene does not hold (`scene.rs:314`). Today's client packs the whole Planet so this is rare; with the rings of #48/#49 it is *every new region*, i.e. a 1.25 s freeze per frame of streaming. Incremental region add is not an optimisation, it is a prerequisite.
- **Generation dominates packing by two orders of magnitude.** A brick costs ≈ 60–190 µs of generation per core (the repo's own 20–25 s for 2.63 M bricks implies 60–80 µs) and 0.5 µs of packing [measured]. So scheduling is about *getting generation off the frame thread and in the right order*, not about packing faster.
- **Worker order: visible > prefetch > look-around, with cancellation and a starvation reserve.** A strict priority queue starves far levels while the player flies; a strict FIFO wastes a second of 8-core work after a 180° turn. Use class weights, an age boost, reserved in-flight slots per class, and drop jobs at brick granularity when the camera's desired set changes.
- **Keep the one-thread renderer; add workers, not a render thread.** winit's event loop owns the window and, on macOS, the main thread; a second render thread buys nothing here while adding a frame of latency. The seam that matters is "the frame thread only applies already-packed byte ranges, under a budget".
- **Ranked shortlist:** (A1) incremental region add, (A2) stable ref/brick runs with free lists, (A3) per-buffer growth instead of whole-scene re-creation, (A4) a per-frame upload budget with one staging belt; then (B1) generation pool with priority and cancellation, (B2) edit coalescing and one propagation per level, (B3) background defragmenter. All are image-identical — the same Voxels reach the GPU, only later and in smaller pieces.
- **The bench cannot see any of this today.** `client bench` never edits and never streams, and reports p95, not p99.9 or a hitch count. #64's path needs a dig storm, a turn, a teleport and a zoom, plus a `scene_work_ms` and `upload_bytes` series.
- **The 2 s spawn-ring target needs a size decision.** A 120 m full-res ring holds ≈ 1.1–2.5 M bricks at the Planet's crust thickness [estimate]; at today's 77–130 K bricks/s on 8 cores that is **9–32 s**, not 2 s. Hitting 2 s means ≈ 200 K bricks: a 90–120 m ring only one brick deep, or a 40–50 m ring with the full crust.
- **A 1 m dig is invisible above level 0 by the majority rule.** A coarse Voxel needs 4 of 8 child bricks to lose the same Voxel, and a 1 m crater touches two or three bricks. Measured: 19 level-0 bricks changed, 29 coarse bricks remade, **0 coarse bricks differ** across 19 levels. #49's "a crater dug at the surface is visible from orbit" is not true for the standard brush; it needs a big crater or a fine enough orbit level. This is a fidelity-rule consequence, not a scheduling bug.

**Plain-language recommendation.** Split the work by *who can afford to be slow*. Brick generation and ring packing go to background workers; the frame thread only copies small, already-packed byte ranges to the GPU within a fixed budget per frame. Make the scene's arrays reusable in place, so an edit rewrites a few kilobytes instead of triggering a 1.25 s repack, and add regions one at a time instead of refusing and repacking. Then measure the worst case with a bench path that actually digs, turns and teleports.

---

## 1. What "hitch-free" has to mean

The map's contract is 1440p60 on a 4090 (≈ 8.6 ms of GPU work in a 16.7 ms frame) with no visible change. For scheduling, that adds three requirements:

- **A frame budget for CPU scene work.** Not the whole frame: the GPU still needs its 8.6 ms. Treat **2 ms of scene work in a frame** as the ceiling; anything above is on the path to a missed vsync.
- **A hitch is a distribution, not a mean.** One 1.25 s frame in an hour is a hitch even if the mean is 16.7 ms. The bench must report p99.9 *and* the count and worst of frames over 33 ms (two vsyncs).
- **"No visible change" includes no holes and no stale Edits.** It is acceptable for a brick to arrive a frame or two late *if nothing is drawn in its place that would not have been drawn*; it is not acceptable to draw a background sky pixel through a hole, or to show the pre-edit terrain after an Edit was accepted. This constrains the order of uploads (§7), not just their size.

The three hitch classes named in the ticket map onto three different fixes:

| Hitch | Today's cause | Fix class |
|---|---|---|
| Generation | `planet_gen::generate_planet` is 34–62 s [measured] for 2.63 M bricks, and #49 wants it per-ring and on demand | worker pool, priority, cancellation (§6) |
| Scene rebuild | `Scene::new` on the frame thread, 1.25–1.37 s [measured] | incremental paths + stable storage (§5, §8) |
| Upload | whole-scene buffer re-creation, 136 MB [measured]; future ring swaps | byte budget + staging + growth policy (§7) |

## 2. What the code does today, measured

The client is one thread. `ControlFlow::Poll` redraws as fast as it can (`main.rs:526`); each redraw reads the server (`about_to_wait`, `main.rs:498`), applies the camera and draws. Edits arrive in the *event handler*: `WindowEvent::MouseInput` → `App::dig` → `World::dig` → `sim::apply` + `Scene::update` → `Renderer::update_scene` (`main.rs:450–464`). A large edit or a compaction therefore blocks input, the overlay and the next frame.

Measured on the test Planet (2,633,386 bricks, 59,325 regions, 33.3 M words = **136 MB**):

| Step | Time | Note |
|---|---|---|
| `planet_gen::generate_planet` | 34.2 s and 62.5 s in two runs | 8 workers; the repo's own note says 20–25 s (`planet-gen/src/lib.rs:170`); the VM is noisy, so quote the range |
| `BrickStore::sorted_coords` | 139 ms | 2.6 M coordinates, sorts all of them |
| `BrickStore::clone` | 277 ms | the "snapshot for a worker" cost, not free |
| `Scene::new` (full pack) | **1,304–1,368 ms** | code comment says ≈ 1.6 s (`scene.rs:307`); 0.50 µs/brick |
| — of which table/regions/refs/bricks | 2.0 / 4.0 / 19.1 / 110.0 MB | 136 MB total |
| `Scene::update`, 1 m brush | p50 0.02, p95 0.03, p99 0.05, max 0.09 ms | 3,479 patches |
| `Scene::update` → `Rebuilt` | **1,254 ms** once, after 2,177 digs | 1.1 K words of garbage per dig |
| bytes uploaded per dig | ≈ 5.3 KB | 18.5 MB over 3,479 digs |
| `sim::apply`, 1 m brush (r=10) | mean 0.57, p99 1.03, max 1.09 ms | on the frame thread, before the patch |
| `sim::apply`, 2 m brush (r=20) | mean 4.2, p99 9.8, max 10.7 ms | **a hitch the ticket does not list** |
| `BrickStore::build_lod(19)` | 7.85 s | 20 levels, 410 K + 72 K + 14 K + … bricks per level |
| `BrickStore::refresh_lod`, 19 levels, per edit | mean 0.25, p99 0.42 ms | 29 coarse bricks remade per 1 m dig |
| coarse bricks whose content changed | **0.0** per 1 m dig | majority downsampling hides a 1 m hole at every level |

Two things fall out immediately:

1. **The per-edit path is already fine.** 20–90 µs to patch and 5 KB to upload means a player digging as fast as the client accepts edits never causes a scene hitch. The 1.25 s is an *amplification* event, not the cost of editing.
2. **The brush itself is the second-largest per-edit cost.** A 2 m brush (the largest #15 allows) spends 4–10 ms in `sim::apply` on the frame thread. That is 25–60 % of a frame, for a single click. It is invisible in every current measurement because the bench never digs.

### Where a rebuild comes from

`Scene::update` (`scene.rs:311`) has exactly two exits to `Scene::new`:

- **A changed brick in a region the scene does not hold** (`scene.rs:314–317`). The scene cannot add a region: `regions` is uploaded with no spare capacity (`tracer.rs:214`) and the table is sized once. So it rebuilds everything. This is rare today (the whole Planet is packed) and *constant* once rings stream.
- **Garbage over the spare budget** (`scene.rs:346–349`): `garbage > len/16 + 4096`. Garbage grows by ~1.1 K words per 1 m dig, so the test Planet reaches it after ~2 K digs. The ticket's "~1,000 digs" is the same phenomenon at a denser dig rate or a 2 m brush.

There is no third exit, and no compaction that copies live data: the "compaction" *is* a full rebuild from the store, which is why it costs 1.25 s rather than a memcpy.

### Where an upload hitch comes from

`Tracer::update_scene` (`tracer.rs:406`) does one of two things:

- `Patched`: one `queue.write_buffer` per changed region record (68 B each) plus one per appended tail — fine.
- `Rebuilt`, **or any capacity overflow** (`tracer.rs:420`, `tracer.rs:424–427`): `SceneBuffers::new`, which allocates and fills **all five** buffers again — header, table (2 MB), regions (4 MB), refs (19 MB), bricks (110 MB) — and a new bind group. Every word is written through a `mapped_at_creation` mapping, so the CPU touches all 136 MB before the frame can be submitted.

So a rebuild is a 1.25 s CPU pack followed by a ~136 MB upload. The upload part is **5–25 ms** [estimate: 136 MB at 20–25 GB/s of PCIe 4.0 ×16 usable bandwidth, plus the CPU-side staging copy at ~10 GB/s] — a dropped frame or two rather than a freeze, but it lands right after the freeze, so the same click costs a stall and then a stutter.

Two more one-time stutters sit on the same path: `Renderer::set_scene` (`window.rs:147`) constructs a whole new `Tracer`, which re-creates both compute pipelines (planet-arrives hitch, behind the loading title); and every scene update calls `accumulator.reset()` (`tracer.rs:413`), which restarts the reference image — correct, but it means the reference mode never converges while anything streams. The fidelity gate must therefore render with streaming quiesced, or the gate will measure a permanent 1-sample image (§10).

## 3. Decomposing the 1.6 s

| Part | Cost | Why | Structural fix |
|---|---|---|---|
| Sort 2.6 M coords | 139 ms [measured] | determinism: `sorted_coords()` sorts every brick so the pack is order-independent (`store.rs:202`) | pack per region from an index that is already ordered, or sort only the region list (59 K) and keep bricks in coordinate order inside a region |
| Group into 59 K regions | ~300–600 ms [estimate] | `BTreeMap<[i32;3], Region>` with a `Vec` per region, 2.6 M inserts | add/remove regions incrementally; never re-group |
| Pack every brick | ~500–800 ms [estimate] | `pack_brick` per brick, 0.5 µs/brick [measured] | pack only changed bricks (already the patch path) |
| Allocate and grow `bricks` | ~50–150 ms [estimate] | `Vec` doubling to 110 MB; ~35 reallocs, ~200 MB copied | `with_capacity`/growth hysteresis, or a chunked arena |
| Re-create GPU buffers | 5–25 ms [estimate] + a new bind group | all five buffers, `mapped_at_creation` | grow only the buffer that overflowed (§7) |

The lesson: most of the rebuild is bookkeeping that exists only because the code chose to rebuild *from the store* rather than patch *in place*. A rebuild is a symptom of a data structure that cannot be updated; the fix is the update path, not a faster rebuild.

A parallel `Scene::new` (a thread pool over regions) would take 1.25 s → 0.2–0.4 s [estimate, 8 cores, medium confidence]. It is a 2–3 day stopgap, not a fix: it is still a 200–400 ms freeze per event.

## 4. The target architecture

```
           input, camera                         ready byte ranges + region records
  ┌──────────────────────────┐   edits   ┌───────────────────────────────────┐
  │ frame thread             │──────────▶│ world thread                      │
  │  winit + wgpu device     │           │  BrickStore, Predictor            │
  │  trace, present          │◀──────────│  Scene(s), pack, edit bookkeeping │
  │  applies ≤ budget of     │  batches  │  coalesces, orders, marks dirty   │
  │  already-packed writes   │           └───────┬───────────────┬───────────┘
  └──────────────────────────┘                   │ jobs          │ changed bricks
                                                 ▼               ▼
                                        ┌──────────────────────────────────┐
                                        │ N-2 workers                      │
                                        │  generate(level, coord) → Brick  │
                                        │  pack(Brick) → words             │
                                        │  pack ring slabs → words         │
                                        └──────────────────────────────────┘
```

Four invariants do the work:

1. **One owner per mutable thing.** The frame thread owns the device and queue; the world thread owns the store and the packed arrays; workers own nothing but their job. No locks on the frame path, and determinism is preserved because packing is a pure function of a brick.
2. **The frame thread's work is bounded by bytes, not by scene size.** It applies an `UploadBatch` of at most `upload_budget_bytes` and `upload_writes` per frame. It never packs, never sorts, never allocates a scene buffer during play.
3. **Data before reference.** Within a batch, brick data is written before the region record that points at it, and the whole batch is submitted with the frame that draws it. Then a region can never point at bytes the GPU has not received, so a partial upload cannot produce a hole.
4. **Every operation is either O(changed) or budgeted.** Generation and ring packing are O(their job) on a worker; editing is O(brush); defragmentation is O(chunk) and off the frame path. No operation is O(Planet) after startup.

Where the pieces live is deliberately close to today: the store and `Scene` stay on the world thread (a `std::thread` with channels, as `sim::spawn` already does for the server), because `sim::apply` is 0.6–10 ms and does not belong on the frame thread either. The frame thread still needs a dig target: either the world thread answers the single cursor ray within the frame (a channel round trip when the world thread is idle, tens of microseconds), or it publishes a read-only snapshot of the scene with each batch.

**Do not add a render thread.** winit's event loop owns the window and, on macOS, must run on the main thread; a dedicated render thread would add a frame of input latency and a second command queue without removing the real cost, which is CPU work on the planet-sized arrays [judgement]. The wording in #49 — "off the UI thread" — should be read as "off the frame thread", and satisfied by the world thread and the pool.

## 5. Ranked shortlist

None of these changes a pixel or a GPU pass: the same Voxels reach the GPU, only later and in smaller pieces. So the ranking is by *hitch removed per agent-week*, then by the VRAM they cost. Frame time is unaffected except negatively — an unbudgeted upload delays a frame's dispatch — which is why A4 is in tier A.

| # | Technique | Hitch win | Confidence | Cost (agent-wk) | Risk | Fidelity | Depends on | VRAM | Tier |
|---|---|---|---|---|---|---|---|---|---|
| A1 | Incremental region add/remove + table growth | removes the 1,250–1,370 ms missing-region rebuild; adds 30–100 µs/region | high on mechanism, medium on per-region cost | 0.5–1 | table growth needs a header update and a 0.5–4 MB re-upload | none: the table is an index; the same Voxels are shown | #48 (ring unit), #49 | +2 MB table slack | must-have |
| A2 | Stable ref runs + brick-data free lists | garbage/dig 1.1 K words → ≈ 0; compaction never triggers; patch stays 20–90 µs | medium-high | 1–1.5 | shader lookup change; free-list bugs are silent | none: same Voxels (buffer layout may differ) | #48 | refs ≤ 2× worst case (+19 MB max), typically ~0 | must-have |
| A3 | Grow only the buffer that overflowed, with geometric slack | removes the 136 MB all-buffer re-upload; leaves one buffer at 1.25× | high | 0.5 | per-buffer capacity and bind-group bookkeeping | none | #48 | +25 % on the grown buffer (~28 MB for `bricks`) | must-have |
| A4 | Per-frame upload budget + one `StagingBelt` batch | bounds frame upload to 0.2–0.6 ms; removes 10–25 ms spikes | medium | 0.5 | starving the budget produces holes | none if the publish order holds | #48, #49 | a few MB of staging chunks | must-have |
| B1 | Generation pool: classes, age, cancellation | after a 180° turn, time-to-correct-view cut by ~40–70 %; removes queued-stale work | medium | 1–1.5 (mostly #49) | starvation of low classes; cancellation races | none | #47, #49 | none | high-value |
| B2 | Edit coalescing + one propagation per level | per-click `apply` 4–10 ms (2 m brush) and patch × 20 levels → one per frame | medium | 0.5 | a delayed predicted Edit is visible latency; bound to 1 frame | predicted Edit must still show in its frame | #48, #50 | none | high-value |
| B3 | Background/budgeted compaction | the last O(planet) op becomes 0.1–0.2 ms/frame or off-thread | medium | 0.5–1 | needs quiesced regions | none | A2 (or instead of it) | 2× one moved buffer transiently | conditional |
| C1 | Move `sim::apply` off the frame thread | 4–10 ms per 2 m dig → 0 | medium | 0.5 | prediction feel, one frame of latency | none | #49 | none | conditional |
| C2 | Render thread split (Bevy-style) | none for this bottleneck | high (it does not help) | 2–3 | platform event-loop rules | none | — | +1 frame of buffers | rejected |

### A1. Incremental region add (and remove)

*What.* Give the scene an `add_region(key) -> index` and a `remove_region(index)` that append/free a 17-word record and insert a table entry, growing the table by doubling when its load factor passes ~50 %. Upload the new record and the new table (0.5–4 MB) before any brick in the region is referenced.

*Why it matters.* It is the difference between "streaming works" and "streaming freezes once per frame". #48's acceptance criterion "re-centres rings without re-uploading unchanged bricks" is unreachable while a new region forces a full rebuild.

*Numbers.* Region record: 17 words = 68 B. Table at 45 % load today: 2 MB for 59 K regions, so doubling is a 2–4 MB upload ≈ 0.1–0.2 ms of PCIe [estimate]. A `Scene::new` over the same region's bricks is 512 × 0.5 µs = 0.26 ms [estimate] if done as a region-sized pack. Both are comfortably under the 2 ms frame ceiling.

*Risk.* The header (`table_mask`) and the table binding both change on growth, so the bind group must be rebuilt and the new buffer bound before the frame that uses it; the old table must outlive the frame in flight (wgpu keeps it alive with the bind group, so this is a "keep the old bind group until the frame submits" issue, not a use-after-free).

*Experiment.* E4.

### A2. Stable ref runs and brick-data free lists

*What.* Today a patch gives the region a *new* run of refs at the end of `brick_refs` and abandons the old run (`scene.rs:320–345`), then appends every changed brick's words. Replace both with size-class allocators:

- **Refs**: a run is `popcount(mask)` words, allocated from buckets rounded up to a power of two (1, 2, 4, …, 512). If the new popcount fits the old bucket, rewrite in place, zero garbage.
- **Brick data**: a mixed brick's size depends only on its palette length and index width (17 + ⌈palette/2⌉ + 16·bits words). With a full palette that is 34, 51, 69, 89, 113 words for 1–5 bits, and a dig usually leaves a two-Material brick at 34. Allocate from per-size free lists; a solid brick costs zero words and frees its old run.

*Numbers.* Today: 1.1 K words (≈ 4.6 KB) of garbage per 1 m dig [measured], hitting the ~2 M-word spare after ~2 K digs. With in-place runs, a 1 m dig rewrites at most one or two runs (≤ 2 KB each) plus ≈ 27 bricks × 34–113 words (≈ 4–12 KB) *in place* when the size is unchanged, which it is for a dig that only removes Voxels from a brick already mixed. Worst case a brick changes size class and the old run is freed for reuse: still zero growth in steady state.

*Cost in memory.* Refs currently 19.1 MB; power-of-two buckets would put a thin crust at ≤ 38 MB worst case, and most regions sit at 1–7 bricks so the waste is small. Simpler alternative if that matters: exact-length free lists (a `Vec<Vec<usize>>` indexed by length) — 512 buckets, O(1), no waste, slightly more bookkeeping. Either way compaction stops being a thing that happens during play.

*Risk.* The ref lookup computes `brick_refs[first_ref + popcount(lower mask bits)]` (`scene.rs:393`, mirrored in `trace.wgsl`); rewriting a run in place keeps that formula. Freeing a run must happen only after the batch that last referenced it has been submitted — hold freed runs for two frames (or until the submission index retires).

*Fidelity.* None: the same Voxels produce the same image. Buffer *layout* will differ from a from-scratch pack, because a free list reuses slots; that is fine (buffers are not shipped), but the content invariant must be tested: 10,000 random digs must show the same Voxels as a from-scratch pack, which `an_updated_scene_shows_the_same_voxels_...` already does for a handful.

*Experiment.* E1: the dig storm must produce zero `Rebuilt` and zero growth events after warm-up.

### A3. Per-buffer growth

*What.* `SceneBuffers` holds `capacity: [usize; 2]` but re-creates everything on any overflow (`tracer.rs:424–426`). Grow only the buffer that overflowed, to `max(len × 1.25, len + 1 MiB)`, and keep the others and the bind group (rebuilding the bind group only for the buffer that changed).

*Numbers.* A `bricks` overflow today costs a 136 MB re-upload of every buffer. With geometric slack it happens every ~25 % growth, and only that one buffer is affected: create it at the new size with `STORAGE | COPY_DST | COPY_SRC`, copy the old contents device-to-device (110 MB at ≈ 500 GB/s ≈ 0.2 ms [estimate]), fill the new tail from the CPU (27 MB ≈ 1–3 ms), and keep the old buffer alive until the frame that used it retires. The worst case is therefore one 27 MB tail upload, not 136 MB.

*Risk.* Two buffers alive at once during a grow (+25 % VRAM for a frame); the bind group must be rebuilt, and `COPY_SRC` must be added to the usage (free on desktop Vulkan).

*Experiment.* E1 with a `buffer_growths` counter: the worst frame around each growth event.

### A4. Per-frame upload budget

*What.* The world thread emits ordered `UploadBatch { writes: Vec<(buffer, offset, bytes)> }`. The frame thread takes at most `budget_bytes` (default 4 MB) and `budget_writes` (default 64) per frame into a `StagingBelt`, finishes it and submits with the frame. Leftovers stay queued; edits (≈ 5 KB) always fit and are never delayed.

*Why a belt.* `Queue::write_buffer` copies into staging memory immediately, and "currently on native platforms… the staging memory will be a new allocation… released after the next submission" ([wgpu 30.0.1 `Queue` docs](https://docs.rs/wgpu/30.0.1/wgpu/struct.Queue.html#method.write_buffer)). `StagingBelt` exists exactly for "writing very many small pieces of data" and sub-allocates chunks; its docs recommend a chunk size "larger than the largest single write" and "1–4 times less than the total amount of data uploaded per submission" ([`StagingBelt` docs](https://docs.rs/wgpu/30.0.1/wgpu/util/struct.StagingBelt.html)). With 64 writes per frame that is 64 allocations per frame without it.

*Numbers.* 4 MB/frame at 60 fps = 240 MB/s ≈ 1 % of a PCIe 4.0 ×16 link (16 GT/s, 128b/130b → ≈ 31.5 GB/s per direction, 20–25 GB/s usable [estimate]). The cost per frame is therefore not bandwidth but the CPU staging copy: 4 MB at ~10 GB/s ≈ 0.4 ms [estimate, medium confidence], which is why the budget is ~4 MB and not 40. The transfer itself is queued before the frame's dispatches, so it adds its copy time to that frame's critical path — which is exactly what the budget bounds.

*Starvation.* If the budget cannot keep up with the ring, holes appear. Budget must be sized from the worst *steady* demand (ring re-centre at 50 m/s), and readiness targets should be checked against it: a 200 K-brick spawn ring (≈ 27 MB) at 4 MB/frame is 7 frames (0.12 s), so budget is not the limiting factor for spawn — generation is (§6).

*Experiment.* E2.

### B1. Generation pool: classes, age, cancellation

Covered in §6. *Experiment:* E3.

### B2. Edit coalescing and one propagation per level

*What.* Collect edits for one frame (or 16–50 ms), dedupe by brick coordinate, apply once, and propagate to the LOD pyramid and to every resident ring/level once, in one batch. Today each click runs `apply` + `refresh_lod` + `scene.update`, and each server message can run it again (`world.rs:60–75`); a burst of clicks amplifies each other through `Predictor::apply_authoritative`'s revert-and-replay.

*Numbers.* `sim::apply` is 0.57 ms for a 1 m brush and 4.2 ms for a 2 m brush [measured]; `refresh_lod` over 19 levels is 0.25 ms per edit [measured]. Coalescing k clicks in a frame turns k × (apply + refresh) into 1 × (apply over the union + refresh), which for clicks in the same crater is close to a single 2 m brush. Bound the window to one frame so a predicted Edit is never visibly late; there is no fidelity cost because the world thread's state is the *final* state and the upload batch is atomic.

*Experiment.* E1, counting `apply` calls and batches per frame.

### B3 / C1 / C2

B3 is only needed if A2 is skipped, or as a safety net for size-class fragmentation; C1 is the "move `sim::apply` off the frame thread" option; C2 is rejected (§12).

## 6. Worker pool, priorities, cancellation

**The job is one brick, not one region.** Generation is deterministic per `(seed, level, coord)` (`planet-gen/src/lib.rs:1–12`), so a job needs no shared state and can be cancelled at a brick boundary. A brick is 60–190 µs of work per core [measured: 2.63 M bricks / 34–62 s / 8 workers, and the repo's own 20–25 s], so mid-brick cancellation is not worth the complexity: the worst delay after a cancelled pop is one brick.

**Four classes, weighted, with age.**

| Class | What | Why it exists |
|---|---|---|
| P0 nearest/visible | the bricks the current view's rays can reach first (the fine ring in front of the camera) | the player sees them now |
| P1 prefetch | the slab the camera will cross into next (50 m/s × 2 s ahead) and the next LOD level at the incoming transition | hides the streaming latency |
| P2 look-around | the rest of the resident rings, including behind | turning must not reveal holes |
| P3 far/orbit | coarse levels not currently in view | needed for a Zoom, not for this frame |

Strict priority is wrong in both directions: P0 always full starves P3 forever (the player who flies continuously never gets an orbit view), and a strict FIFO wastes a second of 8-core work after a 180° turn. Use **deficit round robin across classes** (e.g. 6:3:2:1 slots per 12) with **an age boost** (a job waiting > 500 ms moves up one class) and **per-class in-flight caps** so a burst of P0 cannot occupy all workers. With work stealing, the classic makespan bound is `T_p ≤ T_1/p + O(T_∞)` ([Blumofe & Leiserson, *Scheduling Multithreaded Computations by Work Stealing*, JACM 1999](https://dl.acm.org/doi/10.1145/324133.324234)); with priorities there is no such bound, which is exactly why the reserved slots are needed [judgement].

**Cancellation.** The world thread owns a *desired set* per class (ring level → set of region coords). When the camera moves or turns, it diffs the desired set and bumps an epoch. Workers pop jobs and discard those whose epoch is stale or whose key is no longer desired; the queue itself is not rebuilt (a stale entry costs one hash lookup to reject). Turning 180° at 35° half-FOV invalidates most of P0/P1, so the epoch check is what keeps a turn from costing a second of wall time.

**Fairness numbers.** A 1 m dig changes 19 level-0 bricks [measured]; at 5 digs/s that is ~95 bricks/s to pack against 77–130 K bricks/s to generate, so edits cannot starve generation. The real starvation risk is P0 generation starving P3 during continuous flight, and P2 starving while the player digs in one spot — both are handled by the per-class caps, not by tuning the queue.

**Spawn/stream arithmetic.** The crust runs from `CORE_RADIUS` (Voxel 1462) out to the local surface (mean 1500, relief ±34), so it is 38 ± 34 Voxels thick: 4–9 brick layers. A 120 m-radius full-res ring therefore holds ≈ 1.1–2.5 M bricks [estimate: 4π·120² m² ÷ 0.8² m²/brick ≈ 283 K bricks per layer × 4–9 layers]. At 77–130 K bricks/s aggregate that is **9–32 s**, not the 2 s in #49. To reach 2 s the ring must be ≈ 200 K bricks: a 90–120 m ring one brick deep, or a 40–50 m ring with the full crust. This is a *size* decision for #48/#49, not something scheduling can fix. Conversely, the orbit target is comfortable: levels 1–4 hold 411 K + 72 K + 14 K + 3 K bricks [measured], so "whole-Planet coarse LOD in ≤ 5 s" is 4–6 s of generation at today's rate and less with #47's direct generation.

## 7. Per-frame upload budget

**Bytes.** Default 4 MB/frame, 64 writes/frame; both configurable and reported. Rationale: 4 MB is ~0.4 ms of CPU staging and ~0.2 ms of link time at 60 fps [estimate], it is ~5× the largest steady demand (a ring re-centre slab is ≈ 5.7 K bricks × 136 B ≈ 0.8 MB [estimate]), and it absorbs a teleport's burst over a few frames.

**Command-buffer time.** Each `write_buffer`/belt write is a copy command in the frame's encoder; 64 small copies cost ~64 × 5–20 µs of driver/CPU work [estimate] and a little GPU copy time. The belt turns the allocations into chunk sub-allocations; the writes themselves stay. Keep "one write per contiguous run, coalesced across regions if adjacent" as the packing rule.

**Staging and double buffering.** No double buffering of the scene is needed: wgpu orders queue writes before the frame's dispatches, so the GPU reads a consistent snapshot within a submit. What *is* needed is a **publish order** across frames: never write a region record until every byte it points at has been written, and keep freed runs alive until the submitting frame retires (two frames of grace). That, plus "edits bypass the budget", is what keeps a partial upload invisible.

**The render thread.** As in §4: keep one thread; the budget is the mechanism that keeps it off scene-sized work [judgement].

## 8. Scene update paths, garbage, fragmentation, compaction

- **`Patched`** is the good path and stays: rewrite the region record in place, rewrite the changed bricks in place (A2), and report exactly the words that changed. With A2 the patch is O(brush), not O(region).
- **`Rebuilt` should disappear from the play path.** Two exits cause it; A1 removes the first, A2 removes the second. Keep `Scene::new` for startup and for tests, and keep `Rebuilt` as a correctness escape hatch for a store/scene mismatch (a desync), not as a steady-state path.
- **`spare_words`** (`scene.rs:79`) is `len/16 + 4096`: 6.25 % slack bought with 8.5 MB of VRAM on the test Planet, and a compaction every 2 K digs. With A2 the slack can stay (it is the free-list headroom) but the compaction trigger is no longer reached.
- **Fragmentation.** A bump allocator with variable-size runs fragments in two ways: external (free gaps too small for the next run) and size drift (a brick's palette grows from 2 to 3 Materials on a placement, changing its word count). Power-of-two buckets bound internal waste at < 2×; exact-length free lists remove drift entirely. With ~10 size classes in practice, fragmentation between classes is what a defragmenter would fix — and with A2 it should be rare enough to run only while the camera is still.
- **When to compact.** Only when (a) a free-list class cannot satisfy a request *and* (b) the wasted bytes exceed ~10 % of the array. Then compact in region-sized chunks (0.1–0.2 ms each) on the world thread, or ship quiesced regions to a worker. Never on the frame thread, never as one operation.

## 9. Edit-aware invalidation and coalescing

An Edit's blast radius is larger than one scene:

1. **Level 0 bricks.** `sim::apply` changes 19 bricks for a 1 m brush [measured]; the client patches those.
2. **The store's LOD pyramid.** `refresh_lod` remakes one ancestor per level per changed brick: 29 coarse bricks for 19 changed bricks over 19 levels, 0.25 ms [measured].
3. **Every resident ring/level showing those bricks.** Each ring is its own `Scene`, so each changed coarse brick maps to a `(ring, brick)` and a patch of that scene. With R rings and L levels that is ≤ L patches per edit, each ~20–90 µs — 0.4–1.8 ms across 20 levels, which is why coalescing (B2) matters more as levels multiply.
4. **The path tracer's history.** Every `update_scene` resets the reference accumulator (`tracer.rs:413`). For #51's TAAU, invalidating history for an off-screen brick is wasted convergence; invalidate only when the changed brick is inside the view's rings.

**Coalescing policy.** Batch per frame, dedupe by brick coordinate (already a `BTreeSet` in the store path), and propagate once per level per batch. Debounce the *upload* by at most one frame and never the *prediction*: a click must show its hole in the frame it is clicked, and the batch is atomic, so there is no stale image. Under a dig storm, the batch grows sub-linearly because overlapping brushes share bricks.

**Invalidation must be exact, not conservative.** Over-invalidating (uploading a whole ring because one brick changed) is a correctness-safe but budget-hostile choice; under-invalidating leaves a stale Edit, which the fidelity gate forbids. The rule is: the set of changed coarse bricks is exactly `LodRefresh::changed` (`store.rs:141`), and the set of scene records to patch is exactly the regions holding them.

## 10. Measurement

Today's bench cannot see any of this:

- It draws a path and never edits or streams (`bench/mod.rs:269–342`).
- It reports mean, p95 and max (`bench/stats.rs`), and `p95` of 240 frames is the 228th sample — p99.9 would just be the maximum.
- `frame_time` is the CPU wall time of one draw + readback, with no breakdown, and the report has no upload or worker fields.

Add, in this order:

1. **A hitch definition and counter.** Frame interval from the start of `RedrawRequested` to the start of the next; a hitch is > 33 ms (two vsyncs) or > 2× the rolling median. Report count, worst, and the frame index of the worst (so a screenshot can be taken there).
2. **p99 and p99.9.** Report both, but be honest about the sample size: p99.9 of 3,000 frames is the third-worst frame, which is a useful tail statistic; of 240 frames it is simply the maximum. Use 3,000 (50 s at 60 fps) for the hitch path, or keep 240 and lean on the hitch count instead.
3. **`scene_work_ms`:** time inside `world.dig`, `world.poll`, `scene.update`, `update_scene`. Separate from `frame_time`.
4. **`upload_bytes`, `upload_writes`, `upload_queue_depth`:** what the budget actually spent.
5. **Worker telemetry:** busy workers / total, queue depth per class, jobs cancelled, mean and p99 job time, stale jobs executed. Sampled atomics, no locks.
6. **Readiness timings:** spawn ring, first frame with no missing in-view bricks, orbit level set, per-ring re-centre.

**Bench paths that force the worst case** (each versioned, deterministic, `--short`-able on a patch):

- `planet-dig-storm-v1`: fly the low pass at 50 m/s while digging 5×/s for 20 s (≈ 100 digs), then 1,000 digs standing still. Forces the garbage/compaction boundary and the per-frame edit upload.
- `planet-turn-v1`: 180° in one frame at 50 m/s, then hold. Forces cancellation and time-to-correct-view.
- `planet-teleport-v1`: two keyframes 1 frame apart on opposite sides of the Planet. Forces a whole ring rebuild and the spawn path.
- `planet-zoom-v1`: surface → orbit → surface in 30 s. Forces level-set changes and the coarse-generation path.
- `planet-hitch-gate-v1`: the reference image rendered twice, once with streaming live and once with it quiesced, diffed. Forces the "no visible change" claim to be measured.

CI (CPU-only, no GPU) should guard the invariants that regress silently: 10,000 random digs never return `Rebuilt`; garbage per dig ≤ K words; bytes per dig ≤ K; the Voxels after an incremental patch equal those of a from-scratch pack (the existing `an_updated_scene_shows_the_same_voxels_...` test, widened); and the same edit history packs to the same bytes.

## 11. Experiments for the 4090

1. **E1 — dig storm, before and after A1–A4** (`planet-dig-storm-v1`). Expect today: one 1,250 ms frame and a 5–25 ms upload frame; after: zero frames > 33 ms, `scene_work_ms` p99.9 ≤ 2 ms, `upload_bytes` ≤ 4 MB/frame. This is the ticket's own worst case and the single most informative run.
2. **E2 — upload budget sweep** (1, 2, 4, 8, 16 MB) on `planet-teleport-v1`. Plot worst frame, frames > 33 ms, and time-to-correct-view against bytes/frame. Finds the knee; expect 4–8 MB [estimate, medium confidence].
3. **E3 — cancellation** (`planet-turn-v1`). Measure stale jobs executed and time-to-correct-view with and without the epoch check. Expect 40–70 % less stale work [estimate, low confidence].
4. **E4 — spawn and orbit readiness** (`planet-teleport-v1`, `planet-zoom-v1`). Reports whether the 2 s/5 s targets in #49 are met, and by how much; expect spawn to miss unless the ring shrinks (§6).
5. **E5 — image-identity gate** (`planet-hitch-gate-v1`). The reference image with streaming on vs. all-resident, after convergence: must be a zero-pixel difference. This is the only experiment that proves "without changing the image" rather than asserting it.

## 12. Rejected and watchlist

- **A faster `Scene::new` (parallel sort/pack).** 1.25 s → 0.2–0.4 s [estimate]. Cheaper to write (2–3 days) but still a freeze; keep as a stopgap only if A1/A2 slip.
- **Rebuild on a worker, swap frames later.** Needs a 277 ms `BrickStore::clone` [measured] or a snapshot scheme, 2× the scene's VRAM (272 MB), and a 136 MB upload at the swap. It converts one 1.25 s freeze into a smaller freeze plus memory. Watchlist.
- **Double-buffered scene with a bind-group flip for ring swaps.** Attractive for #48 (a ring becomes visible atomically), but 2× VRAM for every ring. Fine for the *table* (2 MB), not for `bricks` (110 MB). Watchlist per ring size.
- **Lower LOD / proxy geometry while a ring loads.** Changes the image. Rejected by the fidelity bar.
- **`device.poll(Wait)` to know when an upload landed.** A synchronous stall on the frame thread; use `on_submitted_work_done` callbacks and a two-frame grace instead ([`Queue` docs](https://docs.rs/wgpu/30.0.1/wgpu/struct.Queue.html#method.on_submitted_work_done)).
- **A dedicated render thread.** §4. Rejected for this bottleneck.
- **Building the scene on the GPU.** Out of scope by the map's "current architecture" boundary; it is also the only thing that would make the upload question disappear, so note it for the far future.
- **Mesh fallback, impostors, checkerboard, baked GI.** Out of scope / rejected by the map.

## 13. Risks and open questions

- **The 2 s spawn ring is a size decision.** Confirm the intended ring radius and crust depth for #48/#49; at today's generation rate the ticket's number implies ≈ 200 K bricks.
- **`sim::apply` at 2 m is a hitch (4–10 ms).** Decide whether to move it off the frame thread (C1) or to cap the brush's client-side application rate.
- **A 1 m dig does not change any coarse brick.** Whether that is acceptable is a fidelity-rule question for #71: "visible from orbit" must mean a large crater, because a crater has to be roughly half a coarse brick across to flip a coarse Voxel — ≈ 0.8 m at level 1, doubling per level — and at level 9 that is ≈ 200 m, larger than the Planet's relief. Either the criterion is about big craters, or the orbit ring must carry levels fine enough to show the crater the player actually dug.
- **Table growth needs a bind-group swap.** wgpu keeps a resource alive while a live bind group references it; the risk is dropping the old table or bind group before the frame that uses them is submitted. Keep both for two frames and test it, as with freed runs.
- **Free-list reuse across frames.** Freed runs must not be reused before the submission that last referenced them retires; the ring-of-two-frames rule needs a test that would catch a violation (render a frame, free, reuse, compare).
- **Worker count.** Leave one core for the frame thread and one for the driver: `N-2` workers. On an 8-core machine that is 6, i.e. 75 % of today's generation throughput; the readiness estimates above should be scaled by 1.33 if the pool is sized that way [estimate].
- **Determinism.** Packing on workers must produce the same bytes in the same order; keep the canonical region order and a golden-hash test over the packed arrays, as `building_twice_gives_identical_buffers` does today.
- **Unverified here:** exact PCIe usable bandwidth on the dev's board; the driver's behaviour for `mapped_at_creation` on a 110 MB buffer; whether wgpu's Vulkan backend grows a buffer in place.

## Appendix A. Arithmetic

- **Scene size.** 2,633,386 bricks → 59,325 regions × 17 words = 1,008,525 words = 4.0 MB; table 131,072 slots × 4 words = 524,288 words = 2.0 MB; refs 4,767,183 words = 19.1 MB; bricks 27,501,920 words = 110.0 MB. Total 33.3 M words = **136 MB** [measured].
- **Pack rate.** 1,304–1,368 ms / 2,633,386 bricks = **0.50 µs/brick** [measured].
- **Generate rate.** 2,633,386 / (34.2 s…62.5 s) = 42–77 K bricks/s aggregate; the repo's own 20–25 s → 105–132 K bricks/s. Per core at 8 workers: 5–16.5 K bricks/s = **60–190 µs/brick**, with 60–80 µs at the repo's rate.
- **Garbage per dig.** (5,336,390 − 4,895,740) words refs + (28,088,046 − 27,646,556) words bricks over 250 digs = 1,138 words/dig ≈ 4.6 KB [measured]. Spare = live/16 + 4096 ≈ 2.1 M words; a rebuild was measured at dig 2,177, the same order.
- **Upload per dig.** 18.5 MB / 3,479 digs = 5.3 KB [measured]. At 5 digs/s that is 27 KB/s.
- **PCIe.** 16 lanes × 16 GT/s × 128/130 ≈ 31.5 GB/s per direction; 20–25 GB/s usable [estimate]. 136 MB → 5.4–6.8 ms at link rate, 10–25 ms including CPU staging [estimate].
- **Spawn ring.** 4π(120 m)² / (0.8 m)² = 283 K bricks per brick layer; crust 38 ± 34 Voxels = 4–9 layers → **1.1–2.5 M bricks**; /77–130 K/s = **9–32 s** [estimate]. Two seconds ≈ 200 K bricks: a 90–120 m ring one layer deep, or a 40–50 m ring with the full crust.
- **Orbit levels.** Level 1 410,877; 2 71,680; 3 14,095; 5 717; 9 8 bricks [measured]. Sum of levels 1–4 ≈ 500 K → 4–6 s at today's rate.
