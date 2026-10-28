# Clients generate terrain; the server replicates only Edit operations

Every client generates the Planet itself from `(seed, GENERATOR_VERSION)` with the same deterministic `planet-gen` crate the server uses. The authoritative dedicated server never streams terrain. It validates each Edit, gives it a global sequence number, and sends it to every client Planet-wide as a small operation of about 24 bytes ("dig sphere r at x,y,z"). Clients receive compressed snapshots of edited bricks only when they join or when a brick fingerprint doesn't match.

We chose this because streaming terrain at 10 cm with a huge render distance would cost about 12 Mbit/s per walking player, while the operation stream costs about 61 kbit/s even for 64 busy players. Sending Edits Planet-wide keeps far LOD and the orbital Zoom accurate. The cost is a hard requirement for bit-identical generation and Edit application everywhere. That requirement is enforced by cross-platform hash tests in CI, a fingerprint check when a client joins, and fingerprint checks on bricks near Edits.

Even single-player runs the real server simulation in-process, behind the same message protocol over an in-memory loopback. This exercises the protocol, prediction and acks from M1 onward, so multiplayer later means replacing the loopback with renet rather than retrofitting a network layer.

## Considered Options

- **Server-streamed terrain:** the simplest correctness model, but impossible on bandwidth.
- **Per-Voxel deltas:** about 520 Voxels per 1 m crater, far larger than one operation.
- **Interest-limited Edits with catch-up snapshots:** rejected because the Zoom would show stale terrain far away.
- **Direct calls in M1, network added later:** faster to start, but the riskiest retrofit.
