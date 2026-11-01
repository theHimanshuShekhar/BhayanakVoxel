//! THROWAWAY (#99, branch `measure/reference-99`, never merged): what `client measure-reference`
//! needs of the offscreen renderer, which the library proper does not.

use super::*;
use crate::tracer::Variant;

/// Submitted samples that `reference_submit` lets the GPU have in flight.
const IN_FLIGHT: usize = 3;

impl Offscreen {
    /// Sets the reference's random seed; the image starts over.
    pub fn set_seed(&mut self, seed: u32) {
        self.tracer.set_seed(seed);
    }

    /// The traced and presented sizes.
    pub fn resolution(&self) -> Resolution {
        self.target.resolution()
    }

    /// Replaces the target by one of `resolution`; the image starts over.
    pub fn resize(&mut self, resolution: Resolution) {
        self.target = Target::new(&self.device, resolution);
        self.tracer.reset_accumulation();
    }

    /// Starts the reference image over, whatever the camera.
    pub fn restart(&mut self) {
        self.tracer.reset_accumulation();
    }

    /// Samples in the reference image so far.
    pub fn samples(&self) -> u32 {
        self.tracer.samples()
    }

    fn pass_count(&self, name: &str) -> u64 {
        self.profiler
            .timings()
            .pass(name)
            .map_or(0, |p| p.times.count())
    }

    /// The latest time of pass `name`, if one has come in since it had `before` times.
    fn new_time(&self, name: &str, before: u64) -> Option<f64> {
        let pass = self.profiler.timings().pass(name)?;
        (pass.times.count() > before)
            .then(|| pass.times.last())
            .flatten()
    }

    /// Adds one sample of the production reference kernel, without reading anything back, and
    /// returns its GPU time in milliseconds (`None` without timestamp queries).
    pub fn reference_sample(&mut self, camera: &Camera) -> Option<f64> {
        let before = self.pass_count("reference");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_reference(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &self.sun,
            &self.target,
        );
        self.profiler.resolve(&mut encoder);
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
        self.new_time("reference", before)
    }

    /// One dispatch of a temporary kernel `variant` (`variant.k` samples a pixel, the first of
    /// them sample `first_sample`), timed like a reference sample.
    pub fn variant_dispatch(
        &mut self,
        camera: &Camera,
        variant: Variant,
        first_sample: u32,
    ) -> Option<f64> {
        let name = variant.name();
        let before = self.pass_count(&name);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_variant(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &self.sun,
            &self.target,
            variant,
            first_sample,
        );
        self.profiler.resolve(&mut encoder);
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
        self.new_time(&name, before)
    }

    /// One primary visibility pass, timed (a cross-check of the cost of one traversal).
    pub fn primary_dispatch(&mut self, camera: &Camera) -> Option<f64> {
        let before = self.pass_count("primary");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_primary(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &self.sun,
            &self.target,
        );
        self.profiler.resolve(&mut encoder);
        self.queue.submit([encoder.finish()]);
        self.profiler.collect(&self.device, true);
        self.new_time("primary", before)
    }

    /// Submits one more reference sample and does not wait for it, except to keep at most
    /// [`IN_FLIGHT`] samples queued: the GPU runs flat out, as in a still frame that batches.
    /// Not timed. Call [`Offscreen::wait_idle`] before reading anything.
    pub fn reference_submit(&mut self, camera: &Camera) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.profiler.begin_frame();
        self.tracer.encode_reference(
            &self.device,
            &self.queue,
            &mut self.profiler,
            &mut encoder,
            camera,
            &self.sun,
            &self.target,
        );
        let index = self.queue.submit([encoder.finish()]);
        self.in_flight.push_back(index);
        while self.in_flight.len() > IN_FLIGHT {
            let oldest = self.in_flight.pop_front();
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: oldest,
                    timeout: None,
                })
                .expect("device poll failed");
        }
    }

    /// Waits for everything submitted so far.
    pub fn wait_idle(&mut self) {
        self.in_flight.clear();
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");
    }

    /// The reference's running mean, as the `f32` accumulation buffer holds it: RGB, rows from
    /// the top, at the traced size (the buffer's alpha is dropped).
    pub fn read_accumulation(&mut self) -> Vec<f32> {
        let (width, height) = self.target.resolution().internal;
        let size = u64::from(width) * u64::from(height) * 16;
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let bytes = crate::read_buffer(
            &self.device,
            &self.queue,
            encoder,
            self.target.accum(),
            size,
        );
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for texel in bytes.as_chunks::<16>().0 {
            for channel in texel[..12].as_chunks::<4>().0 {
                rgb.push(f32::from_le_bytes(*channel));
            }
        }
        rgb
    }

    /// What the window shows in reference mode: the traced image stretched to the output size by
    /// the window's own blit (`blit.wgsl`, a linear sampler), read back as 8-bit RGBA at the
    /// output resolution.
    pub fn blit_reference(&mut self) -> Image {
        let (width, height) = self.target.resolution().output;
        let device = &self.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(crate::shaders::BLIT.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(crate::tracer::IMAGE_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.target.lit().view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shown = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blit target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::tracer::IMAGE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = shown.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let descriptor = wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            };
            let mut pass = encoder.begin_render_pass(&descriptor); // untimed
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        let rgba = read_texture(
            &self.device,
            &self.queue,
            encoder,
            &shown,
            (width, height),
            4,
        );
        Image {
            width,
            height,
            rgba,
        }
    }
}
