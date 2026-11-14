//! The two still views of the convergence phase (ADR 0005), both on the patch of the **test
//! Planet** (the small sphere of 1500 Voxels, not the 320 km Planet) that the golden image tests
//! draw: the reference golden's view, and a pit dug into the same hillside.
//!
//! The numbers on #99 were taken at these cameras, suns and this pit, so a baseline made here can
//! be set beside them. The geometry is in Voxels and is the same as it was then; what a Voxel
//! measures changed from 10 cm to 5 cm (ADR 0007), so the pit is 2 m by 3 m and 2 m deep now, and
//! was 4 m by 6 m and 4 m deep. Nothing in the tracer depends on a Voxel's size in metres, so the
//! pictures are the same.

use brick_store::{BrickCoord, BrickStore, VoxelCoord};
use render::{Camera, Scene, Sun};
use serde::Serialize;

/// A still camera and sun on a scene.
pub struct View {
    pub name: &'static str,
    pub scene: Scene,
    pub camera: Camera,
    pub sun: Sun,
    pub description: Description,
}

/// What a view is made of, as the report and a truth's sidecar record it: enough to make the
/// same view again.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Description {
    pub what: &'static str,
    /// The camera's position and the point it looks at, in Voxels.
    pub camera_from: [f64; 3],
    pub camera_to: [f64; 3],
    pub fov_y_deg: f64,
    /// `Sun::from_angles`'s arguments, in radians.
    pub sun_azimuth: f32,
    pub sun_elevation: f32,
    pub sun_angular_radius: f32,
    /// The pit, for the view that has one; null for the other.
    pub pit: Option<PitDescription>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PitDescription {
    /// Voxels dug out of the patch, which is the only thing that differs from the golden's scene.
    pub voxels_dug: u32,
    pub x: (i32, i32),
    pub z: (i32, i32),
    pub floor_y: i32,
    pub rim_low_y: i32,
    pub rim_high_y: i32,
}

/// The patch of the test Planet near the +y pole that the golden image test also renders:
/// Voxels x -440..=447, y 1360..=1575, z -360..=263.
pub fn patch_store() -> BrickStore {
    planet_gen::generate_region(
        crate::SEED,
        BrickCoord([-55, 170, -45]),
        BrickCoord([55, 196, 32]),
    )
}

/// Top of the patch, in Voxels: the highest `y` the pit's scan looks at.
const PATCH_TOP: i32 = 1575;
/// Bottom of the patch, in Voxels.
const PATCH_BOTTOM: i32 = 1360;

fn description(
    what: &'static str,
    (from, to): ([f64; 3], [f64; 3]),
    camera: &Camera,
    sun: (f32, f32, f32),
    pit: Option<PitDescription>,
) -> Description {
    Description {
        what,
        camera_from: from,
        camera_to: to,
        fov_y_deg: camera.fov_y.to_degrees(),
        sun_azimuth: sun.0,
        sun_elevation: sun.1,
        sun_angular_radius: sun.2,
        pit,
    }
}

/// The reference golden's view (`render_reference` in `crates/render/tests/golden.rs`): close to
/// the terraced hillside, with a low sun to the side.
pub fn golden() -> View {
    let (from, to) = ([0.0, 1535.0, 120.0], [0.0, 1508.0, 20.0]);
    let camera = Camera::looking_at(from, to);
    let sun_angles = (0.9, 0.25, 0.04);
    View {
        name: "golden",
        scene: Scene::new(&patch_store(), None),
        camera,
        sun: Sun::from_angles(sun_angles.0, sun_angles.1, sun_angles.2),
        description: description(
            "the reference golden's view: the terraced hillside, low sun to the side",
            (from, to),
            &camera,
            sun_angles,
            None,
        ),
    }
}

/// The pit: a rectangle of the hillside dug out down to a flat floor, `depth` Voxels below the
/// lowest ground on its rim. With the sun across it, one wall and the near part of the floor are
/// lit, and the far wall and the far part of the floor are lit by what those reflect.
const PIT_X: (i32, i32) = (-60, -20);
const PIT_Z: (i32, i32) = (-70, -10);
const PIT_DEPTH: i32 = 40;

/// Top solid Voxel of the column (x, z), scanning down from the top of the patch.
fn ground(store: &BrickStore, x: i32, z: i32) -> Option<i32> {
    (PATCH_BOTTOM..=PATCH_TOP)
        .rev()
        .find(|&y| store.get(VoxelCoord([x, y, z])).is_some())
}

/// The patch with the pit dug, and what was dug. Every Voxel inside the pit's box and above its
/// floor is set to air.
fn dig_pit(store: &mut BrickStore) -> PitDescription {
    // The floor is `PIT_DEPTH` below the lowest ground on the rim.
    let rim: Vec<i32> = (PIT_X.0 - 1..=PIT_X.1 + 1)
        .flat_map(|x| [(x, PIT_Z.0 - 1), (x, PIT_Z.1 + 1)])
        .chain((PIT_Z.0 - 1..=PIT_Z.1 + 1).flat_map(|z| [(PIT_X.0 - 1, z), (PIT_X.1 + 1, z)]))
        .filter_map(|(x, z)| ground(store, x, z))
        .collect();
    let rim_low = *rim.iter().min().expect("the pit's rim is on the patch");
    let rim_high = *rim.iter().max().expect("the pit's rim is on the patch");
    let floor = rim_low - PIT_DEPTH;
    let mut dug = 0;
    for x in PIT_X.0..=PIT_X.1 {
        for z in PIT_Z.0..=PIT_Z.1 {
            for y in floor + 1..=PATCH_TOP {
                let at = VoxelCoord([x, y, z]);
                if store.get(at).is_some() {
                    store.set(at, None);
                    dug += 1;
                }
            }
        }
    }
    PitDescription {
        voxels_dug: dug,
        x: PIT_X,
        z: PIT_Z,
        floor_y: floor,
        rim_low_y: rim_low,
        rim_high_y: rim_high,
    }
}

/// A view with real inter-reflection: the pit, looked into from its rim. The camera is 20 Voxels
/// above the rim's highest ground and 14 Voxels beyond the pit's +z rim, looking down
/// at about 40 degrees at the far (-z) wall. The sun is behind that wall, a little to -x and 46
/// degrees up: it lights the near part of the floor and the near wall, while the far wall and the
/// far part of the floor are in the shadow of the far rim and see the sun only by reflection.
pub fn pit() -> View {
    let mut store = patch_store();
    let pit = dig_pit(&mut store);
    let cx = f64::from(PIT_X.0 + PIT_X.1) / 2.0;
    let (from, to) = (
        [
            cx,
            f64::from(pit.rim_high_y) + 20.0,
            f64::from(PIT_Z.1) + 14.0,
        ],
        [
            cx,
            f64::from(pit.floor_y) + 8.0,
            f64::from(PIT_Z.0) + 25.0,
        ],
    );
    let camera = Camera::looking_at(from, to);
    let sun_angles = (std::f32::consts::PI + 0.26, 0.8, 0.04);
    View {
        name: "pit",
        scene: Scene::new(&store, None),
        camera,
        sun: Sun::from_angles(sun_angles.0, sun_angles.1, sun_angles.2),
        description: description(
            "a pit dug into the golden view's hillside, seen from its rim: the sunlit floor and \
             near wall light the far wall and the shadowed part of the floor",
            (from, to),
            &camera,
            sun_angles,
            Some(pit),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pit_is_dug_to_the_same_floor_as_on_99() {
        // Held against #99's `summary.json` for the pit it measured: the same hillside, the same
        // box, the same depth below the same rim.
        let mut store = patch_store();
        let pit = dig_pit(&mut store);
        assert_eq!(pit.floor_y, pit.rim_low_y - 40);
        assert!(pit.rim_high_y >= pit.rim_low_y);
        assert!(pit.voxels_dug > 40 * 60 * 40 / 2, "{}", pit.voxels_dug);
        // Everything above the floor inside the box is air, the floor itself is not.
        for (x, z) in [(PIT_X.0, PIT_Z.0), (-40, -40), (PIT_X.1, PIT_Z.1)] {
            assert!(store.get(VoxelCoord([x, pit.floor_y + 1, z])).is_none());
            assert!(store.get(VoxelCoord([x, pit.floor_y, z])).is_some());
        }
    }
}
