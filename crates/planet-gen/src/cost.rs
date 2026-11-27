//! An artificial cost per generated brick, for the bench (#242): how much per-brick generation
//! time the streaming gates can afford is measured by adding some and watching the gates, before
//! the terrain that costs it is written (#238, #247).
//!
//! **Nothing here changes what is generated.** [`ExtraCost::burn`] is called as a brick's
//! generation starts and only spends CPU time; the brick, the counts of bricks made and the
//! order are the same with it as without it. A [`crate::planet::Planet`] has none unless
//! [`crate::planet::Planet::with_extra_cost`] gave it one, and the game never does.
//!
//! **What a brick is charged.** The same for every brick, whatever its level: a brick of any level
//! evaluates the same 8 x 8 x 8 Voxels, and the generator's own cost per brick is about the same
//! at every level the loader makes (measured with `gen_rate`, `docs/measurements.md`: 50 µs at
//! level 0 and 83 to 131 µs at levels 1 to 7, so within a factor of 2.6 and not growing or
//! shrinking with the level). So `--gen-extra-us N` is N µs for each brick that the generator is
//! asked for, at level 0 and at every coarser level, whether the brick turns out to be air, one
//! layer or mixed (a new terrain's noise is evaluated before any of that is known; the sky and
//! interior shortcuts of today's generator decide a brick in a few µs, which the charge does not
//! follow). It is charged wherever generation runs: the loader's workers, the spawn ring and the
//! Guard's cells made at the load, and a read of the store's source on the frame thread.
//!
//! **How it is spent.** As CPU work, not a sleep, so that it occupies the thread as noise
//! evaluation would: a loop of a hash that depends on its own last value (so that it can be
//! neither skipped nor run in parallel by the CPU), its result passed to `black_box` so that the
//! compiler cannot drop it. The number of rounds for a µs is measured once per process
//! ([`rounds_per_micro`]), and the time each charge really took, wall clock, is added up, so that
//! the report can say what the hook did ([`ExtraCost::stats`]): the figure under load is a little
//! over what was asked, when the thread was preempted in the middle of a charge.

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// One round of the spin: a splitmix64 step of the last value. Each round depends on the one
/// before, so the loop's time is its number of rounds, whatever the CPU does with its pipeline.
#[inline(always)]
fn round(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// `rounds` rounds from `seed`: the work of the spin, deterministic in its two arguments.
#[inline(never)]
fn spin(seed: u64, rounds: u64) -> u64 {
    let mut state = seed;
    for _ in 0..rounds {
        state = round(state);
    }
    state
}

/// Rounds that make one calibration sample: about 0.2 ms of spinning.
const SAMPLE_ROUNDS: u64 = 100_000;
/// Samples taken, of which the median is the figure (a sample hit by a preemption is slow, one on
/// a core in a turbo burst is fast; the middle one is neither).
const SAMPLES: usize = 15;

/// How many rounds of the spin take a microsecond on this machine, measured once on the first
/// call, with the thread that asks: the median of [`SAMPLES`] samples of [`SAMPLE_ROUNDS`] rounds.
pub fn rounds_per_micro() -> f64 {
    static RATE: OnceLock<f64> = OnceLock::new();
    *RATE.get_or_init(|| {
        // A first sample to warm the clock and the code, not counted.
        black_box(spin(1, SAMPLE_ROUNDS));
        let mut rates: Vec<f64> = (0..SAMPLES as u64)
            .map(|k| {
                let began = Instant::now();
                black_box(spin(black_box(k), SAMPLE_ROUNDS));
                SAMPLE_ROUNDS as f64 / (began.elapsed().as_secs_f64() * 1e6).max(1e-9)
            })
            .collect();
        rates.sort_by(f64::total_cmp);
        rates[rates.len() / 2]
    })
}

/// What an [`ExtraCost`] has done so far.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtraCostStats {
    /// The cost asked for each brick, in µs.
    pub asked_us: u32,
    /// Bricks that generation was started for, with the cost or without (0 asked).
    pub bricks: u64,
    /// Wall-clock time spent in the charges, summed over the threads.
    pub spent: Duration,
    /// `spent` over `bricks`, in µs: what a charge took, 0 when nothing was asked or no brick has
    /// been made.
    pub measured_us: f64,
}

/// The extra cost of each generated brick: a setting, and the counts of what it spent. Shared by
/// every clone of the Planet it was given to, and by every thread that generates for it.
#[derive(Debug)]
pub struct ExtraCost {
    asked_us: u32,
    /// Rounds of the spin that make `asked_us`.
    rounds: u64,
    bricks: AtomicU64,
    spent_ns: AtomicU64,
}

impl ExtraCost {
    /// A cost of `micros` µs for each brick. The first one made in a process calibrates the spin
    /// (about 3 ms), unless it is 0.
    pub fn new(micros: u32) -> Arc<ExtraCost> {
        let rounds = if micros == 0 {
            0
        } else {
            (f64::from(micros) * rounds_per_micro()).round() as u64
        };
        Arc::new(ExtraCost {
            asked_us: micros,
            rounds,
            bricks: AtomicU64::new(0),
            spent_ns: AtomicU64::new(0),
        })
    }

    /// The cost of each brick that was asked for, in µs.
    pub fn asked_us(&self) -> u32 {
        self.asked_us
    }

    /// Charges one brick: spends the cost on the calling thread. Called as a brick's generation
    /// starts. Makes no difference to what is generated.
    pub fn burn(&self) {
        let n = self.bricks.fetch_add(1, Ordering::Relaxed);
        if self.rounds == 0 {
            return;
        }
        let began = Instant::now();
        // The seed is the brick's number, which the compiler does not know: the work is not a
        // constant, and its result is used.
        black_box(spin(black_box(n), self.rounds));
        let ns = u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.spent_ns.fetch_add(ns, Ordering::Relaxed);
    }

    /// What has been charged so far.
    pub fn stats(&self) -> ExtraCostStats {
        let bricks = self.bricks.load(Ordering::Relaxed);
        let spent = Duration::from_nanos(self.spent_ns.load(Ordering::Relaxed));
        ExtraCostStats {
            asked_us: self.asked_us,
            bricks,
            spent,
            measured_us: if self.rounds == 0 || bricks == 0 {
                0.0
            } else {
                spent.as_secs_f64() * 1e6 / bricks as f64
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spin_is_a_function_of_its_seed_and_rounds() {
        assert_eq!(spin(7, 1000), spin(7, 1000));
        assert_ne!(spin(7, 1000), spin(8, 1000));
        assert_ne!(spin(7, 1000), spin(7, 1001));
        assert_eq!(spin(7, 0), 7);
    }

    #[test]
    fn a_charge_of_nothing_counts_the_brick_and_spends_nothing() {
        let cost = ExtraCost::new(0);
        for _ in 0..10 {
            cost.burn();
        }
        let stats = cost.stats();
        assert_eq!((stats.asked_us, stats.bricks), (0, 10));
        assert_eq!((stats.spent, stats.measured_us), (Duration::ZERO, 0.0));
    }

    #[test]
    fn a_charge_takes_about_the_time_asked_on_this_machine() {
        // Calibrated by the machine's own median, so a charge on a quiet core takes what was asked
        // within a few per cent. On a shared one (CI, a test run beside a build) a charge that is
        // preempted takes far longer, and one cannot take less: so the test reads the median of the
        // charges' own times, which a few preemptions do not move, and holds the mean (what the
        // report says) only from below.
        for micros in [25, 100, 400] {
            let cost = ExtraCost::new(micros);
            let mut times: Vec<f64> = (0..101)
                .map(|_| {
                    let began = Instant::now();
                    cost.burn();
                    began.elapsed().as_secs_f64() * 1e6
                })
                .collect();
            times.sort_by(f64::total_cmp);
            let median = times[times.len() / 2] / f64::from(micros);
            assert!(
                (0.6..=1.6).contains(&median),
                "asked {micros} µs, the median charge took {:.2}x",
                median
            );
            let stats = cost.stats();
            assert_eq!(stats.bricks, 101);
            assert!(stats.measured_us >= 0.6 * f64::from(micros), "{stats:?}");
        }
    }

    #[test]
    fn the_calibration_is_stable_and_plausible() {
        let rate = rounds_per_micro();
        // A round is a few ns of dependent multiplies: tens to a few hundred to the µs.
        assert!((5.0..=2000.0).contains(&rate), "{rate} rounds a µs");
        assert_eq!(rate, rounds_per_micro(), "measured once");
    }
}
