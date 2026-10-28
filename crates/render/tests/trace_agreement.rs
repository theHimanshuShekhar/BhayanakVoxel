//! The GPU `trace()` and the CPU reference must agree on hit, position, normal and Material.
//! Needs a Vulkan device: CI and local runs use lavapipe (see `.github/workflows/ci.yml`).

use brick_store::{Brick, BrickCoord, BrickStore, Material, VoxelCoord};
use planet_gen::generate_region;
use render::reference::{Ray, trace};
use render::scene::{Core, Scene};
use render::{Offscreen, camera};

const STONE: Material = Material(7);

/// A small deterministic generator, so the ray sets are fixed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }
}

/// A ray from `position` (f64, Planet frame) towards `towards`.
fn ray(position: [f64; 3], towards: [f64; 3]) -> Ray {
    let d = [0, 1, 2].map(|a| towards[a] - position[a]);
    let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
    let (cell, origin) = camera::split(position);
    Ray {
        cell,
        origin,
        dir: d.map(|c| (c / len) as f32),
    }
}

fn assert_agree(label: &str, rays: &[Ray], scene: &Scene) -> usize {
    let gpu = Offscreen::new(8, 8, scene).unwrap().trace_rays(rays);
    assert_eq!(gpu.len(), rays.len());
    let mut hits = 0;
    for (i, (r, got)) in rays.iter().zip(&gpu).enumerate() {
        let want = trace(scene, r);
        let context = format!("{label} ray {i}: {r:?}\n gpu {got:?}\n cpu {want:?}");
        match (got, &want) {
            (None, None) => {}
            (Some(g), Some(w)) => {
                hits += 1;
                assert_eq!(
                    (g.voxel, g.normal, g.material),
                    (w.voxel, w.normal, w.material),
                    "{context}"
                );
                assert!((g.t - w.t).abs() <= 1e-3 * (1.0 + w.t.abs()), "{context}");
                for a in 0..3 {
                    assert!(
                        (g.position[a] - w.position[a]).abs() <= 1e-3 * (1.0 + w.t.abs()),
                        "{context}"
                    );
                }
            }
            _ => panic!("hit and miss disagree\n{context}"),
        }
    }
    hits
}

fn store_with(voxels: &[[i32; 3]]) -> BrickStore {
    let mut store = BrickStore::new();
    for &v in voxels {
        store.set(VoxelCoord(v), Some(STONE));
    }
    store
}

#[test]
fn a_hand_made_scene_agrees_on_edge_cases() {
    let mut store = store_with(&[
        [10, 0, 0],
        [-900, 35, 70],
        [1000, 3, -500],
        [63, 5, 0],
        [64, 0, 0],
        [3, 4, 5],
    ]);
    store.insert(BrickCoord([5, 8, 0]), Brick::solid(Material(300)));
    for (x, m) in [(1, 1), (2, 2), (3, 300), (4, 9)] {
        store.set(VoxelCoord([x, -20, 0]), Some(Material(m)));
    }
    let scene = Scene::new(&store, None);
    let at = |cell: [i32; 3], dir: [f32; 3]| {
        let len = dir.iter().map(|d| d * d).sum::<f32>().sqrt();
        Ray {
            cell,
            origin: [0.5; 3],
            dir: dir.map(|d| d / len),
        }
    };
    let rays = [
        // Axis-aligned, from all six sides.
        at([20, 0, 0], [-1.0, 0.0, 0.0]),
        at([0, 0, 0], [1.0, 0.0, 0.0]),
        at([10, 9, 0], [0.0, -1.0, 0.0]),
        at([10, -9, 0], [0.0, 1.0, 0.0]),
        at([10, 0, 9], [0.0, 0.0, -1.0]),
        at([10, 0, -9], [0.0, 0.0, 1.0]),
        // Starting inside a solid Voxel and inside a solid brick.
        at([3, 4, 5], [0.2, -1.0, 0.1]),
        at([42, 67, 4], [1.0, 0.0, 0.0]),
        // Misses: away, past, empty scene directions.
        at([20, 0, 0], [1.0, 0.0, 0.0]),
        at([0, 0, 0], [1.0, 0.1, 0.0]),
        at([0, 100, 0], [0.0, 1.0, 0.0]),
        // Across empty regions to far Voxels, either way.
        at([-1000, 0, 0], [100.0, 35.0, 70.0]),
        at([0, 0, 0], [1000.0, 3.0, -500.0]),
        at([1500, 3, -500], [-1.0, 0.0, 0.0]),
        // Region and brick boundaries.
        at([40, 0, 0], [1.0, 0.0, 0.0]),
        at([40, 5, 0], [1.0, 0.0, 0.0]),
        // Every palette entry, and one on a plane exactly.
        at([1, 3, 0], [0.0, -1.0, 0.0]),
        at([2, -10, 0], [0.0, -1.0, 0.0]),
        at([3, -10, 0], [0.0, -1.0, 0.0]),
        at([4, -10, 0], [0.0, -1.0, 0.0]),
        Ray {
            cell: [0, 1, 0],
            origin: [0.0, 0.0, 0.5],
            dir: [1.0, 0.0, 0.0],
        },
    ];
    let hits = assert_agree("hand-made", &rays, &scene);
    assert!(hits >= 12, "{hits}");
}

/// A brick whose first `n` Voxels (x fastest) are `n` distinct Materials.
fn brick_with_materials(n: usize, first: u16) -> Brick {
    let mut brick = Brick::AIR;
    for i in 0..n {
        let local = [(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8];
        brick.set(local, Some(Material(first + i as u16)));
    }
    brick
}

#[test]
fn every_palette_width_decodes_the_same_on_the_gpu_and_the_cpu() {
    // Palettes of 2, 3, 5, 17 and 300 Materials use 1, 2, 4, 8 and 16 bits per index.
    let sizes = [2, 3, 5, 17, 300];
    let mut store = BrickStore::new();
    for (i, n) in sizes.into_iter().enumerate() {
        let first = 1000 * (i as u16 + 1);
        store.insert(
            BrickCoord([i as i32 * 3, 0, 0]),
            brick_with_materials(n, first),
        );
    }
    let scene = Scene::new(&store, None);
    // One ray per solid Voxel, starting inside it, so that each Voxel's own Material is read.
    let mut rays = Vec::new();
    let mut expected = Vec::new();
    for (i, n) in sizes.into_iter().enumerate() {
        for v in 0..n {
            let voxel = [
                i as i32 * 24 + (v % 8) as i32,
                (v / 8 % 8) as i32,
                (v / 64) as i32,
            ];
            rays.push(Ray {
                cell: voxel,
                origin: [0.5; 3],
                dir: [0.0, 0.0, 1.0],
            });
            expected.push((voxel, 1000 * (i as u16 + 1) + v as u16));
        }
    }
    assert_eq!(rays.len(), 327);
    let gpu = Offscreen::new(8, 8, &scene).unwrap().trace_rays(&rays);
    for ((ray, want), got) in rays.iter().zip(&expected).zip(&gpu) {
        let cpu = trace(&scene, ray).expect("starts inside a solid Voxel");
        let got = got.expect("starts inside a solid Voxel");
        assert_eq!((cpu.voxel, cpu.material), *want, "cpu {ray:?}");
        assert_eq!((got.voxel, got.material), *want, "gpu {ray:?}");
    }
}

#[test]
fn a_scene_with_a_core_agrees_inside_on_the_surface_and_outside() {
    let core = Core {
        radius: 150,
        material: STONE,
    };
    let scene = Scene::new(&BrickStore::new(), Some(core));
    let mut rng = Lcg(11);
    let mut rays = Vec::new();
    for _ in 0..200 {
        let from = [
            (rng.next() * 500.0 - 250.0) as f64,
            (rng.next() * 500.0 - 250.0) as f64,
            (rng.next() * 500.0 - 250.0) as f64,
        ];
        let to = [
            (rng.next() * 200.0 - 100.0) as f64,
            (rng.next() * 200.0 - 100.0) as f64,
            (rng.next() * 200.0 - 100.0) as f64,
        ];
        rays.push(ray(from, to));
    }
    let hits = assert_agree("core", &rays, &scene);
    assert!(hits > 100, "{hits}");
}

#[test]
fn a_patch_of_the_test_planet_agrees_for_a_fixed_set_of_rays() {
    // The Planet's surface near +y, from inside the rock to above the highest peak.
    let store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    let scene = Scene::new(&store, None);
    let mut rng = Lcg(2024);
    let mut rays = Vec::new();
    for _ in 0..1024 {
        let from = [
            f64::from(rng.next() * 160.0 - 80.0),
            1560.0 + f64::from(rng.next() * 100.0),
            f64::from(rng.next() * 160.0 - 80.0),
        ];
        let to = [
            f64::from(rng.next() * 160.0 - 80.0),
            1400.0 + f64::from(rng.next() * 120.0),
            f64::from(rng.next() * 160.0 - 80.0),
        ];
        rays.push(ray(from, to));
    }
    let hits = assert_agree("patch", &rays, &scene);
    assert!(hits > 800, "only {hits} of 1024 rays hit");
}

#[test]
fn rays_far_from_the_origin_agree_too() {
    let far = 1_900_000;
    let store = store_with(&[[far + 10, 7, -3], [far + 200, 7, -3], [far + 13, 8, -3]]);
    let scene = Scene::new(&store, None);
    let rays = [
        ray(
            [f64::from(far) + 0.25, 7.5, -2.5],
            [f64::from(far) + 50.0, 7.5, -2.5],
        ),
        ray(
            [f64::from(far) + 100.5, 7.5, -2.5],
            [f64::from(far) + 20.0, 7.5, -2.5],
        ),
        ray(
            [f64::from(far) + 12.5, 20.5, -2.5],
            [f64::from(far) + 13.5, 0.0, -2.5],
        ),
    ];
    let hits = assert_agree("far", &rays, &scene);
    assert_eq!(hits, 3);
}
