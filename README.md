<p align="center">
  <img src="assets/icon.svg" width="88" height="88" alt="">
</p>

<h1 align="center">BhayanakVoxel</h1>

<p align="center">
  A detailed voxel game set on a single spherical planet, where players reshape a finely
  voxelized world and can zoom continuously from their character out to orbit.
</p>

BhayanakVoxel is built as a custom engine in Rust on [wgpu](https://wgpu.rs), with Vulkan as the
only supported backend. It does not use a heightfield, a mesh renderer or a third-party engine:
the Planet is Voxels all the way through, down to 10 cm, and every one of them is editable.

> **Status:** early development. The skeleton (M0) is done; M1a — "Walk the Planet" — is in
> progress. Digging, deterministic Planet generation and the rendering tracer work today.
> Character movement, Zoom, real-time lighting, Debris and Fluids are still being built.

## What makes it different

- **One planet, no map borders.** A single sphere of Voxels, 10 cm across each, with an editable
  surface. Edits propagate through the LOD pyramid, so a crater dug up close stays visible from
  orbit.
- **Continuous Zoom.** The camera pulls back from first/third person to a full view of the Planet
  without a separate map view or a load screen.
- **Sparse 8³ Bricks, not an octree.** Each brick stores a 512-bit occupancy mask plus
  palette-compressed 16-bit Material IDs; uniform bricks cost nothing. Bricks and their coarser
  mip levels are the one representation used everywhere — near terrain, far terrain and orbit.
- **Deterministic generation.** Clients generate the Planet themselves from
  `(seed, GENERATOR_VERSION)` using the same crate as the server, bit-for-bit on every platform.
  The server only replicates Edit operations, which keeps multiplayer bandwidth tiny and far
  terrain accurate.
- **A path tracer as ground truth.** A progressive reference path tracer runs in the same
  renderer as the real-time compute-shader tracer, so lighting work is measured against a correct
  image rather than tuned by eye.

## How it works

| Piece | What it does |
| --- | --- |
| `brick-store` | Sparse 8³ brick storage, coordinates and the LOD pyramid. CPU-only. |
| `planet-gen` | Deterministic terrain generation on top of the brick store, plus the world map. |
| `materials` | Every Material of the game, defined in one `materials.toml` data file. |
| `sim` | The authoritative simulation: Edit validation, prediction, fixed-timestep server. |
| `net` | The client/server message protocol, carried over an in-memory loopback today. |
| `render` | The wgpu renderer: compute-shader DDA through the bricks, the reference path tracer, GPU pass timers and the debug overlay. |
| `client` | The game window, free-fly camera, digging, and the `bench` harness. |
| `server` | The dedicated server. It never links wgpu or winit, and CI enforces that. |

The design decisions behind these choices are recorded as ADRs in
[`docs/adr/`](docs/adr/), and the project's vocabulary is defined in [`CONTEXT.md`](CONTEXT.md).

## Getting started

You need:

- A stable Rust toolchain (the workspace uses edition 2024).
- A Vulkan-capable GPU and driver. Metal, Direct3D 12 and OpenGL are not compiled in.
- On Linux, the Vulkan loader and windowing libraries — e.g. `mesa-vulkan-drivers` plus
  `libxkbcommon` and the usual X11/Wayland development packages.

Build in release mode: a debug build is slow enough to distort frame times, and generating the
test Planet takes noticeably longer.

```sh
# The game window
cargo run --release -p client

# The dedicated server
cargo run --release -p server
```

The client generates the Planet on startup, which takes about 20 seconds in a release build; the
window title says when it is done.

### Controls

| Input | Action |
| --- | --- |
| Click | Capture the mouse; click again to dig |
| `W` `A` `S` `D` | Walk |
| `Shift` | Run |
| `Space` | Jump |
| `Ctrl` or `C` | Crouch |
| `N` | Switch between walking and the debug noclip camera |
| Arrow keys | Turn the sun |
| `R` | Switch between primary visibility and the reference path tracer |
| `V` | Cycle debug views |
| `F1` | Show or hide the debug overlay |
| `Esc` | Release the mouse |

The client walks the Planet as a character (1.8 m tall, walks 4.5 m/s, runs 7, crouches 2, jumps
about 1.2 m, steps up to 0.5 m; numbers in `crates/sim/movement.toml`). In noclip, `W` `A` `S` `D`
fly along the view, `Space` and `Ctrl`/`C` go up and down, and `Shift` flies faster; nothing
collides. Coming out of noclip sets the character down where the camera is.

## Benchmarks

`client bench` flies a fixed camera path over the test Planet, times every GPU pass and writes a
JSON report plus screenshots. It is how "is this PR within the frame budget?" gets a number.

```sh
cargo run --release -p client -- bench --out bench-out
```

It exits with status 2 if a pass is over its budget. Budgets live in
`bins/client/bench-budgets.toml`; the full workflow — including the `needs-benchmark` label and
the Windows artifact — is described in [`docs/agents/benchmarking.md`](docs/agents/benchmarking.md).

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs those on Linux, builds the Windows client, and runs the golden-hash determinism tests
across debug and release builds on both Linux and Windows. Render tests run on lavapipe, the
software Vulkan driver, so they need no GPU.

Contributor and agent guidance lives in [`AGENTS.md`](AGENTS.md), with the issue tracker,
triage labels and domain-doc conventions in [`docs/agents/`](docs/agents/).
