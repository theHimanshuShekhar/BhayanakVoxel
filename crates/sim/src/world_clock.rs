//! The world clock (#203): the time of day of the Planet, as a plain value.
//!
//! The clock is four numbers and nothing else, so that it can be copied into a message as it is:
//! in M1 the client holds it, and in M2 the server does and sends it (a client then only reads it,
//! and shows the time the server says). Nothing here knows the sun: the sky's model
//! (`render::daylight`) turns a time of day into where the sun and the moon are and how bright the
//! night is.
//!
//! **A day is a fraction.** [`WorldClock::time_of_day`] is in `[0, 1)`: 0 is midnight, 0.25 sunrise,
//! 0.5 noon, 0.75 sunset (the sky's model keeps to that). The day's length in seconds sets how fast
//! the fraction runs, so changing the length never moves the sun.
//!
//! **Setting is not running.** [`WorldClock::advance`] moves the time by the seconds that passed
//! and does nothing while the clock is paused. [`WorldClock::set_time_of_day`] puts the time
//! anywhere and counts the set in [`WorldClock::epoch`]: a reader that sees the epoch change knows
//! that the sun was *put* somewhere, not that it moved (the renderer's light caches forget at
//! once for the one and follow the other, `render::daylight::Daylight`). A step of `advance` is
//! never counted, however big: the reader tells a big one by how far the sun went.

/// The day's length in seconds when nothing says otherwise: half an hour (#138).
pub const DEFAULT_DAY_LENGTH_SECONDS: f64 = 1800.0;

/// The time of day of the Planet, its pace, whether it is stopped, and how many times it has been
/// set. A plain value: `Copy`, comparable, with no reference to the machine's time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldClock {
    time_of_day: f64,
    day_length_seconds: f64,
    paused: bool,
    epoch: u32,
}

impl WorldClock {
    /// A running clock at `time_of_day` (wrapped into `[0, 1)`), with a day of `day_length_seconds`
    /// (at least a millisecond: a length that is not is the default).
    pub fn new(time_of_day: f64, day_length_seconds: f64) -> Self {
        Self {
            time_of_day: wrap(time_of_day),
            day_length_seconds: valid_length(day_length_seconds),
            paused: false,
            epoch: 0,
        }
    }

    /// The same clock, stopped.
    pub fn paused(mut self) -> Self {
        self.paused = true;
        self
    }

    /// The fraction of the day that has gone, in `[0, 1)`.
    pub fn time_of_day(&self) -> f64 {
        self.time_of_day
    }

    pub fn day_length_seconds(&self) -> f64 {
        self.day_length_seconds
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// How many times the time has been set. It wraps and nothing else moves it.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// How much of the day passes in a second of a running clock.
    pub fn rate_per_second(&self) -> f64 {
        1.0 / self.day_length_seconds
    }

    /// Moves the time on by `seconds` (a negative or not-finite span is none), wrapping at the end
    /// of the day. Does nothing while the clock is paused.
    pub fn advance(&mut self, seconds: f64) {
        if self.paused || !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        self.time_of_day = wrap(self.time_of_day + seconds / self.day_length_seconds);
    }

    /// Puts the time at `time_of_day` (wrapped into `[0, 1)`; not-finite is midnight) and counts the
    /// set. A paused clock stays paused.
    pub fn set_time_of_day(&mut self, time_of_day: f64) {
        self.time_of_day = wrap(time_of_day);
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Stops the clock, or runs it again from where it is.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Sets the day's length in seconds. The time stays where it is, so the sun does not move; only
    /// how fast it goes changes. A length that is not at least a millisecond is the default.
    pub fn set_day_length_seconds(&mut self, seconds: f64) {
        self.day_length_seconds = valid_length(seconds);
    }
}

impl Default for WorldClock {
    /// A running clock at noon with the default day.
    fn default() -> Self {
        Self::new(0.5, DEFAULT_DAY_LENGTH_SECONDS)
    }
}

/// `time` wrapped into `[0, 1)`; not-finite is 0.
fn wrap(time: f64) -> f64 {
    if !time.is_finite() {
        return 0.0;
    }
    let wrapped = time.rem_euclid(1.0);
    // `rem_euclid` of a tiny negative number rounds to exactly 1.0.
    if wrapped >= 1.0 { 0.0 } else { wrapped }
}

fn valid_length(seconds: f64) -> f64 {
    if seconds.is_finite() && seconds >= 0.001 {
        seconds
    } else {
        DEFAULT_DAY_LENGTH_SECONDS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_a_running_noon_of_half_an_hour() {
        let clock = WorldClock::default();
        assert_eq!(clock.time_of_day(), 0.5);
        assert_eq!(clock.day_length_seconds(), 1800.0);
        assert!(!clock.is_paused());
        assert_eq!(clock.epoch(), 0);
    }

    #[test]
    fn a_running_clock_goes_a_day_in_a_day_length() {
        let mut clock = WorldClock::new(0.0, 600.0);
        clock.advance(150.0);
        assert!((clock.time_of_day() - 0.25).abs() < 1e-12);
        clock.advance(450.0);
        // A whole day later it is where it started, wrapped.
        assert!(clock.time_of_day().abs() < 1e-12, "{}", clock.time_of_day());
    }

    #[test]
    fn the_day_wraps_and_stays_in_zero_to_one() {
        let mut clock = WorldClock::new(0.9, 100.0);
        clock.advance(25.0);
        assert!((clock.time_of_day() - 0.15).abs() < 1e-12);
        // Many days in one step land on the same fraction.
        clock.advance(100.0 * 7.0);
        assert!((clock.time_of_day() - 0.15).abs() < 1e-9);
        for t in [1.0, 2.5, -0.25, -3.0, 1e-300, -1e-20] {
            let c = WorldClock::new(t, 100.0);
            assert!((0.0..1.0).contains(&c.time_of_day()), "{t}");
        }
        assert!((WorldClock::new(-0.25, 100.0).time_of_day() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn a_paused_clock_does_not_advance_and_a_resumed_one_goes_on_from_where_it_was() {
        let mut clock = WorldClock::new(0.3, 60.0).paused();
        clock.advance(30.0);
        assert_eq!(clock.time_of_day(), 0.3);
        clock.set_paused(false);
        clock.advance(6.0);
        assert!((clock.time_of_day() - 0.4).abs() < 1e-12);
    }

    #[test]
    fn setting_puts_the_time_where_it_is_told_counts_the_set_and_keeps_the_pause() {
        let mut clock = WorldClock::new(0.1, 60.0).paused();
        clock.set_time_of_day(0.72);
        assert_eq!(clock.time_of_day(), 0.72);
        assert_eq!(clock.epoch(), 1);
        assert!(clock.is_paused());
        clock.set_time_of_day(1.25);
        assert_eq!(clock.time_of_day(), 0.25);
        assert_eq!(clock.epoch(), 2);
        // The same time set again is still a set: the reader decides what a set of it is.
        clock.set_time_of_day(0.25);
        assert_eq!(clock.epoch(), 3);
    }

    #[test]
    fn running_does_not_count_as_setting() {
        let mut clock = WorldClock::new(0.0, 10.0);
        for _ in 0..1000 {
            clock.advance(0.5);
        }
        assert_eq!(clock.epoch(), 0);
    }

    #[test]
    fn the_day_length_changes_the_pace_and_not_the_time() {
        let mut clock = WorldClock::new(0.4, 1800.0);
        clock.set_day_length_seconds(60.0);
        assert_eq!(clock.time_of_day(), 0.4);
        assert_eq!(clock.epoch(), 0);
        assert!((clock.rate_per_second() - 1.0 / 60.0).abs() < 1e-15);
        clock.advance(30.0);
        assert!((clock.time_of_day() - 0.9).abs() < 1e-12);
    }

    #[test]
    fn a_length_that_cannot_be_a_day_is_the_default() {
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY, 1e-9] {
            assert_eq!(
                WorldClock::new(0.0, bad).day_length_seconds(),
                DEFAULT_DAY_LENGTH_SECONDS,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_span_that_is_negative_or_not_a_number_moves_nothing() {
        let mut clock = WorldClock::new(0.3, 60.0);
        for seconds in [-10.0, f64::NAN, f64::INFINITY, 0.0] {
            clock.advance(seconds);
            assert_eq!(clock.time_of_day(), 0.3, "{seconds}");
        }
        clock.set_time_of_day(f64::NAN);
        assert_eq!(clock.time_of_day(), 0.0);
    }

    #[test]
    fn a_copy_is_the_same_clock() {
        // What a message carries: the whole state is in the value.
        let mut a = WorldClock::new(0.6, 300.0);
        a.set_time_of_day(0.61);
        let mut b = a;
        a.advance(1.0);
        b.advance(1.0);
        assert_eq!(a, b);
    }
}
