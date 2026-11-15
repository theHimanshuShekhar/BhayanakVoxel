//! How many samples a Still frame of the Reference view takes in one dispatch (ADR 0005, #189).
//!
//! A frame at 60 Hz has 16.7 ms, and one sample of the Reference view costs 2.5 ms (the open view)
//! to 4.4 ms (#99's pit, after #188) at 1440p on the 4090 (`docs/measurements.md`), so a Still
//! frame takes as many samples in a single dispatch as fit in [`BATCH_BUDGET_MS`]. This is the
//! CPU side of the choice, a controller with no GPU in it. **Every constant of the policy is in
//! this file.**
//!
//! - **What it is judged from:** the slowest of the last [`RECENT_BATCHES`] timed samples of the
//!   view being drawn (a batch's time over its size). A batch is atomic, so its time is about its
//!   size times the slowest sample, not the mean; that is the research's lesson (`reference-
//!   scheduling.md`, 3.1 and 6). A camera that moves to another view forgets them: the cost is the
//!   view's, and the first frame of a view takes 1.
//! - **Rises slowly:** one more sample after [`RISE_AFTER_FRAMES`] timed batches in a row that
//!   fit. The GPU's times arrive one to three frames after the batches they are of, so when the
//!   size has just gone up, some of those timed batches were drawn at the old size: the next rise
//!   can come sooner than 4 frames at the new size. A timer that is wrong for one frame still
//!   cannot put the batch size up at once.
//! - **Drops at once:** the moment a time says the batch overran the budget, or the slowest
//!   sample says fewer fit, the size is what fits (at least 1). That is the first time that
//!   arrives, however late.
//! - **Never more than [`max_samples`],** whatever the timers say: [`MAX_BATCH_SAMPLES`] at 1440p
//!   and fewer for a bigger image, so that one dispatch's work is bounded.
//! - **A timer that lies is no data:** a time of 0, or under [`MIN_SAMPLE_MS_PER_MPIX`] a sample,
//!   is not a measurement (a driver that writes the same timestamp twice gives 0). It is left
//!   out, and the size is held where it is: it does not rise on it.
//! - **No timers, no data:** the size stays 1. A frame that could not be timed holds the size.
//!
//! If even one sample does not fit in the budget, the size is 1 and the frame rate drops; the
//! quality does not.

use std::collections::VecDeque;

/// What a Still frame's dispatch may take on the GPU, in milliseconds: 12 of the 16.7 at 60 Hz,
/// leaving 1 for the copy to the window and the overlay and 3.7 of slack against a spike
/// (`reference-scheduling.md`, 3.3). The budget is sized from the `reference` pass alone (the
/// timers' tag): the display transform that follows it adds about 0.06 ms on the 4090 (#202), which
/// the 1 ms and the slack cover, so the post passes are not counted in a batch's time.
pub const BATCH_BUDGET_MS: f64 = 12.0;

/// The most samples in one dispatch at 1440p ([`REFERENCE_MPIX`]); a bigger image takes fewer
/// ([`max_samples`]). The hard cap on how long a single dispatch can run.
///
/// **How it was chosen.** A dispatch that outlives the driver's timeout resets the GPU: Windows'
/// TDR is 2 s by default ([`TDR_TIMEOUT_MS`]). What bounds a dispatch is the work in it, samples
/// times pixels, so the cap is on that: [`MAX_BATCH_SAMPLES`] at [`REFERENCE_MPIX`] (16 × 3.69
/// Mpix of samples, 59 Mpix) and the same work at any size.
///
/// - **The worst sample we know of at 1440p** is 10.98 ms, 2.98 ms a Mpix: the maximum of #99's
///   1 024 pit samples on the 4090 ([`WORST_SAMPLE_MS`]; its mean was 6.15 ms and its 95th
///   percentile 7.92 ms, before #188; after it the pit's mean is 4.38 ms). A capped batch of that
///   is 16 × 10.98 = 176 ms, 9 % of the timeout.
/// - **With the margin for a place not met:** ADR 0005 says a snow cave costs about three times a
///   rock one ([`COST_MARGIN`]): 0.53 s, 26 % of the timeout.
/// - **On other GPUs:** a GPU would have to be 11 times slower than the 4090 on that worst sample
///   (3.8 times with the snow cave's margin) before a capped batch reached the timeout. The 4090
///   never needs the cap: the budget gives 4 samples at most in the open view and 2 in the pit at
///   1440p (the 4090's runs of #216). The cap is for what the budget cannot see: timers that lie,
///   and a view that costs under 0.75 ms a sample.
///
/// Tests hold the products to these figures.
pub const MAX_BATCH_SAMPLES: u32 = 16;

/// The size of the image the cap is stated for, 2560 × 1440, in millions of pixels.
pub const REFERENCE_MPIX: f64 = 3.69;

/// Windows' default timeout before it resets a GPU that has not answered (`TdrDelay`).
pub const TDR_TIMEOUT_MS: f64 = 2000.0;
/// The slowest sample measured on the 4090 at 1440p, in milliseconds (#99, the pit, the maximum of
/// 1 024 samples: before #188, which made the pit cheaper).
pub const WORST_SAMPLE_MS: f64 = 10.98;
/// How much dearer a place the measurements have not met may be: a snow cave, about three times a
/// rock one (ADR 0005, "Estimator").
pub const COST_MARGIN: f64 = 3.0;

/// A sample cheaper than this, in milliseconds a million pixels, is not a time that was measured:
/// the cheapest view on the 4090 (the open view, 0.67 ms a Mpix) is 13 times over it.
///
/// **A view that is all sky has not been measured.** Its samples could come near this floor
/// (0.18 ms at 1440p). If one did fall under it, its times would be left out as no data and the
/// batch would stay at 1; that is harmless, because sky converges at once (a sample or two is
/// the answer).
pub const MIN_SAMPLE_MS_PER_MPIX: f64 = 0.05;

/// The most samples one dispatch of an image of `pixels` pixels may take: the cap's work
/// ([`MAX_BATCH_SAMPLES`] at 1440p) shared out, from 1 to [`MAX_BATCH_SAMPLES`]. 16 at 1440p and
/// below, 7 at 4K.
pub fn max_samples(pixels: u64) -> u32 {
    let megapixels = (pixels as f64 / 1e6).max(f64::MIN_POSITIVE);
    ((f64::from(MAX_BATCH_SAMPLES) * REFERENCE_MPIX / megapixels).floor())
        .clamp(1.0, f64::from(MAX_BATCH_SAMPLES)) as u32
}

/// How many of the latest timed samples the slowest is taken over.
pub const RECENT_BATCHES: usize = 16;

/// Frames in a row that fit before the size goes up by one.
pub const RISE_AFTER_FRAMES: u32 = 4;

/// Chooses the size of each Still frame's batch from the times of the ones before.
#[derive(Clone, Debug)]
pub struct BatchBudget {
    /// The size of the next batch.
    samples: u32,
    /// The most it may be, for the size of the image ([`max_samples`]).
    cap: u32,
    /// The least a sample may take to be believed, in milliseconds.
    least_ms: f64,
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
        Self::new(3_686_400)
    }
}

impl BatchBudget {
    /// The first frame of a view takes one sample. `pixels` is the size of the image the batch is
    /// of.
    pub fn new(pixels: u64) -> Self {
        let mut budget = Self {
            samples: 1,
            cap: MAX_BATCH_SAMPLES,
            least_ms: 0.0,
            recent: VecDeque::with_capacity(RECENT_BATCHES),
            fitting: 0,
            view: 0,
        };
        budget.resize(pixels);
        budget
    }

    /// The image is `pixels` big: what a batch may be, and what a time may be, follow. A new size
    /// is a new view, so this is called between views, but it holds either way.
    pub fn resize(&mut self, pixels: u64) {
        self.cap = max_samples(pixels);
        self.least_ms = MIN_SAMPLE_MS_PER_MPIX * pixels as f64 / 1e6;
        self.samples = self.samples.min(self.cap);
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
        if view != self.view || samples == 0 || !batch_ms.is_finite() {
            return;
        }
        let sample_ms = batch_ms / f64::from(samples);
        if sample_ms < self.least_ms || sample_ms <= 0.0 {
            return;
        }
        if self.recent.len() == RECENT_BATCHES {
            self.recent.pop_front();
        }
        self.recent.push_back(sample_ms);
        let slowest = self.recent.iter().copied().fold(0.0, f64::max);
        let fits = fit(slowest, self.cap);
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

/// The samples that fit in the budget if each takes `slowest_ms` (more than 0), from 1 to `cap`.
fn fit(slowest_ms: f64, cap: u32) -> u32 {
    ((BATCH_BUDGET_MS / slowest_ms).floor().min(f64::from(cap)) as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2560 × 1440.
    const P1440: u64 = 3_686_400;

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
        assert_eq!(BatchBudget::new(P1440).samples(), 1);
        let mut budget = BatchBudget::new(P1440);
        run(&mut budget, 40, 1.0);
        assert!(budget.samples() > 1);
        budget.restart();
        assert_eq!(budget.samples(), 1);
    }

    #[test]
    fn it_rises_by_one_after_four_frames_that_fit_and_no_faster() {
        let mut budget = BatchBudget::new(P1440);
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
        let mut budget = BatchBudget::new(P1440);
        // 14 ms a sample: over the budget alone. The frame rate drops, and the size does not.
        assert_eq!(run(&mut budget, 30, 14.0), vec![1; 30]);
        // #99's pit at 1440p, its p95 of 7.92 ms (before #188): 1 fits.
        assert_eq!(run(&mut BatchBudget::new(P1440), 30, 7.92), vec![1; 30]);
    }

    #[test]
    fn it_drops_at_once_on_an_overrun() {
        let mut budget = BatchBudget::new(P1440);
        run(&mut budget, 40, 1.0);
        let before = budget.samples();
        assert!(before >= 8, "{before}");
        // A batch that takes 30 ms: 3 ms a sample, so 4 fit.
        let tag = budget.tag();
        budget.record(tag, 30.0);
        assert_eq!(
            budget.samples(),
            fit(30.0 / f64::from(before), MAX_BATCH_SAMPLES)
        );
        assert!(budget.samples() < before);
    }

    #[test]
    fn a_slow_sample_sets_the_size_until_it_is_old() {
        let mut budget = BatchBudget::new(P1440);
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
    fn it_never_goes_over_the_cap_however_cheap_the_samples_are() {
        // The cheapest view that is believed: just over the least a sample may take.
        let mut budget = BatchBudget::new(P1440);
        let believed = MIN_SAMPLE_MS_PER_MPIX * 3.6864 * 1.01;
        let sizes = run(&mut budget, 400, believed);
        assert_eq!(*sizes.iter().max().unwrap(), MAX_BATCH_SAMPLES);
        assert!(sizes.iter().all(|&s| s <= MAX_BATCH_SAMPLES));
        // On a bigger image the cap is lower, and holds the same way.
        let mut big = BatchBudget::new(8_294_400);
        let sizes = run(&mut big, 400, 0.5);
        assert_eq!(*sizes.iter().max().unwrap(), max_samples(8_294_400));
    }

    #[test]
    fn timers_that_say_zero_or_next_to_it_are_no_data_and_the_size_stays_where_it_is() {
        for lie in [0.0, 1e-9, 0.01] {
            let mut budget = BatchBudget::new(P1440);
            let sizes = run(&mut budget, 200, lie);
            assert_eq!(sizes, vec![1; 200], "a timer that says {lie} ms");
        }
        // A size that was earned is held, not raised and not dropped, by a run of lies.
        let mut budget = BatchBudget::new(P1440);
        run(&mut budget, 40, 2.6);
        let earned = budget.samples();
        assert!(earned > 1);
        let sizes = run(&mut budget, 100, 0.0);
        assert!(sizes.iter().all(|&s| s == earned), "{sizes:?}");
        // Nor do lies undo a slow sample's drop.
        let tag = budget.tag();
        budget.record(tag, 12.0 * f64::from(earned));
        let dropped = budget.samples();
        assert!(dropped < earned);
        run(&mut budget, 10, 0.0);
        assert_eq!(budget.samples(), dropped);
    }

    #[test]
    fn the_cap_follows_the_size_of_the_image() {
        assert_eq!(max_samples(3_686_400), MAX_BATCH_SAMPLES, "1440p");
        assert_eq!(max_samples(2_073_600), MAX_BATCH_SAMPLES, "1080p");
        assert_eq!(max_samples(8_294_400), 7, "4K");
        assert_eq!(max_samples(33_177_600), 1, "8K");
        assert_eq!(max_samples(0), MAX_BATCH_SAMPLES);
        // The work in a dispatch (samples times pixels) never goes over that of the cap at 1440p.
        for pixels in [1u64 << 20, 3_686_400, 5_000_000, 8_294_400, 33_177_600] {
            let work = f64::from(max_samples(pixels)) * pixels as f64;
            let limit = f64::from(MAX_BATCH_SAMPLES) * REFERENCE_MPIX * 1e6;
            assert!(
                work <= limit * 1.001 || max_samples(pixels) == 1,
                "{pixels}"
            );
        }
        // A budget made for 4K never grows past its cap, and a new size takes the cap down.
        let mut budget = BatchBudget::new(8_294_400);
        let sizes = run(&mut budget, 300, 0.5);
        assert_eq!(*sizes.iter().max().unwrap(), 7);
        let mut budget = BatchBudget::new(P1440);
        run(&mut budget, 300, 0.5);
        assert!(budget.samples() > 7);
        budget.resize(8_294_400);
        assert_eq!(budget.samples(), 7);
    }

    #[test]
    fn times_of_the_view_before_are_ignored() {
        let mut budget = BatchBudget::new(P1440);
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
    fn a_capped_batch_of_the_worst_sample_known_is_far_under_the_drivers_timeout() {
        // The worst sample of #99 (the pit's maximum, 10.98 ms at 1440p), 16 of them in one
        // dispatch: under a tenth of Windows' 2 s. With the snow cave's margin, under a third.
        let worst = f64::from(MAX_BATCH_SAMPLES) * WORST_SAMPLE_MS;
        assert!(worst <= TDR_TIMEOUT_MS / 10.0, "{worst}");
        assert!(
            worst * COST_MARGIN <= TDR_TIMEOUT_MS / 3.0,
            "{}",
            worst * COST_MARGIN
        );
        // The same at any size: the cost of a sample grows with the pixels, and the cap with their
        // inverse, so a capped dispatch is never longer than at 1440p.
        for pixels in [3_686_400u64, 5_000_000, 8_294_400, 33_177_600] {
            let ms_a_sample = WORST_SAMPLE_MS * pixels as f64 / (REFERENCE_MPIX * 1e6);
            let longest = f64::from(max_samples(pixels)) * ms_a_sample * COST_MARGIN;
            // 1 sample of the biggest image may cost more than the cap's work, and nothing can
            // be done about that: the size is 1 and the frame rate drops.
            if max_samples(pixels) > 1 {
                assert!(longest <= TDR_TIMEOUT_MS / 3.0, "{pixels}: {longest}");
            }
        }
        // And the budget fits in a frame at 60 Hz with a millisecond for the copy and the overlay.
        const { assert!(BATCH_BUDGET_MS + 1.0 < 1000.0 / 60.0) };
    }
}
