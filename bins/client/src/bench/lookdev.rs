//! The look-dev shot list (#148, from #137): the places on the 320 km Planet that the look-dev
//! flythrough visits, as a recorded bench camera path, one [`Shot`] each, modelled on Crystal
//! Islands' shot list: a forest floor, a canyon with haze, a dark descent, two crystal caves of
//! different dominant hue, a sunlit cliff, a meadow, and a pull-out to the Planet.
//!
//! The shots are cuts, not one flight: the places are kilometres apart, so each shot is its own
//! little [`CameraPath`] (two to four keyframes, with the roll of the debug free camera where the
//! shot tilts), and a consumer renders each from its own piece of the Planet.
//!
//! **The places are recorded, not searched for.** They were found on the seeded Planet by
//! `finder::record_the_look_dev_sites` (an ignored test: it takes the 25 s that the world map
//! does) and are held as numbers in `shots.rs`, so a render of them does not depend on a search.
//! Voxel positions are from the Planet's centre. They are only the places of generator version
//! [`RECORDED_FOR`]: when the generator changes the places move, a test says so, and the finder
//! is run again.
//!
//! **No stamp code lives here.** Where M1b's content does not exist yet (crystals, vegetation),
//! a shot names the [`Place`]s for hand-placed stamps: what kind, and the empty Voxel that touches
//! a surface and the way that surface faces. What a stamp is made of is for whoever places one.

// PROTOTYPE (#148) code, thrown away: AgX's published constants, and a plain save().
#![allow(clippy::excessive_precision, clippy::too_many_arguments)]

use super::path::{CameraPath, Keyframe};

#[cfg(test)]
mod finder;
mod post;
mod run;
mod shading;
// The record's checks are the only readers of its generator, seed and radius outside the tests.
#[cfg_attr(not(test), allow(dead_code))]
mod shots;
mod stamps;

pub use run::main;

#[cfg_attr(not(test), allow(unused_imports))]
pub use shots::{RECORDED_FOR, RECORDED_RADIUS, RECORDED_SEED, SHOTS};

/// The crystal Materials' hues (#135): six to eight opaque, glossy, emissive crystals. All eight
/// are here, although the shot list uses some of them.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hue {
    Blue,
    Cyan,
    Green,
    Magenta,
    Red,
    Amber,
    White,
    Violet,
}

/// What a hand-placed stamp is, where the generators of M1b do not make it yet.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamp {
    /// A cluster of opaque emissive crystal growing out of the surface, of one hue.
    Crystal(Hue),
    /// A tree: a trunk and a leaf-cluster canopy.
    Tree,
    /// A large-leaved plant, shaped leaf by leaf (a fern, a cycad).
    Fern,
    /// A patch of a flower carpet: few-Voxel blobs.
    Flowers,
    /// Moss or grass on an upward-facing rock edge.
    Moss,
}

/// A place for a stamp: the empty Voxel that touches a surface, and the way that surface faces
/// (the axis the stamp grows along: `[0, 1, 0]` is a floor where +y is up).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub stamp: Stamp,
    pub at: [i32; 3],
    pub face: [i8; 3],
}

/// The hues of a crystal cave: the dominant one, the accents, and the floor beds in a second
/// hue (#135).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CavePalette {
    pub dominant: Hue,
    pub accent: Hue,
    pub bed: Hue,
}

/// One camera keyframe of a shot, in Voxels from the Planet's centre, with the roll of the debug
/// free camera there (radians, `render::Camera::roll`).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub position: [f64; 3],
    pub target: [f64; 3],
    pub roll: f64,
}

/// One shot of the flythrough.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub name: &'static str,
    /// What the dev is to judge in it.
    pub subject: &'static str,
    pub keys: &'static [Key],
    /// How many frames it is rendered over, at 60 a second.
    pub frames: u32,
    /// The places for stamps: the crystals of a cave, the trees of a forest.
    pub places: &'static [Place],
    /// A crystal cave's hues; none for the other shots.
    pub cave: Option<CavePalette>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Shot {
    /// The shot as a bench camera path: horizon levelled to the Planet's up where the camera is,
    /// rolled as recorded.
    pub fn path(&self) -> CameraPath {
        CameraPath::new(
            self.keys
                .iter()
                .map(|k| Keyframe {
                    position: k.position,
                    target: k.target,
                })
                .collect(),
        )
        .radial()
        .rolled(self.keys.iter().map(|k| k.roll).collect())
    }
}

#[cfg(test)]
mod tests {
    use brick_store::VOXELS_PER_METRE;
    use planet_gen::world_map::{PEAK, WORLD_RADIUS};

    use super::*;

    fn length(a: [f64; 3]) -> f64 {
        a.iter().map(|c| c * c).sum::<f64>().sqrt()
    }

    fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
        length(std::array::from_fn(|i| a[i] - b[i]))
    }

    fn shot(name: &str) -> Shot {
        *SHOTS
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no shot {name}"))
    }

    #[test]
    fn the_shots_are_the_issues_list_in_order() {
        let names: Vec<&str> = SHOTS.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "forest floor",
                "canyon with haze",
                "dark descent",
                "crystal cave, cyan",
                "crystal cave, magenta",
                "sunlit cliff",
                "meadow",
                "pull-out to the Planet",
            ]
        );
    }

    #[test]
    fn the_places_are_those_of_the_seed_and_radius_the_client_has() {
        // The generator version alone would not notice a new seed or a new radius.
        assert_eq!(RECORDED_SEED, crate::SEED);
        assert_eq!(RECORDED_RADIUS, WORLD_RADIUS);
    }

    #[test]
    fn the_places_are_those_of_the_generator_the_client_has() {
        // The places are the ground of one generator version. When it is bumped they move: find
        // them again (`finder::record_the_look_dev_sites`) and record the version they are for.
        assert_eq!(RECORDED_FOR, planet_gen::GENERATOR_VERSION);
    }

    #[test]
    fn every_shot_moves_its_camera_and_has_a_finite_view() {
        for shot in SHOTS {
            assert!(shot.keys.len() >= 2, "{}", shot.name);
            assert!(shot.frames >= 60, "{} is under a second", shot.name);
            for pair in shot.keys.windows(2) {
                assert!(
                    distance(pair[0].position, pair[1].position) > 1.0,
                    "{}",
                    shot.name
                );
            }
            for key in shot.keys {
                assert!(
                    key.position
                        .iter()
                        .chain(&key.target)
                        .all(|c| c.is_finite()),
                    "{}",
                    shot.name
                );
                assert!(
                    key.roll.abs() < 0.5,
                    "{}: a roll past 28 degrees",
                    shot.name
                );
                assert_ne!(key.position, key.target, "{}", shot.name);
            }
        }
    }

    #[test]
    fn the_keyframes_fall_on_whole_frames_so_a_screenshot_is_taken_where_the_camera_is() {
        for shot in SHOTS {
            let segments = shot.keys.len() as u32 - 1;
            assert_eq!((shot.frames - 1) % segments, 0, "{}", shot.name);
        }
    }

    #[test]
    fn the_shots_are_on_the_320_km_planet() {
        let radius = f64::from(WORLD_RADIUS);
        let relief = f64::from(PEAK) + 100.0 * VOXELS_PER_METRE;
        for shot in SHOTS.iter().filter(|s| s.name != "pull-out to the Planet") {
            for key in shot.keys {
                let from_centre = length(key.position);
                assert!(
                    (from_centre - radius).abs() < relief,
                    "{}: {:.0} m from the centre, the sea is {:.0} m",
                    shot.name,
                    from_centre / VOXELS_PER_METRE,
                    radius / VOXELS_PER_METRE
                );
            }
        }
        // The pull-out leaves the ground and ends in the Zoom's range, which reaches 20 radii.
        let pull = shot("pull-out to the Planet");
        let heights: Vec<f64> = pull
            .keys
            .iter()
            .map(|k| length(k.position) - radius)
            .collect();
        assert!(heights.windows(2).all(|h| h[0] < h[1]), "{heights:?}");
        assert!(heights[0] < 5.0 * VOXELS_PER_METRE + relief);
        let last = heights[heights.len() - 1];
        assert!((500_000.0..6_400_000.0).contains(&(last / VOXELS_PER_METRE)));
        // It ends looking at the Planet.
        assert_eq!(pull.keys.last().map(|k| k.target), Some([0.0; 3]));
    }

    #[test]
    fn the_two_crystal_caves_have_a_dominant_hue_each_and_are_far_apart() {
        let (a, b) = (shot("crystal cave, cyan"), shot("crystal cave, magenta"));
        let (pa, pb) = (a.cave.expect("a palette"), b.cave.expect("a palette"));
        assert_ne!(pa.dominant, pb.dominant);
        for p in [pa, pb] {
            assert!(p.bed != p.dominant && p.accent != p.dominant && p.accent != p.bed);
        }
        assert!(distance(a.keys[0].position, b.keys[0].position) > 1_000.0 * VOXELS_PER_METRE);
        // Only those two have a palette.
        assert_eq!(SHOTS.iter().filter(|s| s.cave.is_some()).count(), 2);
    }

    #[test]
    fn each_cave_names_crystal_places_of_its_hues_with_the_dominant_most_numerous() {
        for shot in SHOTS.iter().filter(|s| s.cave.is_some()) {
            let palette = shot.cave.expect("a palette");
            let count = |hue: Hue| {
                shot.places
                    .iter()
                    .filter(|p| p.stamp == Stamp::Crystal(hue))
                    .count()
            };
            assert_eq!(
                count(palette.dominant) + count(palette.bed) + count(palette.accent),
                shot.places.len(),
                "{}: a place that is not of its hues",
                shot.name
            );
            assert!(count(palette.dominant) >= 3, "{}", shot.name);
            assert!(count(palette.bed) >= 2, "{}", shot.name);
            assert!(
                count(palette.dominant) > count(palette.accent),
                "{}",
                shot.name
            );
        }
    }

    #[test]
    fn the_stamps_the_other_shots_name_are_vegetation_near_the_camera() {
        for (name, want) in [
            ("forest floor", Stamp::Tree),
            ("forest floor", Stamp::Fern),
            ("meadow", Stamp::Flowers),
            ("meadow", Stamp::Moss),
        ] {
            assert!(shot(name).places.iter().any(|p| p.stamp == want), "{name}");
        }
        for shot in SHOTS.iter().filter(|s| s.cave.is_none()) {
            for place in shot.places {
                assert!(
                    !matches!(place.stamp, Stamp::Crystal(_)),
                    "{}: crystals in the open",
                    shot.name
                );
            }
        }
        for shot in SHOTS {
            for place in shot.places {
                let at = place.at.map(f64::from);
                let near = shot
                    .keys
                    .iter()
                    .map(|k| distance(k.position, at))
                    .fold(f64::MAX, f64::min);
                assert!(near < 60.0 * VOXELS_PER_METRE, "{}: {place:?}", shot.name);
                assert_eq!(place.face.iter().map(|c| c.abs()).sum::<i8>(), 1);
            }
        }
        // The shots with no stamps are the ones that have no content to place.
        for name in ["canyon with haze", "dark descent", "pull-out to the Planet"] {
            assert!(shot(name).places.is_empty(), "{name}");
        }
    }

    #[test]
    fn a_shot_is_a_bench_path_levelled_to_the_planets_up_and_rolled_as_recorded() {
        for shot in SHOTS {
            let path = shot.path();
            assert_eq!(path.keyframes(), shot.keys.len());
            for (k, key) in shot.keys.iter().enumerate() {
                let frame = path.keyframe_frames(shot.frames)[k];
                let camera = path.camera_at(frame, shot.frames);
                assert_eq!(camera.position, key.position, "{}", shot.name);
                assert_eq!(camera.roll, key.roll, "{}", shot.name);
                let up: Vec<f64> = key
                    .position
                    .iter()
                    .map(|c| c / length(key.position))
                    .collect();
                assert!(
                    camera
                        .horizon_up
                        .iter()
                        .zip(&up)
                        .all(|(a, b)| (a - b).abs() < 1e-12),
                    "{}",
                    shot.name
                );
            }
        }
        // The canyon banks into the view; the others stay level.
        assert!(shot("canyon with haze").keys.iter().any(|k| k.roll != 0.0));
        assert!(shot("meadow").keys.iter().all(|k| k.roll == 0.0));
    }
}
