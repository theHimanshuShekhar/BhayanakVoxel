# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root, or
- **`CONTEXT-MAP.md`** at the repo root if it exists: it points at one `CONTEXT.md` per context. Read each one relevant to the topic.
- **`docs/adr/`**: read ADRs that touch the area you're about to work in. In multi-context repos, also check `src/<context>/docs/adr/` for context-scoped decisions.

- **`docs/measurements.md`**: the measured numbers behind the ADRs. Read the section for your area when you need a number. When a ticket measures something again, add a dated entry there; edit an ADR only when the new number changes a decision.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved.

## File structure

This repo is single-context:

```
/
├── CONTEXT.md
├── docs/adr/
│   ├── 0001-custom-engine-rust-wgpu.md
│   └── 0002-brick-store-all-voxel-lod.md
├── docs/measurements.md
├── crates/
└── bins/
```

A multi-context repo has a `CONTEXT-MAP.md` at the root, one `CONTEXT.md` per context and
context-scoped `docs/adr/` folders. This one has none.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR 0007 (5 cm Voxels), but worth reopening because…_
