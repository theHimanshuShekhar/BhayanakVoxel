//! The GPU side of tracing: the packed scene in buffers, and the compute pipelines over it.
//! Windowed and offscreen rendering share this, so the golden image test exercises the same
//! code path the window does.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::camera::{Camera, CameraUniform};
use crate::reference::{Hit, Ray};
use crate::scene::Scene;
use crate::{Result, shaders};

/// Format of the traced image: sRGB-encoded values in a plain unorm texture.
pub const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// `SceneHeader` in `trace.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneHeader {
    origin: [i32; 3],
    table_mask: u32,
    dims: [u32; 3],
    pad: u32,
}

pub struct Tracer {
    scene_group: wgpu::BindGroup,
    scene_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    camera_layout: wgpu::BindGroupLayout,
    primary: wgpu::ComputePipeline,
}

fn layout_entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

fn buffer_binding(ty: wgpu::BufferBindingType) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

const STORAGE_READ: wgpu::BufferBindingType = wgpu::BufferBindingType::Storage { read_only: true };

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    entry_point: &str,
    group_1: &wgpu::BindGroupLayout,
    group_0: &wgpu::BindGroupLayout,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(group_0), Some(group_1)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: &module,
        entry_point: Some(entry_point),
        compilation_options: Default::default(),
        cache: None,
    })
}

/// A storage buffer holding `words`, padded to a non-empty size as bindings must be.
fn storage_buffer(device: &wgpu::Device, label: &str, words: &[u32]) -> Result<wgpu::Buffer> {
    let bytes: &[u8] = if words.is_empty() {
        &[0; 16]
    } else {
        bytemuck::cast_slice(words)
    };
    let limit = device.limits().max_storage_buffer_binding_size;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{label} needs {} bytes, more than the device's {limit} limit",
            bytes.len()
        )
        .into());
    }
    Ok(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytes,
            usage: wgpu::BufferUsages::STORAGE,
        }),
    )
}

impl Tracer {
    /// Uploads `scene` and builds the pipelines.
    pub fn new(device: &wgpu::Device, scene: &Scene) -> Result<Self> {
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(1, buffer_binding(STORAGE_READ)),
                layout_entry(2, buffer_binding(STORAGE_READ)),
                layout_entry(3, buffer_binding(STORAGE_READ)),
                layout_entry(4, buffer_binding(STORAGE_READ)),
            ],
        });
        let header = SceneHeader {
            origin: scene.origin,
            table_mask: scene.table_mask(),
            dims: scene.dims,
            pad: 0,
        };
        let header = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene header"),
            contents: bytemuck::bytes_of(&header),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let buffers = [
            storage_buffer(device, "region table", &scene.table)?,
            storage_buffer(device, "regions", &scene.regions)?,
            storage_buffer(device, "brick refs", &scene.brick_refs)?,
            storage_buffer(device, "bricks", &scene.bricks)?,
        ];
        let entries: Vec<_> = std::iter::once(&header)
            .chain(&buffers)
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        let scene_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &scene_layout,
            entries: &entries,
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera and image"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(
                    1,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: IMAGE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let primary = compute_pipeline(
            device,
            "primary",
            shaders::PRIMARY,
            "primary",
            &camera_layout,
            &scene_layout,
        );
        Ok(Self {
            scene_group,
            scene_layout,
            camera_buffer,
            camera_layout,
            primary,
        })
    }

    /// A texture the primary kernel can write and that can be read back or sampled.
    pub fn create_image(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("traced image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Records the primary visibility pass: one ray per pixel of `image` from `camera`.
    pub fn encode_primary(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        image: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&camera.uniform()),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera and image"),
            layout: &self.camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(image),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("primary"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.primary);
        pass.set_bind_group(0, &self.scene_group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
    }

    /// Casts `rays` through `trace()` on the GPU and returns the hits, in order.
    pub fn trace_rays(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rays: &[Ray],
    ) -> Vec<Option<Hit>> {
        if rays.is_empty() {
            return Vec::new();
        }
        // Three vec4s per ray: cell, origin, direction.
        let input: Vec<[u32; 4]> = rays
            .iter()
            .flat_map(|r| {
                let bits = |v: [f32; 3]| [v[0].to_bits(), v[1].to_bits(), v[2].to_bits(), 0];
                [
                    [r.cell[0] as u32, r.cell[1] as u32, r.cell[2] as u32, 0],
                    bits(r.origin),
                    bits(r.dir),
                ]
            })
            .collect();
        let rays_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("probe rays"),
            contents: bytemuck::cast_slice(&input),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let hits_size = (rays.len() * 3 * 16) as u64;
        let hits_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe hits"),
            size: hits_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let probe_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe"),
            entries: &[
                layout_entry(0, buffer_binding(STORAGE_READ)),
                layout_entry(
                    1,
                    buffer_binding(wgpu::BufferBindingType::Storage { read_only: false }),
                ),
            ],
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe"),
            layout: &probe_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rays_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: hits_buffer.as_entire_binding(),
                },
            ],
        });
        let pipeline = compute_pipeline(
            device,
            "probe",
            shaders::PROBE,
            "probe",
            &probe_layout,
            &self.scene_layout,
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &self.scene_group, &[]);
            pass.set_bind_group(1, &group, &[]);
            pass.dispatch_workgroups((rays.len() as u32).div_ceil(64), 1, 1);
        }
        let bytes = crate::read_buffer(device, queue, encoder, &hits_buffer, hits_size);
        let words: &[[u32; 4]] = bytemuck::cast_slice(&bytes);
        words
            .chunks(3)
            .map(|c| {
                let float = |w: u32| f32::from_bits(w);
                (c[1][3] == 1).then(|| Hit {
                    t: float(c[0][3]),
                    position: [float(c[0][0]), float(c[0][1]), float(c[0][2])],
                    normal: [float(c[1][0]), float(c[1][1]), float(c[1][2])].map(|n| n as i32),
                    voxel: [c[2][0] as i32, c[2][1] as i32, c[2][2] as i32],
                    material: c[2][3] as u16,
                })
            })
            .collect()
    }
}
