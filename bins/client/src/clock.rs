//! The time that the loader's and the scene thread's code reads (#180, #181).
//!
//! Streaming code asks the time for its holds (the coarsening's, the wait for an Edit), for how long
//! a slice of work has gone on, and for how often something is looked at. It reads a [`Clock`]
//! instead of the machine's, so that a driver that runs the code a step at a time (see
//! [`crate::loader::Loader::step`] and [`crate::scene_worker::SceneWorker::step`]: the tests, and
//! the streaming mode of `client bench`) can say what time it is and move it on, and get the same
//! run every time.
//!
//! **What the game pays.** A real clock is a clock with no stepped time in it: `now` is
//! `Instant::now` and `pause` is `thread::sleep`, as before, with a test of whether the clock is
//! stepped in front, a null check of a field that is never set in the game, which the compiler lays
//! out as the branch that is not taken (the stepped arm is `#[cold]` and out of line). Stepping
//! needs the stepped arm to be in the shipped binary, since `client bench` is: no feature or
//! `cfg` can leave it out of the one binary that has both.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The time of the machine, or a time that only a driver moves.
#[derive(Clone, Debug)]
pub struct Clock {
    stepped: Option<Arc<Stepped>>,
}

/// A clock that is where it was left: an instant to count from and how far it has been moved.
#[derive(Debug)]
struct Stepped {
    from: Instant,
    nanos: AtomicU64,
}

impl Stepped {
    #[cold]
    #[inline(never)]
    fn now(&self) -> Instant {
        self.from + Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }
}

impl Clock {
    /// The machine's clock.
    #[inline]
    pub fn real() -> Self {
        Self { stepped: None }
    }

    /// A clock that stands still until [`Clock::advance`]d, and is the same for every copy of it.
    pub fn stepped() -> Self {
        Self {
            stepped: Some(Arc::new(Stepped {
                from: Instant::now(),
                nanos: AtomicU64::new(0),
            })),
        }
    }

    /// Whether this is a stepped clock.
    #[cfg(test)]
    pub fn is_stepped(&self) -> bool {
        self.stepped.is_some()
    }

    /// Moves a stepped clock on by `time`. The machine's clock is not moved.
    pub fn advance(&self, time: Duration) {
        let stepped = self
            .stepped
            .as_ref()
            .expect("only a stepped clock is moved");
        let nanos = u64::try_from(time.as_nanos()).expect("a time that fits");
        stepped.nanos.fetch_add(nanos, Ordering::Relaxed);
    }

    /// The time now.
    #[inline]
    pub fn now(&self) -> Instant {
        match &self.stepped {
            None => Instant::now(),
            Some(stepped) => stepped.now(),
        }
    }

    /// How long it is since `then`.
    #[inline]
    pub fn since(&self, then: Instant) -> Duration {
        self.now().saturating_duration_since(then)
    }

    /// Lets `time` go by with nothing done, so that another thread can have a lock. A pause on a
    /// stepped clock is not waited: the clock is moved on by it, so what the pause was for (time to
    /// go by) has happened.
    #[inline]
    pub fn pause(&self, time: Duration) {
        match &self.stepped {
            None => std::thread::sleep(time),
            Some(_) => self.advance(time),
        }
    }

    /// The store's deadline for a job that is to let go of its lock at `at`. A stepped job has
    /// nobody to let go of a lock for, so it has no deadline and runs to its end. The store reads
    /// the machine's clock for a deadline, which is not this one.
    #[inline]
    pub fn deadline(&self, at: Instant) -> Option<Instant> {
        match &self.stepped {
            None => Some(at),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stepped_clock_moves_only_when_it_is_advanced_and_a_pause_on_it_is_time_passing() {
        let clock = Clock::stepped();
        let begun = clock.now();
        assert_eq!(clock.now(), begun, "it does not run by itself");
        clock.advance(Duration::from_millis(300));
        assert_eq!(clock.since(begun), Duration::from_millis(300));
        // A pause is not waited out: the clock says that it was.
        clock.pause(Duration::from_micros(50));
        assert_eq!(clock.since(begun), Duration::from_micros(300_050));
        // What was begun later reads from the same time, whoever holds it.
        let other = clock.clone();
        other.advance(Duration::from_secs(1));
        assert_eq!(clock.since(begun), Duration::from_micros(1_300_050));
        // A deadline is for a lock that is let go of when it passes: a stepped job has no one to let go
        // for, and runs to its end.
        assert_eq!(clock.deadline(clock.now() + Duration::from_millis(1)), None);
    }

    #[test]
    fn the_real_clock_is_the_time_of_the_machine() {
        let clock = Clock::real();
        let begun = clock.now();
        clock.pause(Duration::from_millis(2));
        assert!(clock.since(begun) >= Duration::from_millis(2));
        let at = clock.now() + Duration::from_secs(60);
        assert_eq!(clock.deadline(at), Some(at));
    }
}
