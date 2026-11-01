//! THROWAWAY (#99): the two still views that `client measure-reference` captures, both on the
//! patch of the test Planet that the golden image tests use.

use brick_store::{BrickCoord, BrickStore, VoxelCoord};
use render::{Camera, Scene, Sun};
use serde_json::{Value, json};

const SEED: u64 = 0xB4A7_A9AC;

/// A still camera and sun on a scene, and a description for the summary.
pub struct View {
    pub scene: Scene,
    pub camera: Camera,
    pub sun: Sun,
    pub description: Value,
}

/// The patch of the test Planet near the +y pole that `crates/render/tests/golden.rs` renders:
/// Voxels x -440..=447, y 1360..=1575, z -360..=263.
fn patch() -> BrickStore {
    planet_gen::generate_region(SEED, BrickCoord([-55, 170, -45]), BrickCoord([55, 196, 32]))
}

fn camera_json(camera: &Camera, from: [f64; 3], to: [f64; 3]) -> Value {
    json!({
        "position": from, "target": to,
        "fov_y_deg": camera.fov_y.to_degrees(),
    })
}

fn sun_json(azimuth: f32, elevation: f32, radius: f32) -> Value {
    json!({
        "azimuth_rad": azimuth, "elevation_rad": elevation, "angular_radius_rad": radius,
        "from": "Sun::from_angles(azimuth, elevation, angular_radius)",
    })
}

/// The reference golden's view (`render_reference` in `crates/render/tests/golden.rs`): close to
/// the terraced hillside, with a low sun to the side.
pub fn golden() -> View {
    let (from, to) = ([0.0, 1535.0, 120.0], [0.0, 1508.0, 20.0]);
    let camera = Camera::looking_at(from, to);
    let sun = Sun::from_angles(0.9, 0.25, 0.04);
    View {
        scene: Scene::new(&patch(), None),
        camera,
        sun,
        description: json!({
            "what": "the reference golden's view: the terraced hillside, low sun to the side",
            "scene": "patch of the test Planet, no edits",
            "camera": camera_json(&camera, from, to),
            "sun": sun_json(0.9, 0.25, 0.04),
        }),
    }
}

/// Top solid Voxel of the column (x, z), scanning down from y = 1575.
fn ground(store: &BrickStore, x: i32, z: i32) -> Option<i32> {
    (1360..=1575)
        .rev()
        .find(|&y| store.get(VoxelCoord([x, y, z])).is_some())
}

/// The pit: a rectangle of the hillside dug out down to a flat floor, with the sun across it, so
/// that one wall is lit and the floor and the other wall are lit mostly by what the lit wall and
/// the lit part of the floor reflect.
pub const PIT: Pit = Pit {
    x: (-60, -20),
    z: (-70, -10),
    depth: 40,
};

pub struct Pit {
    pub x: (i32, i32),
    pub z: (i32, i32),
    /// Voxels (0.1 m each) below the lowest ground at the pit's rim.
    pub depth: i32,
}

/// A view with real inter-reflection: a pit dug into the hillside, looked into from its rim.
pub fn interreflect() -> View {
    let mut store = patch();
    if std::env::var_os("MEASURE_PROBE").is_some() {
        for z in (-100..=100).step_by(20) {
            let row: Vec<String> = (-100..=100)
                .step_by(20)
                .map(|x| ground(&store, x, z).map_or("-".into(), |y| y.to_string()))
                .collect();
            eprintln!("ground z={z:>5}: {}", row.join(" "));
        }
    }
    // The floor is `depth` below the lowest ground on the rim.
    let rim: Vec<i32> = (PIT.x.0 - 1..=PIT.x.1 + 1)
        .flat_map(|x| [(x, PIT.z.0 - 1), (x, PIT.z.1 + 1)])
        .chain((PIT.z.0 - 1..=PIT.z.1 + 1).flat_map(|z| [(PIT.x.0 - 1, z), (PIT.x.1 + 1, z)]))
        .filter_map(|(x, z)| ground(&store, x, z))
        .collect();
    let (rim_low, rim_high) = (*rim.iter().min().unwrap(), *rim.iter().max().unwrap());
    let floor = rim_low - PIT.depth;
    let mut dug = 0u32;
    for x in PIT.x.0..=PIT.x.1 {
        for z in PIT.z.0..=PIT.z.1 {
            for y in floor + 1..=1575 {
                let at = VoxelCoord([x, y, z]);
                if store.get(at).is_some() {
                    store.set(at, None);
                    dug += 1;
                }
            }
        }
    }
    // Looking into the pit from 2 m above and a little beyond its +z rim, down at about 40 degrees, towards the
    // far (-z) wall. The sun is behind that wall, a little to -x and 46 degrees up: it lights
    // the near half of the floor and the near wall, while the far wall and the far half of the
    // floor are in the shadow of the far rim and see the sun only by reflection.
    let cx = (PIT.x.0 + PIT.x.1) as f64 / 2.0;
    let (from, to) = (
        [cx, f64::from(rim_high) + 20.0, f64::from(PIT.z.1) + 14.0],
        [cx, f64::from(floor) + 8.0, f64::from(PIT.z.0) + 25.0],
    );
    let camera = Camera::looking_at(from, to);
    let (azimuth, elevation, radius) = (std::f32::consts::PI + 0.26, 0.8, 0.04);
    let sun = Sun::from_angles(azimuth, elevation, radius);
    View {
        scene: Scene::new(&store, None),
        camera,
        sun,
        description: json!({
            "what": "a pit dug into the golden view's hillside, seen from its rim: the sunlit floor \
                     and near wall light the far wall and the shadowed half of the floor",
            "scene": "patch of the test Planet, plus one Edit: every Voxel inside the pit box \
                      and above its floor set to air (BrickStore::set)",
            "pit": {
                "x": [PIT.x.0, PIT.x.1], "z": [PIT.z.0, PIT.z.1],
                "floor_y": floor, "rim_low_y": rim_low, "rim_high_y": rim_high,
                "voxels_dug": dug,
            },
            "camera": camera_json(&camera, from, to),
            "sun": sun_json(azimuth, elevation, radius),
        }),
    }
}
