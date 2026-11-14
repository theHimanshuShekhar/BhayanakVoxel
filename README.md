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
the Planet is Voxels all the way through, down to 5 cm, and every one of them is editable.

> **Status:** early development. The skeleton (M0) is done; M1a — "Walk the Planet" — is in
> progress. Digging, walking, deterministic Planet generation and the rendering tracer work today.
> Flight and Zoom, real-time lighting, saves, Debris and Fluids are still being built. The plan is
> in [`docs/spec.md`](docs/spec.md).

## What makes it different

- **One planet, no map borders.** A single sphere of Voxels, 5 cm across each, with an editable
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

The client generates the Planet on startup, which takes about half a minute in a release build; the
window title says when it is done.

### Controls

| Input | Action |
| --- | --- |
| Click | Capture the mouse; click again to apply the tool. A Dig is made by holding the button |
| `1`-`9` | Pick a hotbar tool (mode and shape): dig sphere / cube / cylinder, place sphere / cube / cylinder, smooth sphere, flatten cube / cylinder |
| Wheel | Step along the hotbar (mouse captured) |
| `Alt` + wheel | Smaller or bigger Brush, from 1 Voxel up to 4 m across |
| `[` `]` | The same, from the keyboard |
| `Tab` | Open or close the Material palette: type to search by name, click a Material or press `Enter` for the first match. Opening it releases the mouse, closing it captures it again if it was captured before; every key goes to the search box while it is open |
| `-` `=` | Previous or next Material (placed by place and flatten) |
| `G` | Brush axes on the local up (snapped to 45 degrees) or on the world grid |
| `W` `A` `S` `D` | Walk |
| `Shift` | Run |
| `Space` | Jump |
| `Ctrl` or `C` | Crouch |
| `N` | Switch between walking and the debug noclip camera |
| `Q` `E` | Roll the noclip camera left or right; `X` levels it |
| Arrow keys | Turn the sun |
| `R` | Switch between primary visibility and the reference path tracer |
| `V` | Cycle debug views |
| `F1` | Show or hide the debug panel (the hotbar stays) |
| `Esc` | Release the mouse |

The client walks the Planet as a character (1.8 m tall, walks 4.5 m/s, runs 7, crouches 2, jumps
about 1.2 m, steps up to 0.5 m; numbers in `crates/sim/movement.toml`). In noclip, `W` `A` `S` `D`
fly along the view, `Space` and `Ctrl`/`C` go up and down, and `Shift` flies faster; `Q` and `E`
roll the camera about where it looks (45 degrees a second) and `X` levels it; nothing
collides. Coming out of noclip sets the character down where the camera is.

The hotbar is along the bottom of the window and is clickable while the mouse is released (`Esc`, or
the palette); the palette has the Brush size buttons too. The Materials are all there for the
taking: no inventory. Placing, smoothing and flattening are instant. A Dig takes as long as the
hardness of the Material at the Voxel aimed at says, 0.15 s per unit of hardness in
`crates/materials/materials.toml` (sand 0.15 s, soil 0.3 s, rock 0.9 s, obsidian 1.2 s, at least
0.05 s), and a bar by the middle of the window shows the hold; looking at another Voxel starts it
over. The client times it and then sends the one ordinary Edit; the server does not check the time.

The Brush is shown as a ghost where the click would apply it: exactly the Voxels it would change
(grey when the server will turn it down). A placed Brush sits on the face that was aimed at, is
aligned to the Planet's local up (exactly radial) and turned to face the way the camera looks, in
steps of 45 degrees. The server holds each player to 20 Edits a second, a Brush of at most 2 m
radius for a Dig or a Place (4 m across) and 1 m for a Smooth or a Flatten, a reach of 8 m from the
eyes (the client aims at 7.5 m), and 8 m^3 (64,000 Voxels) a second of Voxels changed, in bursts of
up to 576,000. Separately, a CPU budget of 4.8 million units a second (2.88 million at once) counts the
Voxels in the box that holds the Brush, whether or not it changes anything (a Smooth's count six
times), which is what stops Edits that change nothing from being free. Edits are made as the
walking character and not from noclip; an Edit over a limit is taken back.

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

CI runs those on Linux, builds the Windows client, runs the non-GPU tests on Windows, and runs the
golden-hash determinism tests across debug and release builds on both Linux and Windows. Render
tests run on lavapipe, the software Vulkan driver, so they need no GPU. The client tests that
start real threads run in a separate job that does not gate merges. Rust and the runner images are
pinned. Details are in [`docs/agents/ci.md`](docs/agents/ci.md).

Contributor and agent guidance lives in [`AGENTS.md`](AGENTS.md), with the issue tracker,
triage labels and domain-doc conventions in [`docs/agents/`](docs/agents/).
