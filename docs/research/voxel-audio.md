# Research: ray-traced audio on a voxel world

Resolves issue [#153](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/153), part of the inspirations map [#130](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/130). It follows [Decide which inspiration features to adopt](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/138), which put audio in M1b: basic positional audio is the floor, and ray-traced occlusion and reverb are added only if this research shows they fit. Researched November 2026. Terms in **bold** follow [`CONTEXT.md`](../../CONTEXT.md). Voxels are **5 cm** ([#135](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/135); the switch is [#147](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/147), not built yet, so the code still says 10 cm).

Every factual claim links to its source. **[measured]** marks a number I timed for this ticket (§6). **[estimate]** marks my own arithmetic, with the working shown. **[judgement]** marks engineering opinion. **[unverified]** marks what I could not check against a primary source. No GPU was run for this ticket, and no audio was played.

## TL;DR

- **The basic floor: kira 0.12.6, pure Rust, MIT/Apache-2.0, with no middleware.**
  - kira (S14) gives the floor out of the box: spatial tracks with distance attenuation and left/right panning, low-pass filter and reverb effects, send tracks, and a cpal backend (WASAPI on Windows, ALSA/PipeWire/PulseAudio on Linux; S17).
  - It also has a `MockBackend`, so CI can test audio without a sound card.
  - It runs its own mixer on cpal's audio callback thread, and the frame thread drives it through handles. That leaves ADR 0006's threads as they are.
  - It has no HRTF. Stereo panning is enough for the floor **[judgement]**.
- **The ray-traced layer fits M1b in a reduced form: occlusion plus *parametric* reverb, written by us, traced on the CPU through the existing `render::reference::trace()`.**
  - "Parametric" means a few numbers per update (enclosure, decay time, early delay, wet level, the direction of the outdoors) driving kira's filter and reverb. Convolution impulse responses are not part of it.
  - It is Vercidium's and Sound Physics Remastered's design, both of which shipped in voxel games (S3, S8). Steam Audio's physically based convolution reverb is not part of M1b.
- **Measured CPU cost [measured, §6].** `render::reference::trace()` was run on a Zen 3 core (Ryzen 7 5700G; the dev's PC has a Ryzen 7 5800X, also Zen 3, S23), at 10 cm, through the bench's real cave site:
  - **1.0 µs per ray segment inside the cave tunnel**;
  - **2.0–3.3 µs per segment at the cave mouth outdoors**, where most rays fly hundreds of metres to distant ground.
  - At 5 cm **[estimate]**, assume up to 2× that: 2–6.5 µs a segment.
- **Budget [estimate, §6]:**
  - **occlusion:** 32 sources × 9 rays at 5 Hz (1,440 rays/s);
  - **listener reverb:** 32 rays × 4 bounces, with a line-of-sight ray back each bounce, at 10 Hz (2,560 segments/s);
  - together **~67 segments a frame at 60 fps, 0.14–0.47 ms of frame-thread CPU**, and **0 ms GPU**.
- **Recommended budget line:** **audio ≤ 0.5 ms of frame-thread CPU per frame (mean), ≤ 1.0 ms p99, 0 GPU, 0 BVH work**, timed in a new `Row::Audio` CPU row on the cave stills and a forest view. The mixer has its own rule: no audio-callback underruns in the bench.
- **Why the frame thread, not an audio-simulation thread.** The frame thread already owns a CPU copy of the scene that it traces to aim ([ADR 0006](../adr/0006-scene-thread.md)). Tracing it anywhere else would need a lock on that copy, a second 542 MiB copy, or a new coarse acoustic grid. The ray budget is fixed per second, whatever the number of players.
- **Rejected or deferred:**
  - **Steam Audio through audionimbus: deferred** to a post-M1b spike. The C library is Apache-2.0 and maintained, and it accepts our own ray tracer through callbacks (S9, S12). But:
    1. audionimbus 0.16.2 hands Steam Audio a **null material pointer** from custom ray-tracer hits, and Steam Audio's reflection and transmission code dereferences it (S9, S12). That is a crash as shipped, by my reading of both sources **[not run]**.
    2. Its reflection cost is rays × bounces × (1 + sources), and its defaults are 4096 rays × 4 bounces at 10 Hz (S9, S11).
    3. It adds a ~180 MB SDK download and Clang to the build (S12).
  - **FMOD, Wwise: rejected.** They are proprietary, with revenue or budget thresholds (S18, S19), and their Rust bindings are stale (S13).
  - **Resonance Audio and Project Acoustics: rejected.** Both repositories are archived (S21), and Project Acoustics is a bake, which Edits break.
  - **The Vercidium Audio SDK: rejected as a dependency, techniques adopted.** It has no Rust bindings, needs a paid licence, and no longer traces voxels (S4, S5).
  - **GPU tracing: deferred.** The rays cost the GPU almost nothing, but the results need an async readback, and a stalled readback is what the bench avoids.
- **Decisions this forces (for the map):**
  1. A new client-only `audio` crate. The server never links it, as ADR 0001 keeps the server free of wgpu.
  2. Each Material gains two lines in `materials.toml`: `acoustic` (an absorption/scattering/transmission class, seeded from Steam Audio's presets) and `sound` (the footstep/dig/place sample set).
  3. `reference::trace` gains a maximum distance.
  4. A `Row::Audio` CPU row and an audio budget line.
  5. Linux CI installs `libasound2-dev` (and `libdbus-1-dev` for kira's default real-time feature; S14, S17).
  6. Wind audio reads #152's client-only wind clock.
  7. Sound assets need their own licence check.

## Sources and how they were used

| Key | Source | Type | Used for |
|---|---|---|---|
| **S1** | J. Lin, ["Ray Traced Reverb, Wind and Sound Occlusion (Path Traced Voxel Project)"](https://www.youtube.com/watch?v=UHzeQZD9t2s), YouTube, 19 Mar 2020, 1:42; description pulled with yt-dlp | **First-party** | "RTX reverb, wind and sound occlusion using OpenAL", in his *old* engine. No method is described, and there is no narration |
| S2 | [John Lin water research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-water/docs/research/inspiration-john-lin-water.md) §2 "Streaming and audio", on branch `research/inspiration-john-lin-water` | Our research | The water video lists "sound calculations" as still to improve, and its sound track was not analysed |
| **S3** | Vercidium (M. Robinson), ["A First Look At Raytraced Audio"](https://www.youtube.com/watch?v=u6EuAUjq92k), YouTube, 5 Apr 2025, 7:55; auto-captions and description pulled with yt-dlp | **First-party** video | The voxel-era design: rays from the player, line-of-sight rays back on each bounce for echo, blending small-room/large-room/outdoor states, escaped rays for outdoors and the direction of rain, thickness ("permeation") rays, background CPU threads, "a voxel grid which is much faster to ray trace against than triangulated models", and coarser voxels to run faster |
| **S4** | Vercidium, ["I Built Raytraced Audio for Godot"](https://www.youtube.com/watch?v=A6bPUXTlic8), YouTube, 1 Sep 2026, description | **First-party** | "Realistic energy models, materials, optimisations and no more voxels". The SDK has no dependencies, and the plugins use OpenAL Soft or FMOD |
| **S5** | [vercidium.com](https://vercidium.com/), read Nov 2026 | **First-party** product page | "Free for non-commercial use"; Indie licence "$300AUD" per title; C and C# SDKs, no Rust; "runs on background threads on the CPU"; compute shaders on the roadmap |
| **S6** | [vercidium-audio/vaudio-unreal README](https://github.com/vercidium-audio/vaudio-unreal) (MIT plugin, pushed 6 Nov 2026) | **First-party** | Example ray counts: listener occlusion 1024 rays × 8 bounces, permeation 256 × 4, per-source reverb 64 × 64. *Leech* sources reuse an emitter's rays ("gunshots and footsteps on one character"). Ambient sounds are filtered by listener enclosure |
| S7 | Vercidium, ["Raytraced Audio – No More Voxels"](https://www.patreon.com/posts/raytraced-audio-142709378), Patreon | First-party, **search snippet only** (HTTP 403 to fetch and curl) | "about 8x faster than voxel raymarching" after moving to 3D primitives **[unverified]** |
| **S8** | [henkelmax/sound-physics-remastered](https://github.com/henkelmax/sound-physics-remastered), source at HEAD (pushed 29 Oct 2026), **GPL-3.0** | **First-party** source | A Minecraft (voxel) mod. Defaults are 32 reverb rays × 4 bounces per sound, ≤ 16 occlusion rays, and 9 occlusion paths unless "strict". Four OpenAL EFX reverb slots are blended. There is per-block reflectivity and occlusion (stone 1.5, wool 0.1, sand 0.2 …). It clones the level within 4 chunks so it can trace off the main thread. **Technique only; GPL code must not be copied** |
| **S9** | [ValveSoftware/steam-audio](https://github.com/ValveSoftware/steam-audio) at `master`, v4.8.1 (11 Feb 2026), Apache-2.0: `core/src/core/phonon.h`, `custom_scene.cpp`, `reflection_simulator.cpp`, `direct_simulator.cpp`, `LICENSE.md`, README | **First-party** source | Platforms; `IPLHit` and `IPLMaterial`; the 11 material presets (3-band absorption, scattering, 3-band transmission); `IPL_SCENETYPE_CUSTOM` callbacks; `hit.material->…` dereferences; per-bounce, per-source shadow rays |
| **S10** | Valve, [Steam Audio C API programmer's guide](https://valvesoftware.github.io/steam-audio/doc/capi/guide.html) | **First-party** docs | Custom ray tracers ("reuse your existing ray tracing infrastructure"); reflections "typically run in a separate thread" and come "at a significant CPU usage cost"; listener-centric reverb "independent of the number of sources" |
| **S11** | Steam Audio Unity integration, [`SteamAudioSettings.cs`](https://github.com/ValveSoftware/steam-audio/blob/master/unity/src/project/SteamAudioUnity/Assets/Plugins/SteamAudio/Scripts/Runtime/SteamAudioSettings.cs), and the [settings page](https://valvesoftware.github.io/steam-audio/doc/unity/settings.html) | **First-party** | Real-time defaults: 4096 rays, 4 bounces, 1.0 s, ambisonic order 1, 32 sources, 5% of CPU cores, 0.1 s update interval, ≤ 16 occlusion samples |
| **S12** | [audionimbus](https://github.com/MaxenceMaire/audionimbus) **0.16.2** (3 Nov 2026), MIT OR Apache-2.0: README and `src/callback.rs`, `src/ray_tracing.rs` at `14e4ffd` | **First-party** source | It tracks Steam Audio 4.8.1, with an `auto-install` download (≈180 MB) and Clang ≥ 9 required. It has `CustomRayTracer`. Its callbacks write `material: std::ptr::null_mut()` into every `IPLHit` |
| **S13** | crates.io API and GitHub repository metadata, read 7 Nov 2026 | Registry data | The versions, dates, licences and download counts in §2 |
| **S14** | [kira](https://github.com/tesselode/kira) **0.12.6** (28 Oct 2026), MIT OR Apache-2.0: `crates/kira/src/{track/sub/spatial_builder.rs, effect/*, backend/mock.rs}`, `Cargo.toml` | **First-party** source | Spatial tracks (distances, attenuation easing, `spatialization_strength` panning); effects (filter, reverb, delay, EQ, compressor); a custom `Effect` trait; `MockBackend`; default features `cpal`, `cpal-realtime`, `cpal-realtime-dbus` |
| S15 | [Firewheel](https://github.com/BillyDM/Firewheel) **0.14.0** (5 Sep 2026), MIT OR Apache-2.0: README, `DESIGN_DOC.md`, `firewheel-nodes/src/spatial_basic.rs` | First-party | An audio graph planned to become Bevy's default; `spatial_basic` is "basic (and naive)"; HRTF is an unchecked roadmap item |
| S16 | [rodio](https://github.com/RustAudio/rodio) 0.23.0 `src/spatial_player.rs`; [fyrox-sound](https://github.com/FyroxEngine/Fyrox/tree/master/fyrox-sound) 1.0.1 README | First-party | rodio's `SpatialPlayer` is "a simple spatial effect". fyrox-sound has HRTF (IRCAM HRIRs) and reverb, MIT, usable outside Fyrox |
| **S17** | [cpal](https://github.com/RustAudio/cpal) 0.18.2 README | First-party | Hosts: WASAPI (Windows); ALSA plus PipeWire/PulseAudio/JACK (Linux). The Linux build needs `libasound2-dev`, and `realtime-dbus` needs `libdbus-1-dev` |
| S18 | FMOD licensing, text from [fmod.com/licensing](https://www.fmod.com/licensing)'s page bundle (`/bundle.js`; the page is client-rendered) | First-party | Indie: "Free or $2,000/title", "budgets under $600k", "Free when company revenue <$200k" |
| S19 | Audiokinetic, [Wwise pricing](https://www.audiokinetic.com/en/wwise/pricing/) (curl; WebFetch got 403) | First-party | Indie free under a "$250K" production budget; Pro "Starting at $8,000 US"; a free trial "limited to 200 media assets" |
| S20 | [kcat/openal-soft](https://github.com/kcat/openal-soft) `COPYING` (LGPL v2), release 1.25.2 (12 May 2026) | First-party | The OpenAL route Lin (S1), Vercidium (S4) and Sound Physics (S8) use; its LGPL licence |
| S21 | [resonance-audio/resonance-audio](https://github.com/resonance-audio/resonance-audio) (archived, last push 8 Sep 2022); [microsoft/ProjectAcoustics](https://github.com/microsoft/ProjectAcoustics) (archived; last release 2022.1, which loads baked "ACE files") | First-party repositories | Both are archived |
| **S22** | This repo at `146cae6` (`main`): `crates/render/src/{reference.rs, tracer.rs, cpu.rs}`, `crates/render/src/shaders/probe.wgsl`, `bins/client/src/{main.rs, world.rs, loader.rs, scene_worker.rs, bench/caves.rs}`, `crates/materials/materials.toml`, `crates/planet-gen/src/world_map.rs`, `.github/workflows/ci.yml`, `docs/agents/benchmarking.md` | Our code | The CPU tracer, threading, Materials, biomes, CI |
| **S23** | A timing test I wrote for this ticket and then deleted (§6), run `--release` on a Ryzen 7 5700G; the dev's PC CPU read over SSH (`Win32_Processor`: AMD Ryzen 7 5800X, 8 cores / 16 threads) | **Measurement** | CPU cost per acoustic ray segment |

## 1. What the engine has today

Read from `main` at `146cae6` (S22).

- **No audio at all.** No crate in the workspace depends on an audio library, and `materials.toml` has no acoustic or sound field.
- **A CPU tracer the client already uses.** `render::reference::trace(scene, ray) -> Option<Hit>` is the line-for-line CPU copy of `trace.wgsl`. `Hit` carries `t` (in level-0 Voxels), the entry-face `normal`, the `voxel` and its `material`.
  - The frame thread calls it through `World::aim` to aim Edits, against its own copy of the packed scene ([ADR 0006](../adr/0006-scene-thread.md): "the one that is traced on the CPU to aim").
  - It has **no maximum distance**: a ray runs until it hits something or leaves the ball round the Planet.
- **A GPU probe exists.** `Tracer::trace_rays` sends rays through the real WGSL `trace()` with `probe.wgsl`. It is a one-shot with its own submit and a blocking readback, made for tests.
- **Client threads:**
  - the frame thread (winit, render, World, Edits);
  - the scene thread (ADR 0006);
  - `workers_for(cores) = cores − 2` ring-loader workers (`loader.rs`), so **14 on the dev's 16-thread 5800X** (S23);
  - the single-player server thread (`sim::server`).
  A 16.7 ms frame leaves the frame thread most of its time free. The last recorded baseline was "frame CPU 0.63 ms" (the dev's 4090 bench notes on main `c152cef`).
- **CPU timing rows exist.** `render::cpu::Row` (`SimTicks`, `UpdateScene`, `GroundFill` …) feeds the overlay and the bench's CPU numbers, so an `Audio` row has an obvious home.
- **Inputs for the floor already exist client-side:**
  - the Material under the character's feet (the controller reads Voxels);
  - the Material's `hardness`, which already sets dig time (`world.rs`: `dig_seconds(target_hardness(…))`);
  - the biome at any point, from `planet_gen::world_map::Biome { Temperate, Desert, Snow, Volcanic }`, which the client can evaluate because it generates terrain itself ([ADR 0003](../adr/0003-client-generated-terrain-edit-ops.md)).
- **There is a `water` Material (`fluid = "water"`) but no flow until M4** ([#19](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/19)). M1b water sound must come from where water Voxels are, not from flow.
- **CI** runs Linux tests with lavapipe and builds Windows (`ci.yml`). Only `mesa-vulkan-drivers` is installed, so no ALSA headers.

## 2. Audio stack options in Rust

Versions, dates and licences from crates.io and GitHub on 7 Nov 2026 (S13). "Recent" is crates.io's 90-day download count.

| Option | Version (date) | Licence | What it gives | Fit for us |
|---|---|---|---|---|
| **kira** (S14) | 0.12.6 (28 Oct 2026); 1.1 M downloads, 286 k recent | MIT OR Apache-2.0 | Mixer with tracks, sends, tweens and clocks. Spatial sub-tracks with listener, distances, attenuation easing and panning strength. Effects: low-pass/high-pass filter, reverb (comb/all-pass), delay, EQ, compressor, distortion. A custom `Effect` trait. cpal backend; `MockBackend` for tests | **Recommended.** Everything the floor needs, plus the knobs the parametric layer drives (a filter cutoff per source, a reverb send). Pure Rust; agents know it. No HRTF |
| **rodio** (S16) | 0.23.0 (11 Oct 2026); 13.8 M, 3.9 M recent | MIT OR Apache-2.0 | Playback, decoding, `SpatialPlayer` ("a simple spatial effect") | Simpler than kira and weaker for games: no tracks or sends. Fine for UI sounds, not chosen |
| **cpal** (S17) | 0.18.2 (16 Aug 2026); 27.7 M | Apache-2.0 | Device I/O only (WASAPI, ALSA/PipeWire/PulseAudio/JACK, CoreAudio) | Comes with kira. On its own we would write a mixer, which agents can do but is pointless **[judgement]** |
| **Firewheel** (S15) | 0.14.0 (5 Sep 2026); 67 k | MIT OR Apache-2.0 | Real-time-safe audio graph, Freeverb, convolution node, `spatial_basic` ("basic (and naive)"); "planned to be upstreamed into the Bevy game engine" | A good engine, but its direction follows Bevy's needs. Keep as the alternative if kira stalls **[judgement]** |
| **oddio** | 0.7.4 (15 Oct 2023); 62 k | MIT OR Apache-2.0 | Lock-free spatial mixer with propagation delay | No release in three years. Rejected for maturity |
| **fyrox-sound** (S16) | 1.0.1 (28 Mar 2026); 85 k | MIT | Spatial sources, **HRTF** (IRCAM HRIR spheres), reverb; usable outside Fyrox | The only pure-Rust stack with HRTF built in. A fallback if HRTF becomes a requirement; the `hrtf` crate (0.9.0, MIT, same author) can also be wrapped as a kira `Effect` |
| **Steam Audio** via **audionimbus** (S9, S12) | Steam Audio 4.8.1 (11 Feb 2026); audionimbus 0.16.2 (3 Nov 2026); 17.6 k | Apache-2.0 (Steam Audio); MIT OR Apache-2.0 (wrapper) | HRTF, ambisonics, occlusion and transmission, real-time reflections, convolution, parametric or hybrid reverb, baking, pathing. Ray tracers: built-in, Embree, Radeon Rays, or **custom callbacks**. Windows, Linux, macOS, Android, iOS | The strongest physics, and the callbacks let it trace our Voxels. Deferred, for the reasons in §3.4 |
| `steam-audio` crate | 0.4.0 (26 Jan 2025); 11 recent downloads | non-standard | Older wrapper | Rejected: inactive |
| `phonon` (phonon_rs) | 0.2.0 (2 Oct 2024) | Apache-2.0 | Partial Rust port of Steam Audio | Rejected: inactive, partial |
| **FMOD** via `libfmod` / `fmod-oxide` | libfmod 2.222.6 (Oct 2024, FMOD 2.02.22); fmod-oxide 0.2.2 (Jul 2026, MPL-2.0) | Proprietary. Indie "Free or $2,000/title", budget < $600k, free when revenue < $200k (S18) | Full middleware with the Studio authoring tool | Rejected: a proprietary binary plus a GUI authoring workflow that agents can't drive **[judgement]**, a licence to track, and thin bindings |
| **Wwise** via `rrise` | rrise 0.2.3 (Nov 2022) | Proprietary. Indie free under a $250K budget; Pro from $8,000 (S19) | Full middleware, Spatial Audio rooms and portals | Rejected: same reasons, and the binding is abandoned |
| **Resonance Audio** | — | Apache-2.0 | HRTF and room effects | Rejected: archived since 2022 (S21) |
| **OpenAL Soft** (S20) | 1.25.2 (12 May 2026) | LGPL v2 (dynamic linking) | 3D sources, HRTF, EFX reverb and filters. What Lin (S1), Vercidium's plugins (S4) and Sound Physics (S8) use | Mature, but a C library under the LGPL, and no maintained Rust binding (crates.io shows `openal-soft-sys` 0.1.0 with ~400 downloads). kira covers the same EFX-style controls **[judgement]** |

**How kira fits the client** (S14, S22). `AudioManager::new` starts cpal's output stream; with kira's default features, cpal asks for real-time priority. kira mixes on cpal's callback thread. The frame thread sends commands through handles: positions, filter cutoffs, reverb mix. Those commands go through kira's own command queues, so the frame thread never waits for the mixer. Nothing in ADR 0006's threads changes. The server binary must never depend on it, as ADR 0001 says of wgpu. Tests use `MockBackend`.

## 3. Acoustic ray tracing

### 3.1 Prior art

- **John Lin (S1, S2).** In March 2020 his old engine had "RTX reverb, wind and sound occlusion using OpenAL": hardware ray tracing on the GPU, with OpenAL (S20) for playback and EFX reverb. He never described the method.
  - The January 2021 water video lists "sound calculations" as still to improve, and its audio was not analysed (S2).
  - **So "John Lin's cave water sound" is the 2020 system applied to water, by inference. No source describes ray-traced water audio** **[unverified]**.
- **Vercidium, voxel era (S3), April 2025.** The clearest description of a game-ready design:
  - **Occlusion ("green rays"):** rays travel "directly towards the music. The more green rays there are, the clearer the sound." They are re-traced live as the building changes.
  - **Echo ("blue rays"):** rays fan out from the player. "Each time the rays bounce, they'll check for line of sight back to your player." The engine blends a small-room state and a large-room state "based on the length of the blue rays", and makes the echo louder the more blue rays return.
  - **Outdoors:** rays that reach the level's edge "escaped outdoors". Indoor and outdoor states blend by the share that escape. "The average direction of all escaping rays is the direction we should hear the rain from."
  - **Permeation ("orange rays"):** rays "travel all the way to the sound". The longer they spend inside a wall, "the more energy they'll lose".
  - **Where it runs:** "on background threads on your CPU". It is traced "against a voxel grid which is much faster to ray trace against than triangulated models. The size of the voxels can also be increased to make it run even faster."
- **Vercidium, current SDK (S4–S7).** "Realistic energy models, materials … and no more voxels" (S4). Per material: absorption, scattering and transmission, producing EAX reverb parameters (S5).
  - The Unreal plugin's example counts are listener occlusion 1024 × 8 and per-source reverb 64 × 64 (S6).
  - *Leech* sources reuse one emitter's rays, for example a character's footsteps and gunshots (S6). That is the pattern for dozens of players.
  - His Patreon claims primitives are "about 8x faster than voxel raymarching" (S7, **[unverified]**). That is against *his* voxel raymarcher, so it says nothing about ours.
- **Sound Physics Remastered (S8): a shipped voxel game, Minecraft with OpenAL EFX.**
  - Each sound is evaluated when it plays: **32 rays × 4 bounces** by default ("Decrease this value if you experience lag spikes when sounds play"). Moving sounds are re-evaluated every 5 ticks only if enabled.
  - Occlusion uses up to **16 rays** and 9 paths ("strict" uses 1).
  - The result is blended across **4 fixed EFX reverb slots**.
  - **Per-block reflectivity:** stone 1.5, metal 1.25, glass 0.75, wood 0.4, grass and gravel 0.3, sand 0.2, snow 0.15, wool 0.1. **Per-block occlusion:** wool 1.5, glass and snow 0.1.
  - It traces off the main thread against a **clone of the level within 4 chunks**, because reading the live world from another thread "will cause instability".
  - The code is GPL-3.0, so we take the technique only.
- **Steam Audio (S9–S11).** The physically based reference.
  - **Occlusion:** a single raycast, or volumetric with up to 16 samples.
  - **Transmission:** one ray from the listener to the source, multiplying `hit.material->transmission[band]` at each hit.
  - **Reflections:** listener rays bounce. At each bounce, a shadow ray goes to *every* source, and its energy is scaled by `(1 − absorption)` and the scattering lobes. The result is an energy field, then an impulse response.
  - **Listener-centric reverb:** "placing the source at the listener position" makes reverb "independent of the number of sources" (S10).
  - **Real-time defaults** (S11): 4096 rays, 4 bounces, a 1 s IR, order 1, 32 sources, 5% of cores, at a 0.1 s interval.
- **Project Acoustics and Wwise rooms and portals.** One is a baked wave simulation (S21); the other needs authored rooms and portals. Neither survives arbitrary Edits to a Planet-sized world **[judgement]**.

**Takeaways [judgement]:**
- Every voxel-game system that shipped (Lin, Vercidium's voxel era, Sound Physics) uses **a few dozen to a few hundred rays per update**, feeding **parametric** reverb (EFX/EAX-style) rather than convolution.
- Every one traces on the **CPU**, except Lin's RTX demo.
- Every one stays off the mixer thread.

### 3.2 What the layer computes (our design)

Two jobs, both fed by `trace()` on the frame thread's scene (§4):

1. **Occlusion and transmission per source (the "green and orange rays").**
   - For each traced source: 1 centre ray plus 8 rays to points jittered within ~0.5 m of the source (Sound Physics' 9 paths; Steam Audio's volumetric samples).
   - Each ray reports whether it reached the source, and the product of its hits' `transmission[band]` while marching through walls up to a thickness cap. Leaves and other foliage count as thin and highly transmissive.
   - The result is a 3-band gain, mapped to kira's per-source volume and low-pass cutoff. The fraction of clear paths smooths the transitions that S10 warns a single ray makes.
2. **The listener's environment, as listener-centric reverb (the "blue rays").**
   - 32 rays a round, 4 bounces each, re-traced 10 times a second. Each bounce casts a line-of-sight ray back to the listener (Vercidium; Steam Audio's shade step with the source at the listener).
   - Energy is multiplied by `1 − absorption[band]` at each hit (S9's model).
   - Accumulated over a sliding 0.5 s window (~160 paths), it gives:
     - **enclosure:** the share of rays that escape, which also gives the outdoors direction;
     - **mean free path:** the mean segment length, which gives the room size;
     - **an RT60 per band:** decay from the energy left after each bounce;
     - **an early delay:** the first return.
   - These drive kira's reverb `feedback`, `damping` and `mix`, a pre-delay through a delay effect, and the outdoor ambience's level and pan.
3. **Ambience and rain-like beds** (wind, rain later, biome beds) use enclosure and the escape direction. This is Vercidium's rain-from-the-window trick, and the reason ambience sounds right in caves for nearly no cost.

Sources beyond the 32 traced ones get distance attenuation only. A character's sounds (footsteps, digs, voice later) share their character's one traced emitter: S6's leech pattern.

### 3.3 CPU vs GPU, rays per source, update rates

- **CPU (recommended).** Audio needs ~10³–10⁴ rays a second, not the 5.5 × 10⁷ a second that primary visibility uses at 60 fps. At ~1–6.5 µs a segment (§6) that is a fraction of a millisecond a frame. Results land the same frame, so there is no readback.
- **GPU via `trace()` (deferred).**
  - The GPU work is negligible: the primary pass traces 921,600 rays in ~0.22 ms on the 4090, ~0.24 ns a ray. So 5,000 audio rays are a few µs of ALU, plus a dispatch **[estimate]**.
  - But today's probe (`Tracer::trace_rays`) submits and blocks on its readback. A production version needs a ring of mapped buffers read 2–3 frames late. The bench docs record that readbacks leave the GPU idle and distort timings (`docs/agents/benchmarking.md`).
  - With #50's hardware path, it would also need ray queries in a compute pass of its own.
  - Revisit only if the CPU line is blown **[judgement]**.
- **Update rates.**
  - Sound Physics evaluates once per sound at play time.
  - Steam Audio's default interval is 0.1 s (S11).
  - Vercidium's SDK is "event-based" with caching (S5, S6).
  - We use 10 Hz for the listener environment and 5 Hz per traced source, round-robin, with all parameters smoothed over ~100–300 ms in kira tweens. Audio tolerates ~100 ms of latency on reverb, and occlusion is smoothed anyway **[judgement]**.
  - New one-shot sounds take their emitter's last result: a character's leech, or the listener's environment for ambience.

### 3.4 Why not Steam Audio for M1b

1. **A null material in custom-tracer hits.**
   - audionimbus 0.16.2's callback trampoline writes `material: std::ptr::null_mut()` into every `IPLHit`, in single and batched closest-hit alike (`callback.rs` lines 472–479 and 641–651; S12).
   - Steam Audio's `ReflectionSimulator::shade` reads `hit.material->scattering` and `hit.material->absorption[j]`, and `DirectSimulator::transmission` reads `hit.material->transmission[j]` (S9). `CustomScene::closestHit` passes our `Hit` through untouched.
   - So reflections or transmission over our tracer would dereference null as shipped. **This is a reading of both sources; I did not run it.**
   - The fix is small: point at a static `IPLMaterial` per acoustic class, through `audionimbus-sys` or an upstream patch. But it is the kind of FFI bug a beginner team can't diagnose from a segfault **[judgement]**.
2. **The cost grows with sources.** Each bounce casts a shadow ray to *every* source (S9). At the defaults (4096 × 4, 10 Hz) even listener-centric reverb (1 source) is 4096 × 4 × 2 = 32,768 segments an update, 3.3 × 10⁵ a second. At 2–6.5 µs a segment that is **0.65–2.1 CPU cores [estimate]**, against our ~0.01–0.03 (§6). It is tunable down, and then it no longer buys accuracy over the parametric layer **[judgement]**.
3. **The build.** A ≈180 MB SDK download in the build script, Clang ≥ 9 for bindgen (S12), a C++ shared library to ship beside the executable, and its fixed-frame effects wrapped as kira `Effect`s. Convolution reverb is the "significant CPU usage" mode (S11's text).
4. **What it would add:** HRTF, ambisonic early reflections, convolution tails and pathing. That is a spike for **after M1b**, once the parametric layer is heard in look-dev, if caves still sound "fake" **[judgement]**.

### 3.5 Reusing our tracer without stealing frame budget

- **Same function, same scene.** Acoustic rays call `render::reference::trace` on the frame thread's scene copy, the one `World::aim` already uses. They see exactly what is drawn, Edits included, the frame they land. That matters when you dig a hole into a cave and the reverb should open up **[judgement]**.
- **One addition: a maximum distance.** `trace` already clips to a `limit` (the Planet's ball), so a `max_t` parameter is a one-line change in `reference.rs`. The WGSL copy needs nothing, because audio never runs there. Occlusion rays stop at the source; reverb rays stop at ~64 m, where the tail is quiet anyway (S8 caps processing at 512 blocks; we need far less) **[judgement]**.
- **Coarse levels are acceptable.** Acoustic wavelengths are 34 cm at 1 kHz and 3.4 m at 100 Hz (λ = 343 m/s ÷ f). Detail below ~0.2 m has no geometric-acoustic meaning, which is why Vercidium says coarser voxels "run even faster" (S3) **[judgement]**. A knob, if cost demands it: start the walk at level 2 (20 cm at 5 cm). That is a `reference.rs`-only option.
- **A hard ray budget, not a time budget.** A fixed number of segments a second, spread evenly over frames, so cost is predictable and testable. A time guard drops the remainder of a frame's quota if the frame is already late.
- **No GPU, no BVH, no uploads.** Nothing touches the GPU budget lines in `bench-budgets.toml`.

## 4. Fitting the client's threads (ADR 0006)

| Placement | What it costs | Verdict |
|---|---|---|
| **Frame thread, time-sliced (recommended)** | ~0.14–0.47 ms a frame **[estimate, §6]**. No lock, no copy. Same-frame results; Edits are seen at once | **Adopt for M1b.** The frame thread owns the traced scene copy, and the CPU line keeps it honest |
| A dedicated audio-sim thread reading the frame thread's scene copy | A read-write lock on a copy the frame thread writes every frame. The frame thread would wait for a reader for up to one batch | Rejected: it adds a stall to the thread ADR 0006 freed |
| A dedicated thread with its own copy of the scene | +542 MiB reserved (ADR 0006's figure), written twice | Rejected |
| A dedicated thread tracing the brick store under the predictor's lock | More readers on the lock whose waits ADR 0006 already lists as a remaining stall. The store is not what is drawn | Rejected (ADR 0006 rejected aiming against the store for the same reason) |
| A dedicated thread with a coarse **acoustic grid** (Sound Physics' level clone, S8). For example a 64 m cube at 20 cm, 1 byte of acoustic class per cell, 320³ ≈ 33 MB, updated from changed bricks | A new structure, an incremental update path and a test suite. Competes with 14 loader threads for cores | **Fallback** if the frame-thread line is exceeded, or once audio wants more rays (convolution, Steam Audio) |
| The mixer | Already on cpal's callback thread, inside kira | Unchanged |

**Multiplayer (M2, dozens of players).** The ray budget is fixed per second: at most 32 traced emitters, chosen by loudness × 1/distance, each a whole character via leeches. More players mean more distance-only sources, not more rays. Audio is client-only and never replicated. The server never links the audio crate.

## 5. The basic floor, item by item

| Item | How | Inputs that exist today |
|---|---|---|
| **Footsteps by Material** | On each step of the character controller (and other players' characters in M2), play a random sample from the Material's `sound` set under the feet, pitch-jittered ±5%, gain by speed | The controller's ground contact; `material` of the Voxel below |
| **Dig and place by hardness** | Dig: a looping scrape from the Material's `sound` set, with pitch and lowpass falling as `hardness` rises and duration following `dig_seconds`. Completion "crumble" scaled by the Edit's Volume. Place: a set per category | `hardness`, `dig_seconds`, the Edit's Volume (#151 shows it) |
| **Per-biome ambience** | 2D looping beds per `Biome` (Temperate, Desert, Snow, Volcanic), cross-faded over ~5 s by the biome weights around the listener, and ducked by **enclosure** (§3.2) under ground. Caves get their own bed by enclosure, without a biome | `world_map::Biome`; enclosure from the reverb rays (or a cheap 6-ray "sky visibility" test if the ray-traced layer is cut) |
| **Water** | M1b water is still. Emitters are clustered from exposed water Voxels near the listener (one per occupied 3.2 m region, at its centroid), with a lapping loop by surface area. Rivers and falls wait for Fluids (M4) | `water` Material; the scene's brick palettes |
| **Wind** | A 2D bed whose gain follows #152's wind strength `s(t)` × altitude × (1 − enclosure), panned toward the escape direction. Leaf rustle emitters (later) use the same field | #152's client-only wind clock (it must not freeze audio when the Reference freezes sway: audio reads the running clock, sway's View key freezes **[judgement]**) |
| **Distance falloff** | kira spatial tracks: `distances` (full volume to silent) and an attenuation easing per class, `spatialization_strength` for panning | — |

None of these need rays except the enclosure input, which has a 6-ray fallback.

## 6. Cost

### 6.1 Measurement [measured]

**Method.** A temporary `#[ignore]` test (S23) beside `bench/caves.rs`:
- It built the bench's real cave site ([#46](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/46)'s `find_site`: a steep cave mouth on the real Planet; a 30 m level-0 ring; the Guard from level 10) and filled the scene with the bench's own `fill()`.
- For each of the two stills, `mouth` (outside, 9 m off and 2 m up) and `tunnel` (inside the cave), it timed `render::reference::trace` single-threaded in `--release`:
  - 4096 Fibonacci-sphere directions from the camera (one segment each);
  - then 4096 diffuse paths of up to 4 bounces.
- 10 cm Voxels (today's code), on a Ryzen 7 5700G (Zen 3, 8C/16T). The test was not committed.

| Still | Bands | One segment from the listener | Paths of ≤ 4 bounces |
|---|---|---|---|
| tunnel | bench (d0 = 100 Voxels) | **0.87 µs/ray**; 90% hit, mean hit distance 12.8 m | **1.01 µs/segment** (14,224 segments, 14.4 ms); 30% of paths escape |
| mouth | bench (d0 = 100) | 2.08 µs/ray; 60% hit, mean 289 m | **1.97 µs/segment**; 92% of paths escape |
| mouth | the client's own d0 = 1028 (level 0 out to 205 m at 10 cm) | 3.81 µs/ray; mean 294 m | **3.25 µs/segment** |
| tunnel | d0 = 1028 | not measured: filling level 0 that far round the cave took over 15 minutes on one thread, and the run was stopped | — |

**Reading it.**
- Indoor rays are short and cheap. Outdoor rays are dear only because they fly hundreds of metres to distant ground, which an acoustic `max_t` of ~64 m cuts.
- The dev's 5800X is the same Zen 3 core with twice the L3 (S23), so these numbers should hold or improve there **[judgement]**.
- **At 5 cm [estimate]:** the same metric ray crosses about twice the level-0 Voxels and bricks. Take **2 µs a segment indoors and ≤ 6.5 µs outdoors** (≈ 3.25 × 2, before `max_t`).

### 6.2 Budget for the M1b layer [estimate]

| Item | Workload | Segments/s | CPU at 2–6.5 µs a segment | Per 60 fps frame |
|---|---|---|---|---|
| Occlusion + transmission | 32 traced emitters × 9 rays × 5 Hz | 1,440 (+ ~20% wall continuations) | 3.5–11 ms/s | 0.06–0.19 ms |
| Listener reverb + enclosure | 32 rays × 4 bounces × (1 + 1 line-of-sight) × 10 Hz | 2,560 | 5.1–17 ms/s | 0.09–0.28 ms |
| **Total, cave view** | | ~4,000 | **~8–28 ms/s (0.8–2.8% of a core)** | **~0.14–0.47 ms** |
| Forest view | Leaves are thin, transmissive occluders. Each pass-through restarts `trace()`; capped at 4 continuations a ray | up to ~1.5× | | **≤ ~0.7 ms worst case**, over the mean line, so the forest is where the knobs get pulled (the cap and `max_t` hold it) |
| Mixer (cpal thread) | ~32–64 voices with a filter each, 1–2 reverbs, a few beds | — | not measured | — (its own rule: no underruns) |
| GPU, BVH, uploads | none | — | — | **0** |

**Recommended budget lines:**
- **Audio simulation ≤ 0.5 ms of frame-thread CPU per frame (mean) and ≤ 1.0 ms p99** at 60 fps, on the cave stills and a forest view. It is a new `render::cpu::Row::Audio`, and the bench reports it next to `UpdateScene`.
- **0 ms GPU, 0 BVH work, 0 brick uploads.**
- **Mixer:** zero underruns over the bench run (kira/cpal report stream errors), with no ms line, because its cost is on a separate real-time thread.

**Knobs, in the order to pull them** if the line is exceeded:
1. `max_t` 64 → 32 m;
2. start acoustic rays at level 2 (20 cm);
3. reverb 32 → 16 rays a round, accumulating over 1 s;
4. traced emitters 32 → 16;
5. move tracing to the acoustic-grid thread (§4).

## 7. Per-Material absorption

- **Steam Audio's presets** are the only primary, published per-band set I found: `IPLMaterial { absorption[low, mid, high], scattering, transmission[low, mid, high] }` (S9, `phonon.h`):

| Preset | Absorption L / M / H | Scattering | Transmission L / M / H |
|---|---|---|---|
| generic | 0.10 / 0.20 / 0.30 | 0.05 | 0.100 / 0.050 / 0.030 |
| brick | 0.03 / 0.04 / 0.07 | 0.05 | 0.015 / 0.015 / 0.015 |
| concrete | 0.05 / 0.07 / 0.08 | 0.05 | 0.015 / 0.002 / 0.001 |
| ceramic | 0.01 / 0.02 / 0.02 | 0.05 | 0.060 / 0.044 / 0.011 |
| gravel | 0.60 / 0.70 / 0.80 | 0.05 | 0.031 / 0.012 / 0.008 |
| carpet | 0.24 / 0.69 / 0.73 | 0.05 | 0.020 / 0.005 / 0.003 |
| glass | 0.06 / 0.03 / 0.02 | 0.05 | 0.060 / 0.044 / 0.011 |
| plaster | 0.12 / 0.06 / 0.04 | 0.05 | 0.056 / 0.056 / 0.004 |
| wood | 0.11 / 0.07 / 0.06 | 0.05 | 0.070 / 0.014 / 0.005 |
| metal | 0.20 / 0.07 / 0.06 | 0.05 | 0.200 / 0.025 / 0.010 |
| rock | 0.13 / 0.20 / 0.24 | 0.05 | 0.015 / 0.002 / 0.001 |

- **The proposal for `materials.toml`:** an `acoustic = "<class>"` line on each of the 55 Materials, referencing a small `[acoustic.<class>]` table, plus a `sound = "<set>"` line for footstep/dig/place samples. A class reference rather than seven numbers per Material keeps 55 tables editable, and matches how Sound Physics keys by sound type (S8) **[judgement]**.
  - **Classes:** the 11 above, plus **soil**, **sand**, **snow**, **foliage**, **cloth** and **water**.
  - **The new classes have no primary source here; their numbers are tuned by ear in look-dev** ([#148](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/148)) **[unverified]**.
  - **Starting points:** soil and sand near gravel; snow and cloth near carpet; foliage with high transmission and high scattering; water near ceramic (hard and reflective).
- **The load rule stays:** every property must be given. Adding two lines to 55 tables is one mechanical agent task.
- **Debris (M3)** would carry its Material's class. Nothing else changes.

## 8. Recommendation

**M1b, the floor (must ship):**
- a new client-only crate `crates/audio` on **kira 0.12** (pin `=0.12.6`, as wgpu is pinned);
- spatial tracks with falloff and panning;
- footsteps by Material, dig/place by hardness, per-biome beds, still-water emitters, and wind fed by #152's clock;
- the two `materials.toml` lines (`acoustic`, `sound`);
- a mute/volume UI;
- `MockBackend` tests;
- Linux CI gains `libasound2-dev` (+ `libdbus-1-dev`, or drop kira's `cpal-realtime-dbus` feature).

**M1b, the ray-traced layer (fits; adopt behind a toggle, after the floor):**
- per-emitter occlusion and transmission (9 rays, 5 Hz, ≤ 32 emitters, leeches per character);
- listener-centric parametric reverb and enclosure (32 × 4 rays, 10 Hz, 0.5 s accumulation) driving kira's filter, reverb and delay;
- enclosure-ducked ambience with an escape direction;
- `reference::trace` gains `max_t`;
- a debug view drawing the acoustic rays (green/blue/orange, after S3) and an overlay of enclosure, RT60 and occlusion per emitter.

Adopt it if (a) it holds the budget line on the cave and forest bench views, and (b) a look-dev listen in #148 hears the cave/outdoor difference and wall muffling. Otherwise ship the floor with the 6-ray enclosure test and defer the rest.

**After M1b (spikes, not commitments):**
- Steam Audio through audionimbus with our tracer as `CustomRayTracer`, once the null-material issue is fixed upstream or worked round, for convolution reverb and HRTF;
- or HRTF alone via `hrtf` wrapped as a kira `Effect`;
- the acoustic-grid thread if more rays are wanted.

**Rejected:** FMOD, Wwise (proprietary, stale bindings), Resonance Audio and Project Acoustics (archived; baked), the Vercidium SDK as a dependency (no Rust, paid, not voxel), oddio (inactive), GPU acoustic tracing for M1b.

**Budget line:** **Audio ≤ 0.5 ms frame-thread CPU per frame mean, ≤ 1.0 ms p99, 0 GPU, 0 BVH, 0 uploads; no mixer underruns.**

## 9. Risks for a graphics-beginner team working with AI agents

- **Agents can't hear.** Every audio bug passes text review: inverted pan, clicks from unsmoothed parameter jumps, a reverb tail that never decays.
  - *Mitigation:* numeric tests on kira's `MockBackend` output (pan sign, gain versus distance, no sample jumps > ε after a parameter change); the acoustic-ray debug view and overlay; the dev's ears in a short playtest clip per PR that touches audio.
- **Occlusion flicker** when one ray grazes a Voxel edge (S10's "sudden transitions"). *Mitigation:* 9 rays, a fraction-clear gain, and smoothing over ≥ 100 ms.
- **Frame-thread stalls** if the ray quota isn't enforced, or `max_t` is forgotten and outdoor rays fly 300 m (3.3 µs each). *Mitigation:* a quota per frame plus a time guard; a unit test that `max_t` stops the walk; the `Row::Audio` p99 in the bench.
- **The forest cost is guessed.** No vegetation exists yet, and leaf continuations restart `trace()`. *Mitigation:* the 4-continuation cap; measure on the first forest bench view.
- **Licences.**
  - Sound Physics is GPL-3.0: agents must not paste its code, only its ideas.
  - Sample libraries each have their own terms. Every sound file needs a recorded licence (CC0 or our own recordings) before it is committed **[judgement]**.
  - We take techniques, never assets (map #130).
- **The Linux CI and audio-device gap.** cpal needs ALSA headers to build, and CI has no sound card. *Mitigation:* the CI package line, `MockBackend` in tests, and a client that runs silent (logs once) when no device opens.
- **Windows device changes** (headphones unplugged). kira's cpal backend has a stream manager that restarts the stream (S14, `stream_manager.rs`), but this is untested here **[unverified]**.
- **Wind-clock coupling.** If wind audio reads the frozen View-key clock, wind falls silent while the Reference accumulates. *Mitigation:* audio reads the running wind clock; only sway freezes.
- **Scope creep toward physical acoustics** (HRTF, convolution, Steam Audio) before the floor ships. *Mitigation:* those are post-M1b spikes by this decision.
- **An FFI segfault**, if someone reaches for audionimbus's custom tracer without the material fix (§3.4).

## 10. Cross-check against what is already decided

| # | Finding | Status | Decision it touches |
|---|---|---|---|
| 1 | Audio in M1b: the floor, plus a ray-traced layer only if it fits | **Already decided**; this research says the parametric layer fits | #138 |
| 2 | Audio is client-only; the server binary never links the audio crate | **Already decided** in spirit (consistent) | ADR 0001 (server never links wgpu); #138 |
| 3 | Acoustic rays use `render::reference::trace` on the frame thread's scene copy | **Consistent** | ADR 0006 (the frame thread's copy is "traced on the CPU to aim") |
| 4 | No audio-sim thread on the store or a second scene copy | **Consistent** | ADR 0006 (rejected aiming against the store; 542 MiB copies) |
| 5 | `reference::trace` gains a maximum distance (CPU-only) | **New** | `reference.rs`'s line-for-line rule with `trace.wgsl` (the WGSL side is unaffected; the test oracle stays) |
| 6 | 0 GPU, 0 BVH; nothing in `bench-budgets.toml` changes | **Consistent** | #10 frame budget |
| 7 | A new CPU budget line and `Row::Audio` | **New** | #10 (only GPU lines exist today), bench report |
| 8 | `materials.toml` gains `acoustic` and `sound` | **New** | #15 (Material schema; "a missing property is a load error") |
| 9 | Wind audio follows #152's client-only wind clock; it reads the running clock, not the frozen one | **New** (amends #152's freeze rule to cover sway only) | #152, #101 |
| 10 | Water sound from still water Voxels in M1b; flow sounds wait for Fluids | **Consistent** | #19, #136 (Fluids in M4) |
| 11 | Leech sources and ≤ 32 traced emitters for dozens of players | **New** | M2 multiplayer |
| 12 | kira pinned exactly, like wgpu and egui | **New** (follows a convention) | Workspace `Cargo.toml` pins |
| 13 | Linux CI installs ALSA (and D-Bus) headers | **New** | `.github/workflows/ci.yml` |
| 14 | Biome beds use `world_map::Biome` evaluated on the client | **Consistent** | ADR 0003 (client-generated terrain) |
| 15 | No copying of game assets or GPL code; sounds need licences | **Already decided** (the techniques-only rule) | #130 Notes |
| 16 | Distances in metres (5 cm Voxels; `max_t` 64 m = 1,280 Voxels) | **Already decided** | #135, #147 |

## 11. Corrections to earlier research

- **[John Lin water research](https://github.com/theHimanshuShekhar/bhayanakvoxel/blob/research/inspiration-john-lin-water/docs/research/inspiration-john-lin-water.md)** row 27, "Ray-traced spatial water audio", status Demo/WIP. The only first-party statement about ray-traced audio is the March 2020 video (S1): RTX reverb and occlusion through OpenAL, ten months *before* the water video, in the same old engine. The water video itself only says "sound calculations" are still to improve (S2). "Ray-traced water audio" is therefore an inference, not a statement. The adoption is unaffected.

## Open questions this research raises

- Does the parametric layer sound convincing in a 5 cm cave? Only a listen can tell; it belongs in #148's look-dev, which so far is visual only.
- Should a character's own footsteps be traced? The listener's environment already colours them. Probably dry plus the listener reverb **[judgement]**.
- Spatial voice chat in M2 (Vercidium's raytraced proximity chat) would reuse the same emitters. It is not in any Milestone.

## Unverified or not checked

- **No audio was played.** kira's output, the latency of its stream and its device-change handling were not exercised.
- **The audionimbus null-material crash** is read from the two sources (S9, S12), not reproduced.
- **5 cm costs** are 2× the measured 10 cm numbers by reasoning, not measured (#147 isn't built). The tunnel at the client's d0 = 1028 was not measured (its fill ran out of time). No vegetation exists for a forest measurement.
- **Measurements ran on a Ryzen 7 5700G, not the dev's 5800X** (same Zen 3 core, half the L3).
- **The mixer's CPU cost** is not measured.
- **Vercidium's "8x faster than voxel raymarching"** (S7) is from a search snippet; Patreon blocked fetches. His newer SDK's ray counts and costs are not published (S5).
- **FMOD's terms** come from the site's JavaScript bundle text, because the page is client-rendered. The Wwise Indie tier's "Free" reading follows the page's plan order.
- **Acoustic numbers for soil, sand, snow, foliage, cloth and water** have no primary source here.
- **Lin's audio method** is undescribed (S1). His water video's sound track was not analysed (S2).
