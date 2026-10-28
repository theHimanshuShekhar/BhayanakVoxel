# Custom engine in Rust + wgpu, Vulkan only

BhayanakVoxel is built as a custom engine in Rust on wgpu, with a pinned wgpu version, WGSL shaders and Vulkan as the only supported backend. We chose it over Bevy, Godot, Unreal, raw Vulkan and C++. No engine provides the hard parts (micro-Voxel storage, the voxel path tracer, Zoom-to-orbit LOD), so an engine would mostly have saved glue code that agents write easily anyway. Rust also gives bit-identical Planet generation on every machine by default, while C++ needs strict float flags on each compiler. The dedicated server is a separate binary that never links wgpu.

## Considered Options

- **Bevy:** the closest runner-up. Rejected because it ships breaking API changes about every quarter, agents' knowledge of it tends to be stale, and its render graph would sit between us and the voxel tracer.
- **C++ + Vulkan:** the most RT material and prior art (gvox_engine). Rejected for determinism fragility and memory and synchronization bugs a beginner can't easily supervise.
- **Raw Vulkan from Rust (ash):** mostly `unsafe`, with a stale crates.io release. Kept only as an escape hatch through wgpu's `as_hal` if hardware ray tracing or NRD/DLSS ever require it.
- **Godot, Unreal:** mesh-centric renderers we would mostly bypass.

## Consequences

- Hardware ray queries in wgpu are experimental, so M1 lighting starts from compute-shader ray marching.
- NVIDIA-only denoisers and upscalers (NRD, DLSS) need native interop.
- Each wgpu upgrade is a deliberate task that comes with its migration guide.
