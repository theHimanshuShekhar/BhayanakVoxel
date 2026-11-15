//! The still-view part of the convergence phase (#189, ADR 0005): what the Reference view does in
//! the first seconds after the camera stops, with the still-frame policy on (batched samples, the
//! 12 ms budget, 60 Hz), timed by wall clock from the stop, and scored against the same truths as
//! the single-sample curve.
//!
//! **What is run.** Frames at 60 Hz, as a window in Reference mode draws them
//! ([`Offscreen::draw_frame`]: the same code, [`render::tracer::Tracer::encode_frame`]): the
//! first two frames after the stop are Primary, then a batch of the Reference view each frame,
//! its size set by the budget from the GPU's own times, and the copy to the "window" that fades the
//! Reference view in. A frame that is over a 60 Hz frame waits for the next vsync, as a window
//! does, so the samples a second are the policy's and not those of a loop that never waits. The
//! clock is the wall clock from the first frame, the stop.
//!
//! **What it reports.** The frame's work time while the Reference view accumulates (encoding to the
//! GPU being done, the copy included) with its 99th percentile and how many frames were over
//! 16.7 ms; the batch's size and GPU time; the samples the run had at 0.1, 0.25, 0.5, 1 and 2 s; the
//! FLIP of the image at each of those against truth A; and the time from the stop to the FLIP
//! lines (0.05 and 0.02), from the single-sample curve's samples to each line and the
//! time the run got to that many.
//!
//! **How the images are made.** A batch of k samples is, bit for bit, k single samples
//! (`crates/render/tests/still_frames.rs` holds it), so the image at a mark is made after the
//! run by drawing that many single samples from the same seed: the run itself is not paused to
//! read 59 MB back at 1440p, which would be the only thing that moves its timing. The numbers of
//! samples and the times are the run's; the pictures are of those numbers.

use std::time::{Duration, Instant};

use render::{Camera, Offscreen};
use serde::Serialize;

use super::score::{self, How};
use super::{Capture, LINES, Line, MIN_LINE_OVER_FLOOR, RUN_SEED, draw};

/// The run goes on past its time until this many frames have taken a batch.
const MIN_BATCHES: usize = 3;
/// The frames per second of the window that is modelled.
pub const HZ: u32 = 60;
/// The times after the stop at which the image is scored, in seconds.
pub const MARKS_S: [f64; 5] = [0.1, 0.25, 0.5, 1.0, 2.0];
/// How long the run lasts when `--short` does not say: the last mark.
pub const RUN_S: f64 = 2.0;
/// The FLIP the acceptance of #189 asks for, within [`WITHIN_S`] of stopping.
pub const FLIP_TARGET: f64 = 0.05;
/// ... within this many seconds of stopping.
pub const WITHIN_S: f64 = 0.25;
/// A frame's work may take this long for a frame rate of [`HZ`], in milliseconds.
pub const FRAME_MS: f64 = 1000.0 / HZ as f64;

/// What to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub marks_s: Vec<f64>,
    pub seconds: f64,
}

impl Settings {
    /// The measurement.
    pub fn full() -> Self {
        Self {
            marks_s: MARKS_S.to_vec(),
            seconds: RUN_S,
        }
    }

    /// A smoke test: two marks, a third of a second.
    pub fn short() -> Self {
        Self {
            marks_s: MARKS_S[..2].to_vec(),
            seconds: 0.3,
        }
    }
}

/// One frame of the run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// When the frame began, from the stop, in milliseconds.
    pub began_ms: f64,
    /// The frame's work, in milliseconds: encoding to the GPU being done.
    pub work_ms: f64,
    /// The batch that the frame added, 0 for a Primary frame.
    pub batch: u32,
    /// Samples in the reference image when the frame was done.
    pub total: u32,
    /// The GPU's time for the Reference pass, the whole batch.
    pub gpu_ms: Option<f64>,
}

impl Frame {
    /// When the frame was done, from the stop, in milliseconds.
    fn done_ms(&self) -> f64 {
        self.began_ms + self.work_ms
    }
}

/// What the run drew, and the images at the marks.
pub struct Run {
    pub frames: Vec<Frame>,
    /// The mark (seconds after the stop) and the index in `captures` of the image of the samples
    /// the run had then. Marks at which the run had none are left out.
    pub marks: Vec<(f64, usize)>,
    /// The images, one for each distinct sample count, in order of samples.
    pub captures: Vec<Capture>,
}

/// Waits until `until` on the clock that began at `start`, sleeping most of the way and spinning
/// the last stretch, which a sleep alone would overshoot.
fn wait_until(start: Instant, until: Duration) {
    const SPIN: Duration = Duration::from_micros(600);
    loop {
        let now = start.elapsed();
        if now >= until {
            return;
        }
        let left = until - now;
        if left > SPIN {
            std::thread::sleep(left - SPIN);
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Whether on this GPU and driver two batches of 3 samples give the accumulation, bit for bit, that
/// 6 single samples do. It is what lets the images of the run be made after it with single
/// samples, and what the "same samples whatever the batching" claim of #189 rests on; a test holds
/// it on lavapipe, and this holds it on the machine that measures.
pub fn batching_is_exact(gpu: &mut Offscreen, camera: &Camera) -> bool {
    let bits = |a: Vec<f32>| a.into_iter().map(f32::to_bits).collect::<Vec<_>>();
    gpu.set_reference_seed(RUN_SEED);
    gpu.reference_batch(camera, 3);
    gpu.reference_batch(camera, 3);
    let batched = bits(gpu.read_accumulation());
    gpu.set_reference_seed(RUN_SEED);
    for _ in 0..6 {
        gpu.reference_sample(camera);
    }
    batched == bits(gpu.read_accumulation())
}

/// Frames of `camera` on `gpu` from the stop, as the window draws them at 60 Hz, for
/// `settings.seconds`; then the images at the marks.
///
/// `gpu` must be a [`render::Offscreen`] whose Reference view is traced at the size it presents.
pub fn run(gpu: &mut Offscreen, camera: &Camera, elsewhere: &Camera, settings: &Settings) -> Run {
    // A few frames of the camera moving, so that the first frames of the run are not the first
    // that the Primary and copy passes draw (a pipeline built on its first use would be the run's
    // first frame, and put the first batch late).
    for n in 0..3 {
        gpu.draw_frame(elsewhere, true, Duration::from_millis(16 * n));
    }
    // The run's own seed, the one the single-sample curve is drawn with: the same samples, so the
    // curve's FLIP at a count is the FLIP of this run at that count.
    gpu.set_reference_seed(RUN_SEED);
    let period = Duration::from_secs_f64(1.0 / f64::from(HZ));
    let limit = Duration::from_secs_f64(settings.seconds);
    let start = Instant::now();
    let mut next = Duration::ZERO;
    let mut frames = Vec::new();
    loop {
        wait_until(start, next);
        let began = start.elapsed();
        // Over the time, with a few batches drawn: a slow device (the smoke test's software
        // driver) would otherwise have none. Never over ten times the time.
        let drawn = frames.iter().filter(|f: &&Frame| f.batch > 0).count();
        if (began > limit && drawn >= MIN_BATCHES) || began > limit * 10 {
            break;
        }
        let drawn = gpu.draw_frame(camera, true, began);
        let done = start.elapsed();
        frames.push(Frame {
            began_ms: began.as_secs_f64() * 1e3,
            work_ms: drawn.wall.as_secs_f64() * 1e3,
            batch: drawn.plan.samples(),
            total: drawn.samples,
            gpu_ms: drawn.reference_ms,
        });
        // The next vsync at or after the frame being done: a frame that took longer than 16.7 ms
        // misses one, and the next begins at the one after.
        let vsyncs = (done.as_secs_f64() / period.as_secs_f64()).ceil().max(1.0);
        next = Duration::from_secs_f64(vsyncs * period.as_secs_f64());
    }

    // The pictures: the samples the run had at each mark, drawn one at a time from the same seed.
    let counts: Vec<(f64, u32)> = settings
        .marks_s
        .iter()
        .map(|&s| (s, samples_at(&frames, s)))
        .filter(|&(_, n)| n > 0)
        .collect();
    let mut stops: Vec<u32> = counts.iter().map(|&(_, n)| n).collect();
    stops.sort_unstable();
    stops.dedup();
    let last = stops.last().copied().unwrap_or(0);
    let captures = draw(gpu, camera, RUN_SEED, &stops, last);
    let marks = counts
        .iter()
        .map(|&(s, n)| {
            let at = captures.iter().position(|c| c.samples == n);
            (s, at.expect("a capture for each count"))
        })
        .collect();
    Run {
        frames,
        marks,
        captures,
    }
}

/// The samples in the reference image `seconds` after the stop: the total of the last frame that
/// was done by then.
pub fn samples_at(frames: &[Frame], seconds: f64) -> u32 {
    frames
        .iter()
        .filter(|f| f.done_ms() <= seconds * 1e3)
        .map(|f| f.total)
        .max()
        .unwrap_or(0)
}

/// Seconds from the stop to the first frame that was done with at least `samples` samples in the
/// image; none if the run did not get that far.
pub fn seconds_to(frames: &[Frame], samples: f64) -> Option<f64> {
    frames
        .iter()
        .find(|f| f64::from(f.total) >= samples)
        .map(|f| f.done_ms() / 1e3)
}

/// Mean, 95th and 99th percentile (nearest rank) and maximum of a series, in milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tail {
    pub samples: usize,
    pub mean_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

impl Tail {
    pub fn of(times: &[f64]) -> Self {
        let mut sorted = times.to_vec();
        sorted.sort_by(f64::total_cmp);
        let rank = |percent: usize| {
            (!sorted.is_empty()).then(|| sorted[(sorted.len() * percent).div_ceil(100) - 1])
        };
        Self {
            samples: sorted.len(),
            mean_ms: (!sorted.is_empty()).then(|| sorted.iter().sum::<f64>() / sorted.len() as f64),
            p95_ms: rank(95),
            p99_ms: rank(99),
            max_ms: sorted.last().copied(),
        }
    }
}

/// The still-view block of a view's report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StillReport {
    /// Whether two batches of 3 samples gave, on this GPU, the accumulation of 6 single samples bit
    /// for bit.
    pub batching_bit_exact: bool,
    /// The frame rate that was modelled: a frame over its time waits for the next vsync.
    pub hz: u32,
    /// The policy's constants, as the renderer has them (`render::batch`, `render::still`).
    pub budget_ms: f64,
    pub max_batch_samples: u32,
    pub still_frames: u32,
    pub fade_ms: u64,
    /// How long the run was, from the stop, in seconds.
    pub seconds: f64,
    /// Frames drawn, and of them the ones that took a batch of the Reference view.
    pub frames: u32,
    pub accumulating_frames: u32,
    /// The work of a frame that took a batch, in milliseconds (encoding to the GPU being done, the
    /// copy to the window included): the acceptance's frame time, whose 99th percentile is to be
    /// at most 16.7 ms.
    pub frame: Tail,
    /// How many of those frames took longer than a frame at [`HZ`] (a missed vsync).
    pub frames_over: u32,
    /// The GPU's time for a batch, in milliseconds; empty without timers.
    pub gpu_batch: Tail,
    /// The size of the first batch (it is 1), of the last, and the largest.
    pub batch_first: u32,
    pub batch_last: u32,
    pub batch_max: u32,
    /// Samples a second over the frames that took a batch, from the first one's start to the last
    /// one's end.
    pub samples_per_s: Option<f64>,
    /// The run at each mark.
    pub marks: Vec<Mark>,
    /// #189's acceptance, for whoever reads the report: FLIP at most [`FLIP_TARGET`] at the mark
    /// [`WITHIN_S`] after the stop (null where FLIP was not run or the mark is missing), and the
    /// 99th percentile of the work of a frame that took a batch at most [`FRAME_MS`] (null where
    /// no frame did). The 4090 is where they count.
    pub flip_within_target: Option<bool>,
    pub frame_p99_within_target: Option<bool>,
    /// The time from the stop to each FLIP line.
    pub to_line: Vec<ToLine>,
}

/// The run's image at a mark, scored against truth A.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Mark {
    pub seconds: f64,
    pub samples: u32,
    /// Null where FLIP was not run (`--short`).
    pub flip_mean: Option<f64>,
    pub flip_p99: Option<f64>,
    pub mse_linear: f64,
}

/// When the run got to a FLIP line.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToLine {
    pub flip: f64,
    /// The samples the single-sample curve needs to get to the line (null if it does not).
    pub samples: Option<f64>,
    pub how: How,
    /// Seconds from the stop until the run's image had that many samples; null if the curve has no
    /// number or the run ended first.
    pub seconds: Option<f64>,
    /// Whether the line counts in this view (it is at least [`MIN_LINE_OVER_FLOOR`] times the
    /// floor); a line that does not count is shown and not used.
    pub counts: bool,
}

/// The block from a run, its scores and the single-sample curve's lines.
///
/// `scores` is the FLIP mean and 99th percentile of each of `run.captures` against truth A (all
/// `None` where FLIP was not run), and `mse` the squared error of its accumulation, in the same
/// order.
pub fn report(
    batching_bit_exact: bool,
    settings: &Settings,
    run: &Run,
    scores: &[Option<score::Flip>],
    mse: &[f64],
    lines: &[Line],
) -> StillReport {
    let accumulating: Vec<&Frame> = run.frames.iter().filter(|f| f.batch > 0).collect();
    let work: Vec<f64> = accumulating.iter().map(|f| f.work_ms).collect();
    let gpu: Vec<f64> = accumulating.iter().filter_map(|f| f.gpu_ms).collect();
    let samples_per_s = accumulating
        .first()
        .zip(accumulating.last())
        .and_then(|(a, b)| {
            let seconds = (b.done_ms() - a.began_ms) / 1e3;
            (seconds > 0.0)
                .then(|| f64::from(accumulating.iter().map(|f| f.batch).sum::<u32>()) / seconds)
        });
    StillReport {
        batching_bit_exact,
        hz: HZ,
        budget_ms: render::batch::BATCH_BUDGET_MS,
        max_batch_samples: render::batch::MAX_BATCH_SAMPLES,
        still_frames: render::still::STILL_FRAMES,
        fade_ms: render::still::FADE.as_millis() as u64,
        seconds: settings.seconds,
        frames: run.frames.len() as u32,
        accumulating_frames: accumulating.len() as u32,
        frame: Tail::of(&work),
        frames_over: work.iter().filter(|&&ms| ms > FRAME_MS).count() as u32,
        gpu_batch: Tail::of(&gpu),
        batch_first: accumulating.first().map_or(0, |f| f.batch),
        batch_last: accumulating.last().map_or(0, |f| f.batch),
        batch_max: accumulating.iter().map(|f| f.batch).max().unwrap_or(0),
        samples_per_s,
        flip_within_target: run
            .marks
            .iter()
            .find(|&&(seconds, _)| (seconds - WITHIN_S).abs() < 1e-9)
            .and_then(|&(_, at)| scores.get(at).copied().flatten())
            .map(|f| f.mean <= FLIP_TARGET),
        frame_p99_within_target: Tail::of(&work).p99_ms.map(|ms| ms <= FRAME_MS),
        marks: run
            .marks
            .iter()
            .map(|&(seconds, at)| Mark {
                seconds,
                samples: run.captures[at].samples,
                flip_mean: scores.get(at).copied().flatten().map(|f| f.mean),
                flip_p99: scores.get(at).copied().flatten().map(|f| f.p99),
                mse_linear: mse.get(at).copied().unwrap_or(f64::NAN),
            })
            .collect(),
        to_line: LINES
            .iter()
            .filter_map(|&flip| lines.iter().find(|l| l.flip == flip))
            .map(|line| ToLine {
                flip: line.flip,
                samples: line.samples,
                how: line.how,
                seconds: line.samples.and_then(|n| seconds_to(&run.frames, n.ceil())),
                counts: line.counts,
            })
            .collect(),
    }
}

/// Prints the block for whoever watches the run.
pub fn print(name: &str, s: &StillReport) {
    let num =
        |v: Option<f64>, digits: usize| v.map_or("n/a".to_string(), |v| format!("{v:.digits$}"));
    eprintln!(
        "  still view, {name}, {} Hz, batching bit-exact on this GPU: {}: {} frames, {} took a batch (first {}, last {}, most {}); frame work mean {} p95 {} p99 {} max {} ms, {} over {:.1} ms; GPU batch mean {} p95 {} ms; {} samples/s",
        s.hz,
        s.batching_bit_exact,
        s.frames,
        s.accumulating_frames,
        s.batch_first,
        s.batch_last,
        s.batch_max,
        num(s.frame.mean_ms, 2),
        num(s.frame.p95_ms, 2),
        num(s.frame.p99_ms, 2),
        num(s.frame.max_ms, 2),
        s.frames_over,
        FRAME_MS,
        num(s.gpu_batch.mean_ms, 2),
        num(s.gpu_batch.p95_ms, 2),
        num(s.samples_per_s, 0),
    );
    let met = |v: Option<bool>| match v {
        Some(true) => "met",
        Some(false) => "NOT met",
        None => "n/a",
    };
    eprintln!(
        "    acceptance: FLIP <= {FLIP_TARGET} within {WITHIN_S} s {}; frame p99 <= {FRAME_MS:.1} ms {}",
        met(s.flip_within_target),
        met(s.frame_p99_within_target)
    );
    for m in &s.marks {
        eprintln!(
            "    {:>5.2} s after the stop: {:>5} samples, FLIP {} (p99 {}), mse {:.3e}",
            m.seconds,
            m.samples,
            num(m.flip_mean, 4),
            num(m.flip_p99, 4),
            m.mse_linear
        );
    }
    for l in &s.to_line {
        let note = if l.counts {
            String::new()
        } else {
            format!(" (does not count here: under {MIN_LINE_OVER_FLOOR} times the floor)")
        };
        eprintln!(
            "    FLIP <= {}: after {} samples, {} s from the stop{note}",
            l.flip,
            num(l.samples, 1),
            num(l.seconds, 3)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(began_ms: f64, work_ms: f64, batch: u32, total: u32) -> Frame {
        Frame {
            began_ms,
            work_ms,
            batch,
            total,
            gpu_ms: Some(work_ms - 0.5),
        }
    }

    /// Two Primary frames, then batches of 1, 2, 2, 3 samples.
    fn frames() -> Vec<Frame> {
        vec![
            frame(0.0, 3.0, 0, 0),
            frame(16.7, 3.0, 0, 0),
            frame(33.3, 5.0, 1, 1),
            frame(50.0, 9.0, 2, 3),
            frame(66.7, 9.0, 2, 5),
            frame(83.3, 12.5, 3, 8),
        ]
    }

    #[test]
    fn the_samples_at_a_time_are_those_of_the_last_frame_done_by_then() {
        let frames = frames();
        assert_eq!(samples_at(&frames, 0.030), 0);
        // The first batch is done at 38.3 ms.
        assert_eq!(samples_at(&frames, 0.040), 1);
        // The second is done at 59 ms.
        assert_eq!(samples_at(&frames, 0.0589), 1);
        assert_eq!(samples_at(&frames, 0.060), 3);
        assert_eq!(samples_at(&frames, 10.0), 8);
    }

    #[test]
    fn the_time_to_a_count_is_when_the_frame_that_got_there_was_done() {
        let frames = frames();
        let near = |got: Option<f64>, want: f64| (got.unwrap() - want).abs() < 1e-9;
        assert!(near(seconds_to(&frames, 1.0), 0.0383));
        assert!(near(seconds_to(&frames, 2.5), 0.059));
        // Past the end of the run: not reached.
        assert_eq!(seconds_to(&frames, 9.0), None);
    }

    #[test]
    fn the_tail_is_the_nearest_rank() {
        let times: Vec<f64> = (1..=100).map(f64::from).collect();
        let tail = Tail::of(&times);
        assert_eq!(tail.p95_ms, Some(95.0));
        assert_eq!(tail.p99_ms, Some(99.0));
        assert_eq!(tail.max_ms, Some(100.0));
        assert_eq!(Tail::of(&[]).p99_ms, None);
    }

    #[test]
    fn the_report_counts_the_frames_that_took_a_batch_and_those_over_a_frame() {
        let mut frames = frames();
        frames.push(frame(100.0, 20.0, 3, 11));
        let run = Run {
            frames,
            marks: Vec::new(),
            captures: Vec::new(),
        };
        let report = report(true, &Settings::short(), &run, &[], &[], &[]);
        assert_eq!((report.frames, report.accumulating_frames), (7, 5));
        assert_eq!(report.frames_over, 1);
        assert_eq!(report.frame.max_ms, Some(20.0));
        assert_eq!(
            (report.batch_first, report.batch_last, report.batch_max),
            (1, 3, 3)
        );
        assert_eq!(report.budget_ms, 12.0);
        // 11 samples in the 86.7 ms from the first batch's start to the last one's end.
        assert!(
            report
                .samples_per_s
                .is_some_and(|s| (s - 11.0 / 0.0867).abs() < 1.0)
        );
    }
}
