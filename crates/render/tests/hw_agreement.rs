//! The hardware ray-query backend (#50, a spike) against the compute `trace()`, and the choice
//! between them. Needs a Vulkan device: CI uses lavapipe, which may or may not have ray queries
//! (it depends on its Mesa): the fallback test holds either way, the Edit test skips itself where
//! the adapter has none, and the comparison on a set of rays is `#[ignore]`d (run it on a GPU with
//! ray queries: `cargo test -p render --test hw_agreement -- --ignored --nocapture`).

mod common;

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};
use common::{Lcg, ray};
use planet_gen::generate_region;
use render::lod::Bands;
use render::scene::{Bound, Core, Scene};
use render::{Camera, Offscreen, Resolution, RtBackend, choose_backend};

const STONE: Material = Material(7);

#[test]
fn the_hardware_backend_is_chosen_only_where_the_adapter_has_ray_queries_and_otherwise_compute_runs_with_no_error()
 {
    let limits = wgpu::Limits {
        max_blas_primitive_count: 1 << 24,
        max_tlas_instance_count: 1 << 24,
        ..wgpu::Limits::default()
    };
    // The default is compute, on any adapter.
    let none = choose_backend(RtBackend::Compute, wgpu::Features::empty(), &limits);
    assert_eq!((none.active, none.fallback), (RtBackend::Compute, None));
    // Hardware without the feature (lavapipe): compute, and the reason.
    let lacking = choose_backend(RtBackend::Hardware, wgpu::Features::empty(), &limits);
    assert_eq!(lacking.requested, RtBackend::Hardware);
    assert_eq!(lacking.active, RtBackend::Compute);
    assert!(lacking.fallback.is_some_and(|r| r.contains("ray quer")));
    // With the feature and its limits: hardware.
    let has = choose_backend(
        RtBackend::Hardware,
        wgpu::Features::EXPERIMENTAL_RAY_QUERY,
        &limits,
    );
    assert_eq!((has.active, has.fallback), (RtBackend::Hardware, None));
    // The feature without the limits (0 primitives) is not enough.
    let zero = choose_backend(
        RtBackend::Hardware,
        wgpu::Features::EXPERIMENTAL_RAY_QUERY,
        &wgpu::Limits::default(),
    );
    assert_eq!(zero.active, RtBackend::Compute);

    // On this adapter, whatever it is: asking for hardware never fails, and draws a frame.
    let store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    let scene = Scene::new(&store, None);
    let mut gpu =
        Offscreen::with_backend(Resolution::native(32, 18), &scene, RtBackend::Hardware).unwrap();
    let rt = gpu.rt().clone();
    assert_eq!(rt.requested, RtBackend::Hardware);
    assert_eq!(rt.fallback.is_none(), rt.active == RtBackend::Hardware);
    eprintln!(
        "asked for hardware; ran {:?} ({:?})",
        rt.active, rt.fallback
    );
    let camera = Camera::looking_at([0.0, 1620.0, 100.0], [0.0, 1560.0, 0.0]);
    let image = gpu.render(&camera);
    assert_eq!((image.width, image.height), (32, 18));
    // The compute default draws the same frame where it fell back.
    if rt.active == RtBackend::Compute {
        let mut compute = Offscreen::new(32, 18, &scene).unwrap();
        assert!(gpu.hardware_update().is_none());
        assert_eq!(compute.rt().active, RtBackend::Compute);
        assert_eq!(compute.render(&camera).rgba, image.rgba);
    }
}

/// Rays through a patch of the test Planet, a core and a hand-made scene, from far and near,
/// along the axes and not.
fn rays_for(scene_name: &str) -> Vec<render::reference::Ray> {
    let mut rng = Lcg(2025);
    let mut rays = Vec::new();
    match scene_name {
        "patch" => {
            for _ in 0..4096 {
                let from = [
                    f64::from(rng.next() * 160.0 - 80.0),
                    1560.0 + f64::from(rng.next() * 100.0),
                    f64::from(rng.next() * 160.0 - 80.0),
                ];
                let to = [
                    f64::from(rng.next() * 160.0 - 80.0),
                    1500.0 + f64::from(rng.next() * 40.0),
                    f64::from(rng.next() * 160.0 - 80.0),
                ];
                rays.push(ray(from, to));
            }
        }
        _ => {
            for _ in 0..2048 {
                let from = [0, 1, 2].map(|_| f64::from(rng.next() * 500.0 - 250.0));
                let to = [0, 1, 2].map(|_| f64::from(rng.next() * 200.0 - 100.0));
                rays.push(ray(from, to));
            }
        }
    }
    rays
}

#[test]
#[ignore = "needs an adapter with ray queries; run it on the 4090"]
fn hardware_and_compute_find_the_same_hits_on_a_small_scene() {
    let patch = Scene::new(
        &generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12])),
        None,
    );
    let mut hand = BrickStore::new();
    for v in [
        [10, 0, 0],
        [-90, 35, 70],
        [100, 3, -50],
        [63, 5, 0],
        [64, 0, 0],
    ] {
        hand.set(VoxelCoord(v), Some(STONE));
    }
    let core = Scene::new(
        &hand,
        Some(Core {
            radius: 150,
            material: STONE,
        }),
    );
    for (name, scene) in [("patch", patch), ("core", core)] {
        let mut hw =
            Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
        if hw.rt().active != RtBackend::Hardware {
            eprintln!("skipped: {:?}", hw.rt().fallback);
            return;
        }
        let mut compute = Offscreen::new(8, 8, &scene).unwrap();
        let rays = rays_for(name);
        let (a, b) = (hw.trace_rays(&rays), compute.trace_rays(&rays));
        let (mut hits, mut differing) = (0, 0);
        for (r, (h, c)) in rays.iter().zip(a.iter().zip(&b)) {
            match (h, c) {
                (None, None) => {}
                (Some(h), Some(c)) => {
                    hits += 1;
                    // The same Voxel and Material; the distance as far as the BVH's floats allow.
                    if (h.voxel, h.material) != (c.voxel, c.material)
                        || (h.t - c.t).abs() > 1e-2 * (1.0 + c.t.abs())
                    {
                        differing += 1;
                        eprintln!("{name}: {r:?}\n hw {h:?}\n compute {c:?}");
                    }
                }
                _ => {
                    differing += 1;
                    eprintln!("{name}: hit and miss differ for {r:?}\n hw {h:?}\n compute {c:?}");
                }
            }
        }
        assert!(hits > 200, "{name}: only {hits} hits");
        // Rays that graze a box's edge may differ by rounding: a handful in thousands is the
        // BVH's floats, more is a bug.
        assert!(
            differing <= rays.len() / 200,
            "{name}: {differing} of {} differ",
            rays.len()
        );
        eprintln!("{name}: {hits} hits of {}, {differing} differ", rays.len());
    }
}

#[test]
fn an_edit_rebuilds_only_the_groups_it_touched_and_the_hardware_still_agrees_with_compute() {
    let mut store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    store.build_lod(5);
    let camera = [0.0, 1565.0, 0.0];
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    let mut scene = Scene::rings(&store, camera, Bands::with_d0(64.0), bound).unwrap();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    let first = hw.take_hardware_updates();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, "build");
    let total = first[0].blas_total;
    assert!(total > 1, "the scene has {total} groups");

    // Place a block of an odd Material a few Voxels across under the ground, at the middle of the
    // patch (setting air under the ground would do nothing: the rock there is not held).
    let mut changed = std::collections::BTreeSet::new();
    for x in 0..8 {
        for y in 1520..1528 {
            for z in 0..8 {
                let at = VoxelCoord([x, y, z]);
                store.set(at, Some(STONE));
                changed.insert(at.brick());
            }
        }
    }
    let changed: Vec<BrickCoord> = changed.into_iter().collect();
    store.refresh_lod(changed.iter().copied());
    let update = scene.update(&store, &changed).unwrap();
    hw.update_scene(&scene, &update).unwrap();
    let after = hw.take_hardware_updates();
    let patch = after.iter().find(|u| u.kind == "patch").unwrap_or_else(|| {
        panic!("an Edit's update is a patch of the groups it touched: {after:?}")
    });
    assert!(
        patch.blas_rebuilt >= 1 && patch.blas_rebuilt < total,
        "{} of {total} groups rebuilt",
        patch.blas_rebuilt
    );
    assert!(patch.tlas_rebuilt);

    // And what it draws is what compute draws of the edited scene, through the block.
    let mut rng = Lcg(5);
    let rays: Vec<_> = (0..1024)
        .map(|_| {
            let from = [
                f64::from(rng.next() * 40.0 - 20.0),
                1560.0 + f64::from(rng.next() * 20.0),
                f64::from(rng.next() * 40.0 - 20.0),
            ];
            let to = [
                f64::from(rng.next() * 8.0),
                1520.0 + f64::from(rng.next() * 8.0),
                f64::from(rng.next() * 8.0),
            ];
            ray(from, to)
        })
        .collect();
    let mut compute = Offscreen::new(8, 8, &scene).unwrap();
    let (a, b) = (hw.trace_rays(&rays), compute.trace_rays(&rays));
    let differing = a
        .iter()
        .zip(&b)
        .filter(|(h, c)| match (h, c) {
            (None, None) => false,
            (Some(h), Some(c)) => (h.voxel, h.material) != (c.voxel, c.material),
            _ => true,
        })
        .count();
    assert!(
        b.iter().flatten().count() > 100,
        "the rays should hit the block"
    );
    assert!(differing <= rays.len() / 200, "{differing} differ");
}
