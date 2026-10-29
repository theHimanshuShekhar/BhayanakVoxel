//! When progressive accumulation continues and when it starts over.
//!
//! The reference path tracer adds one sample per frame to a running mean. That mean is only
//! valid while every sample looks at the same thing, so anything that changes the picture (the
//! camera, the sun, the size of the image, the Planet) starts it over. This is the CPU side of
//! that decision, as a pure state machine with no GPU in it.

/// Counts samples while the thing being rendered stays the same.
#[derive(Clone, Debug)]
pub struct Accumulator<K> {
    /// What the samples so far were taken of.
    key: Option<K>,
    samples: u32,
}

impl<K> Default for Accumulator<K> {
    fn default() -> Self {
        Self {
            key: None,
            samples: 0,
        }
    }
}

impl<K: Clone + PartialEq> Accumulator<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts taking one more sample of `key` and returns its index: 0 for the first sample
    /// after a reset, which replaces whatever the buffer held, then 1, 2, ... The index is also
    /// what seeds the sample's random numbers, so that a given view and sample count always
    /// give the same image.
    pub fn begin_sample(&mut self, key: &K) -> u32 {
        if self.key.as_ref() != Some(key) {
            self.key = Some(key.clone());
            self.samples = 0;
        }
        let index = self.samples;
        self.samples = self.samples.saturating_add(1);
        index
    }

    /// Forgets the samples so far, for changes that `key` cannot see (a new Planet).
    pub fn reset(&mut self) {
        self.key = None;
        self.samples = 0;
    }

    /// Samples taken since the last reset.
    pub fn samples(&self) -> u32 {
        self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_accumulated_before_the_first_sample() {
        let accumulator = Accumulator::<u8>::new();
        assert_eq!(accumulator.samples(), 0);
    }

    #[test]
    fn a_still_view_counts_up_one_sample_at_a_time() {
        let mut accumulator = Accumulator::new();
        let indices: Vec<u32> = (0..4).map(|_| accumulator.begin_sample(&"view")).collect();
        assert_eq!(indices, [0, 1, 2, 3]);
        assert_eq!(accumulator.samples(), 4);
    }

    #[test]
    fn a_new_view_starts_over_from_sample_zero() {
        let mut accumulator = Accumulator::new();
        accumulator.begin_sample(&1);
        accumulator.begin_sample(&1);
        assert_eq!(accumulator.begin_sample(&2), 0);
        assert_eq!(accumulator.samples(), 1);
        assert_eq!(accumulator.begin_sample(&2), 1);
    }

    #[test]
    fn going_back_to_an_earlier_view_does_not_resume_it() {
        // The buffer was overwritten in between, so the old samples are gone.
        let mut accumulator = Accumulator::new();
        accumulator.begin_sample(&1);
        accumulator.begin_sample(&1);
        accumulator.begin_sample(&2);
        assert_eq!(accumulator.begin_sample(&1), 0);
    }

    #[test]
    fn a_reset_restarts_even_when_the_view_is_unchanged() {
        let mut accumulator = Accumulator::new();
        accumulator.begin_sample(&1);
        accumulator.begin_sample(&1);
        accumulator.reset();
        assert_eq!(accumulator.samples(), 0);
        assert_eq!(accumulator.begin_sample(&1), 0);
    }

    #[test]
    fn the_same_history_always_gives_the_same_indices() {
        // Determinism: the indices seed the RNG, so they must depend on nothing else.
        let run = || {
            let mut accumulator = Accumulator::new();
            [1, 1, 2, 2, 2, 1].map(|view| accumulator.begin_sample(&view))
        };
        assert_eq!(run(), [0, 1, 0, 1, 2, 0]);
        assert_eq!(run(), run());
    }

    #[test]
    fn the_count_saturates_instead_of_wrapping_back_to_a_reset() {
        let mut accumulator = Accumulator {
            key: Some(1),
            samples: u32::MAX,
        };
        assert_eq!(accumulator.begin_sample(&1), u32::MAX);
        assert_eq!(accumulator.samples(), u32::MAX);
    }
}
