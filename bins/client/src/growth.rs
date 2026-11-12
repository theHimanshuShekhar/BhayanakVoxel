//! Watching that the tables made roomy up front stay that size. A table that grows in the middle of
//! a frame moves every entry (tens to hundreds of milliseconds, 0.9 s for the selection's nodes), and a scene that runs out of words grows
//! that buffer (a new buffer on the GPU and a copy of the old, and the frame thread's copy made
//! longer), so either is a sign that `room_for` and `expected_words` in `main.rs` are too small. The client logs one to stderr, at most once in a
//! while for each kind; the full-size test asserts there is none.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use brick_store::TableSize;
use render::Rebuild;

use crate::world::World;

/// How often a kind of growth is logged at the most.
const LOG_EVERY: Duration = Duration::from_secs(30);

/// How often the tables are looked at: the scene's words cost a walk over its free ranges.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// The tables of `after` that have room for more than `before` says they have ever had, as
/// `(name, was, is)`; `before` is kept with [`raised`]. A table that `before` does not have is
/// new, not grown. It has to be more than ever: a hash table's room reads less while it holds
/// deleted entries, and is what it was when it has cleaned them out in its place (which costs
/// a rehash, but allocates nothing). The regions each level lists are not tables that were made
/// roomy: a scene fits them again whenever it is made again, so they are not looked at.
pub fn grown(before: &[TableSize], after: &[TableSize]) -> Vec<(String, usize, usize)> {
    after
        .iter()
        .filter(|now| !now.name.ends_with(" regions"))
        .filter_map(|now| {
            let was = before.iter().find(|t| t.name == now.name)?;
            (now.capacity > was.capacity).then(|| (now.name.clone(), was.capacity, now.capacity))
        })
        .collect()
}

/// `after`, with each table's room raised to the most it has had in `before`.
pub fn raised(before: &[TableSize], mut after: Vec<TableSize>) -> Vec<TableSize> {
    for now in &mut after {
        if let Some(was) = before.iter().find(|t| t.name == now.name) {
            now.capacity = now.capacity.max(was.capacity);
        }
    }
    after
}

/// Lets each kind of message through once, and then again only after `interval`, counting what it
/// held back.
#[derive(Debug)]
pub struct RateLimit {
    interval: Duration,
    /// For each kind: when it was last let through, and how many have been held back since.
    kinds: HashMap<String, (Instant, usize)>,
}

impl RateLimit {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            kinds: HashMap::new(),
        }
    }

    /// Whether a message of `kind` at `now` is to be logged: `Some` of how many of its kind were
    /// held back since the last one, or `None` if this one is.
    pub fn allow(&mut self, kind: &str, now: Instant) -> Option<usize> {
        match self.kinds.get_mut(kind) {
            None => {
                self.kinds.insert(kind.to_owned(), (now, 0));
                Some(0)
            }
            Some((at, held)) if now.duration_since(*at) >= self.interval => {
                let was = *held;
                (*at, *held) = (now, 0);
                Some(was)
            }
            Some((_, held)) => {
                *held += 1;
                None
            }
        }
    }
}

/// Logs to stderr when a table of the client's grows or the scene is packed again, so that a
/// playtest shows it.
#[derive(Debug)]
pub struct GrowthWatch {
    /// The most room each table has had when last looked at (see [`raised`]).
    highest: Vec<TableSize>,
    /// Set by a rebuild: the tables were replaced, so the next look is their new starting point
    /// and not compared with the ones before.
    rebased: bool,
    /// Whether the bias was changed: the store holds more bricks at a finer bias, so its tables
    /// grow, which is expected.
    bias_changed: bool,
    checked: Option<Instant>,
    log: RateLimit,
}

impl Default for GrowthWatch {
    fn default() -> Self {
        Self {
            highest: Vec::new(),
            rebased: false,
            bias_changed: false,
            checked: None,
            log: RateLimit::new(LOG_EVERY),
        }
    }
}

impl GrowthWatch {
    /// Looks at the tables, if it has been a second since it last did, and logs those that have
    /// grown. The first look only takes their sizes.
    pub fn check(&mut self, world: &World, now: Instant) {
        if self
            .checked
            .is_some_and(|at| now.duration_since(at) < CHECK_EVERY)
        {
            return;
        }
        self.checked = Some(now);
        for message in self.observe(world.table_sizes(), now) {
            eprintln!("{message}");
        }
    }

    /// Takes the sizes the tables have now, and returns what is to be logged of the growth since
    /// the last ones.
    fn observe(&mut self, sizes: Vec<TableSize>, now: Instant) -> Vec<String> {
        let grown = if self.rebased {
            self.rebased = false;
            self.highest = Vec::new();
            Vec::new()
        } else {
            grown(&self.highest, &sizes)
        };
        self.highest = raised(&self.highest, sizes);
        let tail = if self.bias_changed {
            "expected after a change of bias, which holds more bricks"
        } else {
            "the reservation is too small"
        };
        grown
            .into_iter()
            .filter_map(|(name, was, is)| {
                self.say(&name, now, |held| {
                    format!(
                        "table grew: {name} had room for {was} entries and now has {is} \
                         ({tail}){held}"
                    )
                })
            })
            .collect()
    }

    /// Logs that the scene was packed again and uploaded in full, and why if it is known. Both
    /// reasons are expected: running out of room grows a buffer, and does not pack again.
    pub fn rebuilt(&mut self, why: Option<Rebuild>, now: Instant) {
        if let Some(message) = self.rebuilt_message(why, now) {
            eprintln!("{message}");
        }
    }

    fn rebuilt_message(&mut self, why: Option<Rebuild>, now: Instant) -> Option<String> {
        // The tables the scene made again has are new ones, and the next look starts from them.
        self.rebased = true;
        self.checked = None;
        let (kind, what) = match why {
            Some(Rebuild::Bands) => {
                self.bias_changed = true;
                ("rebuilt for bias", "for a new bias, as expected")
            }
            Some(Rebuild::Jump) => (
                "rebuilt after jump",
                "after the camera jumped further than the rings follow, as expected",
            ),
            None => ("rebuilt", "for a reason that is not known"),
        };
        self.say(kind, now, |held| {
            format!("scene rebuilt {what}: packed again and uploaded in full{held}")
        })
    }

    fn say(
        &mut self,
        kind: &str,
        now: Instant,
        message: impl FnOnce(&str) -> String,
    ) -> Option<String> {
        let held = self.log.allow(kind, now)?;
        let held = match held {
            0 => String::new(),
            n => format!(" ({n} more since the last report)"),
        };
        Some(message(&held))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(name: &str, len: usize, capacity: usize) -> TableSize {
        TableSize::new(name, len, capacity)
    }

    #[test]
    fn a_table_with_more_room_than_before_is_grown_and_a_new_or_fuller_one_is_not() {
        let before = [size("a", 5, 10), size("b", 5, 10), size("c", 1, 10)];
        let after = [
            size("a", 9, 10),
            size("b", 11, 20),
            size("c", 1, 10),
            size("d", 1, 99),
        ];
        assert_eq!(grown(&before, &after), [("b".to_owned(), 10, 20)]);
    }

    #[test]
    fn room_that_reads_less_and_then_the_same_again_is_not_a_growth() {
        // A hash table with deleted entries reads less room, and the same after it cleaned out.
        let mut seen = vec![size("a", 5, 100)];
        for room in [80, 100, 70, 100] {
            let now = vec![size("a", 5, room)];
            assert!(grown(&seen, &now).is_empty(), "{room}");
            seen = raised(&seen, now);
        }
        assert_eq!(seen[0].capacity, 100);
        assert_eq!(grown(&seen, &[size("a", 5, 200)]).len(), 1);
    }

    #[test]
    fn growth_after_a_rebuild_is_seen_against_the_tables_the_rebuild_made() {
        let now = Instant::now();
        let mut watch = GrowthWatch::default();
        // The first look is the starting point; growth after it is reported.
        assert!(watch.observe(vec![size("nodes", 5, 100)], now).is_empty());
        let grew = watch.observe(vec![size("nodes", 90, 200)], now);
        assert_eq!(grew.len(), 1);
        assert!(grew[0].contains("reservation is too small"), "{grew:?}");
        // A rebuild replaces the tables, with less room than the highest there was before: that
        // is the new starting point, not a growth, and growing from it is one.
        assert!(watch.rebuilt_message(Some(Rebuild::Jump), now).is_some());
        assert!(watch.observe(vec![size("nodes", 5, 100)], now).is_empty());
        let later = now + Duration::from_secs(100);
        assert_eq!(watch.observe(vec![size("nodes", 90, 200)], later).len(), 1);
    }

    #[test]
    fn a_rebuild_says_why_and_both_are_expected() {
        let now = Instant::now();
        let mut watch = GrowthWatch::default();
        let message = |watch: &mut GrowthWatch, why| watch.rebuilt_message(why, now).unwrap();
        assert!(message(&mut watch, Some(Rebuild::Jump)).contains("jumped"));
        assert!(message(&mut watch, Some(Rebuild::Bands)).contains("new bias, as expected"));
        // After a change of bias the store's tables are expected to grow.
        watch.observe(vec![size("store", 5, 100)], now);
        let grew = watch.observe(vec![size("store", 90, 200)], now);
        assert!(
            grew[0].contains("expected after a change of bias"),
            "{grew:?}"
        );
    }

    #[test]
    fn the_regions_a_scene_fits_again_are_not_reported_as_growth() {
        let before = [size("scene level 3 regions", 5, 100)];
        let after = [size("scene level 3 regions", 5, 400)];
        assert!(grown(&before, &after).is_empty());
    }

    #[test]
    fn a_kind_is_logged_once_and_again_only_after_the_interval_with_the_ones_held_back_counted() {
        let start = Instant::now();
        let mut limit = RateLimit::new(Duration::from_secs(30));
        assert_eq!(limit.allow("a", start), Some(0));
        // Held back for the whole interval, however often it happens.
        assert_eq!(limit.allow("a", start + Duration::from_secs(1)), None);
        assert_eq!(limit.allow("a", start + Duration::from_secs(29)), None);
        // Another kind has its own turn.
        assert_eq!(limit.allow("b", start + Duration::from_secs(2)), Some(0));
        assert_eq!(limit.allow("a", start + Duration::from_secs(30)), Some(2));
        // The interval is counted from the last one let through.
        assert_eq!(limit.allow("a", start + Duration::from_secs(31)), None);
        assert_eq!(limit.allow("a", start + Duration::from_secs(60)), Some(1));
    }
}
