# CI

The workflow is `.github/workflows/ci.yml`. It runs on every pull request and on every push to
`main`.

## Which jobs gate

No ruleset requires status checks (the ruleset has only deletion and non-fast-forward), so which
jobs gate is by convention: these must be green to merge. The tests run with
[cargo-nextest](https://nexte.st), each test in its own process, and the long jobs are split into
shards that run in parallel (#263).

| Job | What it runs |
| --- | --- |
| `linux` | `cargo fmt --check`, clippy with `-D warnings`, `cargo test --doc --workspace` (nextest does not run doctests), the checks of the real-thread list (below), and the check that `server` does not depend on wgpu or winit. It runs no other tests. |
| `linux-tests (1/5)` to `(5/5)` | `cargo nextest run --workspace` on lavapipe (software Vulkan) without the real-thread tests, a fifth of the tests each (hardware ray queries are the game's default backend since #50 Part 2, and lavapipe has them where its Mesa does: the goldens are matched on both backends, and `hw_agreement` holds the backends to each other; on a runner whose lavapipe has no ray queries those tests skip, and the log says `SKIPPED ...: why`, which is the "CI falls back to compute and says so" of the ticket). All five must be green. |
| `windows-tests (slow)`, `(1/3)`, `(2/3)`, `(3/3)` | Tests of `brick-store`, `machine`, `materials`, `net`, `planet-gen`, `sim`, `prefetch-sim` and `server`, and the client's unit tests that need no GPU and are not in the real-thread list. `slow` runs the two client streaming tests that take 8 to 13 minutes each (`flying_away_and_back_...`, `flying_with_margins_...`) and nothing else, `1/3` to `3/3` run the rest between them. The stepped streaming tests are among them. `slow` also runs the doctests. |
| `windows-client` | Builds the release client and uploads it as the `client-windows` artifact, which the benchmark uses. |
| `determinism` | The golden hash tests of `planet-gen` and `sim`, in debug and release, on Linux and Windows. (These run with `cargo test`, not nextest.) |

The shards are `cargo nextest run --partition hash:N/M`: a test's shard is a hash of its binary and
name, so it is the same on every run, and the shards together run every test once. A test that is
slow is a floor under a shard's wall time, so a new very long test is better split in two than added.
`.config/nextest.toml` gives the tests that take minutes a `priority`, so that a shard starts them
first and does not reach one at the end; it changes the order and nothing else. The two longest
client tests (`flying_away_and_back_...`, about 10 minutes on a Windows runner, and
`the_day_cycle_reports_...`) are CPU-bound and set the least a shard can take. The shards build the
`client` and `render` crates at opt-level 2 (`--config profile.dev.package.<crate>.opt-level=2`),
not the workspace's unoptimised build, for the same reason; the tests run as written, with debug
assertions and overflow checks on.
cargo-nextest is pinned in `.github/actions/setup-nextest` (the version and the `taiki-e/install-action`
commit), and the job installs the release binary, not a build.

### Run the same split locally

```
cargo install cargo-nextest --version 0.9.152 --locked      # or a release binary from nexte.st
filterset=$(bash .github/scripts/real-thread-filterset.sh)
cargo nextest run --workspace --no-fail-fast -E "not ($filterset)"                       # all of linux-tests
cargo nextest run --workspace --no-fail-fast --partition hash:1/5 -E "not ($filterset)"  # linux-tests (1/5)
cargo test --doc --workspace
bash .github/scripts/check-real-thread-list.sh all     # counts per side; needs cargo-nextest and jq
```

`cargo nextest list --workspace -E "not ($filterset)"` shows which tests gate. A test that has to wait on
the wall clock in the gating jobs (it should not: see below) can be given a lower parallelism with
a `test-groups` entry in `.config/nextest.toml`; the test itself is not to be weakened.

## Which job does not gate

`real-thread-tests` runs on Linux and Windows and has `continue-on-error`, so a failure in it does
not fail the workflow: the run still ends as a success. The job's own check shows as failed when
it fails, so look at it on a PR, but do not make it a required check.

It runs the client tests that are about real threads or real time (see the next section), among them
the smoke test of the bench's real-time mode on the test Planet and its error case (#162): CI never runs the
full-size mode, which is for the 4090 (`docs/agents/benchmarking.md`; its fields are in `docs/perf/bench-report-reference.md`), and the tests of its gates
need no threads and gate. Some of
them wait on wall-clock time, and they fail now and then on a busy runner. Run them locally when
you change the loader, the scene thread or streaming:

```
cargo test -p client --bin client -- streaming_tests:: loader:: priority::
```

The job also runs two `#[ignore]`d wall-clock tests of the `sim` crate in a step of their own (the
list below is for the client binary only; #193), and that step fails if either did not run:
`a_tick_of_the_kilometre_walk_stays_within_the_time_it_has_...` (`crates/sim/tests/walk.rs`) holds
the mean and the 99th percentile of a tick's cost to the time a tick has, and
`on_its_own_thread_it_ticks_at_about_60_hz_and_answers` (`crates/sim/src/server.rs`) holds the
server thread's ticks in half a second to a range. Their counted twins gate: the kilometre walk's
character never waits for the ground (#166), makes at most `Ground::rate` bricks a tick and takes
bounded memory; the server thread ticks on and answers. A wall-clock limit does not belong in a
gating test: put the counted part in the gating one and the timed part in an `#[ignore]`d test
that this job runs.

The list of these tests is `.github/real-thread-tests.txt`, one test-name filter per line. The
gating jobs skip each line, as a nextest filterset made by `.github/scripts/real-thread-filterset.sh`
(`not (test(~line) | ...)`: `~` is "contains", as libtest's filter is), and `real-thread-tests`
runs only those lines with `cargo test` (libtest), so every client test runs in exactly one place.
A line is a string of letters, digits, `_`, `:` and `.` only, since nextest cannot take a quoted
name here.

Three checks keep the list honest (`.github/scripts/check-real-thread-list.sh`). The
`real-thread-tests` job (its Linux leg) fails when a line matches no test of the client binary: a test was
renamed or removed, and its line is stale. The `linux` job fails when a line matches a test outside
the client binary: the gating jobs skip a line in the whole workspace but the real-thread job runs
it in the client binary only, so that test would run nowhere. The `linux` job also checks that
nextest and libtest see the same tests, and that the filterset and the libtest filters select the same
ones in the client binary. The script prints how many tests gate and how many run in the real-thread
job (on 2026-11-28, at #260: 1600 and 17 under libtest, counted by `cargo test --workspace -- --list`), and how many gate in each crate.

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
clock on by the pause. The stepped parts are in the shipped binary, since `client bench --streaming` (#181) steps the same code and is that binary: the game starts threads and never steps. Its clock (`bins/client/src/clock.rs`) is the machine's, `Instant::now` and `thread::sleep`, behind a test of whether the clock is stepped that is never true there. The streaming bench's counts are checked by stepped tests (`bench::stream`) that gate on Linux and on Windows; see "The streaming mode" in `docs/perf/bench-report-reference.md`.

## No GPU on Windows

The Windows runners have no Vulkan driver. So `windows-tests` does not run the `render` crate or
`bins/client/tests/bench.rs`. It also skips six client unit tests by name, because they make an
`Offscreen` renderer: `bench::caves::tests::the_stills_are_timed_and_photographed`,
`bench::landscape::the_landscape_from_the_ground_500_m_and_20_km_matches_the_approved_images` (#166),
`bench::crystals::a_cluster_on_a_sunlit_slope_and_a_crystal_cave_match_the_approved_images` (#204),
`bench::edits::tests::the_largest_brush_is_dug_and_placed_in_turn_at_no_more_than_the_metric_rate`,
`bench::lookdev::run::tests::a_short_run_writes_a_folder_for_each_shot_and_a_sheet_that_names_every_image`
(#202) and `bench::tests::the_flights_updates_are_logged_by_where_in_the_run_they_were_made` (#50).
A new client test that needs a GPU must be skipped there too. The `linux-tests` shards run all of them.

## Pins

- **Rust** is pinned in `rust-toolchain.toml` (with `rustfmt` and `clippy`). The action
  `.github/actions/setup-rust` installs it with `rustup toolchain install`; it does not use
  `dtolnay/rust-toolchain`, which ignores that file. Every job prints `rustc --version` first.
- **Runner images** are named in `ci.yml`: `ubuntu-24.04` and `windows-2025-vs2026`. The second is
  what `windows-latest` was on 2026-11-11.

- **cargo-nextest** is pinned in `.github/actions/setup-nextest` (`nextest@0.9.152`, installed as a
  release binary by `taiki-e/install-action`, itself pinned by commit). The version is also named in
  the local-run command above. Every job that uses it prints `cargo nextest --version`.

To bump Rust: change `channel` in `rust-toolchain.toml` to the new exact version. Fix any new
clippy warnings in the same PR. Check that every job's log shows the new `rustc --version`.

To bump an image: change its name everywhere it appears in `ci.yml` (the `runs-on` lines and the
matrix `os` lists), in one PR. The `determinism` matrix names change with it, so a branch
protection rule that names those checks must be updated.

A bump is its own PR, so that a red CI is never a mystery.
