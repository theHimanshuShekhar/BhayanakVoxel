# Research: engine foundation & language

Resolves [#2](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/2). Feeds the decision [#7 Choose engine foundation & language](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/7).
Checked on 2026-10-27. Version numbers come from GitHub releases and crates.io as of that date.

## TL;DR

- **Every option leaves the hard part to us.** None of the engines ships a voxel Planet, a micro-Voxel path tracer or orbit-scale LOD. We write those ourselves on top of compute shaders either way. So the real question is which foundation gives the cleanest access to the GPU and the easiest headless server, plus enough ready-made "glue" (window, input, audio, physics, networking).
- **Hardware ray tracing is a bonus, not a gate.** The closest prior art, Douglas Dwyer's Octo engine (Rust, WebGPU), gets path-traced lighting on micro-Voxels by ray marching in compute shaders. Hardware ray queries can come later. ([octo-release README](https://github.com/DouglasDwyer/octo-release))
- **The general-purpose engines fit poorly.** Unreal 5 and Godot 4 are built around triangle meshes. We would mostly bypass their renderers and fight their tooling, and Unreal also needs a source build just to make a dedicated server.

**Ranked recommendation for a beginner steering AI agents:**

1. **Custom engine in Rust + wgpu (recommended).** You own the renderer and the server outright. Compute shaders are first-class, and experimental hardware ray queries are available, including procedural-AABB acceleration structures that suit Voxels. The server is just a second Cargo binary that never touches the GPU. `cargo test` and `cargo clippy` give agents a tight, automatable feedback loop, and the Rust compiler catches whole classes of agent mistakes before anything runs. Fill the gaps with mature crates (list in [Custom-route library stack](#custom-route-library-stack)).
2. **Bevy (Rust, built on wgpu).** A strong runner-up. It comes with window, input, assets, audio, ECS, hot-reload, a headless mode and a Bevy-specific networking and physics ecosystem, and you write the voxel renderer as custom render-graph nodes. The cost: a breaking release about every 3 months, and AI agents tend to write code for *older* Bevy APIs. Bevy's own path tracer (Solari) traces triangle meshes, not Voxels.
3. **Custom engine in C++ + Vulkan.** Gives the most control and has by far the most tutorials, samples and training data for Vulkan and ray tracing. It is harder for a beginner to supervise: memory-safety bugs, GPU synchronization bugs and CMake builds fail in ways that are hard to diagnose.
4. **Custom engine in Rust + ash (raw Vulkan).** The same control as C++ in a better build system, but most renderer code ends up `unsafe`, and ash has not cut a crates.io release since April 2024. Prefer #1, which can drop down to raw Vulkan where needed.
5. **Godot 4 + GDExtension/compute.** The best glue and the easiest headless server of the engines here, but you would bypass its renderer. Its new Vulkan ray-tracing API can't run ray queries on hardware in 4.7.x; the fix ships in 4.8 (beta).
6. **Unreal 5.** The wrong tool here: heavy engine-level shader work for a custom path tracer, a source build for dedicated servers, binary assets agents can't edit, and slow iteration.

## Why "custom" is not a penalty here

The user is open to a fully custom engine. The question then is what an engine actually saves us. For BhayanakVoxel, the systems that decide success are custom in every option:

| System | Provided by Bevy / Godot / Unreal? |
|---|---|
| Micro-Voxel storage, Edits, streaming (#3, #4) | No. Godot has the third-party [godot_voxel](https://github.com/Zylann/godot_voxel) module and Bevy has [bevy_voxel_world](https://github.com/splashdust/bevy_voxel_world), but both are mesh-based block voxels, not micro-Voxel path tracing (assumption from their READMEs; not benchmarked). |
| Path-traced voxel lighting (#5) | No. Bevy Solari traces triangle meshes (`RaytracingMesh3d`) ([bevy_solari src](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_solari/src/lib.rs)). Unreal Lumen and Godot target meshes too. |
| Spherical Planet + Zoom to orbit LOD (#4) | No. |
| Debris / Fluid on Voxels | No. General rigid-body engines (Jolt, Rapier, Chaos) don't simulate voxel fluids. |
| Window, input, audio, UI, asset loading | **Yes**. This is what engines save. |
| Generic networking transport, physics for player capsules | **Yes**, or mature standalone libraries. |

So an engine mainly saves glue. Glue is also what agents write well, because it is common and well documented. The renderer is where control matters, and it is what you would have to fight Unreal and Godot over.

## Comparison

Legend: ✅ good · ⚠️ works with caveats · ❌ poor fit. "Judgement" marks an assessment, not a measured fact.

### 1. Compute shaders and hardware ray tracing

| Option | Compute | Hardware RT (ray query / acceleration structures) |
|---|---|---|
| Rust + wgpu 30.0.1 | ✅ First-class (WGSL; SPIR-V passthrough) | ⚠️ `EXPERIMENTAL_RAY_QUERY`: "THIS IS EXPERIMENTAL… expected to be subject to breaking changes", docs list **Vulkan** only ([docs.rs Features](https://docs.rs/wgpu/30.0.1/wgpu/struct.Features.html)). The changelog also shows Metal acceleration structures and HLSL ray-query fixes, so DX12/Metal support is in progress ([CHANGELOG](https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md)). v30 (2026-07-01) added **procedural AABB BLAS geometry** (useful for voxel bricks) and has a `ray_aabb_compute` example ([examples](https://github.com/gfx-rs/wgpu/tree/trunk/examples/features/src)). Full ray tracing pipelines are under development (`EXPERIMENTAL_RAY_TRACING_PIPELINES`, SPIR-V out only). Raw Vulkan escape hatch: `as_hal` and `AccelerationStructure::raw_handle` (unreleased) ([CHANGELOG](https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md)). |
| Bevy 0.20 (wgpu 30) | ✅ Custom render-graph/compute nodes; uses WESL shaders since 0.20 ([Bevy 0.20 notes](https://bevy.org/news/bevy-0-20/)) | ⚠️ Same wgpu features. Solari path tracer is "experimental" and needs `EXPERIMENTAL_RAY_QUERY` ([bevy_solari](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_solari/src/lib.rs)); it is mesh-based. Bevy uses wgpu 30 ([bevy_render Cargo.toml](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/Cargo.toml)). |
| Rust + ash 0.38 | ✅ Full Vulkan | ✅ Full `VK_KHR_ray_query` / `ray_tracing_pipeline`. But the crates.io release targets Vulkan headers 1.3.281 (2024-04-01). Vulkan 1.4 helpers and newer RT extensions are only on master, unreleased ([crates.io](https://crates.io/crates/ash), [Changelog](https://github.com/ash-rs/ash/blob/master/Changelog.md)). |
| C++ + Vulkan | ✅ Full | ✅ Full, with the best learning material: Khronos' official tutorial uses Vulkan 1.4, Slang and C++20 and includes a ray-tracing course and a "Building a simple game engine" series ([Khronos Vulkan Tutorial](https://docs.vulkan.org/tutorial/latest/00_Introduction.html)). |
| Godot 4.7.2 | ⚠️ Raw GLSL via `RenderingDevice`, Forward+/Mobile renderers only ([docs](https://docs.godotengine.org/en/stable/tutorials/shaders/compute_shaders.html)) | ⚠️ Low-level Vulkan RT plumbing landed in 4.7 ([PR #99119](https://github.com/godotengine/godot/pull/99119)), but 4.7.x never enables `VK_KHR_ray_query` and has BLAS/push-constant bugs. Fixed in [PR #122051](https://github.com/godotengine/godot/pull/122051), milestone **4.8** (4.8-beta1 out 2026-10-15, [godot-builds](https://github.com/godotengine/godot-builds/releases)). An NVIDIA fork also exists ([NVIDIA-RTX/godot](https://github.com/NVIDIA-RTX/godot)). |
| Unreal 5.8 | ⚠️ Global shaders / RDG in engine C++ | ⚠️ Hardware RT is used by Lumen/MegaLights for meshes ([5.8 release](https://forums.unrealengine.com/t/unreal-engine-5-8-released/2729274)). A custom voxel path tracer means engine-level renderer work (judgement). |

Note: John Lin's own implementation details (software ray marching vs hardware RT) are **not published** in a primary source I could find. The video description only lists results: 5-bounce path-traced GI, a missing spatial denoiser, and 512³-detail voxels ([video](https://youtu.be/8ptH79R53c0)). Octo shows that compute ray marching alone can give path-traced AO, shadows and emissives on micro-Voxels ([octo-release](https://github.com/DouglasDwyer/octo-release)). The rendering trade-off belongs to #5/#10.

### 2. Headless dedicated server

| Option | Assessment |
|---|---|
| Rust + wgpu | ✅ The server is a separate binary that links the shared simulation crate and never links wgpu (judgement: standard Cargo workspace design). wgpu also has a `noop` backend for tests ([bevy_render Cargo.toml](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/Cargo.toml) uses `features = ["noop"]`). |
| Bevy | ✅ `ScheduleRunnerPlugin` + `default-features = false` runs without windowing or rendering ([examples/app/headless.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/examples/app/headless.rs)). |
| Rust + ash / C++ + Vulkan | ✅ Same as wgpu: the server simply doesn't initialize Vulkan. |
| Godot | ✅ `--headless` on any 4.x binary, or a "dedicated server" export preset that strips visuals ([docs](https://docs.godotengine.org/en/stable/tutorials/export/exporting_for_dedicated_servers.html)). |
| Unreal | ⚠️ "You must use a source build of Unreal Engine" for dedicated servers. The official 5.8 tutorial walks through Windows only ([docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/setting-up-dedicated-servers-in-unreal-engine)). |

### 3. Networking and physics ecosystem

| Option | Networking | Physics |
|---|---|---|
| Rust (wgpu or ash) | [renet](https://github.com/lucaspoffo/renet) 2.0 (engine-agnostic client/server with reliable and unreliable channels, fragmentation and netcode encryption), or `quinn` (QUIC) | [rapier3d](https://crates.io/crates/rapier3d) 0.36.1 (engine-agnostic) |
| Bevy | [lightyear](https://github.com/cBournhonesque/lightyear) 0.31 (prediction/interpolation), [bevy_replicon](https://github.com/projectharmonia/bevy_replicon) 0.45.4, bevy_renet | [avian3d](https://github.com/Jondolf/avian) 0.8 (ECS-native), bevy_rapier |
| C++ | [GameNetworkingSockets](https://github.com/ValveSoftware/GameNetworkingSockets) (Valve), [yojimbo](https://github.com/networkprotocol/yojimbo) | [Jolt](https://github.com/jrouwe/JoltPhysics) 5.6.0 |
| Godot | Built-in high-level multiplayer (ENet) (judgement; not re-verified this pass) | Jolt is the default for new 3D projects; it was added in 4.4 ([docs](https://docs.godotengine.org/en/stable/tutorials/physics/using_jolt_physics.html)) |
| Unreal | Built-in replication; the Iris system's status in 5.8 is unclear from first-party notes (only a forum user claims it is Production Ready) ([5.8 thread](https://forums.unrealengine.com/t/unreal-engine-5-8-released/2729274)) | Chaos |

For an editable voxel Planet, none of these replicate voxel Edits for us. Generic entity replication covers players and Debris; Edit and chunk sync is custom in every option (see #6).

### 4. Maturity, docs and training-data representation

Training-data share can't be measured directly. These are proxies (GitHub stars as of 2026-10-27, crates.io downloads), followed by a judgement.

| Option | Proxies | Agent-relevant notes |
|---|---|---|
| Rust + wgpu | wgpu 18.4k stars, 44.6M crate downloads ([crates.io](https://crates.io/crates/wgpu)) | ⚠️ A breaking major release about every quarter: v27 2025-10-01, v28 2025-12-17, v29 2026-03-18, v30 2026-07-01 ([CHANGELOG](https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md)). Agents will mix versions; pinning one version and giving agents docs.rs links helps (judgement). Core compute/buffer API has been stable in shape. |
| Bevy | 49.3k stars | ⚠️ README: "Bevy is still in the early stages of development… breaking changes… approximately once every 3 months" ([README](https://github.com/bevyengine/bevy/blob/v0.20.0/README.md)). In 0.20 alone, all custom WGSL had to be rewritten as WESL ([0.20 notes](https://bevy.org/news/bevy-0-20/)). Agents' Bevy knowledge is likely 1–3 versions stale (judgement). |
| Rust + ash | 2.4k stars | Thin 1:1 Vulkan bindings, so C/C++ Vulkan knowledge transfers. Release cadence has stalled (last crates.io release 2024-04-01). |
| C++ + Vulkan | Vulkan-Samples 5.4k stars, official Khronos tutorial | ✅ The largest body of Vulkan and RT material. Vulkan core is backward-compatible. Khronos even has an "AI-Assisted / LLM-Assistant Vulkan Development" chapter ([tutorial](https://docs.vulkan.org/tutorial/latest/00_Introduction.html)). |
| Godot | 119k stars | ✅ Large community. GDExtension in Rust via [gdext](https://github.com/godot-rust/gdext) 0.5.5 (pre-1.0). LLMs often confuse Godot 3 and Godot 4 APIs (judgement, widely reported, not verified). |
| Unreal | n/a (private repo) | ⚠️ Large but mesh/Blueprint-centric material. 5.8 adds an *experimental* MCP plugin for LLM agents ([5.8 release](https://forums.unrealengine.com/t/unreal-engine-5-8-released/2729274)). `.uasset` files and Blueprints are binary, so agents can't diff or edit them (judgement). |

### 5. Iteration speed and build/test automation

- **Rust (wgpu / ash / Bevy):** Cargo gives one command for build, test, lint and format, with no build-system authoring. That makes it easy to script in CI and for agents to run (judgement). Rust compile times are the main cost. Bevy is the heaviest of the Rust options; it offers a dynamic-linking feature for faster rebuilds (from Bevy's setup docs; not re-verified this pass).
- **GPU tests in CI without a GPU:** Mesa's CPU Vulkan driver lavapipe supports `VK_KHR_acceleration_structure`, `VK_KHR_ray_query` and RT pipelines since Mesa 24.1 ([relnotes](https://docs.mesa3d.org/relnotes/24.1.0.html)). So compute and ray-query shaders can be smoke-tested in headless Linux CI on Vulkan-based stacks (wgpu-on-Vulkan, ash, C++). Slow, but deterministic (judgement).
- **C++ + Vulkan:** CMake plus a package manager (vcpkg/Conan) is extra setup for agents to maintain. Debugging memory corruption is harder for a beginner to supervise (judgement).
- **Godot:** Fast for GDScript and scenes, but a GDExtension rebuild plus `.tscn`/`.tres` editor resources sit in the loop (text formats, so agent-editable).
- **Unreal:** Slow C++ builds, a large editor, binary assets. The worst fit for agent-driven automated tests (judgement).

### 6. Prior voxel projects

| Foundation | Projects |
|---|---|
| Rust + wgpu | **Octo** (Douglas Dwyer): Rust, WebGPU, compute ray marching, LOD, real-time path-traced AO/shadows/emissives, editable terrain, rigid bodies, TCP multiplayer ([octo-release](https://github.com/DouglasDwyer/octo-release)). This is the closest analogue to BhayanakVoxel; the current version is closed. **Veloren** (voxel RPG, `wgpu = "27"`, winit, egui) ([voxygen Cargo.toml](https://gitlab.com/veloren/veloren/-/blob/master/voxygen/Cargo.toml)). |
| Bevy | [bevy_voxel_world](https://github.com/splashdust/bevy_voxel_world) (meshed block voxels). **Tiny Glade** shipped on Bevy ECS with its own custom renderer ([This Week in Bevy](https://thisweekinbevy.com/issue/2024-06-03-tiny-glade-vj-performances-and-2d-lighting), [Bevy on X](https://x.com/BevyEngine/status/1838302608370602111)). This pattern (Bevy for glue, custom renderer) is directly relevant. |
| Rust + ash | **kajiya** (Embark; real-time GI renderer in Rust + Vulkan, now archived) ([repo](https://github.com/EmbarkStudios/kajiya)) |
| C++ + Vulkan | **gvox_engine** (Gabe Rundlett) on **Daxa**, a C++ Vulkan abstraction ([gvox_engine](https://github.com/GabeRundlett/gvox_engine), [Daxa](https://github.com/Ipotrick/Daxa)) |
| Godot | **godot_voxel** (Zylann; C++ module: meshed voxels, LOD, smooth terrain) ([repo](https://github.com/Zylann/godot_voxel)) |
| Unreal | Voxel Plugin (commercial; not re-verified this pass). 5.8 adds an experimental Mesh Terrain with caves and overhangs, but it is meshes, not Voxels ([5.8 release](https://forums.unrealengine.com/t/unreal-engine-5-8-released/2729274)). |

## Custom-route library stack

If #7 picks option 1 (or 3), these libraries cover the glue so agents are not writing it from scratch:

**Rust + wgpu (recommended)**

| Gap | Crate |
|---|---|
| Window + input | [winit](https://github.com/rust-windowing/winit) (used by Veloren and Bevy); `gilrs` for gamepads (not re-verified) |
| GPU | [wgpu](https://crates.io/crates/wgpu) 30, pinned. Drop to Vulkan via `as_hal` only if needed. |
| Math | `glam` (Bevy's math crate; not re-verified this pass). Use `f64` or origin rebasing for Planet-scale positions (see #4). |
| ECS / app structure (optional) | `bevy_ecs` used standalone, as Tiny Glade did, or plain structs |
| Physics (players, Debris) | [rapier3d](https://crates.io/crates/rapier3d) 0.36 |
| Networking | [renet](https://github.com/lucaspoffo/renet) 2.0 or `quinn`. Replication logic is custom (#6). |
| Audio | [kira](https://github.com/tesselode/kira) |
| Debug UI | [egui](https://github.com/emilk/egui) + `egui-wgpu` + `egui-winit` (as Veloren does) |
| Assets | `gltf`, `image` crates (not re-verified) |
| Shaders | WGSL natively, or Slang → SPIR-V passthrough ([shader-slang](https://github.com/shader-slang/slang)) |

**C++ + Vulkan:** [SDL3](https://github.com/libsdl-org/SDL) (window, input, audio), [vk-bootstrap](https://github.com/charles-lunarg/vk-bootstrap), [VulkanMemoryAllocator](https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator), Slang, [Jolt](https://github.com/jrouwe/JoltPhysics), [GameNetworkingSockets](https://github.com/ValveSoftware/GameNetworkingSockets) or [yojimbo](https://github.com/networkprotocol/yojimbo), [EnTT](https://github.com/skypjack/entt), Dear ImGui (not re-verified).

**The honest extra cost of custom:** roughly a few agent-weeks of glue before the first Voxel is on screen: window/input loop, swapchain/resize, asset loading, a debug UI, a fixed-timestep simulation loop shared by client and server, and audio. This is an estimate, not measured. It is well-trodden code that agents write reliably. Bevy removes most of it, at the price of API churn and a render-graph abstraction between you and the GPU.

## Open questions for #7

- **Bevy vs plain wgpu is the closest call.** If faster early visible progress (UI, audio, hot-reload) matters more than renderer simplicity, pick Bevy. If you pick plain wgpu, keep the option to adopt `bevy_ecs` standalone later.
- **Is hardware RT required for M1?** If #5 concludes hardware ray queries are needed on day one, the wgpu path is experimental and Vulkan-first. That is fine on RTX 3060 / RX 6700 under Vulkan, but DX12 support should be confirmed. If it is a hard requirement, C++/Vulkan or ash is the safer fallback.
- **API churn policy:** pin wgpu/Bevy versions and upgrade deliberately, giving agents the migration guide each time.
- **Unverified here:** John Lin's actual technique; Godot networking and Unreal Iris status; exact compile-time numbers; training-data share (proxies only).
