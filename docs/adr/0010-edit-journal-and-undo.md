# An Edit journal for durability, and undo by restored bricks

Decided in the plan review of 2026-11-10. Both parts amend ADR 0003, which stands otherwise: clients generate terrain and the server replicates Edit operations.

**The journal.** The server appends every accepted Edit, with its sequence number, to a journal in the world's `world.db` as it applies it, and flushes about once a second. The autosave of about every minute becomes a checkpoint: it writes the diverged bricks and then truncates the journal up to the sequence number it covers. Loading a world reads the last checkpoint and replays the journal after it. A crash loses about a second of Edits at the most, not a minute.

We chose it because an Edit is already a small, ordered, deterministic operation (about 24 bytes, ADR 0003), so logging it is nearly free, and replaying it is the code path every client already runs. It also settles the order of the streaming work: saves (#62) come before Edit epochs (#144), which need to know what a persisted brick is.

**Undo.** A player can undo their own last Edits (about 32 a player, kept for the session and not persisted, as #138 decided). The server keeps, for each of those Edits, the bricks it overwrote. An undo is a small operation from the client; the server restores the bricks and sends them to every client through the snapshot path that edited bricks already use on a join or a fingerprint mismatch. Voxels that another player, Debris or a Fluid has changed since are left alone (#138's clip rule).

This replaces #138's transport, which made undo a new kind of Edit operation that every client applied. Restoring bricks needs no inverse of each Brush (a Smooth has none that is exact) and no second deterministic code path to keep bit-identical.

## Considered Options

- **Autosave only:** the spec's first design. A crash loses up to a minute of building.
- **Write diverged bricks as they change:** one 2 m Edit touches thousands of bricks; the journal entry is 24 bytes.
- **Journal only, never checkpoint:** load time grows with the world's whole history.
- **Undo as an inverse Edit operation** (#138): small on the wire, but each Brush needs an exact inverse, applied identically everywhere.
- **Undo on the client only:** the server is authoritative; a client cannot put matter back.

## Consequences

- The journal and the checkpoint are one transaction boundary: a checkpoint that truncates must be atomic with the bricks it wrote.
- Replaying needs the same `GENERATOR_VERSION`; the save migration framework (#62) covers a version change by checkpointing before it.
- Undo history costs server memory: bricks overwritten by up to 32 Edits a player. Its bound is set when M1b-1 is ticketed, and the bot soak reports memory per player.
- An undo is bigger on the wire than an Edit (compressed bricks, not 24 bytes). It is rare, and rate-limited like an Edit.
- Undo is in M1b-1; the spec no longer lists it as out of scope.
