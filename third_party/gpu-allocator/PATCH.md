# gpu-allocator 0.28.0, patched

This is the published `gpu-allocator` 0.28.0 (MIT OR Apache-2.0) with its examples, dev-dependencies
and README left out, and one change, in `src/vulkan/mod.rs` (`MemoryType::free`): an empty block that
supports sub-allocations is **kept**, not destroyed. The workspace's `[patch.crates-io]` uses it, so
wgpu-hal's allocator keeps every memory block it has made, up to the most that were in use at once.

Why (#236): wgpu makes the scratch buffer of an acceleration-structure build for each build call and
frees it when the GPU is done. When no block had room, the allocator made a 256 MiB block for it and,
when the scratch was freed, destroyed the block again as it was empty. On the 4090 that made a block
on about 1% of the frames of cold start and of a flight, and each of them stalled the GPU for 6.5 to 7 ms
before the builds' first command (`hardware.gap_new_blocks`; every other frame's gap is under 0.6 ms).

To go back to the published crate: delete this directory, the `[patch.crates-io]` entry and the
`exclude` in the root `Cargo.toml`, and the test `empty_blocks_are_kept` in
`crates/render/tests/hw_process_diagnosis.rs`. Bumping wgpu: check that wgpu-hal still takes
`gpu-allocator` 0.28 and whether the upstream crate keeps an option for this.
