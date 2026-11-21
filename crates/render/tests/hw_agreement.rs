//! The hardware ray-query backend (#50, a spike) against the compute `trace()`, and the choice
//! between them. Needs a Vulkan device: CI uses lavapipe, which has ray queries or not by its Mesa
//! (the dev box's, Mesa 25.0.7, has them). The tests of the fallback hold on either, through a
//! device made without the feature; those that compare the backends skip themselves, printing why,
//! on an adapter that has none (`cargo test -p render --test hw_agreement -- --nocapture` shows
//! which ran), and all of them run on the 4090.

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
    // Both hardware variants are chosen and refused alike.
    let both = choose_backend(
        RtBackend::HardwareNearest,
        wgpu::Features::EXPERIMENTAL_RAY_QUERY,
        &limits,
    );
    assert_eq!(both.active, RtBackend::HardwareNearest);
    let neither = choose_backend(RtBackend::HardwareNearest, wgpu::Features::empty(), &limits);
    assert_eq!(neither.active, RtBackend::Compute);
}

fn small_scene() -> Scene {
    Scene::new(
        &generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12])),
        None,
    )
}

/// The real device request without the feature: an adapter that has no ray queries (lavapipe in
/// CI, if its Mesa has none; any adapter here, by hiding them) runs compute with no error, says why,
/// has no acceleration structures, and draws what the default backend draws.
#[test]
fn a_device_without_ray_queries_runs_compute_with_no_error_and_draws_what_the_default_draws() {
    let scene = small_scene();
    let camera = Camera::looking_at([0.0, 1620.0, 100.0], [0.0, 1560.0, 0.0]);
    let mut compute = Offscreen::new(32, 18, &scene).unwrap();
    let want = compute.render(&camera);
    for backend in [RtBackend::Hardware, RtBackend::HardwareNearest] {
        let mut gpu =
            Offscreen::without_ray_queries(Resolution::native(32, 18), &scene, backend).unwrap();
        let rt = gpu.rt().clone();
        assert_eq!(rt.requested, backend);
        assert_eq!(rt.active, RtBackend::Compute);
        assert!(rt.fallback.is_some_and(|r| r.contains("ray quer")));
        assert!(gpu.hardware_update().is_none());
        assert!(gpu.take_hardware_updates().is_empty());
        assert_eq!(gpu.render(&camera).rgba, want.rgba);
    }
}

/// Asking for hardware on this adapter, whatever it is, never fails: it runs what it can, and the
/// choice says which.
#[test]
fn asking_for_hardware_never_fails_and_says_what_ran() {
    let scene = small_scene();
    let gpu =
        Offscreen::with_backend(Resolution::native(32, 18), &scene, RtBackend::Hardware).unwrap();
    let rt = gpu.rt();
    assert_eq!(rt.requested, RtBackend::Hardware);
    assert_eq!(rt.fallback.is_none(), rt.active == RtBackend::Hardware);
    eprintln!(
        "asked for hardware; ran {:?} ({:?})",
        rt.active, rt.fallback
    );
}

/// The lit frame of each hardware variant is the compute frame: the sun's shadow rays, which end at
/// the first hit with `Hardware` and look for the nearest with `HardwareNearest`, find what the
/// compute walk finds. Pixels may differ only where a ray grazes a box's edge.
#[test]
fn the_shadows_of_both_hardware_variants_are_the_compute_backends() {
    let scene = small_scene();
    let camera = Camera::looking_at([0.0, 1620.0, 100.0], [0.0, 1560.0, 0.0]);
    let mut compute = Offscreen::new(32, 18, &scene).unwrap();
    let want = compute.render(&camera);
    for backend in [RtBackend::Hardware, RtBackend::HardwareNearest] {
        let mut gpu = Offscreen::with_backend(Resolution::native(32, 18), &scene, backend).unwrap();
        if gpu.rt().active != backend {
            eprintln!("skipped: {:?}", gpu.rt().fallback);
            return;
        }
        let got = gpu.render(&camera);
        let differing = got
            .rgba
            .chunks(4)
            .zip(want.rgba.chunks(4))
            .filter(|(a, b)| a.iter().zip(*b).any(|(&a, &b)| a.abs_diff(b) > 2))
            .count();
        assert!(
            differing <= 32 * 18 / 100,
            "{backend:?}: {differing} of {} pixels differ from compute's",
            32 * 18
        );
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

/// Runs wherever the adapter has ray queries (CI's lavapipe, if its Mesa has them: the test prints
/// why when it skips itself); `--nocapture` shows the counts.
#[test]
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

/// Builds the rings of a patch of the test Planet around `camera`, builds the hardware backend's
/// acceleration structures for it, places a block of Stone, `size` Voxels each way, on the ground at
/// `x, z` (Voxels), and brings the backend up to date as the bench's Edit stress does. Returns the
/// scene's patch, the update that the backend recorded and how many groups there are in all, after
/// checking that the hits of the hardware backend and of a fresh compute backend on rays down at the
/// block are the same. `None` where the adapter has no ray queries.
fn place_a_block_and_compare(
    camera: [f64; 3],
    (x0, z0): (i32, i32),
    size: i32,
) -> Option<(render::scene::RingPatch, render::HwUpdate, u32)> {
    let mut store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    store.build_lod(5);
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    let mut scene = Scene::rings(&store, camera, Bands::with_d0(64.0), bound).unwrap();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return None;
    }
    let first = hw.take_hardware_updates();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, "build");
    let total = first[0].blas_total;
    assert!(total > 1, "the scene has {total} groups");

    // A block of Stone on the ground: higher than the bricks' tight boxes are, so a box that is
    // not made again is below the block.
    let top = (1400..1650)
        .rev()
        .find(|&y| store.get(VoxelCoord([x0, y, z0])).is_some())
        .expect("ground in the patch");
    let mut changed = std::collections::BTreeSet::new();
    for x in x0..x0 + size {
        for y in top + 1..top + 1 + size {
            for z in z0..z0 + size {
                let at = VoxelCoord([x, y, z]);
                store.set(at, Some(STONE));
                changed.insert(at.brick());
            }
        }
    }
    let changed: Vec<BrickCoord> = changed.into_iter().collect();
    store.refresh_lod(changed.iter().copied());
    let update = scene.update(&store, &changed).unwrap();
    let render::scene::SceneUpdate::Rings(patch) = &update else {
        panic!("an Edit of rings is a patch of rings");
    };
    let patch = patch.clone();
    hw.update_scene(&scene, &update).unwrap();
    let after = hw.take_hardware_updates();
    assert_eq!(after.len(), 1, "{after:?}");
    let recorded = after[0].clone();

    // Rays down at the block from above: what they hit is the block, through the old boxes' tops.
    let mut rng = Lcg(5);
    let top = top as f64;
    let rays: Vec<_> = (0..1024)
        .map(|_| {
            let x = f64::from(x0) + f64::from(rng.next() * size as f32);
            let z = f64::from(z0) + f64::from(rng.next() * size as f32);
            let from = [
                x + f64::from(rng.next() - 0.5),
                top + 60.0,
                z + f64::from(rng.next() - 0.5),
            ];
            ray(from, [x, top + 1.0 + f64::from(size) / 2.0, z])
        })
        .collect();
    let mut compute = Offscreen::new(8, 8, &scene).unwrap();
    let (a, b) = (hw.trace_rays(&rays), compute.trace_rays(&rays));
    let differing = a
        .iter()
        .zip(&b)
        .filter(|(h, c)| match (h, c) {
            (None, None) => false,
            (Some(h), Some(c)) => {
                (h.voxel, h.material) != (c.voxel, c.material)
                    || (h.t - c.t).abs() > 1e-2 * (1.0 + c.t.abs())
            }
            _ => true,
        })
        .count();
    assert!(
        b.iter().flatten().count() > 900,
        "the rays should hit the block"
    );
    assert!(
        differing <= rays.len() / 200,
        "{differing} of {} differ (hw {:?} compute {:?})",
        rays.len(),
        a.iter().flatten().next(),
        b.iter().flatten().next()
    );
    Some((patch, recorded, total))
}

#[test]
fn an_edit_near_the_camera_rebuilds_only_the_groups_it_touched_and_the_hardware_agrees() {
    let Some((_, update, total)) = place_a_block_and_compare([0.0, 1565.0, 0.0], (0, 0), 8) else {
        return;
    };
    assert_eq!(update.kind, "patch", "{update:?}");
    assert!(
        update.blas_rebuilt >= 1 && update.blas_rebuilt < total,
        "{} of {total} groups rebuilt",
        update.blas_rebuilt
    );
    assert!(update.tlas_rebuilt);
}

/// A coarse brick whose words are written in place, with no word of `regions` or `brick_refs`
/// written for it, still has a stale box (it is as tight as the words it was made from): the
/// patch names its region in `rewritten_coarse`, and the group is made again. Without that, the
/// update is `none` and the rays hit the ground under the block.
#[test]
fn an_edit_in_place_at_a_coarse_level_makes_its_group_again() {
    let Some((patch, update, total)) = place_a_block_and_compare([0.0, 1565.0, 0.0], (100, 100), 4)
    else {
        return;
    };
    assert!(
        !patch.rewritten_coarse.is_empty(),
        "the case is not one of a coarse brick rewritten in place: {patch:?}"
    );
    assert_eq!(update.kind, "patch", "{update:?}");
    assert!(
        update.blas_rebuilt >= 1 && update.blas_rebuilt < total,
        "{} of {total} groups rebuilt",
        update.blas_rebuilt
    );
}
