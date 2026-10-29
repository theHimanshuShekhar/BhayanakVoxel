//! What GPU pass timers report: the rolling averages, maxima and totals that the overlay shows and
//! that the bench harness reads. This is the bookkeeping only, with no GPU in it: the
//! [`Profiler`](crate::Profiler) turns timestamp queries into the numbers that go in here.

use std::collections::VecDeque;

/// Frames that the rolling average and maximum span.
pub const WINDOW: usize = 120;

/// Milliseconds between two GPU timestamps, given the queue's `period_ns`, the nanoseconds per
/// tick (`Queue::get_timestamp_period`). `None` if the timestamps cannot be a real interval:
/// the end before the beginning (a counter that wrapped, or a driver that wrote nothing), or a
/// period that is not a positive number.
pub fn ticks_to_ms(begin: u64, end: u64, period_ns: f32) -> Option<f64> {
    if !(period_ns.is_finite() && period_ns > 0.0) {
        return None;
    }
    let ticks = end.checked_sub(begin)?;
    Some(ticks as f64 * f64::from(period_ns) / 1e6)
}

/// A pass's place in the timestamps of one frame: the beginning is written to query `first`, the
/// end to `first + 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Index of the pass in the registry.
    pub pass: usize,
    pub first: u32,
}

/// The times of one frame, in milliseconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTimes {
    /// `(pass, ms)` for each pass that ran and has a valid time.
    pub passes: Vec<(usize, f64)>,
    /// From the beginning of the first pass to the end of the last, gaps included.
    pub total: Option<f64>,
}

/// Turns the resolved `timestamps` of a frame into times. A pass whose timestamps do not make an
/// interval is left out; the total spans the passes that remain.
pub fn frame_times(timestamps: &[u64], slots: &[Slot], period_ns: f32) -> FrameTimes {
    let mut times = FrameTimes::default();
    let mut span: Option<(u64, u64)> = None;
    for slot in slots {
        let first = slot.first as usize;
        let (Some(&begin), Some(&end)) = (timestamps.get(first), timestamps.get(first + 1)) else {
            continue;
        };
        let Some(ms) = ticks_to_ms(begin, end, period_ns) else {
            continue;
        };
        times.passes.push((slot.pass, ms));
        span = Some(span.map_or((begin, end), |(b, e)| (b.min(begin), e.max(end))));
    }
    times.total = span.and_then(|(begin, end)| ticks_to_ms(begin, end, period_ns));
    times
}

/// The times of one thing (a pass, or the whole frame) over the last [`WINDOW`] frames.
#[derive(Clone, Debug)]
pub struct Series {
    window: VecDeque<f64>,
    count: u64,
    /// Every time ever recorded, if asked for with [`Timings::set_keep_history`].
    history: Option<Vec<f64>>,
}

impl Series {
    fn new(keep_history: bool) -> Self {
        Self {
            window: VecDeque::with_capacity(WINDOW),
            count: 0,
            history: keep_history.then(Vec::new),
        }
    }

    fn record(&mut self, ms: f64) {
        if self.window.len() == WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(ms);
        self.count += 1;
        if let Some(history) = &mut self.history {
            history.push(ms);
        }
    }

    /// Mean over the last [`WINDOW`] times, or `None` if there are none (a timer that is
    /// unsupported, or that has not reported yet).
    pub fn average(&self) -> Option<f64> {
        (!self.window.is_empty())
            .then(|| self.window.iter().sum::<f64>() / self.window.len() as f64)
    }

    /// The longest of the last [`WINDOW`] times.
    pub fn max(&self) -> Option<f64> {
        self.window.iter().copied().reduce(f64::max)
    }

    /// The latest time.
    pub fn last(&self) -> Option<f64> {
        self.window.back().copied()
    }

    /// Times recorded since the start, which grows by one with each new time: a reader that
    /// polls can tell a new time from one it has seen.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Every time recorded, oldest first. Empty unless [`Timings::set_keep_history`] was on.
    pub fn history(&self) -> &[f64] {
        self.history.as_deref().unwrap_or_default()
    }
}

/// A GPU pass and its times.
#[derive(Clone, Debug)]
pub struct Pass {
    pub name: String,
    /// Whether the latest frame that reported ran this pass. A pass that only runs in some
    /// modes keeps its old times while it is off.
    pub active: bool,
    pub times: Series,
}

/// The times of every GPU pass, in the order the passes were first used, and of the frame.
#[derive(Clone, Debug)]
pub struct Timings {
    passes: Vec<Pass>,
    gpu_total: Series,
    wall: Series,
    keep_history: bool,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            passes: Vec::new(),
            gpu_total: Series::new(false),
            wall: Series::new(false),
            keep_history: false,
        }
    }
}

impl Timings {
    /// The index of the pass called `name`, which is added at the end of the list if it is new.
    pub fn register(&mut self, name: &str) -> usize {
        if let Some(index) = self.passes.iter().position(|p| p.name == name) {
            return index;
        }
        self.passes.push(Pass {
            name: name.to_string(),
            active: false,
            times: Series::new(self.keep_history),
        });
        self.passes.len() - 1
    }

    /// Adds the times of one frame. Passes it does not mention become inactive.
    pub fn record_frame(&mut self, frame: &FrameTimes) {
        for pass in &mut self.passes {
            pass.active = false;
        }
        for &(index, ms) in &frame.passes {
            let pass = &mut self.passes[index];
            pass.active = true;
            pass.times.record(ms);
        }
        if let Some(total) = frame.total {
            self.gpu_total.record(total);
        }
    }

    /// Adds the time between two frames on the CPU's clock, which is there with or without GPU
    /// timers.
    pub fn record_wall(&mut self, ms: f64) {
        self.wall.record(ms);
    }

    /// Every pass used so far, in the order they were first used.
    pub fn passes(&self) -> &[Pass] {
        &self.passes
    }

    /// The pass called `name`.
    pub fn pass(&self, name: &str) -> Option<&Pass> {
        self.passes.iter().find(|p| p.name == name)
    }

    /// GPU time of the whole frame, from the start of its first pass to the end of its last.
    pub fn total(&self) -> &Series {
        &self.gpu_total
    }

    /// Time from one frame to the next on the CPU's clock.
    pub fn wall(&self) -> &Series {
        &self.wall
    }

    /// Keeps every time from now on (see [`Series::history`]), for reports over a whole run.
    /// Turning it off drops what was kept.
    pub fn set_keep_history(&mut self, keep: bool) {
        self.keep_history = keep;
        let series = self
            .passes
            .iter_mut()
            .map(|p| &mut p.times)
            .chain([&mut self.gpu_total, &mut self.wall]);
        for s in series {
            s.history = keep.then(|| s.history.take().unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_become_milliseconds_by_the_period() {
        // 1 ns per tick: a million ticks is a millisecond.
        assert_eq!(ticks_to_ms(5, 1_000_005, 1.0), Some(1.0));
        // 52 ns per tick, as on some GPUs.
        let ms = ticks_to_ms(0, 19_230, 52.0).unwrap();
        assert!((ms - 0.99996).abs() < 1e-9);
    }

    #[test]
    fn timestamps_that_are_not_an_interval_give_no_time() {
        assert_eq!(ticks_to_ms(10, 9, 1.0), None);
        assert_eq!(ticks_to_ms(0, 10, 0.0), None);
        assert_eq!(ticks_to_ms(0, 10, f32::NAN), None);
        assert_eq!(ticks_to_ms(7, 7, 1.0), Some(0.0));
    }

    #[test]
    fn a_frame_has_a_time_per_pass_and_a_total_over_the_gaps() {
        // Pass 0 takes 2 ms, then there is a 1 ms gap, then pass 1 takes 4 ms.
        let stamps = [0, 2_000_000, 3_000_000, 7_000_000];
        let slots = [Slot { pass: 0, first: 0 }, Slot { pass: 1, first: 2 }];
        let times = frame_times(&stamps, &slots, 1.0);
        assert_eq!(times.passes, [(0, 2.0), (1, 4.0)]);
        assert_eq!(times.total, Some(7.0));
    }

    #[test]
    fn a_pass_with_bad_timestamps_is_left_out_of_the_frame() {
        let stamps = [0, 2_000_000, 9, 3, 3_000_000, 5_000_000];
        let slots = [
            Slot { pass: 0, first: 0 },
            Slot { pass: 1, first: 2 },
            Slot { pass: 2, first: 4 },
            // Beyond what was resolved.
            Slot { pass: 3, first: 6 },
        ];
        let times = frame_times(&stamps, &slots, 1.0);
        assert_eq!(times.passes, [(0, 2.0), (2, 2.0)]);
        assert_eq!(times.total, Some(5.0));
    }

    #[test]
    fn a_frame_with_no_passes_has_no_total() {
        assert_eq!(frame_times(&[], &[], 1.0), FrameTimes::default());
    }

    fn frame(times: &[(usize, f64)]) -> FrameTimes {
        FrameTimes {
            passes: times.to_vec(),
            total: Some(times.iter().map(|t| t.1).sum()),
        }
    }

    #[test]
    fn passes_are_listed_in_the_order_they_were_first_registered() {
        let mut timings = Timings::default();
        assert_eq!(timings.register("primary"), 0);
        assert_eq!(timings.register("blit"), 1);
        assert_eq!(timings.register("primary"), 0);
        assert_eq!(timings.register("overlay"), 2);
        let names: Vec<_> = timings.passes().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["primary", "blit", "overlay"]);
    }

    #[test]
    fn a_pass_with_no_times_has_no_average_or_max() {
        let mut timings = Timings::default();
        timings.register("primary");
        let times = &timings.passes()[0].times;
        assert_eq!(times.average(), None);
        assert_eq!(times.max(), None);
        assert_eq!(times.last(), None);
        assert_eq!(times.count(), 0);
        assert_eq!(timings.total().average(), None);
        assert_eq!(timings.wall().average(), None);
    }

    #[test]
    fn the_average_and_max_follow_the_recorded_times() {
        let mut timings = Timings::default();
        let a = timings.register("a");
        for ms in [1.0, 5.0, 3.0] {
            timings.record_frame(&frame(&[(a, ms)]));
        }
        let times = &timings.passes()[a].times;
        assert_eq!(times.average(), Some(3.0));
        assert_eq!(times.max(), Some(5.0));
        assert_eq!(times.last(), Some(3.0));
        assert_eq!(times.count(), 3);
        assert_eq!(timings.total().average(), Some(3.0));
    }

    #[test]
    fn only_the_last_window_of_times_counts() {
        let mut timings = Timings::default();
        let a = timings.register("a");
        // A spike, then a whole window of steady times that pushes it out.
        timings.record_frame(&frame(&[(a, 100.0)]));
        for _ in 0..WINDOW {
            timings.record_frame(&frame(&[(a, 2.0)]));
        }
        let times = &timings.passes()[a].times;
        assert_eq!(times.average(), Some(2.0));
        assert_eq!(times.max(), Some(2.0));
        assert_eq!(times.count(), WINDOW as u64 + 1);
    }

    #[test]
    fn a_pass_that_skips_a_frame_is_inactive_and_keeps_its_times() {
        let mut timings = Timings::default();
        let primary = timings.register("primary");
        let reference = timings.register("reference");
        timings.record_frame(&frame(&[(primary, 2.0)]));
        timings.record_frame(&frame(&[(reference, 8.0)]));
        let passes = timings.passes();
        assert!(!passes[primary].active && passes[reference].active);
        assert_eq!(passes[primary].times.average(), Some(2.0));
        assert_eq!(passes[reference].times.average(), Some(8.0));
        assert_eq!(timings.pass("reference").unwrap().times.last(), Some(8.0));
        assert!(timings.pass("nope").is_none());
    }

    #[test]
    fn wall_time_is_recorded_without_any_pass() {
        let mut timings = Timings::default();
        timings.record_wall(16.0);
        timings.record_wall(18.0);
        assert_eq!(timings.wall().average(), Some(17.0));
        assert_eq!(timings.total().average(), None);
    }

    #[test]
    fn history_is_kept_only_when_asked_for() {
        let mut timings = Timings::default();
        let a = timings.register("a");
        timings.record_frame(&frame(&[(a, 1.0)]));
        assert!(timings.passes()[a].times.history().is_empty());

        timings.set_keep_history(true);
        let b = timings.register("b");
        for ms in 0..(WINDOW + 5) {
            timings.record_frame(&frame(&[(a, ms as f64), (b, 1.0)]));
        }
        // Past the window, every time is still there.
        assert_eq!(timings.passes()[a].times.history().len(), WINDOW + 5);
        assert_eq!(timings.passes()[b].times.history().len(), WINDOW + 5);
        assert_eq!(timings.total().history().len(), WINDOW + 5);

        timings.set_keep_history(false);
        assert!(timings.passes()[a].times.history().is_empty());
    }
}
