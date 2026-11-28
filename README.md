<p align="center">
  <img src="assets/icon.svg" width="88" height="88" alt="">
</p>

<h1 align="center">BhayanakVoxel</h1>

<p align="center">
  A voxel sandbox on one spherical Planet, 320 km in radius, where every 5 cm Voxel is editable
  and the camera can zoom continuously from the character out to orbit.
</p>

BhayanakVoxel is a hobby and tech-demo game on a custom engine in Rust and [wgpu](https://wgpu.rs),
with Vulkan as the only backend. It has no heightfield, no mesh renderer and no third-party engine:
the Planet is Voxels all the way through, and lighting is path-traced.

The Vertical slice is a shared creative sandbox: shape the Planet with Brushes, with free Materials
and no rules. After the slice it grows into moving Pieces, survival with Tools, a solar system of
Planets, Debris and Fluids. What is planned, Milestone by Milestone, is in
[`docs/spec.md`](docs/spec.md).

## What works today

The game is in early development. M0 (the skeleton) is done and M1a, "Walk the Planet", is in
progress; [#41](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/41) tracks it.

- **The Planet.** Generated from `(seed, GENERATOR_VERSION)`, bit-for-bit the same on every
  platform: a coarse world map (continents, climate, biomes, drainage), Caves, emissive Crystal
  clusters, and a brick at any level of detail made on demand. The world map is built on the first
  launch and kept in a cache file.
- **Streaming and levels of detail.** Sparse 8³ Bricks and an all-voxel LOD pyramid out to orbit,
  with a ring of full detail around the camera, a coarse Guard for the whole Planet, and a level of
  detail that coarsens as the camera speeds up.
- **Walking and flying.** A character (walk, run, crouch, jump) with a capsule that sweeps against
  Voxels, and a noclip debug camera.
- **Editing.** Dig, place, smooth and flatten with sphere, cube and cylinder Brushes up to 4 m,
  about 60 Materials from one data file, a hotbar and a Material palette, hardness-timed digging,
  and a Ghost that shows the Voxels an Edit would change. The in-process server checks every Edit.
- **Lighting.** Primary visibility, sun shadows, radiance-cache global illumination, emissive
  Voxels, an atmosphere, a day and night cycle, a denoiser and upscaler, and a progressive
  **Reference** path tracer (`R`) that is the ground truth the Primary view is measured against.
  Rays find the Planet with hardware ray queries where the GPU has them, and with a compute-shader
  walk of the Bricks where it does not.

Not built yet: swimming, climbing and the full Zoom, the avatar, saves and the start menu,
multiplayer over a network, vegetation, rivers, Debris and Fluids. The dedicated `server` binary
only ticks at 60 Hz today; single-player runs the simulation in the client's process.

## Requirements

- A Rust toolchain. `rust-toolchain.toml` pins the exact version, and `rustup` installs it when you
  build. The workspace uses edition 2024.
- A Vulkan GPU and driver. Metal, Direct3D 12 and OpenGL are not compiled in. On Linux, install the
  Vulkan loader and driver (for example `mesa-vulkan-drivers`) and the usual X11 or Wayland
  development packages.
- **Ray tracing:** hardware ray queries are the default where the adapter and its driver offer
  Vulkan ray queries. Where they do not, or where you choose compute in the debug panel (`F1`, "Hardware ray tracing (setting)", kept in `settings.toml` in the
  per-user config folder), the compute tracer runs instead.
- **Machine:** the reference is an RTX 4090 at 1440p, 60 fps. The low-end reference, which the game
  runs on without failing, is **4 threads, 8 GB of RAM (about 5 GiB free), 4 GB of video memory and
  no ray queries** (a GTX 1650 or RX 570 class card). The game sizes its threads, memory and
  level-of-detail bias from the machine it finds.

## Build and run

Build in release mode: a debug build is slow enough to distort frame times.

```sh
cargo run --release -p client                        # the game window
cargo run --release -p client -- --profile low-end   # the same, held to the low-end reference
cargo run --release -p server                        # the dedicated server (a 60 Hz tick loop so far)
```

The first launch builds the world map (about 7 s on 16 threads, 23 s on 4) and writes it to
`$XDG_CACHE_HOME/bhayanakvoxel` (`~/.cache/bhayanakvoxel` if that is unset, `%LOCALAPPDATA%\bhayanakvoxel` on Windows); later launches read it
back in a fraction of a second. `BHAYANAK_CACHE_DIR` names another folder and
`BHAYANAK_NO_MAP_CACHE=1` turns the cache off. The window title says while the Planet is still being
generated.

### Machine flags

The `client` takes the flags, anywhere on the line, and the `server` reads only the environment
variables. They only lower what the machine has.

| Flag | Environment variable | Meaning |
| --- | --- | --- |
| `--profile low-end` (or `auto`) | `BHAYANAK_PROFILE` | Run as the low-end reference machine does, on any machine, with the process pinned to 4 logical processors |
| `--threads N` | `BHAYANAK_THREADS` | Use at most N threads |
| `--memory-mib N` | `BHAYANAK_MEMORY_MIB` | Use at most N MiB of memory |
| `--video-memory-mib N` | `BHAYANAK_VIDEO_MEMORY_MIB` | Use at most N MiB of video memory |
| `--no-ray-queries` | `BHAYANAK_RAY_QUERIES=0` | Act as if the adapter had no ray queries (the compute tracer) |

The client prints the profile it runs with on its first line.

### Subcommands of `client`

| Command | What it does |
| --- | --- |
| `client bench [--out DIR] [--short] [--streaming [--real-time]] ...` | A scripted flight over the Planet, offscreen, that times every GPU pass and writes `report.json` and screenshots. Exits with status 2 if a pass or a gate is over its budget. `--help` lists every option. |
| `client lookdev --out DIR [--size WxH] [--spp N] [--shot NAME] ...` | Renders the look-dev shot list (a forest floor, a canyon, crystal caves, a pull-out to the Planet) with the real pipeline and writes a contact sheet. `--short` is a smoke test on any device. |
| `client map-build [--no-write]` | Builds the full-size world map cold, writes the cache, reads it back, checks the two are the same map and says how long each took. |

The benchmark workflow, its budgets (`bins/client/bench-budgets.toml`) and the 4090 runs are in
[`docs/agents/benchmarking.md`](docs/agents/benchmarking.md). `cargo run --release -p prefetch-sim`
prints the CPU prefetch simulator's report.

## Controls

| Input | Action |
| --- | --- |
| Click | Capture the mouse; click again to apply the tool. A Dig is made by holding the button |
| `Esc` | Release the mouse |
| `W` `A` `S` `D` | Walk |
| `Shift` | Run |
| `Space` | Jump |
| `Ctrl` or `C` | Crouch |
| `N` | Switch between walking and the debug noclip camera |
| `1`-`9` | Pick a hotbar tool: dig sphere, cube or cylinder; place sphere, cube or cylinder; smooth sphere; flatten cube or cylinder |
| Wheel | Step along the hotbar (mouse captured) |
| `Alt` + wheel, or `[` `]` | Smaller or bigger Brush, from 1 Voxel up to 4 m across |
| `Tab` | Open or close the Material palette: type to search, click a Material or press `Enter` for the first match |
| `-` `=` | Previous or next Material (placed by place and flatten) |
| `G` | Brush axes on the local up (snapped to 45 degrees) or on the world grid |
| `R` | Switch between the Primary view and the Reference path tracer |
| `V` | Cycle the debug views |
| Arrow keys | Set the time of day, half an hour a press |
| `P` | Stop or start the world clock |
| `F1` | Show or hide the debug panel (the hotbar stays) |

In noclip, `W` `A` `S` `D` fly along the view, `Space` and `Ctrl` or `C` go up and down, `Shift` flies
faster, `Q` and `E` roll the camera and `X` levels it; nothing collides. Edits are made as the
walking character, not from noclip.

The character is 1.8 m tall, walks at 4.5 m/s, runs at 7, crouches at 2, jumps about 1.2 m and steps
up 0.5 m (`crates/sim/movement.toml`). A Dig takes 0.15 s for each unit of the Material's hardness
(at least 0.05 s), shown by a bar by the middle of the window. The server holds each player to 20
Edits a second, a Brush of at most 2 m radius (1 m for a smooth or a flatten), a reach of 8 m and
8 m³ of Voxels changed a second; the limits and their reasons are in `crates/sim/src/edit.rs`.

## Repository

| Path | What it is |
| --- | --- |
| `crates/brick-store` | Sparse 8³ Brick storage, coordinates and the LOD pyramid. CPU only. |
| `crates/planet-gen` | Deterministic Planet generation: the world map and its cache, terrain, Caves and Crystal clusters. |
| `crates/materials` | Every Material of the game, defined in `materials.toml`. |
| `crates/sim` | The simulation shared by client and server: Edit validation and budgets, the character controller, prediction, the world clock and the fixed-timestep server. |
| `crates/net` | The client and server message protocol, carried over an in-memory loopback today. |
| `crates/render` | The wgpu renderer: the compute and hardware tracers, the Reference path tracer, GI, denoiser, atmosphere, GPU pass timers and the debug overlay. |
| `crates/machine` | Reads the machine's threads, memory and ray queries in one place, the low-end profile, and the deterministic parallel helpers. |
| `crates/prefetch-sim` | A CPU simulator that replays camera paths to size the streaming shell, margins and Guard. |
| `bins/client` | The game window, streaming, editing, and the `bench`, `lookdev` and `map-build` commands. |
| `bins/server` | The dedicated server. It never links wgpu or winit, and CI enforces that. |
| `third_party/gpu-allocator` | A patched copy of `gpu-allocator` ([`PATCH.md`](third_party/gpu-allocator/PATCH.md)). |

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace    # needs cargo-nextest; cargo test --workspace also works, more slowly
```

CI runs these on Linux and Windows, with render tests on lavapipe (a software Vulkan driver, so no
GPU is needed), the golden-hash determinism tests in debug and release on both, and builds the
Windows client. The jobs that gate merges, and how Rust and the runner images are pinned, are in
[`docs/agents/ci.md`](docs/agents/ci.md).

## Where the docs are

| Doc | What is in it |
| --- | --- |
| [`docs/spec.md`](docs/spec.md) | The Vertical-slice spec: the design, the Milestones and the playtest checklists |
| [`CONTEXT.md`](CONTEXT.md) | The project's vocabulary |
| [`docs/adr/`](docs/adr/) | The architecture decisions |
| [`docs/measurements.md`](docs/measurements.md) | The measured numbers behind the decisions |
| [`docs/perf/resource-audit.md`](docs/perf/resource-audit.md) | The audit of how the game uses the whole machine |
| [`docs/research/`](docs/research/) | Research notes; [`INSPIRATION.md`](INSPIRATION.md) links the talks that inspired the game |
| [`AGENTS.md`](AGENTS.md) and [`docs/agents/`](docs/agents/) | Guidance for contributors and agents: the issue tracker, triage labels, CI and benchmarking |
