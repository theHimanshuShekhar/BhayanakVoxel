# gpu-allocator 0.28.0, patched

This is the published `gpu-allocator` 0.28.0 (MIT OR Apache-2.0) with its examples, dev-dependencies
and README left out, and one change, in `src/vulkan/mod.rs` (`MemoryType::free`, and the constant
`KEPT_EMPTY_BLOCKS`): of the **empty blocks that support sub-allocations, a memory type keeps up to
two** (`KEPT_EMPTY_BLOCKS = 2`), and destroys the rest as they empty. Upstream destroys an empty
block whenever it is not the last one. A block that is dedicated or personal to one allocation is
freed with it, as upstream does. The workspace's `[patch.crates-io]` uses this copy, so wgpu-hal's
allocator uses it (the Vulkan allocator only; the D3D12 one is not patched).

Why (#236): wgpu makes the scratch buffer of an acceleration-structure build for each build call and
frees it when the GPU is done. When no block had room, the allocator made a 256 MiB block for it and,
when the scratch was freed, destroyed the block again as it was empty. On the 4090 that made a block
on about 1% of the frames of cold start and of a flight, and each of them stalled the GPU for 6.5 to 7 ms
before the builds' first command (`hardware.gap_new_blocks`; every other frame's gap is under 0.6 ms).
The hardware backend also makes the blocks of six 60 MiB buffers before its first build
(`hw::make_spare_blocks`), so the first builds find them.

The bound is why the reserve is not the most memory that was ever live at once: the first version
kept every empty block and the 4090's run ended with 14 blocks, 3042 MiB reserved and 1603 MiB
allocated. The report's `memory.device_blocks`, `device_reserved_mib`, `device_allocated_mib`,
`device_empty_blocks` and `device_empty_mib` say what is kept and what of it is empty.

To go back to the published crate, take out together: this directory, the `[patch.crates-io]` entry and
the `exclude` in the root `Cargo.toml` (and `Cargo.lock` follows); `make_spare_blocks`,
`SPARE_BUFFERS`, `SPARE_BUFFER_BYTES` and `POOL_USAGES`' use in it in `crates/render/src/hw.rs`; the
test file `crates/render/tests/allocator_keeps_blocks.rs`; the test
`the_hardware_backend_has_its_spare_blocks_and_its_updates_make_no_block` in
`crates/render/tests/hw_agreement.rs`; and the check of `(u.blocks_made, u.reserved_grew)` in
`crates/render/tests/hw_process_diagnosis.rs`. Bumping wgpu: check that wgpu-hal still takes
`gpu-allocator` 0.28 and whether the upstream crate has an option for this.
