//! How many samples a Still frame of the Reference view takes in one dispatch (ADR 0005, #189).
//!
//! A frame at 60 Hz has 16.7 ms, and one sample of the Reference view costs 2.5 to 8 ms at 1440p
//! on the 4090 (`docs/measurements.md`), so a Still frame takes as many samples in a single
//! dispatch as fit in [`BATCH_BUDGET_MS`]. This is the CPU side of the choice, a controller with
//! no GPU in it. **Every constant of the policy is in this file.**
//!
//! - **What it is judged from:** the slowest of the last [`RECENT_BATCHES`] timed samples of the
//!   view being drawn (a batch's time over its size). A batch is atomic, so its time is about its
//!   size times the slowest sample, not the mean; that is the research's lesson (`reference-
//!   scheduling.md`, 3.1 and 6). A camera that moves to another view forgets them: the cost is the
//!   view's, and the first frame of a view takes 1.
//! - **Rises slowly:** one more sample after [`RISE_AFTER_FRAMES`] frames in a row that fit, so a
//!   timer that is wrong for one frame cannot put the whole batch size up at once.
//! - **Drops at once:** the moment a batch overran the budget, or the slowest sample says fewer
//!   fit, the size is what fits (at least 1).
//! - **Never more than [`MAX_BATCH_SAMPLES`],** whatever the timers say.
//! - **No timers, no data:** the size stays 1. A frame that could not be timed holds the size.
//!
//! If even one sample does not fit in the budget, the size is 1 and the frame rate drops; the
//! quality does not.

use std::collections::VecDeque;

/// What a Still frame's dispatch may take on the GPU, in milliseconds: 12 of the 16.7 at 60 Hz,
/// leaving 1 for the copy to the window and the overlay and 3.7 of slack against a spike
/// (`reference-scheduling.md`, 3.3).
pub const BATCH_BUDGET_MS: f64 = 12.0;

/// The most samples in one dispatch, and the hard cap on how long a single dispatch can run.
///
/// **How it was chosen.** A dispatch that outlives the driver's timeout resets the GPU: Windows'
/// TDR is 2 s by default ([`TDR_TIMEOUT_MS`]). The slowest sample measured on the 4090 at 1440p
/// is 7.92 ms (the pit's 95th percentile, #99; [`SLOWEST_SAMPLE_MS`]), and ADR 0005 says a snow
/// cave costs about three times a rock one ([`COST_MARGIN`]). 16 samples of that is 0.38 s, a fifth
/// of the timeout, and it only comes into play if the timers are wrong (a time of 0 would
/// otherwise ask for any number), because the budget is what sets the size in every other case. A
/// test holds the product to a quarter of the timeout. At 1440p the budget gives 1 to 4 samples
/// (2.5 to 8 ms each), so the cap never limits the quality there; it limits a view that costs
/// under 0.75 ms a sample.
pub const MAX_BATCH_SAMPLES: u32 = 16;

/// Windows' default timeout before it resets a GPU that has not answered (`TdrDelay`).
pub const TDR_TIMEOUT_MS: f64 = 2000.0;
/// The slowest sample measured on the 4090 at 1440p, in milliseconds (#99, the pit).
pub const SLOWEST_SAMPLE_MS: f64 = 7.92;
/// How much dearer a place the measurements have not met may be: a snow cave, about three times a
/// rock one (ADR 0005, "Estimator").
pub const COST_MARGIN: f64 = 3.0;

/// How many of the latest timed samples the slowest is taken over.
pub const RECENT_BATCHES: usize = 16;

/// Frames in a row that fit before the size goes up by one.
pub const RISE_AFTER_FRAMES: u32 = 4;

/// Chooses the size of each Still frame's batch from the times of the ones before.
#[derive(Clone, Debug)]
pub struct BatchBudget {
    /// The size of the next batch.
    samples: u32,
    /// The time of each of the latest timed samples of this view, in milliseconds.
    recent: VecDeque<f64>,
    /// Frames in a row that fit within the budget and have room for one more sample.
    fitting: u32,
    /// Counts the views, so that a time that arrives late for the one before is told from this
    /// view's.
    view: u32,
}

impl Default for BatchBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchBudget {
    /// The first frame of a view takes one sample.
    pub fn new() -> Self {
        Self {
            samples: 1,
            recent: VecDeque::with_capacity(RECENT_BATCHES),
            fitting: 0,
            view: 0,
        }
    }

    /// Samples for the next frame's batch.
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// A new view: nothing is known of its cost, so the next batch is 1. Times that come in for
    /// the view before are left out ([`BatchBudget::tag`]).
    pub fn restart(&mut self) {
        self.samples = 1;
        self.recent.clear();
        self.fitting = 0;
        self.view = self.view.wrapping_add(1);
    }

    /// A label for the batch about to be drawn, to give back with its time
    /// ([`BatchBudget::record`]): the view and the size, which say what the time is of even when
    /// it comes in a frame or two late.
    pub fn tag(&self) -> u64 {
        (u64::from(self.view) << 32) | u64::from(self.samples)
    }

    /// The time of a batch, `tag` being what [`BatchBudget::tag`] gave when it was drawn. Ignored
    /// if it is of an earlier view, or not a time.
    pub fn record(&mut self, tag: u64, batch_ms: f64) {
        let (view, samples) = ((tag >> 32) as u32, tag as u32);
        if view != self.view || samples == 0 || !batch_ms.is_finite() || batch_ms < 0.0 {
            return;
        }
        if self.recent.len() == RECENT_BATCHES {
            self.recent.pop_front();
        }
        self.recent.push_back(batch_ms / f64::from(samples));
        let slowest = self.recent.iter().copied().fold(0.0, f64::max);
        let fits = fit(slowest);
        if fits < self.samples {
            // Drops at once, to what fits. A batch that overran has a sample slower than the
            // budget allows for its size, so it always lands here (unless the size is 1).
            self.samples = fits;
            self.fitting = 0;
        } else if fits > self.samples {
            self.fitting += 1;
            if self.fitting >= RISE_AFTER_FRAMES {
                self.samples += 1;
                self.fitting = 0;
            }
        } else {
            self.fitting = 0;
        }
    }
}

/// The samples that fit in the budget if each takes `slowest_ms`, from 1 to the cap.
fn fit(slowest_ms: f64) -> u32 {
    if slowest_ms <= 0.0 {
        return MAX_BATCH_SAMPLES;
    }
    ((BATCH_BUDGET_MS / slowest_ms)
        .floor()
        .min(f64::from(MAX_BATCH_SAMPLES)) as u32)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws `frames` batches of a view whose samples take `ms` each (a batch of k takes k times
    /// that, as measured: 1.00 to 1.06 times), and returns the size of each.
    fn run(budget: &mut BatchBudget, frames: u32, ms: f64) -> Vec<u32> {
        (0..frames)
            .map(|_| {
                let (tag, samples) = (budget.tag(), budget.samples());
                budget.record(tag, ms * f64::from(samples));
                samples
            })
            .collect()
    }

    #[test]
    fn the_first_frame_of_a_view_takes_one_sample() {
        assert_eq!(BatchBudget::new().samples(), 1);
        let mut budget = BatchBudget::new();
        run(&mut budget, 40, 1.0);
        assert!(budget.samples() > 1);
        budget.restart();
        assert_eq!(budget.samples(), 1);
    }

    #[test]
    fn it_rises_by_one_after_four_frames_that_fit_and_no_faster() {
        let mut budget = BatchBudget::new();
        // 2.6 ms a sample, the open view at 1440p: 4 fit in 12 ms.
        let sizes = run(&mut budget, 20, 2.6);
        assert_eq!(sizes[..4], [1; 4]);
        assert_eq!(sizes[4..8], [2; 4]);
        assert_eq!(sizes[8..12], [3; 4]);
        assert_eq!(sizes[12..], [4; 8]);
        assert!(
            sizes.windows(2).all(|w| w[1] <= w[0] + 1),
            "never more than one up at a time: {sizes:?}"
        );
    }

    #[test]
    fn a_view_where_even_one_sample_does_not_fit_stays_at_one() {
        let mut budget = BatchBudget::new();
        // 14 ms a sample: over the budget alone. The frame rate drops, and the size does not.
        assert_eq!(run(&mut budget, 30, 14.0), vec![1; 30]);
        // The pit at 1440p, 7.92 ms: 1 fits.
        assert_eq!(run(&mut BatchBudget::new(), 30, 7.92), vec![1; 30]);
    }

    #[test]
    fn it_drops_at_once_on_an_overrun() {
        let mut budget = BatchBudget::new();
        run(&mut budget, 40, 1.0);
        let before = budget.samples();
        assert!(before >= 8, "{before}");
        // A batch that takes 30 ms: 3 ms a sample, so 4 fit.
        let tag = budget.tag();
        budget.record(tag, 30.0);
        assert_eq!(budget.samples(), fit(30.0 / f64::from(before)));
        assert!(budget.samples() < before);
    }

    #[test]
    fn a_slow_sample_sets_the_size_until_it_is_old() {
        let mut budget = BatchBudget::new();
        run(&mut budget, 40, 1.0);
        // One sample of 6 ms (a batch of 1 that was slow): the slowest of the recent ones is now
        // 6, so 2 fit, at once.
        let tag = (u64::from(budget.view) << 32) | 1;
        budget.record(tag, 6.0);
        assert_eq!(budget.samples(), 2);
        // It stays the slowest for 16 timed samples, and then the size may rise again.
        let sizes = run(&mut budget, RECENT_BATCHES as u32 - 1, 1.0);
        assert!(sizes.iter().all(|&s| s <= 2), "{sizes:?}");
        let later = run(&mut budget, 40, 1.0);
        assert!(later.last().unwrap() > &2, "{later:?}");
    }

    #[test]
    fn it_never_goes_over_the_cap_whatever_the_timers_say() {
        let mut budget = BatchBudget::new();
        // A timer that always says 0 ms.
        let sizes = run(&mut budget, 200, 0.0);
        assert_eq!(*sizes.iter().max().unwrap(), MAX_BATCH_SAMPLES);
        assert_eq!(fit(0.0), MAX_BATCH_SAMPLES);
        assert_eq!(fit(1e-9), MAX_BATCH_SAMPLES);
    }

    #[test]
    fn times_of_the_view_before_are_ignored() {
        let mut budget = BatchBudget::new();
        let old = budget.tag();
        budget.restart();
        // A time of 500 ms for the old view arrives late: it must not make the new view's cost.
        budget.record(old, 500.0);
        run(&mut budget, 40, 1.0);
        assert!(budget.samples() > 4, "{}", budget.samples());
        // Nor is a time that is not a time.
        let tag = budget.tag();
        let before = budget.samples();
        budget.record(tag, f64::NAN);
        budget.record(tag, -1.0);
        budget.record(0, 1.0);
        assert_eq!(budget.samples(), before);
    }

    #[test]
    fn the_cap_is_far_under_the_drivers_timeout() {
        // The product the cap's documentation states: 16 of the slowest measured sample, three
        // times over, is under a quarter of Windows' 2 s.
        let longest_ms = f64::from(MAX_BATCH_SAMPLES) * SLOWEST_SAMPLE_MS * COST_MARGIN;
        assert!(longest_ms <= TDR_TIMEOUT_MS / 4.0, "{longest_ms}");
        // And the budget fits in a frame at 60 Hz with a millisecond for the copy and the overlay.
        const { assert!(BATCH_BUDGET_MS + 1.0 < 1000.0 / 60.0) };
    }
}
