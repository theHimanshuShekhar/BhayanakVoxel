//! Moving the camera and the Planet together by a billion Voxels must change nothing the GPU
//! computes, so there is no jitter far from the origin. Needs a Vulkan device (lavapipe in CI).

use brick_store::{BrickCoord, BrickStore};
use planet_gen::generate_region;
use render::reference::Ray;
use render::{Camera, Offscreen, Scene, camera};

/// A multiple of 64 Voxels, so that regions line up the same way as at the origin.
const SHIFT: i32 = 1_000_000_000;

fn shifted(store: &BrickStore, by: i32) -> BrickStore {
    let mut moved = BrickStore::new();
    for coord in store.sorted_coords() {
        let brick = store.brick(coord).unwrap().clone();
        let [x, y, z] = coord.0;
        moved.insert(BrickCoord([x + by / 8, y, z]), brick);
    }
    moved
}

/// Rays from above the surface patch down into it, as positions in f64.
fn rays(offset: f64) -> Vec<Ray> {
    let mut rays = Vec::new();
    for i in 0..16 {
        for j in 0..16 {
            let (u, v) = (f64::from(i) / 16.0, f64::from(j) / 16.0);
            let from = [
                1570.0 + offset + 0.123 * u,
                -40.0 + 80.0 * u + 0.37,
                -40.0 + 80.0 * v + 0.61,
            ];
            let to = [1480.0 + offset, -30.0 + 60.0 * v, -30.0 + 60.0 * u];
            let d = [0, 1, 2].map(|a| to[a] - from[a]);
            let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
            let (cell, origin) = camera::split(from);
            rays.push(Ray {
                cell,
                origin,
                dir: d.map(|c| (c / len) as f32),
            });
        }
    }
    rays
}

#[test]
fn the_gpu_gives_the_same_hits_a_billion_voxels_from_the_origin() {
    let near = generate_region(3, BrickCoord([176, -6, -6]), BrickCoord([196, 6, 6]));
    let far = shifted(&near, SHIFT);
    let near = Offscreen::new(8, 8, &Scene::new(&near, None)).unwrap();
    let far = Offscreen::new(8, 8, &Scene::new(&far, None)).unwrap();

    let a = near.trace_rays(&rays(0.0));
    let b = far.trace_rays(&rays(f64::from(SHIFT)));
    let hits = a.iter().flatten().count();
    assert!(hits > 200, "only {hits} of 256 rays hit");
    assert_eq!(a.len(), b.len());
    for (i, (n, f)) in a.iter().zip(&b).enumerate() {
        match (n, f) {
            (None, None) => {}
            (Some(n), Some(f)) => {
                assert_eq!(
                    f.voxel,
                    [n.voxel[0] + SHIFT, n.voxel[1], n.voxel[2]],
                    "ray {i}"
                );
                assert_eq!((f.normal, f.material), (n.normal, n.material), "ray {i}");
                // The rays differ only by f64 rounding of the camera position at 1e9, about 1e-7.
                // A single f32 position there would be off by tens of Voxels.
                assert!((f.t - n.t).abs() < 1e-4, "ray {i}: {} vs {}", f.t, n.t);
                for a in 0..3 {
                    assert!(
                        (f.position[a] - n.position[a]).abs() < 1e-4,
                        "ray {i}: {f:?} vs {n:?}"
                    );
                }
            }
            _ => panic!("ray {i}: one hit and one miss: {n:?} {f:?}"),
        }
    }
}

#[test]
fn the_camera_uniform_far_away_is_small_and_exact() {
    let camera = Camera::looking_at(
        [f64::from(SHIFT) + 1500.25, 3.5, -2.75],
        [f64::from(SHIFT), 0.0, 0.0],
    );
    let u = camera.uniform();
    assert_eq!(u.cell[..3], [SHIFT + 1500, 3, -3]);
    assert_eq!(u.origin[..3], [0.25, 0.5, 0.25]);
}
