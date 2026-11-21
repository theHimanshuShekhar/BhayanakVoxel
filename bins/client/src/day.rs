//! The day (#203): the world clock, the sky it says, and the one rule that keeps a running clock
//! from stopping the Reference view from ever starting.
//!
//! **A moving sun and Still.** A view is Still when the camera, the sun and the window have not
//! changed for 2 frames (ADR 0005), and the Reference view accumulates while it is. A clock that
//! runs moves the sun on every frame, so a view could never be Still. The rule: **the clock only
//! runs in a frame in which the camera (and what goes with it, the bias, the Edits) is not the
//! same as the frame before's.** The player who stops has the time of day stop with them, one frame
//! later, and the view is Still two frames after that; moving again, or an Edit, lets it run. A
//! clock that is *set* (the overlay's slider, the keys, the bench's flag) moves the sun, and so
//! restarts the Reference view as any change of the sun does; it is no part of the rule, since the
//! rule is about the clock's running and not about its being set.

use render::{Daylight, Drive};
use sim::WorldClock;

use crate::world::Stillness;

/// The world clock of the client, and what it takes to turn a frame of it into a sky.
///
/// `K` is the key of the *camera's* view: everything that makes a frame's view differ from the last
/// one's except the sun (the camera, the LOD bias, the count of Edits). It is not the renderer's
/// own view, which has the sun in it.
pub struct DayClock<K> {
    clock: WorldClock,
    daylight: Daylight,
    still: Stillness<K>,
}

impl<K: PartialEq> DayClock<K> {
    pub fn new(clock: WorldClock) -> Self {
        Self {
            clock,
            daylight: Daylight::new(),
            still: Stillness::default(),
        }
    }

    pub fn clock(&self) -> &WorldClock {
        &self.clock
    }

    /// The clock, to set, pause or give another length. A set is a jump of the sun; a pause or a
    /// length is not one.
    pub fn clock_mut(&mut self) -> &mut WorldClock {
        &mut self.clock
    }

    /// Takes in a frame of `seconds` whose camera's view has the key `key`: runs the clock if the
    /// key is not the last frame's (the rule of this module), and says what the sky is, and whether
    /// it was put there or drifted ([`Daylight::step`]).
    pub fn frame(&mut self, seconds: f64, key: K) -> Drive {
        if self.still.observe(key) == 0 {
            self.clock.advance(seconds);
        }
        self.daylight
            .step(self.clock.time_of_day(), self.clock.epoch())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A day of the default half hour and frames of a sixtieth of a second: a step of the sun far
    /// under a jump's 2 degrees.
    fn clock() -> DayClock<u32> {
        DayClock::new(WorldClock::new(0.5, 1800.0))
    }

    const FRAME: f64 = 1.0 / 60.0;

    #[test]
    fn a_moving_camera_runs_the_clock_a_frame_at_a_time() {
        let mut day = clock();
        let first = day.frame(FRAME, 0).sky;
        let mut last = first;
        for camera in 1..=30 {
            let sky = day.frame(FRAME, camera).sky;
            assert_ne!(sky.time_of_day, last.time_of_day);
            last = sky;
        }
        // The first frame (a new key) ran it too: 31 frames in all.
        let seconds = 31.0 * FRAME;
        assert!((day.clock().time_of_day() - (0.5 + seconds / 1800.0)).abs() < 1e-9);
    }

    #[test]
    fn a_camera_that_stops_stops_the_clock_and_the_sky_is_the_same_in_every_frame_after_it() {
        let mut day = clock();
        day.frame(FRAME, 0);
        let moving = day.frame(FRAME, 1).sky;
        // The camera is at 1 and stays there: the frame it first arrives in still ran the clock
        // (it was new), the ones after it do not.
        let arrived = day.frame(FRAME, 2).sky;
        assert_ne!(moving, arrived);
        for _ in 0..120 {
            let drive = day.frame(FRAME, 2);
            assert_eq!(drive.sky, arrived);
            assert!(!drive.jumped);
        }
    }

    #[test]
    fn a_camera_that_moves_again_lets_the_clock_run_from_where_it_stopped() {
        let mut day = clock();
        day.frame(FRAME, 0);
        let stopped = day.frame(FRAME, 1).sky;
        for _ in 0..10 {
            day.frame(FRAME, 1);
        }
        let again = day.frame(FRAME, 2).sky;
        let step = again.time_of_day - stopped.time_of_day;
        assert!((step - FRAME / 1800.0).abs() < 1e-9, "{step}");
    }

    #[test]
    fn setting_the_clock_while_the_view_is_still_is_one_jump_and_then_the_sky_holds_again() {
        let mut day = clock();
        day.frame(FRAME, 7);
        for _ in 0..10 {
            day.frame(FRAME, 7);
        }
        day.clock_mut().set_time_of_day(0.8);
        let set = day.frame(FRAME, 7);
        assert!(set.jumped);
        assert!((set.sky.time_of_day - 0.8).abs() < 1e-9);
        for _ in 0..10 {
            let drive = day.frame(FRAME, 7);
            assert!(!drive.jumped);
            assert_eq!(drive.sky, set.sky);
        }
    }

    #[test]
    fn a_paused_clock_does_not_run_under_a_moving_camera() {
        let mut day = clock();
        let first = day.frame(FRAME, 0);
        day.clock_mut().set_paused(true);
        for camera in 1..30 {
            assert_eq!(day.frame(FRAME, camera).sky, first.sky);
        }
    }

    #[test]
    fn a_running_clock_under_a_moving_camera_drifts_and_does_not_jump() {
        let mut day = clock();
        assert!(day.frame(FRAME, 0).jumped, "the first sky is a jump");
        for camera in 1..600 {
            assert!(!day.frame(FRAME, camera).jumped, "frame {camera}");
        }
    }
}
