## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues on theHimanshuShekhar/bhayanakvoxel, using the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Uses the default label names: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### CI

Which CI jobs gate merges and which do not, and how to bump the pinned Rust and runner images. See `docs/agents/ci.md`.

### Benchmarking

PRs that can change GPU timings (shaders, passes, the tracer, GPU uploads) get the `needs-benchmark` label; the orchestrating agent runs `client bench` on the PR's Windows artifact on the 4090. CPU-only PRs merge on green CI; ones that touch streaming (the loader, prefetch, eviction, generation) get `needs-stream-bench`, and the agent runs `client bench --streaming --real-time` on the 4090 (the full-size real-time run, gated, exit status 2). See `docs/agents/benchmarking.md`.

## Use the whole machine

The game and its tools use the machine's CPU, GPU and memory to the full, sized from the machine they run on and not from constants, for the best performance and quality at any time (owner, 2026-11-27, #264): "any modern game or engine should be utilizing the system resources in the best possible way." "Low end pc should be able to run the game as best as possible but high end pc should be able to utilize their hardware to the maximum potential too." Every ticket's brief and review checks this.

- **A serial hot path is a defect**, and so is a **fixed thread cap** or a **fixed CPU, GPU or memory budget**. Profile with per-thread CPU samples (`/proc/PID/task/*/stat`, or a profiler) before and after, not by guessing, and put the numbers in the PR.
- **It scales both ways.** Every budget derived from the machine has a **floor** (a low-end machine degrades quality and reach gracefully and never fails, stalls or runs out of memory) and a **ceiling** (a big one is used, up to what the work can use). The low-end reference is 4 threads, 8 GB RAM (about 5 GiB free), 4 GB VRAM and no ray queries (the compute tracer: GTX 1650 / RX 570 class), which `--profile low-end` (or `BHAYANAK_PROFILE=low-end`) runs on any machine, the process pinned to 4 logical processors; the 4090 PC is the high end.
- **Read the machine in one place, the `machine` crate.** Threads are `machine::threads()`, `bulk_threads()` (all but one, for work the frame thread must not wait behind) and `loader_workers()`; do not call `available_parallelism` anywhere else. A new budget takes `machine::profile()`, has a floor and a ceiling, and is tested at both.
- **Parallel code is deterministic** (ADR 0003; goldens and determinism hashes never change):
  - Cut work into pieces of a **fixed size**, never one piece per thread (`machine::par_pieces`, `par_map`, `par_fill`, `par_chunks`: the next free thread takes the next piece, so an uneven piece leaves no one idle at the tail).
  - Reduce in **piece order**. Only fold per thread (`par_fold`) with integer sums, least and greatest, never float sums.
  - No shared RNG, no atomic counters handing out ids, no `HashMap` iteration order in output.
  - Test it: the same hash at 1, 4 and all threads (see `world_map`'s `the_map_is_the_same_at_1_4_and_all_threads...`).
- **What is not cheap goes on a follow-up list** in the PR for the owner, with its measured cost. `docs/perf/resource-audit.md` is the audit; add to it.
