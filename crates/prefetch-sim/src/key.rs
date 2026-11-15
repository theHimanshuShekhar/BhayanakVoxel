//! A brick's identity, packed into one word, and a fast hasher for it: the simulator keeps
//! millions of them in hash sets.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

use brick_store::BrickCoord;

/// A brick's identity, packed into one 128-bit word: the level in the top bits, then the three
/// coordinates as the 32-bit integers they are (biased to be unsigned). A 320 km Planet has 800 000
/// bricks of level 0 from its centre to its surface, which 20 bits an axis (the packing's old
/// width, for a 16 km Planet in one 64-bit word) cannot hold; a whole `i32` an axis can, so no
/// Planet overflows it.
const BITS: u32 = 32;
const MASK: u128 = (1 << BITS) - 1;

/// A brick of a level: the level in the top bits, then the three coordinates, biased to be
/// unsigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(u128);

impl Key {
    pub fn new(level: usize, coord: BrickCoord) -> Self {
        debug_assert!(level < 16);
        let packed = coord.0.iter().fold(0u128, |acc, &c| {
            acc << BITS | u128::from((c as u32) ^ (1 << (BITS - 1)))
        });
        Key((level as u128) << (3 * BITS) | packed)
    }

    pub fn level(self) -> usize {
        (self.0 >> (3 * BITS)) as usize
    }

    pub fn coord(self) -> BrickCoord {
        let at = |axis: u32| {
            (((self.0 >> (BITS * (2 - axis))) & MASK) as u32 ^ (1 << (BITS - 1))) as i32
        };
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

    fn write_u128(&mut self, word: u128) {
        self.write_u64(word as u64);
        self.write_u64((word >> 64) as u64);
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
            for coord in [
                [0, 0, 0],
                [-1, 5, -22_500],
                [22_500, -3, 7],
                // The 320 km Planet's extreme brick, 800 000 from the centre: past 20 bits an axis.
                [-800_000, 800_000, 1_048_577],
                [i32::MIN, i32::MAX, 0],
            ] {
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
