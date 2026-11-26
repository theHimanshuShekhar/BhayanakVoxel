//! The allocator keeps up to two memory blocks per memory type that have become empty (#236): the
//! patched `gpu-allocator` (`third_party/gpu-allocator/PATCH.md`). The published crate destroys an
//! empty block unless it is the type's last active general block, and the next allocation that
//! finds no room makes it again, which stalled the GPU for about 6.5 ms on the 4090 on the frame
//! that needed it.

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
fn two_blocks_that_are_empty_again_are_kept_and_the_rest_are_freed() {
    let Some(device) = device() else {
        eprintln!("skipped: no adapter");
        return;
    };
    let Some(before) = DeviceMemory::of(&device) else {
        eprintln!("skipped: the allocator cannot say what it has");
        return;
    };
    assert_eq!(before.empty_blocks, 0, "{before:?}");
    // Blocks are 128 MiB and then 256 on a GPU, and 64 then 128 where all the memory can be mapped
    // (a CPU's): twelve buffers of 60 MiB are in at least four blocks of either, and none is as
    // big as a block, which would have one of its own that is freed with it.
    let held: Vec<wgpu::Buffer> = (0..12).map(|_| buffer(&device, 60)).collect();
    let made = DeviceMemory::of(&device).unwrap();
    assert!(made.blocks >= before.blocks + 4, "{before:?} then {made:?}");
    drop(held);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll failed");
    let after = DeviceMemory::of(&device).unwrap();
    assert_eq!(after.allocated_bytes, before.allocated_bytes, "{after:?}");
    // Two are kept, empty, and the others are gone (upstream keeps none here: the device has
    // another block in use, so none of these is the last one).
    assert_eq!(
        (after.blocks, after.empty_blocks),
        (before.blocks + 2, 2),
        "{made:?} then {after:?}"
    );
    assert!(after.reserved_bytes < made.reserved_bytes);
    // And the next buffer is in one of them: the allocator makes none.
    let again = buffer(&device, 60);
    assert_eq!(DeviceMemory::of(&device).unwrap().blocks, after.blocks);
    drop(again);
}
