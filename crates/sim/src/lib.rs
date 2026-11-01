//! Simulation logic shared by the server and the in-process single-player server.

mod character;
mod edit;
mod ground;
mod movement;
mod prediction;
mod server;

use std::time::Duration;

pub use character::{Cell, Character, Occupancy, TICK_DT};
pub use edit::{MAX_BRUSH_RADIUS, Undo, apply, validate};
pub use ground::{Footing, Ground};
pub use movement::{ConfigError, Movement, MovementConfig, VOXELS_PER_METRE};
pub use prediction::Predictor;
pub use server::{Outcome, Server, ServerHandle, TICK_RATE_HZ, spawn};

/// Fixed-timestep accumulator: turns elapsed wall-clock time into a whole number of ticks.
pub struct FixedTimestep {
    step: Duration,
    max_ticks_per_advance: u32,
    accumulator: Duration,
}

impl FixedTimestep {
    /// `max_ticks_per_advance` bounds the catch-up after a stall (spiral of death).
    pub fn new(step: Duration, max_ticks_per_advance: u32) -> Self {
        Self {
            step,
            max_ticks_per_advance,
            accumulator: Duration::ZERO,
        }
    }

    /// Adds `elapsed` and returns how many ticks to run now.
    pub fn advance(&mut self, elapsed: Duration) -> u32 {
        self.accumulator += elapsed;
        let mut ticks = 0;
        while self.accumulator >= self.step {
            self.accumulator -= self.step;
            ticks += 1;
            if ticks == self.max_ticks_per_advance {
                self.accumulator = Duration::ZERO;
                break;
            }
        }
        ticks
    }

    /// Time left until the next tick is due.
    pub fn until_next_tick(&self) -> Duration {
        self.step - self.accumulator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: Duration = Duration::from_micros(16_667);

    #[test]
    fn one_step_elapsed_runs_one_tick() {
        let mut clock = FixedTimestep::new(STEP, 5);
        assert_eq!(clock.advance(STEP), 1);
    }

    #[test]
    fn partial_steps_accumulate_across_advances() {
        let mut clock = FixedTimestep::new(STEP, 5);
        let half = STEP / 2;
        assert_eq!(clock.advance(half), 0);
        assert_eq!(clock.advance(half), 1);
        assert_eq!(clock.advance(half), 0);
    }

    #[test]
    fn a_long_stall_is_clamped_and_the_backlog_dropped() {
        let mut clock = FixedTimestep::new(STEP, 5);
        assert_eq!(clock.advance(STEP * 100), 5);
        assert_eq!(clock.advance(Duration::ZERO), 0);
    }

    #[test]
    fn until_next_tick_counts_down_the_remaining_step() {
        let mut clock = FixedTimestep::new(Duration::from_millis(10), 5);
        assert_eq!(clock.until_next_tick(), Duration::from_millis(10));
        clock.advance(Duration::from_millis(4));
        assert_eq!(clock.until_next_tick(), Duration::from_millis(6));
    }
}
