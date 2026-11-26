//! The allocator keeps a memory block that has become empty (#236): the patched `gpu-allocator`
//! (`third_party/gpu-allocator/PATCH.md`). The published crate destroys it when it is not the last
//! one, and the next allocation that finds no room makes it again, which stalled the GPU for about
//! 6.5 ms on the 4090 on the frame that needed it.

use render::hw::DeviceMemory;

fn device() -> Option<wgpu::Device> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let limits = adapter.limits();
    let (device, _queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits,
        ..Default::default()
    }))
    .ok()?;
    Some(device)
}

fn buffer(device: &wgpu::Device, mib: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("a block's worth"),
        size: mib << 20,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[test]
fn a_block_that_is_empty_again_is_still_there() {
    let Some(device) = device() else {
        eprintln!("skipped: no adapter");
        return;
    };
    let Some(before) = DeviceMemory::of(&device) else {
        eprintln!("skipped: the allocator cannot say what it has");
        return;
    };
    // Blocks are 128 MiB and then 256 on a GPU, and 64 then 128 where all the memory can be mapped
    // (a CPU's): nine buffers of 60 MiB are in at least three blocks of either, and none is as big
    // as a block, which would have one of its own that is freed with it.
    let held: Vec<wgpu::Buffer> = (0..9).map(|_| buffer(&device, 60)).collect();
    let made = DeviceMemory::of(&device).unwrap();
    assert!(made.blocks >= before.blocks + 3, "{before:?} then {made:?}");
    drop(held);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll failed");
    let after = DeviceMemory::of(&device).unwrap();
    assert_eq!(after.allocated_bytes, before.allocated_bytes, "{after:?}");
    assert_eq!(
        after.blocks, made.blocks,
        "the blocks that are empty are kept: {made:?} then {after:?}"
    );
    assert_eq!(after.reserved_bytes, made.reserved_bytes);
    // And the next buffer is in one of them: the allocator makes none.
    let again = buffer(&device, 60);
    assert_eq!(DeviceMemory::of(&device).unwrap().blocks, made.blocks);
    drop(again);
}
