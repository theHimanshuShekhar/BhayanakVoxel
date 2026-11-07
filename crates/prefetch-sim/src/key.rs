//! A brick's identity, packed into one word, and a fast hasher for it: the simulator keeps
//! millions of them in hash sets.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

use brick_store::BrickCoord;

/// Bits of each coordinate: the Planet's 18 km radius is 22,500 bricks of level 0.
const BITS: u32 = 20;
const MASK: u64 = (1 << BITS) - 1;
const BIAS: i32 = 1 << (BITS - 1);

/// A brick of a level: the level in the top 4 bits, then the three coordinates, biased to be
/// unsigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(u64);

impl Key {
    pub fn new(level: usize, coord: BrickCoord) -> Self {
        debug_assert!(level < 16);
        let packed = coord.0.iter().fold(0u64, |acc, &c| {
            debug_assert!((-BIAS..BIAS).contains(&c), "coordinate {c} out of range");
            acc << BITS | (c + BIAS) as u64
        });
        Key((level as u64) << (3 * BITS) | packed)
    }

    pub fn level(self) -> usize {
        (self.0 >> (3 * BITS)) as usize
    }

    pub fn coord(self) -> BrickCoord {
        let at = |axis: u32| ((self.0 >> (BITS * (2 - axis))) & MASK) as i32 - BIAS;
        BrickCoord([at(0), at(1), at(2)])
    }

    /// The brick one level coarser that holds this one.
    pub fn parent(self) -> Key {
        Key::new(self.level() + 1, self.coord().parent())
    }
}

/// Multiplicative hashing of the one word that a [`Key`] is.
#[derive(Default)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, word: u64) {
        let mixed = (self.0 ^ word).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = mixed ^ (mixed >> 32);
    }
}

pub type KeyMap<V> = HashMap<Key, V, BuildHasherDefault<KeyHasher>>;
pub type KeySet = HashSet<Key, BuildHasherDefault<KeyHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_round_trips_negative_coordinates_and_levels() {
        for level in [0, 7, 12] {
            for coord in [[0, 0, 0], [-1, 5, -22_500], [22_500, -3, 7]] {
                let key = Key::new(level, BrickCoord(coord));
                assert_eq!(key.level(), level);
                assert_eq!(key.coord(), BrickCoord(coord));
            }
        }
    }

    #[test]
    fn a_keys_parent_is_the_brick_that_holds_it() {
        let key = Key::new(2, BrickCoord([-3, 4, 5]));
        assert_eq!(key.parent(), Key::new(3, BrickCoord([-2, 2, 2])));
    }
}
