# CI

The workflow is `.github/workflows/ci.yml`. It runs on every pull request and on every push to
`main`.

## Which jobs gate

These must be green to merge:

| Job | What it runs |
| --- | --- |
| `linux` | `cargo fmt --check`, clippy with `-D warnings`, `cargo test --workspace` on lavapipe (software Vulkan) without the real-thread tests, and the check that `server` does not depend on wgpu or winit. |
| `windows-tests` | Tests of `brick-store`, `materials`, `net`, `planet-gen`, `sim`, `prefetch-sim` and `server`, and the client's unit tests that need no GPU and start no real threads. |
| `windows-client` | Builds the release client and uploads it as the `client-windows` artifact, which the benchmark uses. |
| `determinism` | The golden hash tests of `planet-gen` and `sim`, in debug and release, on Linux and Windows. |

## Which job does not gate

`real-thread-tests` runs on Linux and Windows and has `continue-on-error`, so a failure in it does
not fail the workflow: the run still ends as a success. The job's own check shows as failed when
it fails, so look at it on a PR, but do not make it a required check.

It runs the client tests that start real threads: the loader's workers, the scene thread, or a
plain thread. Some of them wait on wall-clock time, and they fail now and then on a busy runner.
A later ticket gives CI a stepped, single-threaded mode for them. Until then, run them locally
when you change the loader, the scene thread or streaming:

```
cargo test -p client --bin client -- streaming_tests:: loader:: priority::
```

The list of these tests is `.github/real-thread-tests.txt`, one test-name filter per line. The
gating jobs `--skip` each line and `real-thread-tests` runs only those lines, so every client test
runs in exactly one place. **A new test that starts a loader or the scene thread must be added to
that file.**

## No GPU on Windows

The Windows runners have no Vulkan driver. So `windows-tests` does not run the `render` crate or
`bins/client/tests/bench.rs`. It also skips two client unit tests by name, because they make an
`Offscreen` renderer: `bench::caves::tests::the_stills_are_timed_and_photographed` and
`bench::edits::tests::the_largest_brush_is_dug_and_placed_in_turn_at_no_more_than_the_metric_rate`.
A new client test that needs a GPU must be skipped there too. The `linux` job runs all of them.

## Pins

- **Rust** is pinned in `rust-toolchain.toml` (with `rustfmt` and `clippy`). The action
  `.github/actions/setup-rust` installs it with `rustup toolchain install`; it does not use
  `dtolnay/rust-toolchain`, which ignores that file. Every job prints `rustc --version` first.
- **Runner images** are named in `ci.yml`: `ubuntu-24.04` and `windows-2025-vs2026`. The second is
  what `windows-latest` was on 2026-11-11.

To bump Rust: change `channel` in `rust-toolchain.toml` to the new exact version. Fix any new
clippy warnings in the same PR. Check that every job's log shows the new `rustc --version`.

To bump an image: change its name everywhere it appears in `ci.yml` (the `runs-on` lines and the
matrix `os` lists), in one PR. The `determinism` matrix names change with it, so a branch
protection rule that names those checks must be updated.

A bump is its own PR, so that a red CI is never a mystery.
