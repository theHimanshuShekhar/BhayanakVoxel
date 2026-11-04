//! Hardness-timed digging: how long the player holds the button before a Dig is made.
//!
//! The client times the hold and then sends one ordinary `EditOp`, so the network message and the
//! simulation do not know of it. The server does not check the time (see #90).

use net::EditOp;

/// Seconds of holding per unit of hardness (0 to 10 in `materials.toml`): sand (1) takes 0.15 s,
/// soil (2) 0.3 s, rock (6) 0.9 s, obsidian (8) 1.2 s.
pub const SECONDS_PER_HARDNESS: f64 = 0.15;

/// The least a Dig takes, so that a Material with no hardness (water) is not dug faster than the
/// server's 20 Edits a second.
pub const MIN_SECONDS: f64 = 0.05;

/// How long a Dig takes to make, in seconds, in a Material of this hardness. The hardness is that
/// of the Material at the Voxel aimed at: what the player points at is what they feel, it needs no
/// read of the Voxels the Brush covers, and it is the same on every frame of the hold.
pub fn dig_seconds(hardness: f32) -> f64 {
    (f64::from(hardness) * SECONDS_PER_HARDNESS).max(MIN_SECONDS)
}

/// The progress of the Dig being held.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dig {
    /// The Edit being dug towards and the seconds held on it. Aiming at another Edit starts over.
    target: Option<(EditOp, f64)>,
}

impl Dig {
    /// Holds for `dt` seconds on `aim`, the Edit now aimed at and the seconds it takes. Returns
    /// whether the Dig is done, which starts the next one. Looking away to another Edit, or at
    /// nothing, loses what was held.
    pub fn hold(&mut self, dt: f64, aim: Option<(EditOp, f64)>) -> bool {
        let Some((op, seconds)) = aim else {
            self.target = None;
            return false;
        };
        let held = match self.target {
            Some((target, held)) if target == op => held + dt,
            _ => dt,
        };
        if held >= seconds {
            self.target = None;
            true
        } else {
            self.target = Some((op, held));
            false
        }
    }

    /// Lets go of the button.
    pub fn release(&mut self) {
        self.target = None;
    }

    /// How much of the Dig is done, 0 to 1, if one is being held; `seconds` is what it takes.
    pub fn progress(&self, seconds: f64) -> Option<f32> {
        self.target
            .map(|(_, held)| (held / seconds).clamp(0.0, 1.0) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::VoxelCoord;
    use materials::MaterialTable;

    fn hardness(name: &str) -> f32 {
        MaterialTable::built_in().by_name(name).unwrap().hardness
    }

    fn at(x: i32) -> EditOp {
        EditOp::dig_sphere(VoxelCoord([x, 0, 0]), 10)
    }

    #[test]
    fn the_time_scales_with_the_hardness() {
        let (sand, soil, rock, obsidian) = (
            dig_seconds(hardness("sand")),
            dig_seconds(hardness("soil")),
            dig_seconds(hardness("rock")),
            dig_seconds(hardness("obsidian")),
        );
        assert!(sand < soil && soil < rock && rock < obsidian);
        // In proportion: twice as hard takes twice as long.
        assert!((dig_seconds(4.0) - 2.0 * dig_seconds(2.0)).abs() < 1e-12);
        assert!((rock / soil - 3.0).abs() < 1e-9);
        assert!((obsidian - 1.2).abs() < 1e-9);
    }

    #[test]
    fn a_material_with_no_hardness_still_takes_the_least_time() {
        assert_eq!(dig_seconds(hardness("water")), MIN_SECONDS);
        assert_eq!(dig_seconds(0.0), MIN_SECONDS);
    }

    #[test]
    fn the_dig_is_made_once_it_has_been_held_for_the_time() {
        let mut dig = Dig::default();
        let aim = Some((at(0), 1.0));
        assert!(!dig.hold(0.4, aim));
        assert_eq!(dig.progress(1.0), Some(0.4));
        assert!(!dig.hold(0.4, aim));
        assert!(dig.hold(0.4, aim), "1.2 s of 1 s");
        // The next one starts from nothing.
        assert_eq!(dig.progress(1.0), None);
        assert!(!dig.hold(0.4, aim));
    }

    #[test]
    fn harder_material_needs_more_frames_of_the_same_hold() {
        let frames = |hardness: f32| {
            let mut dig = Dig::default();
            (1..)
                .find(|_| dig.hold(1.0 / 60.0, Some((at(0), dig_seconds(hardness)))))
                .unwrap()
        };
        assert!(frames(hardness("sand")) < frames(hardness("rock")));
        assert!(frames(hardness("rock")) < frames(hardness("obsidian")));
    }

    #[test]
    fn aiming_elsewhere_or_at_nothing_or_letting_go_starts_over() {
        let mut dig = Dig::default();
        dig.hold(0.5, Some((at(0), 1.0)));
        assert!(!dig.hold(0.6, Some((at(1), 1.0))), "another Edit");
        assert_eq!(dig.progress(1.0), Some(0.6));
        assert!(!dig.hold(0.1, None));
        assert_eq!(dig.progress(1.0), None);
        dig.hold(0.9, Some((at(1), 1.0)));
        dig.release();
        assert!(!dig.hold(0.2, Some((at(1), 1.0))));
    }

    #[test]
    fn the_least_dig_is_made_once_the_least_time_has_been_held() {
        let mut dig = Dig::default();
        assert!(!dig.hold(MIN_SECONDS / 2.0, Some((at(0), MIN_SECONDS))));
        assert!(dig.hold(MIN_SECONDS / 2.0, Some((at(0), MIN_SECONDS))));
    }
}
