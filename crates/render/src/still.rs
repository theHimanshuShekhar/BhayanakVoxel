//! When the Reference view is shown, and how (ADR 0005, #189): the policy of a Still frame, as a
//! state machine with no GPU in it.
//!
//! - **Still** is the same view (the camera, the sun, the window's size) and the same Planet (no
//!   Edit, no bricks arriving) for [`STILL_FRAMES`] frames in a row, in Reference mode. Until
//!   then the frame is **Primary**.
//! - **A change cuts to Primary at once**, with no blend, and the Reference view starts again from
//!   sample 0 (an Edit too: the whole image, never a part of it).
//! - **A Still frame is a batch** of as many samples as fit in the budget ([`BatchBudget`]).
//! - **The fade:** from the frame that draws the first batch, the Reference view is mixed over the
//!   last Primary image for [`FADE`]. The mix is for display only. It is made by the pass that
//!   copies the images to the window, after the Reference pass, and nothing is ever written back,
//!   so the accumulation holds the samples and only the samples.

use std::time::Duration;

use crate::batch::BatchBudget;

/// Frames in a row that the view must be unchanged before the Reference view is shown, and before
/// loading pauses for it (the client uses this too).
pub const STILL_FRAMES: u32 = 2;

/// How long the Reference view takes to fade in over the Primary image: about 150 ms.
pub const FADE: Duration = Duration::from_millis(150);

/// What a frame draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Plan {
    /// Primary, with nothing of the Reference view: the view is changing, or has not been still
    /// for long enough.
    Primary,
    /// A batch of `samples` for the Reference view, shown mixed over the last Primary image by
    /// `fade` (0 is all Primary, 1 is all Reference).
    Reference { samples: u32, fade: f32 },
}

impl Plan {
    /// How much of the Reference view the window shows: 0 for Primary.
    pub fn fade(self) -> f32 {
        match self {
            Plan::Primary => 0.0,
            Plan::Reference { fade, .. } => fade,
        }
    }

    /// The samples this frame adds to the accumulation: 0 for Primary.
    pub fn samples(self) -> u32 {
        match self {
            Plan::Primary => 0,
            Plan::Reference { samples, .. } => samples,
        }
    }
}

/// How far the fade is `elapsed` after the first batch: linear from 0 to 1 over [`FADE`].
pub fn fade_at(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
}

/// Decides every frame what is drawn. `K` is everything that decides what the Reference view is
/// of (the view, and which Planet): when it changes, the frame is Primary.
#[derive(Clone, Debug)]
pub struct Still<K> {
    last: Option<K>,
    /// Frames in a row that the key has equalled the frame before's.
    unchanged: u32,
    budget: BatchBudget,
    /// When the first batch of this Still view was drawn.
    first_batch: Option<Duration>,
    /// What the last frame planned.
    last_plan: Plan,
}

impl<K> Default for Still<K> {
    fn default() -> Self {
        Self {
            last: None,
            unchanged: 0,
            budget: BatchBudget::default(),
            first_batch: None,
            last_plan: Plan::Primary,
        }
    }
}

impl<K: Clone + PartialEq> Still<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// What to draw for a frame of `key` at time `now` (from any clock, as long as it is the same
    /// one every frame). With `reference` false (Primary mode) the frame is always Primary, and
    /// the state follows the view all the same, so that switching to Reference mode on a view
    /// that is already still shows it at once.
    ///
    /// `pixels` is the size of the Reference image: it sets the most a batch may be
    /// ([`crate::batch::max_samples`]).
    ///
    /// A Primary frame is also the cue to start the Reference view over: see
    /// [`Plan::Primary`].
    pub fn plan(&mut self, key: &K, reference: bool, now: Duration, pixels: u64) -> Plan {
        self.budget.resize(pixels);
        if self.last.as_ref() == Some(key) {
            self.unchanged = self.unchanged.saturating_add(1);
        } else {
            self.last = Some(key.clone());
            self.unchanged = 0;
        }
        let plan = if reference && self.unchanged >= STILL_FRAMES {
            let began = *self.first_batch.get_or_insert(now);
            Plan::Reference {
                samples: self.budget.samples(),
                fade: fade_at(now.saturating_sub(began)),
            }
        } else {
            self.first_batch = None;
            self.budget.restart();
            Plan::Primary
        };
        self.last_plan = plan;
        plan
    }

    /// The label to give the GPU's timer for the batch just planned, to bring back with its time
    /// ([`Still::timed`]).
    pub fn tag(&self) -> u64 {
        self.budget.tag()
    }

    /// The time of a batch that came in, `tag` being what [`Still::tag`] gave for it. It may be
    /// a frame or two late.
    pub fn timed(&mut self, tag: u64, batch_ms: f64) {
        self.budget.record(tag, batch_ms);
    }

    /// What the last frame was planned as.
    pub fn last_plan(&self) -> Plan {
        self.last_plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);
    /// 2560 × 1440.
    const PIXELS: u64 = 3_686_400;

    /// Frame `n` of a clock at 60 Hz.
    fn at(n: u32) -> Duration {
        FRAME * n
    }

    #[test]
    fn a_view_is_shown_as_primary_for_two_frames_and_then_as_reference() {
        let mut still = Still::new();
        let plans: Vec<Plan> = (0..5)
            .map(|n| still.plan(&"a", true, at(n), PIXELS))
            .collect();
        assert_eq!(plans[0], Plan::Primary);
        assert_eq!(plans[1], Plan::Primary);
        assert!(matches!(plans[2], Plan::Reference { .. }), "{plans:?}");
        assert!(matches!(plans[4], Plan::Reference { .. }));
    }

    #[test]
    fn any_change_cuts_to_primary_at_once_with_no_blend() {
        // The key is everything that decides the view: the camera, the sun, the window's size and
        // the Planet (an Edit), here as a tuple.
        let still_view = (1, 1, (640, 360), 0);
        let changes = [
            ("the camera", (2, 1, (640, 360), 0)),
            ("the sun", (1, 2, (640, 360), 0)),
            ("the window's size", (1, 1, (1280, 720), 0)),
            ("an Edit", (1, 1, (640, 360), 1)),
        ];
        for (what, changed) in changes {
            let mut still = Still::new();
            for n in 0..40 {
                still.plan(&still_view, true, at(n), PIXELS);
            }
            let shown = still.plan(&still_view, true, at(40), PIXELS);
            assert_eq!(shown.fade(), 1.0, "{what}: the fade is over by now");
            assert!(shown.samples() > 0);
            // The very next frame is Primary, and has none of the Reference view in it.
            assert_eq!(
                still.plan(&changed, true, at(41), PIXELS),
                Plan::Primary,
                "{what}"
            );
            assert_eq!(
                still.plan(&changed, true, at(42), PIXELS),
                Plan::Primary,
                "{what}"
            );
            // Still again: it starts over, with a batch of 1 and the fade from 0.
            let again = still.plan(&changed, true, at(43), PIXELS);
            assert_eq!(
                again,
                Plan::Reference {
                    samples: 1,
                    fade: 0.0
                },
                "{what}"
            );
        }
    }

    #[test]
    fn primary_mode_is_always_primary_and_reference_mode_picks_up_a_view_that_is_still() {
        let mut still = Still::new();
        for n in 0..10 {
            assert_eq!(still.plan(&"a", false, at(n), PIXELS), Plan::Primary);
        }
        assert!(matches!(
            still.plan(&"a", true, at(10), PIXELS),
            Plan::Reference { .. }
        ));
    }

    #[test]
    fn the_fade_goes_from_nothing_to_all_in_150_ms_from_the_first_batch() {
        let mut still = Still::new();
        still.plan(&"a", true, at(0), PIXELS);
        still.plan(&"a", true, at(1), PIXELS);
        let started = at(2);
        let fades: Vec<f32> = (0..12)
            .map(|n| still.plan(&"a", true, started + FRAME * n, PIXELS).fade())
            .collect();
        assert_eq!(
            fades[0], 0.0,
            "it starts at the frame the first batch lands"
        );
        assert!(fades.windows(2).all(|w| w[1] >= w[0]), "{fades:?}");
        // 150 ms is 9 frames at 60 Hz.
        assert!(fades[4] > 0.3 && fades[4] < 0.6, "{fades:?}");
        assert_eq!(fades[9], 1.0, "{fades:?}");
        assert_eq!(fades[11], 1.0);
        assert_eq!(fade_at(Duration::from_millis(75)), 0.5);
        assert_eq!(fade_at(Duration::from_secs(10)), 1.0);
    }

    #[test]
    fn a_change_in_the_middle_of_a_fade_starts_the_next_fade_from_nothing() {
        let mut still = Still::new();
        for n in 0..6 {
            still.plan(&"a", true, at(n), PIXELS);
        }
        assert!(still.plan(&"a", true, at(6), PIXELS).fade() > 0.0);
        assert_eq!(still.plan(&"b", true, at(7), PIXELS), Plan::Primary);
        still.plan(&"b", true, at(8), PIXELS);
        let first = still.plan(&"b", true, at(9), PIXELS);
        assert_eq!(first.fade(), 0.0);
    }

    #[test]
    fn the_batch_grows_while_the_view_stays_still_and_starts_at_one_in_a_new_one() {
        let mut still = Still::new();
        let mut sizes = Vec::new();
        for n in 0..60 {
            let plan = still.plan(&"a", true, at(n), PIXELS);
            if let Plan::Reference { samples, .. } = plan {
                // The GPU says each sample took 1 ms.
                let tag = still.tag();
                still.timed(tag, f64::from(samples));
                sizes.push(samples);
            }
        }
        assert_eq!(sizes[0], 1);
        assert!(*sizes.last().unwrap() > 4, "{sizes:?}");
        // A new view: 1 on its first frame, whatever the old one had got to.
        still.plan(&"b", true, at(60), PIXELS);
        still.plan(&"b", true, at(61), PIXELS);
        assert_eq!(still.plan(&"b", true, at(62), PIXELS).samples(), 1);
    }

    #[test]
    fn a_big_image_never_plans_more_than_its_cap_whatever_the_timers_say() {
        // 4K, every batch timed at 0.5 ms a sample: 12 fit in the budget, but the work in a
        // dispatch of 8.3 Mpix is capped at 7 samples.
        const FOUR_K: u64 = 8_294_400;
        let mut still = Still::new();
        let mut most = 0;
        for n in 0..300 {
            let plan = still.plan(&"a", true, at(n), FOUR_K);
            if let Plan::Reference { samples, .. } = plan {
                let tag = still.tag();
                still.timed(tag, 0.5 * f64::from(samples));
                most = most.max(samples);
            }
        }
        assert_eq!(most, 7, "it rises to the cap and no further");

        // The same Still at 1440p reaches 12 or more (the cap there is 16), and then the image
        // is made 4K: the plan drops to the cap at once.
        let mut still = Still::new();
        let mut n = 0;
        let mut at_1440p = 0;
        while n < 300 {
            if let Plan::Reference { samples, .. } = still.plan(&"a", true, at(n), PIXELS) {
                let tag = still.tag();
                still.timed(tag, 0.5 * f64::from(samples));
                at_1440p = at_1440p.max(samples);
            }
            n += 1;
        }
        assert!(at_1440p > 7, "{at_1440p}");
        // A real window's key has the size in it, so it would cut to Primary here; this holds the
        // key to check the cap on its own: it follows the size, and drops at once.
        let plan = still.plan(&"a", true, at(n), FOUR_K);
        assert!(plan.samples() <= 7, "{plan:?}");
    }
}
