//! The day (#203): the world clock, the sky it says, and the one rule that keeps a running clock
//! from stopping the Reference view from ever starting.
//!
//! **The clock always runs; the sky that is shown may be held.** `WorldClock` is the sim's, and
//! advances every frame whatever the camera and the mode do (a player standing still does not stop
//! the time of day; in M2 the server runs it). What is *shown* is another matter: a view is Still
//! when the camera, the sun and the window have not changed for 2 frames (ADR 0005), and the
//! Reference view accumulates while it is, which a sun that moves every frame would stop. So the
//! shown sky is **held while the Reference view is accumulating a Still view**: the same gate as
//! the scene's hold ([`loading_paused_for`]: the mode is Reference and the camera, the bias, the
//! Edits and the reference image's size have not changed for [`STILL_FRAMES`] frames; the key is
//! the scene's without the sun). In Primary, and in Reference while the camera moves, the shown
//! sky is the clock's, frame by frame.
//!
//! When the view stops being Still the shown sky catches up with the clock in one step. It is a
//! jump of the sun if it is more than [`render::daylight::JUMP_ANGLE`] away (the usual
//! [`Daylight::step`] rule, applied to the sky last shown), a drift if it is less. A clock that is
//! *set* (the overlay's slider, the keys, the bench's flag) is shown at once, held or not, as a
//! jump, and restarts the Reference view as any change of the sun does.

use render::{DaySky, Daylight, Drive, Mode};
use sim::WorldClock;

use crate::world::{Stillness, loading_paused_for};

/// The world clock of the client, and what it takes to turn a frame of it into a sky.
///
/// `K` is the key of the *camera's* view: everything that makes a frame's view differ from the last
/// one's except the sun (the camera, the LOD bias, the count of Edits). It is not the renderer's
/// own view, which has the sun in it.
pub struct DayClock<K> {
    clock: WorldClock,
    daylight: Daylight,
    still: Stillness<K>,
    /// The sky last shown, and the epoch of the clock it was shown for.
    shown: Option<(DaySky, u32)>,
}

impl<K: PartialEq> DayClock<K> {
    pub fn new(clock: WorldClock) -> Self {
        Self {
            clock,
            daylight: Daylight::new(),
            still: Stillness::default(),
            shown: None,
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

    /// Takes in a frame of `seconds` drawn in `mode` whose camera's view has the key `key`: runs
    /// the clock, and says what sky to show, and whether it was put there or drifted
    /// ([`Daylight::step`]). The sky is the one last shown while the Reference view is
    /// accumulating a Still view (the rule of this module), unless the clock was set.
    pub fn frame(&mut self, seconds: f64, key: K, mode: Mode) -> Drive {
        let still_frames = self.still.observe(key);
        self.clock.advance(seconds);
        if let Some((sky, epoch)) = self.shown
            && epoch == self.clock.epoch()
            && loading_paused_for(mode, still_frames)
        {
            return Drive { sky, jumped: false };
        }
        let drive = self
            .daylight
            .step(self.clock.time_of_day(), self.clock.epoch());
        self.shown = Some((drive.sky, self.clock.epoch()));
        drive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::STILL_FRAMES;

    /// A day of the default half hour and frames of a sixtieth of a second: a step of the sun far
    /// under a jump's 2 degrees.
    fn clock() -> DayClock<u32> {
        DayClock::new(WorldClock::new(0.5, 1800.0))
    }

    const FRAME: f64 = 1.0 / 60.0;
    const STEP: f64 = FRAME / 1800.0;

    /// Stands the camera at `key` in `mode` until the sky is held (the frames that gate it, and
    /// one more), and gives the sky held.
    fn hold(day: &mut DayClock<u32>, key: u32) -> DaySky {
        for _ in 0..=STILL_FRAMES {
            day.frame(FRAME, key, Mode::Reference);
        }
        let held = day.frame(FRAME, key, Mode::Reference);
        assert!(!held.jumped);
        held.sky
    }

    #[test]
    fn a_moving_camera_runs_the_clock_and_shows_it_a_frame_at_a_time() {
        for mode in [Mode::Primary, Mode::Reference] {
            let mut day = clock();
            day.frame(FRAME, 0, mode);
            let mut last = day.frame(FRAME, 1, mode).sky;
            for camera in 2..=30 {
                let sky = day.frame(FRAME, camera, mode).sky;
                assert!((sky.time_of_day - last.time_of_day - STEP).abs() < 1e-9);
                last = sky;
            }
            assert!((day.clock().time_of_day() - (0.5 + 31.0 * STEP)).abs() < 1e-9);
        }
    }

    #[test]
    fn in_primary_a_player_standing_still_does_not_stop_the_day() {
        let mut day = clock();
        day.frame(FRAME, 7, Mode::Primary);
        let mut last = day.frame(FRAME, 7, Mode::Primary).sky;
        for _ in 0..600 {
            let drive = day.frame(FRAME, 7, Mode::Primary);
            assert!(!drive.jumped);
            assert!((drive.sky.time_of_day - last.time_of_day - STEP).abs() < 1e-9);
            last = drive.sky;
        }
        assert!((day.clock().time_of_day() - (0.5 + 602.0 * STEP)).abs() < 1e-9);
    }

    #[test]
    fn a_still_reference_view_holds_the_sky_it_shows_and_the_clock_runs_on() {
        let mut day = clock();
        day.frame(FRAME, 0, Mode::Reference);
        let held = hold(&mut day, 1);
        let before = day.clock().time_of_day();
        for _ in 0..600 {
            let drive = day.frame(FRAME, 1, Mode::Reference);
            assert_eq!(drive.sky, held);
            assert!(!drive.jumped);
        }
        let ran = day.clock().time_of_day() - before;
        assert!((ran - 600.0 * STEP).abs() < 1e-9, "{ran}");
    }

    #[test]
    fn the_sky_is_held_from_the_frame_the_scene_gate_closes_and_not_before() {
        let mut day = clock();
        // Frame n of a camera that stays has been unchanged for n frames; the scene's hold (and so
        // the sky's) begins at STILL_FRAMES.
        let shown: Vec<f64> = (0..STILL_FRAMES + 3)
            .map(|_| day.frame(FRAME, 5, Mode::Reference).sky.time_of_day)
            .collect();
        for n in 1..STILL_FRAMES as usize {
            assert!(shown[n] > shown[n - 1], "frame {n} still runs: {shown:?}");
        }
        for n in STILL_FRAMES as usize..shown.len() {
            assert_eq!(shown[n], shown[n - 1], "frame {n} is held: {shown:?}");
        }
    }

    #[test]
    fn a_view_that_moves_again_catches_the_shown_sky_up_with_the_clock() {
        let mut day = clock();
        day.frame(FRAME, 0, Mode::Reference);
        let held = hold(&mut day, 1);
        // A short stand (4 s of a half hour's day, under a degree): the catch-up is a drift.
        for _ in 0..240 {
            day.frame(FRAME, 1, Mode::Reference);
        }
        let moved = day.frame(FRAME, 2, Mode::Reference);
        assert!(!moved.jumped);
        assert_eq!(moved.sky.time_of_day, day.clock().time_of_day());
        assert!(moved.sky.time_of_day - held.time_of_day > 200.0 * STEP);
    }

    #[test]
    fn a_catch_up_beyond_the_jump_threshold_is_a_jump() {
        let mut day = clock();
        day.frame(FRAME, 0, Mode::Reference);
        hold(&mut day, 1);
        // Two minutes of a half hour's day: 24 degrees of the sun's turn at the time of noon and
        // more, well over the 2 a jump is.
        for _ in 0..(120 * 60) {
            day.frame(FRAME, 1, Mode::Reference);
        }
        let moved = day.frame(FRAME, 2, Mode::Reference);
        assert!(moved.jumped);
        assert_eq!(moved.sky.time_of_day, day.clock().time_of_day());
        // And it is one jump: the frame after is a drift again.
        assert!(!day.frame(FRAME, 3, Mode::Reference).jumped);
    }

    #[test]
    fn leaving_reference_for_primary_releases_the_hold_too() {
        let mut day = clock();
        day.frame(FRAME, 0, Mode::Reference);
        let held = hold(&mut day, 1);
        for _ in 0..30 {
            day.frame(FRAME, 1, Mode::Reference);
        }
        let primary = day.frame(FRAME, 1, Mode::Primary);
        assert_eq!(primary.sky.time_of_day, day.clock().time_of_day());
        assert!(primary.sky.time_of_day > held.time_of_day);
    }

    #[test]
    fn setting_the_clock_while_the_sky_is_held_shows_it_at_once_as_one_jump_and_then_holds_again() {
        let mut day = clock();
        day.frame(FRAME, 0, Mode::Reference);
        hold(&mut day, 7);
        day.clock_mut().set_time_of_day(0.8);
        let set = day.frame(FRAME, 7, Mode::Reference);
        assert!(set.jumped);
        assert!((set.sky.time_of_day - 0.8 - STEP).abs() < 1e-9);
        for _ in 0..10 {
            let drive = day.frame(FRAME, 7, Mode::Reference);
            assert!(!drive.jumped);
            assert_eq!(drive.sky, set.sky);
        }
    }

    #[test]
    fn a_paused_clock_does_not_run_under_a_moving_camera() {
        let mut day = clock();
        let first = day.frame(FRAME, 0, Mode::Primary);
        day.clock_mut().set_paused(true);
        for camera in 1..30 {
            assert_eq!(day.frame(FRAME, camera, Mode::Primary).sky, first.sky);
        }
    }

    #[test]
    fn a_running_clock_under_a_moving_camera_drifts_and_does_not_jump() {
        let mut day = clock();
        assert!(
            day.frame(FRAME, 0, Mode::Primary).jumped,
            "the first sky is a jump"
        );
        for camera in 1..600 {
            assert!(
                !day.frame(FRAME, camera, Mode::Primary).jumped,
                "frame {camera}"
            );
        }
    }
}
