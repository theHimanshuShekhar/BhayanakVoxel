//! GPU timers: a timestamp at the start and end of every pass, resolved and read back without
//! stalling the frame.
//!
//! Every pass of the renderer is begun through [`Profiler::compute_pass`] or
//! [`Profiler::render_pass`], which register it by name and give it its timestamps, so a pass
//! added later is timed without any further wiring. The results go into [`Timings`].
//!
//! A frame is: [`begin_frame`](Profiler::begin_frame), the passes, [`resolve`](Profiler::resolve)
//! before the encoder is finished, then [`collect`](Profiler::collect) after it is submitted.
//! The readback buffers form a ring, and a frame's times arrive a frame or two after it, once its
//! buffer has been mapped; the frame that finds the whole ring in use is not timed.
//!
//! Where the device has no timestamp queries, passes are created as usual and still register,
//! but have no times: [`Profiler::supported`] says which.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use crate::timing::{Slot, Timings, frame_times};

/// Timestamp queries are all this needs: they are written by the pass descriptors, not from
/// inside passes or encoders.
pub const FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY;

/// Passes timed in one frame; further passes run untimed.
const MAX_PASSES: usize = 16;
const MAX_QUERIES: u32 = MAX_PASSES as u32 * 2;
const QUERY_BYTES: u64 = size_of::<u64>() as u64;

/// Frames that can be waiting for their buffers to be mapped.
const RING: usize = 3;

const PENDING: u8 = 0;
const MAPPED: u8 = 1;
const FAILED: u8 = 2;

/// A frame that has been submitted and whose timestamps are on their way to the CPU.
struct InFlight {
    /// Where each pass's timestamps are.
    slots: Vec<Slot>,
    queries: u32,
    /// Set by the callback of `map_async`.
    state: Arc<AtomicU8>,
    /// Whether `map_async` has been called yet.
    mapping: bool,
    /// Counts frames, so that the oldest is read first.
    sequence: u64,
}

struct Readback {
    buffer: wgpu::Buffer,
    in_flight: Option<InFlight>,
}

/// What exists only when the device can time passes.
struct Queries {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    ring: Vec<Readback>,
    /// Nanoseconds per timestamp tick.
    period_ns: f32,
}

impl Queries {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let size = u64::from(MAX_QUERIES) * QUERY_BYTES;
        Self {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("pass timers"),
                ty: wgpu::QueryType::Timestamp,
                count: MAX_QUERIES,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pass timers resolved"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            ring: (0..RING)
                .map(|_| Readback {
                    buffer: crate::staging_buffer(device, size),
                    in_flight: None,
                })
                .collect(),
            period_ns: queue.get_timestamp_period(),
        }
    }
}

/// Hands out timestamps to passes and collects what they measured.
pub struct Profiler {
    queries: Option<Queries>,
    timings: Timings,
    /// The passes timed so far this frame.
    slots: Vec<Slot>,
    /// The ring slot that [`resolve`](Self::resolve) copied this frame to, until `collect` maps it.
    resolved_to: Option<usize>,
    sequence: u64,
    last_frame: Option<Instant>,
}

impl Profiler {
    /// A profiler for `device`, which times passes if it was created with [`FEATURES`].
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let queries = device
            .features()
            .contains(FEATURES)
            .then(|| Queries::new(device, queue));
        Self {
            queries,
            timings: Timings::default(),
            slots: Vec::new(),
            resolved_to: None,
            sequence: 0,
            last_frame: None,
        }
    }

    /// Whether passes are timed. If not, every time is missing and the overlay shows "n/a".
    pub fn supported(&self) -> bool {
        self.queries.is_some()
    }

    /// The times so far. They trail the frames being drawn by a frame or two.
    pub fn timings(&self) -> &Timings {
        &self.timings
    }

    /// Keeps every time from now on, for a report over a whole run (see
    /// [`Series::history`](crate::timing::Series::history)).
    pub fn set_keep_history(&mut self, keep: bool) {
        self.timings.set_keep_history(keep);
    }

    /// Starts a frame, whose passes are begun through this profiler until
    /// [`resolve`](Self::resolve). It also times the CPU side of the frame, from the previous
    /// call.
    pub fn begin_frame(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.timings
                .record_wall(now.duration_since(last).as_secs_f64() * 1e3);
        }
        self.slots.clear();
    }

    /// The timestamps to put in a pass called `name`. The name registers the pass the first time
    /// it is used.
    fn timestamps(&mut self, name: &str) -> Option<[u32; 2]> {
        let pass = self.timings.register(name);
        self.queries.as_ref()?;
        if self.slots.len() >= MAX_PASSES {
            return None;
        }
        let first = self.slots.len() as u32 * 2;
        self.slots.push(Slot { pass, first });
        Some([first, first + 1])
    }

    /// Begins a compute pass called `name`, timed.
    pub fn compute_pass<'e>(
        &mut self,
        encoder: &'e mut wgpu::CommandEncoder,
        name: &str,
    ) -> wgpu::ComputePass<'e> {
        let writes = self.timestamps(name);
        let timestamp_writes = self.queries.as_ref().zip(writes).map(|(q, [begin, end])| {
            wgpu::ComputePassTimestampWrites {
                query_set: &q.set,
                beginning_of_pass_write_index: Some(begin),
                end_of_pass_write_index: Some(end),
            }
        });
        encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(name),
            timestamp_writes,
        })
    }

    /// Begins a render pass called `name` into `color_attachments`, timed.
    pub fn render_pass<'e>(
        &mut self,
        encoder: &'e mut wgpu::CommandEncoder,
        name: &str,
        color_attachments: &[Option<wgpu::RenderPassColorAttachment<'_>>],
    ) -> wgpu::RenderPass<'e> {
        let writes = self.timestamps(name);
        let timestamp_writes = self.queries.as_ref().zip(writes).map(|(q, [begin, end])| {
            wgpu::RenderPassTimestampWrites {
                query_set: &q.set,
                beginning_of_pass_write_index: Some(begin),
                end_of_pass_write_index: Some(end),
            }
        });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(name),
            color_attachments,
            timestamp_writes,
            ..Default::default()
        })
    }

    /// Records the copy of this frame's timestamps to a readback buffer, after the last pass.
    /// If every buffer of the ring is still waiting to be read, the frame is not timed.
    pub fn resolve(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.resolved_to = None;
        // Whatever happens, the next frame starts with no passes.
        let slots = std::mem::take(&mut self.slots);
        let Some(q) = &mut self.queries else { return };
        if slots.is_empty() {
            return;
        }
        let Some(index) = q.ring.iter().position(|r| r.in_flight.is_none()) else {
            return;
        };
        let queries = slots.len() as u32 * 2;
        let size = u64::from(queries) * QUERY_BYTES;
        encoder.resolve_query_set(&q.set, 0..queries, &q.resolve, 0);
        encoder.copy_buffer_to_buffer(&q.resolve, 0, &q.ring[index].buffer, 0, size);
        q.ring[index].in_flight = Some(InFlight {
            slots,
            queries,
            state: Arc::new(AtomicU8::new(PENDING)),
            mapping: false,
            sequence: self.sequence,
        });
        self.sequence += 1;
        self.resolved_to = Some(index);
    }

    /// After the frame's commands are submitted: starts reading back this frame's timestamps, and
    /// takes in those of earlier frames that have arrived. With `wait`, this blocks until this
    /// frame's are in (for tests and tools that render one frame at a time).
    pub fn collect(&mut self, device: &wgpu::Device, wait: bool) {
        let Some(q) = &mut self.queries else { return };
        if let Some(index) = self.resolved_to.take() {
            let readback = &mut q.ring[index];
            let flight = readback
                .in_flight
                .as_mut()
                .expect("resolve left a frame in flight");
            let state = Arc::clone(&flight.state);
            flight.mapping = true;
            readback
                .buffer
                .slice(..u64::from(flight.queries) * QUERY_BYTES)
                .map_async(wgpu::MapMode::Read, move |result| {
                    state.store(
                        if result.is_ok() { MAPPED } else { FAILED },
                        Ordering::Release,
                    );
                });
        }
        let poll = if wait {
            wgpu::PollType::wait_indefinitely()
        } else {
            wgpu::PollType::Poll
        };
        // Map callbacks run from here. A failed poll only means the times come later.
        let _ = device.poll(poll);

        // Oldest first, so that times are recorded in frame order.
        let mut ready: Vec<usize> = (0..q.ring.len())
            .filter(|&i| {
                q.ring[i]
                    .in_flight
                    .as_ref()
                    .is_some_and(|f| f.mapping && f.state.load(Ordering::Acquire) != PENDING)
            })
            .collect();
        ready.sort_by_key(|&i| q.ring[i].in_flight.as_ref().map(|f| f.sequence));
        for i in ready {
            let readback = &mut q.ring[i];
            let flight = readback
                .in_flight
                .take()
                .expect("filtered to frames in flight");
            if flight.state.load(Ordering::Acquire) != MAPPED {
                continue;
            }
            let range = readback
                .buffer
                .slice(..u64::from(flight.queries) * QUERY_BYTES)
                .get_mapped_range()
                .expect("a mapped buffer has a range");
            let stamps: Vec<u64> = bytemuck::cast_slice(&range).to_vec();
            drop(range);
            readback.buffer.unmap();
            self.timings
                .record_frame(&frame_times(&stamps, &flight.slots, q.period_ns));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Every `.rs` file under `dir`, recursively.
    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Passes are only begun through the profiler, so that a new one is timed without anyone
    /// remembering to. A pass that is meant to be untimed says so with `// untimed` on its line
    /// (the `trace_rays` test probe, which has its own submit). The profiler itself, and the
    /// shader unit tests in `sampling_tests.rs`, are exempt.
    #[test]
    fn only_the_profiler_begins_passes() {
        let mut files = Vec::new();
        sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(files.len() > 5, "found only {files:?}");
        for file in files {
            let name = file.file_name().unwrap().to_str().unwrap();
            if ["profiler.rs", "sampling_tests.rs"].contains(&name) {
                continue;
            }
            for (number, line) in std::fs::read_to_string(&file).unwrap().lines().enumerate() {
                let begins =
                    line.contains(".begin_compute_pass(") || line.contains(".begin_render_pass(");
                assert!(
                    !begins || line.contains("// untimed"),
                    "{}:{} begins a pass by hand; use the Profiler",
                    file.display(),
                    number + 1
                );
            }
        }
    }
}
