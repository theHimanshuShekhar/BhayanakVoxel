//! The temporal upscaler (TAAU): the CPU side of `shaders/upscale.wgsl`.
//!
//! Primary visibility traces at the internal resolution, with the sample of each pixel moved to
//! a different point in it every frame (jitter). The upscale pass then builds the output image
//! from the current samples and a history of the output, reprojected to where the camera has
//! moved to and clamped to the colours of the new samples around it. This is the decision of #10
//! (1280x720 traced for 1440p), and the pass that later stages (shadows, GI, denoise) plug in
//! next to.
//!
//! What lives here is what does not need a GPU: how big the two images are, which point of the
//! pixel each frame traces, and when the history is valid.

use bytemuck::{Pod, Zeroable};

use crate::camera::{Camera, CameraUniform};
use crate::debug_view::DebugView;
use crate::profiler::Profiler;
use crate::shaders;
use crate::tracer::{
    HISTORY_FORMAT, IMAGE_FORMAT, Target, buffer_binding, layout_entry, storage_texture,
};

/// How big the traced image and the presented one are. They are independent: the upscaler
/// resamples one to the other, whatever their sizes (their aspect ratios should agree). The
/// reference path tracer traces at the internal size and needs no upscaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    /// What primary visibility traces, and the G-buffer's size.
    pub internal: (u32, u32),
    /// What is presented.
    pub output: (u32, u32),
}

/// The share of the output's width and height that is traced: half per axis, 1280x720 for 1440p
/// (#10).
pub const DEFAULT_SCALE: f32 = 0.5;

impl Resolution {
    /// Traced at the size it is presented at.
    pub fn native(width: u32, height: u32) -> Self {
        Self {
            internal: (width, height),
            output: (width, height),
        }
    }

    /// Presented at `output`, traced at `scale` of it in each direction (at least one pixel).
    pub fn scaled(output: (u32, u32), scale: f32) -> Self {
        let scaled = |n: u32| ((n as f32 * scale).round() as u32).max(1);
        Self {
            internal: (scaled(output.0), scaled(output.1)),
            output,
        }
    }
}

/// Samples before the sequence of jitter offsets repeats.
const JITTER_PERIOD: u32 = 16;

/// Index `index` of the Halton sequence in `base`: a number in `0..1` that fills the interval
/// evenly however many have been taken.
fn halton(mut index: u32, base: u32) -> f32 {
    let (mut scale, mut value) = (1.0, 0.0);
    while index > 0 {
        scale /= base as f32;
        value += scale * (index % base) as f32;
        index /= base;
    }
    value
}

/// Where in its pixel the sample of frame `frame` (counting from the last reset of the history)
/// is traced, from (0, 0) at the top left to (1, 1). Frame 0, which has no history to fill in
/// the rest, is traced at the centre, so it is plain primary visibility. The others follow the
/// Halton sequence (2, 3), which repeats every [`JITTER_PERIOD`] frames.
pub fn jitter(frame: u32) -> [f32; 2] {
    if frame == 0 {
        return [0.5; 2];
    }
    let index = (frame - 1) % JITTER_PERIOD + 1;
    [halton(index, 2), halton(index, 3)]
}

/// `Upscale` in `upscale.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct UpscaleUniform {
    camera: CameraUniform,
    /// The camera of the frame before: the one the history is of.
    previous: CameraUniform,
    /// How far this camera is from that one, in Voxels; the fourth lane is unused. The CPU takes
    /// the difference in f64, so it is exact wherever on the Planet the camera is.
    shift: [f32; 4],
    /// Where in its pixel the traced sample is, then two unused.
    jitter: [f32; 4],
    /// 1 if there is a history to use, 0 on the first frame; then the debug view; then two
    /// unused.
    params: [u32; 4],
}

/// What the upscaler remembers from frame to frame, and decides about the next.
#[derive(Debug, Default)]
pub(crate) struct History {
    /// What the history is of, if it is of anything.
    resolution: Option<Resolution>,
    /// The camera of the last frame in it.
    previous: Option<Camera>,
    /// Frames in it, which also picks the jitter of the next.
    frames: u32,
    /// Which of the target's two history images holds the last frame.
    latest: usize,
}

/// One frame's share of the upscaler's inputs.
pub(crate) struct Frame {
    uniform: UpscaleUniform,
    /// The history image to read, and the one to write: 0 or 1.
    read: usize,
    write: usize,
}

impl Frame {
    /// Where in its pixel the frame is traced.
    pub fn jitter(&self) -> [f32; 2] {
        [self.uniform.jitter[0], self.uniform.jitter[1]]
    }
}

impl History {
    /// Starts a frame at `resolution` from `camera`. The history starts over if the sizes
    /// changed, or if `temporal` is false: a debug view shows what the tracer drew, and its
    /// colours are not a history to carry on from.
    pub fn begin(&mut self, resolution: Resolution, camera: &Camera, view: DebugView) -> Frame {
        let temporal = matches!(view, DebugView::Lit | DebugView::HistoryLength);
        if !temporal || self.resolution != Some(resolution) {
            *self = Self::default();
        }
        let previous = self.previous.unwrap_or(*camera);
        let shift: [f32; 3] =
            std::array::from_fn(|a| (camera.position[a] - previous.position[a]) as f32);
        let [x, y] = jitter(self.frames);
        let frame = Frame {
            uniform: UpscaleUniform {
                camera: camera.uniform(),
                previous: previous.uniform(),
                shift: [shift[0], shift[1], shift[2], 0.0],
                jitter: [x, y, 0.0, 0.0],
                params: [u32::from(self.previous.is_some()), view.index(), 0, 0],
            },
            read: self.latest,
            write: 1 - self.latest,
        };
        if temporal {
            self.resolution = Some(resolution);
            self.previous = Some(*camera);
            self.frames = self.frames.saturating_add(1);
            self.latest = frame.write;
        }
        frame
    }

    /// Forgets the history: the next frame is the first.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The upscale pass: the compute pipeline of `upscale.wgsl` over the traced image, the G-buffer's
/// depth and the history.
pub(crate) struct UpscalePass {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
}

impl UpscalePass {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture = |binding| {
            layout_entry(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("upscale"),
            entries: &[
                layout_entry(0, buffer_binding(wgpu::BufferBindingType::Uniform)),
                texture(1),
                texture(2),
                texture(3),
                layout_entry(4, storage_texture(HISTORY_FORMAT)),
                layout_entry(5, storage_texture(IMAGE_FORMAT)),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("upscale"),
            source: wgpu::ShaderSource::Wgsl(shaders::UPSCALE.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("upscale"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("upscale"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("upscale_pass"),
            compilation_options: Default::default(),
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("upscale"),
            size: size_of::<UpscaleUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            uniform,
        }
    }

    /// Records the pass for `frame`: the lit image and G-buffer depth of `target` and the
    /// history in, the image and the next history out. Timed by `profiler` as `upscale`.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        profiler: &mut Profiler,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Frame,
        target: &Target,
    ) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&frame.uniform));
        fn view(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            }
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("upscale"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                view(1, &target.lit().view),
                view(2, &target.depth().view),
                view(3, &target.history(frame.read).view),
                view(4, &target.history(frame.write).view),
                view(5, &target.image().view),
            ],
        });
        let (width, height) = target.resolution().output;
        let mut pass = profiler.compute_pass(encoder, "upscale");
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RES: Resolution = Resolution {
        internal: (4, 4),
        output: (8, 8),
    };

    fn camera() -> Camera {
        Camera::looking_at([0.0, 0.0, 5.0], [0.0, 0.0, 0.0])
    }

    #[test]
    fn the_first_frame_is_traced_at_the_centre_of_its_pixels() {
        assert_eq!(jitter(0), [0.5, 0.5]);
    }

    #[test]
    fn the_jitter_stays_inside_the_pixel_and_covers_it_evenly() {
        let offsets: Vec<[f32; 2]> = (1..=JITTER_PERIOD).map(jitter).collect();
        assert!(
            offsets
                .iter()
                .all(|o| o.iter().all(|c| (0.0..1.0).contains(c)))
        );
        for axis in 0..2 {
            let mean = offsets.iter().map(|o| o[axis]).sum::<f32>() / JITTER_PERIOD as f32;
            assert!((mean - 0.5).abs() < 0.05, "axis {axis}: mean {mean}");
        }
        // No two frames of a period sample the same point.
        for (i, a) in offsets.iter().enumerate() {
            assert!(offsets[i + 1..].iter().all(|b| a != b));
        }
    }

    #[test]
    fn the_jitter_repeats_after_a_period_without_going_back_to_the_centre() {
        assert_eq!(jitter(1), jitter(1 + JITTER_PERIOD));
        assert_eq!(jitter(JITTER_PERIOD), jitter(2 * JITTER_PERIOD));
        assert_ne!(jitter(JITTER_PERIOD + 1), jitter(0));
    }

    #[test]
    fn scaling_by_half_gives_the_internal_resolution_behind_1440p() {
        let resolution = Resolution::scaled((2560, 1440), DEFAULT_SCALE);
        assert_eq!(resolution.internal, (1280, 720));
        assert_eq!(resolution.output, (2560, 1440));
        assert_eq!(Resolution::scaled((3, 1), 0.1).internal, (1, 1));
        assert_eq!(Resolution::native(64, 36).internal, (64, 36));
    }

    #[test]
    fn the_first_frame_has_no_history_and_the_next_has() {
        let mut history = History::default();
        let first = history.begin(RES, &camera(), DebugView::Lit);
        assert_eq!(first.uniform.params[0], 0);
        assert_eq!(first.uniform.jitter[..2], [0.5, 0.5]);
        let second = history.begin(RES, &camera(), DebugView::Lit);
        assert_eq!(second.uniform.params[0], 1);
        assert_eq!(second.uniform.jitter[..2], jitter(1));
    }

    #[test]
    fn the_two_history_images_take_turns() {
        let mut history = History::default();
        let frames: Vec<_> = (0..4)
            .map(|_| history.begin(RES, &camera(), DebugView::Lit))
            .collect();
        let reads: Vec<usize> = frames.iter().map(|f| f.read).collect();
        let writes: Vec<usize> = frames.iter().map(|f| f.write).collect();
        assert_eq!(reads, [0, 1, 0, 1]);
        assert_eq!(writes, [1, 0, 1, 0]);
    }

    #[test]
    fn the_previous_camera_is_the_one_of_the_frame_before_and_the_shift_is_how_far_this_one_moved()
    {
        let mut history = History::default();
        let mut moved = camera();
        history.begin(RES, &moved, DebugView::Lit);
        moved.position = [2.0, -1.0, 5.5];
        let frame = history.begin(RES, &moved, DebugView::Lit);
        assert_eq!(frame.uniform.shift, [2.0, -1.0, 0.5, 0.0]);
        assert_eq!(frame.uniform.previous, camera().uniform());
    }

    #[test]
    fn a_new_size_starts_the_history_over() {
        let mut history = History::default();
        history.begin(RES, &camera(), DebugView::Lit);
        let other = Resolution::native(8, 8);
        let frame = history.begin(other, &camera(), DebugView::Lit);
        assert_eq!(frame.uniform.params[0], 0);
        assert_eq!(frame.uniform.jitter[..2], [0.5, 0.5]);
    }

    #[test]
    fn a_debug_view_has_no_history_and_leaves_none_behind() {
        let mut history = History::default();
        history.begin(RES, &camera(), DebugView::Lit);
        history.begin(RES, &camera(), DebugView::Lit);
        let normals = history.begin(RES, &camera(), DebugView::Normals);
        assert_eq!(normals.uniform.params[0], 0);
        assert_eq!(normals.uniform.jitter[..2], [0.5, 0.5]);
        // Back to the lit image: it starts afresh rather than from the debug view's colours.
        let lit = history.begin(RES, &camera(), DebugView::Lit);
        assert_eq!(lit.uniform.params[0], 0);
    }

    #[test]
    fn the_history_length_view_keeps_the_history_it_shows() {
        let mut history = History::default();
        history.begin(RES, &camera(), DebugView::Lit);
        let frame = history.begin(RES, &camera(), DebugView::HistoryLength);
        assert_eq!(frame.uniform.params[0], 1);
        assert_eq!(frame.uniform.params[1], DebugView::HistoryLength.index());
    }
}
