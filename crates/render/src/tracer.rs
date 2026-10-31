//! The GPU side of tracing: the packed scene in buffers, and the compute pipelines over it.
//! Windowed and offscreen rendering share this, so the golden image test exercises the same
//! code path the window does.

use bytemuck::{Pod, Zeroable};
use materials::MaterialTable;
use wgpu::util::DeviceExt;

use crate::accumulation::Accumulator;
use crate::camera::{Camera, CameraUniform};
use crate::debug_view::DebugView;
use crate::gpu_materials;
use crate::profiler::Profiler;
use crate::reference::{Hit, Ray};
use crate::scene::{MAX_LEVELS, REGION_WORDS, Scene, SceneUpdate, spare_words};
use crate::sun::{Sun, SunUniform};
use crate::{Result, shaders};

/// Format of the traced image: sRGB-encoded values in a plain unorm texture.
pub const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// `Level` in `trace.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LevelHeader {
    occupied_first: [i32; 3],
    table_mask: u32,
    occupied_end: [i32; 3],
    table_base: u32,
    hole_first: [i32; 3],
    pad0: u32,
    hole_end: [i32; 3],
    pad1: u32,
}

/// `SceneHeader` in `trace.wgsl`: the number of levels, then the first `level_count` of the
/// levels.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneHeader {
    level_count: u32,
    bound_radius: f32,
    pad0: [u32; 2],
    bound_centre: [i32; 3],
    pad1: u32,
    levels: [LevelHeader; MAX_LEVELS],
}

impl SceneHeader {
    fn new(scene: &Scene) -> Self {
        let mut header = SceneHeader::zeroed();
        header.level_count = scene.levels.len() as u32;
        if let Some(bound) = scene.bound {
            header.bound_radius = bound.clip_radius();
            header.bound_centre = bound.centre;
        }
        for (level, (slot, l)) in header.levels.iter_mut().zip(&scene.levels).enumerate() {
            let (hole_first, hole_end) = scene.hole(level);
            *slot = LevelHeader {
                occupied_first: l.occupied_first,
                table_mask: l.table_mask,
                occupied_end: l.occupied_end,
                table_base: l.table_base,
                hole_first,
                pad0: 0,
                hole_end,
                pad1: 0,
            };
        }
        header
    }
}

/// Seeds the reference path tracer's random numbers: a given view, sun and sample count always
/// give the same image.
pub const RNG_SEED: u32 = 0xB4A7_A9AC;

/// `Frame` in `common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    sun: SunUniform,
    /// The sample's index, the random seed, the debug view, then one unused.
    params: [u32; 4],
}

/// What the kernels draw into: the image, and the float buffer that holds the reference path
/// tracer's running mean, one `vec4<f32>` per pixel.
pub struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    accum: wgpu::Buffer,
    size: (u32, u32),
}

impl Target {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = Tracer::create_image(device, width, height);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let accum = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("accumulation"),
            size: u64::from(width) * u64::from(height) * 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        Self {
            texture,
            view,
            accum,
            size: (width, height),
        }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
}

/// Everything that decides what the reference image looks like, bar the scene (a new scene is a
/// new `Tracer`). Accumulation restarts when any of it changes.
#[derive(Clone, PartialEq)]
struct View {
    camera: Camera,
    sun: Sun,
    size: (u32, u32),
}

pub struct Tracer {
    scene: SceneBuffers,
    scene_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    frame_buffer: wgpu::Buffer,
    /// The Material table (see [`gpu_materials`]), which nothing changes after the upload.
    materials: wgpu::Buffer,
    camera_layout: wgpu::BindGroupLayout,
    primary: wgpu::ComputePipeline,
    reference: wgpu::ComputePipeline,
    accumulator: Accumulator<View>,
    debug_view: DebugView,
    /// Bytes of the Planet that have been written to the GPU, creating its buffers included.
    uploaded: u64,
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

/// A storage buffer holding `words`, with room for `spare` more words (as far as the device's
/// limit allows), padded to a non-empty size as bindings must be. Returns it and its size in
/// words. It can be written to with `COPY_DST`.
fn storage_buffer(
    device: &wgpu::Device,
    label: &str,
    words: &[u32],
    spare: usize,
) -> Result<(wgpu::Buffer, usize)> {
    let limit = device.limits().max_storage_buffer_binding_size;
    if words.len() as u64 * 4 > limit {
        return Err(format!(
            "{label} needs {} bytes, more than the device's {limit} limit",
            words.len() * 4
        )
        .into());
    }
    let capacity = (words.len() + spare).min((limit / 4) as usize).max(4);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: capacity as u64 * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    {
        let mut view = buffer
            .get_mapped_range_mut(..)
            .expect("a buffer created mapped");
        view.slice(..words.len() * 4)
            .copy_from_slice(bytemuck::cast_slice(words));
    }
    buffer.unmap();
    Ok((buffer, capacity))
}

/// Writes `words` to `buffer`, starting at word `at`, and returns the bytes written.
fn write_words(queue: &wgpu::Queue, buffer: &wgpu::Buffer, words: &[u32], at: usize) -> u64 {
    queue.write_buffer(buffer, at as u64 * 4, bytemuck::cast_slice(words));
    words.len() as u64 * 4
}

/// The Planet's packed bricks on the GPU. The buffers of an ordinary scene have room to grow,
/// since an Edit appends to `brick_refs` and `bricks` (see [`Scene::update`]); those of rings
/// are as big as the scene's, which has all its room already.
struct SceneBuffers {
    header: wgpu::Buffer,
    table: wgpu::Buffer,
    regions: wgpu::Buffer,
    brick_refs: wgpu::Buffer,
    bricks: wgpu::Buffer,
    /// Sizes of `brick_refs` and `bricks`, in words.
    capacity: [usize; 2],
    /// Bytes of GPU memory in the five buffers.
    bytes: u64,
    group: wgpu::BindGroup,
}

impl SceneBuffers {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, scene: &Scene) -> Result<Self> {
        let header = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene header"),
            contents: bytemuck::bytes_of(&SceneHeader::new(scene)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let spare = |len| {
            if scene.is_rings() {
                0
            } else {
                spare_words(len)
            }
        };
        let (table, _) = storage_buffer(device, "region table", &scene.table, 0)?;
        let (regions, _) = storage_buffer(device, "regions", &scene.regions, 0)?;
        let (brick_refs, refs_capacity) = storage_buffer(
            device,
            "brick refs",
            &scene.brick_refs,
            spare(scene.brick_refs.len()),
        )?;
        let (bricks, bricks_capacity) =
            storage_buffer(device, "bricks", &scene.bricks, spare(scene.bricks.len()))?;
        let bytes = [&header, &table, &regions, &brick_refs, &bricks]
            .iter()
            .map(|buffer| buffer.size())
            .sum();
        let entries: Vec<_> = [&header, &table, &regions, &brick_refs, &bricks]
            .into_iter()
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout,
            entries: &entries,
        });
        Ok(Self {
            header,
            table,
            regions,
            brick_refs,
            bricks,
            capacity: [refs_capacity, bricks_capacity],
            bytes,
            group,
        })
    }
}

impl Tracer {
    /// Uploads `scene` and the built-in Material table, and builds the pipelines.
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
        let scene_buffers = SceneBuffers::new(device, &scene_layout, scene)?;
        let scene_bytes = scene_buffers.bytes;

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera, image, accumulation and materials"),
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
                layout_entry(2, buffer_binding(wgpu::BufferBindingType::Uniform)),
                layout_entry(
                    3,
                    buffer_binding(wgpu::BufferBindingType::Storage { read_only: false }),
                ),
                layout_entry(4, buffer_binding(STORAGE_READ)),
            ],
        });
        let materials = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("materials"),
            contents: bytemuck::cast_slice(&gpu_materials::pack(MaterialTable::built_in())),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera"),
            size: size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame"),
            size: size_of::<FrameUniform>() as u64,
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
        let reference = compute_pipeline(
            device,
            "reference",
            shaders::PATHTRACE,
            "reference",
            &camera_layout,
            &scene_layout,
        );
        Ok(Self {
            scene: scene_buffers,
            scene_layout,
            camera_buffer,
            frame_buffer,
            materials,
            camera_layout,
            primary,
            reference,
            accumulator: Accumulator::new(),
            debug_view: DebugView::default(),
            uploaded: scene_bytes,
        })
    }

    /// A texture the kernels can write and that can be read back or sampled.
    fn create_image(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
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

    /// Records the primary visibility pass: one ray per pixel of `target` from `camera`, lit by
    /// `sun`. The pass is timed by `profiler`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_primary(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
    ) {
        self.write_uniforms(queue, camera, sun, 0);
        self.dispatch(device, profiler, encoder, "primary", &self.primary, target);
    }

    /// Records one more sample of the reference path tracer for every pixel of `target`, added
    /// to the running mean there. If the camera, the sun or the size of `target` differs from
    /// the last call, the mean starts over.
    ///
    /// The uniforms are written when this is called, so each sample must be submitted before the
    /// next is encoded. The pass is timed by `profiler`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_reference(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        camera: &Camera,
        sun: &Sun,
        target: &Target,
    ) {
        let view = View {
            camera: *camera,
            sun: *sun,
            size: target.size,
        };
        let sample_index = self.accumulator.begin_sample(&view);
        self.write_uniforms(queue, camera, sun, sample_index);
        self.dispatch(
            device,
            profiler,
            encoder,
            "reference",
            &self.reference,
            target,
        );
    }

    /// Brings the GPU copy of the Planet up to date with `scene`, which `update` describes the
    /// change to, and starts the reference image over. Only what changed is written, unless it
    /// no longer fits in the buffers, when all of `scene` is uploaded again.
    pub fn update_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        update: &SceneUpdate,
    ) -> Result<()> {
        self.accumulator.reset();
        let buffers = &self.scene;
        let mut written = 0;
        match update {
            SceneUpdate::Rebuilt => {
                self.scene = SceneBuffers::new(device, &self.scene_layout, scene)?;
                self.uploaded += self.scene.bytes;
                return Ok(());
            }
            SceneUpdate::Rings(patch) => {
                // The scene has all its room, so what changed is written where it is.
                if patch.levels {
                    let header = SceneHeader::new(scene);
                    queue.write_buffer(&buffers.header, 0, bytemuck::bytes_of(&header));
                    written += size_of::<SceneHeader>() as u64;
                }
                for &slot in &patch.table {
                    written += write_words(
                        queue,
                        &buffers.table,
                        &scene.table[slot * 4..][..4],
                        slot * 4,
                    );
                }
                for &region in &patch.regions {
                    let at = region * REGION_WORDS;
                    written += write_words(
                        queue,
                        &buffers.regions,
                        &scene.regions[at..][..REGION_WORDS],
                        at,
                    );
                }
                for (buffer, words, ranges) in [
                    (&buffers.brick_refs, &scene.brick_refs, &patch.brick_refs),
                    (&buffers.bricks, &scene.bricks, &patch.bricks),
                ] {
                    for range in ranges {
                        written += write_words(queue, buffer, &words[range.clone()], range.start);
                    }
                }
            }
            SceneUpdate::Patched {
                regions,
                brick_refs_from,
                bricks_from,
            } => {
                if scene.brick_refs.len() > buffers.capacity[0]
                    || scene.bricks.len() > buffers.capacity[1]
                {
                    self.scene = SceneBuffers::new(device, &self.scene_layout, scene)?;
                    self.uploaded += self.scene.bytes;
                    return Ok(());
                }
                for &region in regions {
                    let at = region * REGION_WORDS;
                    written += write_words(
                        queue,
                        &buffers.regions,
                        &scene.regions[at..][..REGION_WORDS],
                        at,
                    );
                }
                for (buffer, words, from) in [
                    (&buffers.brick_refs, &scene.brick_refs, *brick_refs_from),
                    (&buffers.bricks, &scene.bricks, *bricks_from),
                ] {
                    if from < words.len() {
                        written += write_words(queue, buffer, &words[from..], from);
                    }
                }
            }
        }
        self.uploaded += written;
        Ok(())
    }

    /// Bytes of GPU memory that the Planet's bricks take: the five buffers of the scene.
    pub fn scene_bytes(&self) -> u64 {
        self.scene.bytes
    }

    /// Bytes of the Planet written to the GPU so far: what [`Tracer::new`] made its buffers
    /// from, and what each update wrote.
    pub fn uploaded_bytes(&self) -> u64 {
        self.uploaded
    }

    /// Starts the reference image over, for what the `View` cannot see: a new `Target`, whose
    /// buffer is empty whatever the count says.
    pub fn reset_accumulation(&mut self) {
        self.accumulator.reset();
    }

    pub fn debug_view(&self) -> DebugView {
        self.debug_view
    }

    /// Chooses what the kernels draw (see [`DebugView`]). The reference image keeps
    /// accumulating whichever is chosen.
    pub fn set_debug_view(&mut self, view: DebugView) {
        self.debug_view = view;
    }

    /// Samples in the reference image so far.
    pub fn samples(&self) -> u32 {
        self.accumulator.samples()
    }

    fn write_uniforms(&self, queue: &wgpu::Queue, camera: &Camera, sun: &Sun, sample_index: u32) {
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&camera.uniform()),
        );
        let frame = FrameUniform {
            sun: sun.uniform(),
            params: [sample_index, RNG_SEED, self.debug_view.index(), 0],
        };
        queue.write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&frame));
    }

    fn dispatch(
        &self,
        device: &wgpu::Device,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        name: &str,
        pipeline: &wgpu::ComputePipeline,
        target: &Target,
    ) {
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera, image, accumulation and materials"),
            layout: &self.camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(target.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: target.accum.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.materials.as_entire_binding(),
                },
            ],
        });
        let mut pass = profiler.compute_pass(encoder, name);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.scene.group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(target.size.0.div_ceil(8), target.size.1.div_ceil(8), 1);
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
            // A one-shot probe with its own submit, so not timed.
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default()); // untimed
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &self.scene.group, &[]);
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
