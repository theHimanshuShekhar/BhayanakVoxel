# CI

The workflow is `.github/workflows/ci.yml`. It runs on every pull request and on every push to
`main`.

## Which jobs gate

These must be green to merge:

| Job | What it runs |
| --- | --- |
| `linux` | `cargo fmt --check`, clippy with `-D warnings`, `cargo test --workspace` on lavapipe (software Vulkan) without the real-thread tests, the check that no line of the real-thread list matches a test outside the client binary, and the check that `server` does not depend on wgpu or winit. |
| `windows-tests` | Tests of `brick-store`, `materials`, `net`, `planet-gen`, `sim`, `prefetch-sim` and `server`, and the client's unit tests that need no GPU and are not in the real-thread list. The stepped streaming tests are among them. |
| `windows-client` | Builds the release client and uploads it as the `client-windows` artifact, which the benchmark uses. |
| `determinism` | The golden hash tests of `planet-gen` and `sim`, in debug and release, on Linux and Windows. |

## Which job does not gate

`real-thread-tests` runs on Linux and Windows and has `continue-on-error`, so a failure in it does
not fail the workflow: the run still ends as a success. The job's own check shows as failed when
it fails, so look at it on a PR, but do not make it a required check.

It runs the client tests that are about real threads or real time (see the next section). Some of
them wait on wall-clock time, and they fail now and then on a busy runner. Run them locally when
you change the loader, the scene thread or streaming:

```
cargo test -p client --bin client -- streaming_tests:: loader:: priority::
```

The list of these tests is `.github/real-thread-tests.txt`, one test-name filter per line. The
gating jobs `--skip` each line and `real-thread-tests` runs only those lines, so every client test
runs in exactly one place.

Two checks keep the list honest (`.github/scripts/check-real-thread-list.sh`). The
`real-thread-tests` job fails when a line matches no test of the client binary: a test was
renamed or removed, and its line is stale. The `linux` job fails when a line matches a test outside
the client binary: the gating jobs skip a line in the whole workspace but the real-thread job runs
it in the client binary only, so that test would run nowhere. The script prints how many tests gate
and how many run in the real-thread job.

## Stepped tests and real-thread tests

Streaming runs on threads: the loader's workers and the scene thread. A test of what that code does
should not need the threads.

**Write a stepped test** when the point is what the code does: what becomes resident, what is
drawn, in what order, what is evicted, soft misses, whether an Edit shows up, what a pause or a
budget or a hold does. A stepped test has no threads, no sleeps and no wall-clock limits. It gives
the same run every time, so it gates merges.

To write one, use `spawned_world(Run::Stepped)` in `streaming_tests.rs`, which is
`World::with_stepped_loader` (and `Loader::stepped` for the loader alone). Each `recentre` or
`stream` is one step: the loader makes up to 256 jobs and the scene thread does one round of its
work, on the test's thread. Calls that wait for the scene thread (`sync`, `set_bands`) step it until
the job is done. Time is a clock that only the test moves: `pace(&world, time)` moves it on, and a
hold (the coarsening's, the soft-miss count's) ends when the test has moved the clock past it. Use
`Patience` to wait for something: it counts steps, not seconds. Do not call `Instant::now` or
`thread::sleep` in a stepped test.

A stepped scene thread can be busy too. `world.worker().busy_for(time)` has it do nothing for `time`
of the clock. A wait that has a timeout, such as the frame thread's `EDIT_WAIT` for an Edit, then
gives up when the timeout is over on that clock, and the Edit shows in the first look after the
thread is free. That is the stepped test of ADR 0006's bound.

**Write a real-thread test** only when the point is a thread or the time: how long the frame thread
waits, a lock that is held too long, a worker that panics or races, a scene thread that is busy or
parked. It uses `Run::Threads(n)`, and its name goes in `.github/real-thread-tests.txt` with a
comment that says why. If a test is part behaviour and part time, split it in two.

How the stepped mode shares code with the real one: the loader's worker loop is `poll` (decide),
`generate` and `complete` (do); a stepped loader calls the same three on the calling thread. The scene
thread's loop is a wait (`Engine::await_work`) and a round (`Engine::round`); a stepped scene
thread calls the round. What a stepped mode does not have is the waiting itself, so a test of how a
parked thread wakes is a real-thread test. A step takes no time on the clock, and a pause moves the
clock on by the pause. The stepped parts are in the shipped binary, since `client bench --streaming` (#181) steps the same code and is that binary: the game starts threads and never steps. Its clock (`bins/client/src/clock.rs`) is the machine's, `Instant::now` and `thread::sleep`, behind a test of whether the clock is stepped that is never true there. The streaming bench's counts are checked by stepped tests (`bench::stream`) that gate on Linux and on Windows; see `docs/agents/benchmarking.md`.

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
