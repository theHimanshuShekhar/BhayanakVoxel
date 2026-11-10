## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues on theHimanshuShekhar/bhayanakvoxel, using the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Uses the default label names: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Benchmarking

PRs that can change GPU timings (shaders, passes, the tracer, GPU uploads) get the `needs-benchmark` label; the orchestrating agent runs `client bench` on the PR's Windows artifact on the 4090. CPU-only PRs merge on green CI; ones that touch streaming get `needs-stream-bench` once the streaming bench exists. See `docs/agents/benchmarking.md`.
