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

/// A flat stone floor with a tall pillar at its middle, in a world whose up is +y: a long shadow.
fn pillar_scene() -> Scene {
    let mut store = BrickStore::new();
    for x in -40..40 {
        for z in -40..40 {
            store.set(VoxelCoord([x, -1, z]), Some(STONE));
        }
    }
    for y in 0..30 {
        for x in 0..8 {
            for z in 0..8 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    Scene::new(&store, None)
}

/// The linear light of the frame, the post stages off: one number a pixel (the green channel).
fn linear_green(gpu: &mut Offscreen, camera: &Camera) -> Vec<f32> {
    gpu.set_stages(render::Stages::NONE);
    gpu.render(camera);
    let ((_, _, light), _) = gpu.read_hdr_and_bloom();
    light.iter().map(|p| p[1]).collect()
}

/// The moon is the key light by night (#203): the same shadow ray goes towards it, on each backend,
/// and finds what the compute walk finds. The frame is of a pillar on a floor (a long shadow), in
/// linear light (a night's shadows are not lost in the 8-bit encoding), with the sun and the sky
/// alone: a backend that cast no shadows would light every pixel the moon cannot reach.
#[test]
fn the_moons_shadows_are_the_same_on_the_hardware_backend_as_on_compute() {
    const SIZE: u32 = 96;
    let scene = pillar_scene();
    let camera = Camera::looking_at([4.0, 100.0, 4.4], [4.0, 0.0, 4.0]);
    let sky = render::daylight::DaySky::at(0.9);
    assert!(sky.moon[1] > 0.2, "the moon is up at 0.9: {:?}", sky.moon);
    let mut compute = Offscreen::new(SIZE, SIZE, &scene).unwrap();
    compute.set_sun_and_sky_only(true);
    compute.set_day_sky(sky, true);
    assert!(
        compute.key_light(&camera).is_moon,
        "the key at 0.9 is the moon"
    );
    let want = linear_green(&mut compute, &camera);
    // There is a shadow to agree on: a good share of the pixels are far darker than the lit floor.
    let lit = want.iter().copied().fold(0.0f32, f32::max);
    let shadowed = want.iter().filter(|&&l| l < 0.5 * lit).count();
    assert!(
        shadowed > want.len() / 50,
        "{shadowed} of {} pixels in shadow",
        want.len()
    );
    for backend in [RtBackend::Hardware, RtBackend::HardwareNearest] {
        let mut gpu =
            Offscreen::with_backend(Resolution::native(SIZE, SIZE), &scene, backend).unwrap();
        if gpu.rt().active != backend {
            eprintln!("skipped: {:?}", gpu.rt().fallback);
            return;
        }
        gpu.set_sun_and_sky_only(true);
        gpu.set_day_sky(sky, true);
        assert!(gpu.key_light(&camera).is_moon);
        let got = linear_green(&mut gpu, &camera);
        let differing = got
            .iter()
            .zip(&want)
            .filter(|(g, w)| (**g - **w).abs() > 0.05 * g.max(**w) + 1e-6)
            .count();
        assert!(
            differing <= want.len() / 200,
            "{backend:?}: {differing} of {} pixels differ from compute's by the moon",
            want.len()
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
    grouping: Grouping,
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
    let mut total = first[0].blas_total;
    if grouping != Grouping::default() {
        hw.set_hardware_grouping(&scene, grouping);
        total = hw.take_hardware_updates().last().unwrap().blas_total;
    }
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
    for grouping in groupings() {
        let Some((_, update, total)) =
            place_a_block_and_compare([0.0, 1565.0, 0.0], (0, 0), 8, grouping)
        else {
            return;
        };
        assert_eq!(update.kind, "patch", "{update:?}");
        assert!(
            update.blas_rebuilt >= 1 && update.blas_rebuilt < total.max(2),
            "{} of {total} groups rebuilt, groups of {}",
            update.blas_rebuilt,
            grouping.describe()
        );
        assert!(update.tlas_rebuilt);
    }
}

/// A coarse brick whose words are written in place, with no word of `regions` or `brick_refs`
/// written for it, still has a stale box (it is as tight as the words it was made from): the
/// patch lists its group (`RingPatch::groups`), and the group is made again. Without that, the
/// update is `none` and the rays hit the ground under the block.
#[test]
fn an_edit_in_place_at_a_coarse_level_makes_its_group_again() {
    let Some((patch, update, total)) =
        place_a_block_and_compare([0.0, 1565.0, 0.0], (100, 100), 4, Grouping::default())
    else {
        return;
    };
    assert!(
        patch.regions.is_empty() && patch.groups.iter().any(|&((level, _), _)| level > 0),
        "the case is not one of a coarse brick rewritten in place: {patch:?}"
    );
    assert_eq!(update.kind, "patch", "{update:?}");
    assert!(
        update.blas_rebuilt >= 1 && update.blas_rebuilt < total,
        "{} of {total} groups rebuilt",
        update.blas_rebuilt
    );
}

// ----------------------------------------------------------------------------------------------
// #236: the boxes are made on the GPU, and the acceleration structures follow streaming without
// waiting for it.

use render::hw::{Grouping, blas_counts, blas_prims, group_counts, group_prims};

/// The groupings the tests that follow the scene are run under: a cell a BLAS, BLASes of 8
/// regions an edge, and a mixture (4 below level 2, 32 above).
fn groupings() -> [Grouping; 3] {
    [
        Grouping::default(),
        Grouping::uniform(8).unwrap(),
        Grouping::tiers(4, &[(16, 1), (32, 2)]).unwrap(),
    ]
}

/// The boxes of every group of `scene` as the GPU makes them, and as the CPU makes them: equal,
/// box for box. Runs on any adapter (the pass needs no ray queries).
fn assert_the_gpu_makes_the_cpus_boxes(label: &str, scene: &Scene) -> usize {
    let counts = group_counts(scene);
    assert!(!counts.is_empty(), "{label}: no groups");
    let gpu = Offscreen::new(8, 8, scene).unwrap();
    let jobs: Vec<(u8, [i32; 3], u32)> = counts.iter().map(|(&(l, g), &n)| (l, g, n)).collect();
    let made = gpu.box_pass_boxes(&jobs);
    let mut boxes = 0;
    for ((&(level, group), _), got) in counts.iter().zip(&made) {
        let want: Vec<[u32; 8]> = group_prims(scene, level as usize, group)
            .iter()
            .map(render::hw::Prim::record)
            .collect();
        assert_eq!(
            got.len(),
            want.len(),
            "{label}: level {level} group {group:?}"
        );
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "{label}: level {level} group {group:?} box {i}");
        }
        boxes += want.len();
    }
    boxes
}

/// The test Planet's rings (every level), a core (whole regions that are one Material), a
/// hand-made scene (solid bricks, bricks of one Voxel, bricks of a few at the corners) and the
/// rings after an Edit: the GPU's boxes are the CPU's.
#[test]
fn the_gpu_makes_the_cpus_boxes_box_for_box() {
    let mut store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    store.build_lod(5);
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    let mut rings = Scene::rings(&store, [0.0, 1565.0, 0.0], Bands::with_d0(64.0), bound).unwrap();
    let n = assert_the_gpu_makes_the_cpus_boxes("rings", &rings);
    assert!(n > 1000, "the rings have {n} boxes");
    // A level of every kind: the coarse ones have bricks of their own.
    assert!(group_counts(&rings).keys().any(|&(level, _)| level >= 1));

    let mut hand = BrickStore::new();
    for v in [
        [10, 0, 0],
        [-90, 35, 70],
        [100, 3, -50],
        [63, 5, 0],
        [64, 0, 0],
        [7, 7, 7],
        [0, 0, 0],
        [1, 1, 1],
    ] {
        hand.set(VoxelCoord(v), Some(STONE));
    }
    hand.insert(
        BrickCoord([5, 5, 5]),
        brick_store::Brick::solid(Material(300)),
    );
    let hand_scene = Scene::new(
        &hand,
        Some(Core {
            radius: 150,
            material: STONE,
        }),
    );
    assert_the_gpu_makes_the_cpus_boxes("hand and core", &hand_scene);
    let core = Scene::new(
        &BrickStore::new(),
        Some(Core {
            radius: 700,
            material: STONE,
        }),
    );
    let n = assert_the_gpu_makes_the_cpus_boxes("core", &core);
    assert!(n > 1);

    // After an Edit, and after the rings move (a region gains and loses bricks).
    let top = (1400..1650)
        .rev()
        .find(|&y| store.get(VoxelCoord([3, y, 3])).is_some())
        .unwrap();
    let mut changed = std::collections::BTreeSet::new();
    for x in 3..7 {
        for y in top + 1..top + 5 {
            for z in 3..7 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
                changed.insert(VoxelCoord([x, y, z]).brick());
            }
        }
    }
    let changed: Vec<BrickCoord> = changed.into_iter().collect();
    store.refresh_lod(changed.iter().copied());
    rings.update(&store, &changed).unwrap();
    assert_the_gpu_makes_the_cpus_boxes("rings after an Edit", &rings);
    for step in 1..=4 {
        let camera = [f64::from(step) * 30.0, 1565.0, -f64::from(step) * 20.0];
        rings.recentre(&store, camera).unwrap();
    }
    assert_the_gpu_makes_the_cpus_boxes("rings after a walk", &rings);
}

/// How many of `rays` the hardware backend and a fresh compute backend disagree on, and how many
/// hit (rounding at a box's edge may flip a handful).
fn disagreement(
    hw: &mut Offscreen,
    scene: &Scene,
    rays: &[render::reference::Ray],
) -> (usize, usize) {
    let mut compute = Offscreen::new(8, 8, scene).unwrap();
    let (a, b) = (hw.trace_rays(rays), compute.trace_rays(rays));
    let hits = b.iter().flatten().count();
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
    (hits, differing)
}

/// How the hardware backend's hits differ from a fresh compute backend's on `rays`: what the
/// hardware does not hit that compute does (a hole), hits farther than compute (a hole that
/// something behind fills), and any other difference (a different Voxel or Material, a nearer hit
/// or a hit where compute has none).
#[derive(Debug, Default, PartialEq)]
struct Kinds {
    hits: usize,
    holes: usize,
    farther: usize,
    other: usize,
}

fn kinds_of_disagreement(
    hw: &mut Offscreen,
    compute: &mut Offscreen,
    rays: &[render::reference::Ray],
) -> Kinds {
    let (a, b) = (hw.trace_rays(rays), compute.trace_rays(rays));
    let mut kinds = Kinds {
        hits: b.iter().flatten().count(),
        ..Kinds::default()
    };
    for (h, c) in a.iter().zip(&b) {
        match (h, c) {
            (None, None) => {}
            (None, Some(_)) => kinds.holes += 1,
            (Some(h), Some(c)) if (h.voxel, h.material) == (c.voxel, c.material) => {
                if (h.t - c.t).abs() > 1e-2 * (1.0 + c.t.abs()) {
                    kinds.other += 1;
                }
            }
            (Some(h), Some(c)) if h.t > c.t => kinds.farther += 1,
            _ => kinds.other += 1,
        }
    }
    kinds
}

/// Rays down at the ground of the test Planet's patch from `height` Voxels up, at a camera at `at`
/// (x, z), with a spread of 60 Voxels.
fn rays_down_from(at: [f64; 2], height: f64, seed: u32) -> Vec<render::reference::Ray> {
    let mut rng = Lcg(u64::from(seed));
    (0..1024)
        .map(|_| {
            let x = at[0] + f64::from(rng.next() * 120.0 - 60.0);
            let z = at[1] + f64::from(rng.next() * 120.0 - 60.0);
            let from = [at[0], 1500.0 + height, at[1]];
            ray(from, [x, 1500.0 + f64::from(rng.next() * 40.0), z])
        })
        .collect()
}

fn walking_scene() -> (BrickStore, Scene) {
    let mut store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    store.build_lod(5);
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    let scene = Scene::rings(&store, [-60.0, 1565.0, -60.0], Bands::with_d0(64.0), bound).unwrap();
    (store, scene)
}

/// The structures are as the scene is: every group the scene has is built, from as many boxes as
/// it has, and the boxes that the GPU holds are the CPU's, and no group is there that the scene
/// has not.
fn assert_not_stale(label: &str, hw: &Offscreen, scene: &Scene) {
    assert_eq!(hw.hardware_pending(), 0, "{label}");
    let built = hw.hardware_groups();
    let grouping = hw.hardware_grouping();
    assert_eq!(
        built,
        blas_counts(scene, &grouping),
        "{label}: groups and their boxes"
    );
    for ((level, group), boxes) in hw.hardware_boxes() {
        let want: Vec<[u32; 8]> = blas_prims(scene, &grouping, level as usize, group)
            .iter()
            .map(render::hw::Prim::record)
            .collect();
        assert_eq!(boxes, want, "{label}: level {level} group {group:?}");
    }
}

/// Ring moves (groups gained, changed and emptied), at the full budget and at one so small that
/// the updates queue: hits agree with compute's once the queue is built, no group is stale then,
/// and while a group waits its old BLAS stays in the TLAS (no group of the scene that was built
/// goes, no hole), and the hits through what did not change are as before.
#[test]
fn the_hardware_follows_the_rings_as_they_move_with_no_group_stale_and_no_hole() {
    let budgets = [render::hw::BUILD_BUDGET, 1];
    for (budget, grouping) in budgets
        .into_iter()
        .flat_map(|b| groupings().into_iter().map(move |g| (b, g)))
    {
        let (store, mut scene) = walking_scene();
        let mut hw =
            Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
        if hw.rt().active != RtBackend::Hardware {
            eprintln!("skipped: {:?}", hw.rt().fallback);
            return;
        }
        hw.set_hardware_grouping(&scene, grouping);
        hw.take_hardware_updates();
        assert_not_stale("the first build", &hw, &scene);
        hw.set_hardware_budget(budget);
        let mut ever_pending = 0;
        let mut waiting_at_a_trace = false;
        let mut patches = 0;
        for step in 1..=10 {
            let camera = [
                -60.0 + f64::from(step) * 14.0,
                1565.0,
                -60.0 + f64::from(step) * 9.0,
            ];
            let before = hw.hardware_groups();
            let Some(update) = scene.recentre(&store, camera).unwrap() else {
                continue;
            };
            patches += 1;
            hw.update_scene(&scene, &update).unwrap();
            ever_pending = ever_pending.max(hw.hardware_pending());
            // No hole: a group that was built and that the scene still has is still built.
            let wanted = blas_counts(&scene, &grouping);
            let now = hw.hardware_groups();
            for key in before.keys().filter(|k| wanted.contains_key(k)) {
                assert!(
                    now.contains_key(key),
                    "budget {budget}, {}: step {step}: group {key:?} went",
                    grouping.describe()
                );
            }
            // Whatever is built traces as the compute path does where nothing changed: rays
            // down at where the camera is.
            let (hits, differing) = disagreement(
                &mut hw,
                &scene,
                &rays_down_from([camera[0], camera[2]], 65.0, step as u32),
            );
            if budget == render::hw::BUILD_BUDGET {
                assert_eq!(hw.hardware_pending(), 0);
                assert!(hits > 300, "step {step}: {hits} hits");
                assert!(differing <= 1024 / 100, "step {step}: {differing} differ");
            } else {
                // No stale tracing while groups wait (#236): the hits are compute's where the
                // camera is, ahead of it (where the rings gain bricks) and behind it (where they
                // lose them), with nothing missed and nothing found farther. A ray that
                // goes through the trace builds what is next in the queue first, as a draw does.
                waiting_at_a_trace |= hw.hardware_pending() > 0;
                let mut compute = Offscreen::new(8, 8, &scene).unwrap();
                for (n, at) in [[0.0, 0.0], [90.0, 60.0], [-90.0, -60.0], [0.0, 120.0]]
                    .into_iter()
                    .enumerate()
                {
                    let kinds = kinds_of_disagreement(
                        &mut hw,
                        &mut compute,
                        &rays_down_from([camera[0] + at[0], camera[2] + at[1]], 65.0, 7 + n as u32),
                    );
                    assert_eq!(
                        (kinds.holes, kinds.farther),
                        (0, 0),
                        "step {step}, rays at {at:?}: {kinds:?}, {} groups wait",
                        hw.hardware_pending()
                    );
                    assert!(
                        kinds.other <= 1024 / 100,
                        "step {step}, rays at {at:?}: {kinds:?}"
                    );
                }
            }
        }
        assert!(patches >= 3, "the rings moved {patches} times");
        if budget == 1 {
            assert!(ever_pending > 0, "a budget of one box left nothing waiting");
            assert!(waiting_at_a_trace, "no group waited at a trace");
        }
        // Built to the end of the queue: not stale.
        hw.settle_hardware();
        assert_not_stale(
            &format!("budget {budget}, groups of {}", grouping.describe()),
            &hw,
            &scene,
        );
        let (hits, differing) = disagreement(
            &mut hw,
            &scene,
            &rays_down_from([-60.0 + 140.0, -60.0 + 90.0], 65.0, 99),
        );
        assert!(
            hits > 300 && differing <= 1024 / 100,
            "{hits} hits, {differing} differ"
        );
    }
}

/// A group of a queue that has waited is built from the scene as it is when its turn comes, not as
/// it was when it was queued: an Edit in a group that waited and another after it.
#[test]
fn a_group_that_waits_is_built_from_the_scene_as_it_is_then_and_edits_show_at_once() {
    let (mut store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.set_hardware_budget(1);
    // Moves that queue groups far from the camera.
    for step in 1..=6 {
        let camera = [-60.0 + f64::from(step) * 22.0, 1565.0, -60.0];
        if let Some(update) = scene.recentre(&store, camera).unwrap() {
            hw.update_scene(&scene, &update).unwrap();
        }
    }
    let waiting = hw.hardware_pending();
    // An Edit at the camera, with groups still waiting: it is the nearest, so it is built in the
    // update that brings it, and the rays down at the block agree.
    let camera = [-60.0 + 6.0 * 22.0, 1565.0, -60.0];
    let (x0, z0) = (camera[0] as i32, camera[2] as i32);
    let top = (1400..1650)
        .rev()
        .find(|&y| store.get(VoxelCoord([x0, y, z0])).is_some())
        .expect("ground");
    let mut changed = std::collections::BTreeSet::new();
    for x in x0..x0 + 6 {
        for y in top + 1..top + 7 {
            for z in z0..z0 + 6 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
                changed.insert(VoxelCoord([x, y, z]).brick());
            }
        }
    }
    let changed: Vec<BrickCoord> = changed.into_iter().collect();
    store.refresh_lod(changed.iter().copied());
    let update = scene.update(&store, &changed).unwrap();
    hw.take_hardware_updates();
    hw.update_scene(&scene, &update).unwrap();
    let edit = hw.take_hardware_updates();
    assert_eq!(edit.len(), 1);
    assert!(edit[0].blas_rebuilt >= 1, "{edit:?}");
    let mut rng = Lcg(3);
    let top = top as f64;
    let rays: Vec<_> = (0..512)
        .map(|_| {
            let x = f64::from(x0) + f64::from(rng.next() * 6.0);
            let z = f64::from(z0) + f64::from(rng.next() * 6.0);
            ray([x, top + 50.0, z], [x, top + 4.0, z])
        })
        .collect();
    let (hits, differing) = disagreement(&mut hw, &scene, &rays);
    assert!(hits > 450, "{hits} hits on the block");
    assert!(
        differing <= rays.len() / 100,
        "{differing} differ with {waiting} groups waiting"
    );
    hw.settle_hardware();
    assert_not_stale("after the queue", &hw, &scene);
}

/// A jump is a scene made again: every group is built at once, whatever the budget, and nothing of
/// the old one is left.
#[test]
fn a_jump_builds_every_group_at_once_and_leaves_none_of_the_old_scene() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.set_hardware_budget(1);
    hw.take_hardware_updates();
    let update = scene
        .jump_to(&store, [60.0, 1565.0, 60.0], Bands::with_d0(32.0))
        .unwrap()
        .expect("rings");
    assert_eq!(update, render::scene::SceneUpdate::Rebuilt);
    hw.update_scene(&scene, &update).unwrap();
    let log = hw.take_hardware_updates();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].kind, "build");
    assert_eq!(log[0].pending, 0);
    assert_not_stale("after a jump", &hw, &scene);
    let (hits, differing) = disagreement(&mut hw, &scene, &rays_down_from([60.0, 60.0], 65.0, 7));
    assert!(
        hits > 300 && differing <= 1024 / 100,
        "{hits} hits, {differing} differ"
    );
}

/// The TLAS's frame follows the camera without waiting and without changing a hit: rays from far
/// above (a camera 9000 Voxels up) move it, rays from the ground move it back.
#[test]
fn the_tlas_frame_moves_with_the_camera_and_the_hits_stay_compute_s() {
    let (_, scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    let first = hw.take_hardware_updates();
    for (height, seed) in [(9000.0, 1u32), (65.0, 2), (9000.0, 3)] {
        let (hits, differing) =
            disagreement(&mut hw, &scene, &rays_down_from([0.0, 0.0], height, seed));
        assert!(hits > 300, "from {height}: {hits} hits");
        assert!(differing <= 1024 / 100, "from {height}: {differing} differ");
    }
    let moves = hw.take_hardware_updates();
    assert!(
        moves.iter().filter(|u| u.kind == "anchor").count() >= 2,
        "{moves:?} (first {first:?})"
    );
    assert!(moves.iter().all(|u| u.blas_rebuilt == 0), "{moves:?}");
    assert_not_stale("after the frame moved", &hw, &scene);
}

/// The box pass dispatches a chunk of jobs at a time (a dimension of a dispatch is 65535 and a jump
/// has a job for every group of the Planet): the boxes are the same whatever the chunk, so
/// that a jump of any size makes the CPU's boxes and cannot panic the frame thread.
#[test]
fn the_box_pass_makes_the_same_boxes_in_one_dispatch_or_in_many() {
    let (_, scene) = walking_scene();
    let counts = group_counts(&scene);
    let once: Vec<(u8, [i32; 3], u32)> = counts.iter().map(|(&(l, g), &n)| (l, g, n)).collect();
    // The groups three times over (a chunk is at least 8 jobs: 256 bytes of them).
    let jobs: Vec<_> = (0..3).flat_map(|_| once.clone()).collect();
    assert!(jobs.len() > 16, "{} jobs", jobs.len());
    let gpu = Offscreen::new(8, 8, &scene).unwrap();
    let whole = gpu.box_pass_boxes(&jobs);
    let in_chunks = gpu.box_pass_boxes_chunked(&jobs, 8);
    assert_eq!(whole, in_chunks);
    assert!(whole.iter().map(Vec::len).sum::<usize>() > 1000);
    assert_eq!(whole[..once.len()], whole[once.len()..2 * once.len()]);
}

/// The budget is a frame's, not a draw's: the draws of one frame (the views of a window's frame)
/// have one build of what waits between them, a frame after it another, and what has waited
/// `max_wait` frames is built whatever the budget.
#[test]
fn the_build_budget_is_a_frames_whatever_its_views_and_what_waits_too_long_is_built() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.set_hardware_budget(1);
    hw.set_hardware_max_wait(100);
    // Moves, with no draw between them, until several groups wait.
    let mut camera = [-60.0, 1565.0, -60.0];
    for step in 1..=10 {
        camera = [
            -60.0 + f64::from(step) * 14.0,
            1565.0,
            -60.0 + f64::from(step) * 9.0,
        ];
        if let Some(update) = scene.recentre(&store, camera).unwrap() {
            hw.update_scene(&scene, &update).unwrap();
        }
    }
    let waiting = hw.hardware_pending();
    assert!(waiting >= 4, "{waiting} groups wait");
    // The frame the moves were in is closed by its draw, which has the budget its updates left.
    hw.follow_hardware(Some(0), camera);
    let waiting = hw.hardware_pending();
    hw.take_hardware_updates();
    // Frame 1: two views. The first has what the budget leaves (one group, which the frame has not
    // yet had); the second has none.
    hw.follow_hardware(Some(1), camera);
    let after_first = hw.hardware_pending();
    hw.follow_hardware(Some(1), camera);
    let rebuilt: u32 = hw
        .take_hardware_updates()
        .iter()
        .map(|u| u.blas_rebuilt)
        .sum();
    assert_eq!(
        (after_first, hw.hardware_pending(), rebuilt),
        (waiting - 1, waiting - 1, 1),
        "two views of one frame built more than one group of {waiting}"
    );
    // Frame 2: one more.
    hw.follow_hardware(Some(2), camera);
    hw.follow_hardware(Some(2), camera);
    assert_eq!(hw.hardware_pending(), waiting - 2);
    // Frame 3: what is left has waited 3 frames (0 to 2), which is as long as it may: all of it is
    // built, whatever the budget.
    hw.set_hardware_max_wait(3);
    hw.follow_hardware(Some(3), camera);
    assert_eq!(hw.hardware_pending(), 0, "a group that waited stayed");
    assert_not_stale("after the aged groups", &hw, &scene);
}

/// A log nobody takes (the window client never does) does not grow without end.
#[test]
fn the_log_of_updates_keeps_the_latest_few_thousand_when_nobody_takes_it() {
    let (_, scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    let nothing = render::SceneUpdate::Rings(render::RingPatch::default());
    for _ in 0..render::hw::LOG_LIMIT + 50 {
        hw.update_scene(&scene, &nothing).unwrap();
    }
    assert_eq!(hw.take_hardware_updates().len(), render::hw::LOG_LIMIT);
    assert!(hw.take_hardware_updates().is_empty());
}

/// The builds of a draw that follows the camera are counted as the hardware backend's (the bench
/// takes them out of the encode and puts them in the frame's update, #236): a draw that builds a
/// group that waited has `DrawTimes::hardware` of the time that took, inside `encode`; one that
/// builds nothing has none.
#[test]
fn a_draw_that_builds_a_group_that_waited_counts_it_as_the_hardwares() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(32, 18), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.set_hardware_budget(1);
    let mut camera = [-60.0, 1565.0, -60.0];
    for step in 1..=10 {
        camera = [
            -60.0 + f64::from(step) * 14.0,
            1565.0,
            -60.0 + f64::from(step) * 9.0,
        ];
        if let Some(update) = scene.recentre(&store, camera).unwrap() {
            hw.update_scene(&scene, &update).unwrap();
        }
    }
    assert!(hw.hardware_pending() > 0);
    let looking = render::Camera::looking_at(camera, [camera[0], 1500.0, camera[2] + 100.0]);
    hw.take_hardware_updates();
    // Frame 1 closes the frame of the moves; frame 2 builds one group that waited.
    hw.draw_upscaled_timed(&looking);
    hw.draw_upscaled_timed(&looking);
    let drawn = hw.last_draw_times();
    assert!(
        drawn.hardware > std::time::Duration::ZERO && drawn.hardware <= drawn.encode,
        "{drawn:?}"
    );
    hw.settle_hardware();
    hw.draw_upscaled_timed(&looking);
    assert_eq!(
        hw.last_draw_times().hardware,
        std::time::Duration::ZERO,
        "a draw that builds nothing"
    );
}

/// The allocator has the memory blocks before the first build and keeps them (#236): the builds'
/// scratch buffers and the pool's buffers find room, and none of the updates makes a block, which
/// stalled the GPU for about 6.5 ms on the frame that did on the 4090.
#[test]
fn the_hardware_backend_has_its_spare_blocks_and_its_updates_make_no_block() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    let Some(start) = hw.device_memory() else {
        eprintln!("skipped: the allocator cannot say what it has");
        return;
    };
    // Six buffers of 60 MiB, in whatever blocks the device makes of them.
    assert!(start.reserved_bytes >= 360 << 20, "{start:?}");
    assert_eq!(
        render::hw::SPARE_BUFFERS as u64 * render::hw::SPARE_BUFFER_BYTES,
        360 << 20
    );
    for step in 1..=6 {
        let camera = [
            -60.0 + f64::from(step) * 14.0,
            1565.0,
            -60.0 + f64::from(step) * 9.0,
        ];
        if let Some(update) = scene.recentre(&store, camera).unwrap() {
            hw.update_scene(&scene, &update).unwrap();
        }
    }
    let log = hw.take_hardware_updates();
    assert!(log.iter().any(|u| u.blas_rebuilt > 0), "{log:?}");
    let end = hw.device_memory().unwrap();
    assert_eq!(end.blocks, start.blocks, "{start:?} then {end:?}");
}

/// An update's phases are the parts of its cost: they add up to `prepare + build` (the bench's
/// split of the hardware backend's frame-thread time is of these, #236), and an update that built
/// something spent time in the plan, the recording and the submit.
#[test]
fn the_phases_of_an_update_add_up_to_what_it_cost() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.take_hardware_updates();
    for step in 1..=6 {
        let camera = [
            -60.0 + f64::from(step) * 14.0,
            1565.0,
            -60.0 + f64::from(step) * 9.0,
        ];
        if let Some(update) = scene.recentre(&store, camera).unwrap() {
            hw.update_scene(&scene, &update).unwrap();
        }
    }
    let log = hw.take_hardware_updates();
    let built: Vec<_> = log.iter().filter(|u| u.blas_rebuilt > 0).collect();
    assert!(!built.is_empty(), "{log:?}");
    for u in &log {
        let p = u.phases;
        let sum = p.before
            + p.plan
            + p.instances
            + p.boxes
            + p.wait_boxes
            + p.builds
            + p.finish
            + p.submit
            + p.wait;
        assert_eq!(sum, u.prepare + u.build, "{u:?}");
    }
    for u in built {
        let p = u.phases;
        assert!(
            p.plan > std::time::Duration::ZERO
                && p.builds > std::time::Duration::ZERO
                && p.submit > std::time::Duration::ZERO,
            "{u:?}"
        );
    }
}

/// The diagnosis of the GPU's time (#236): with the update waiting, the box pass is waited for
/// apart from the builds; and the updates count the BLASes they made (not only built).
#[test]
fn waiting_for_the_gpu_tells_the_box_pass_from_the_builds_and_the_updates_count_what_they_made() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.take_hardware_updates();
    let mut walk = |hw: &mut Offscreen, wait: bool, from: i32| {
        hw.set_hardware_wait(wait);
        for step in from..from + 6 {
            let camera = [
                -60.0 + f64::from(step) * 14.0,
                1565.0,
                -60.0 + f64::from(step) * 9.0,
            ];
            if let Some(update) = scene.recentre(&store, camera).unwrap() {
                hw.update_scene(&scene, &update).unwrap();
            }
        }
        hw.take_hardware_updates()
    };
    let free = walk(&mut hw, false, 1);
    let built: Vec<_> = free.iter().filter(|u| u.blas_rebuilt > 0).collect();
    assert!(!built.is_empty(), "{free:?}");
    assert!(
        built.iter().any(|u| u.blas_created > 0),
        "a walk makes BLASes for the groups that are new: {free:?}"
    );
    for u in &free {
        assert!(u.blas_created <= u.blas_rebuilt, "{u:?}");
        assert_eq!(u.phases.wait_boxes, std::time::Duration::ZERO, "{u:?}");
    }
    let waited = walk(&mut hw, true, 7);
    let built: Vec<_> = waited.iter().filter(|u| u.blas_rebuilt > 0).collect();
    assert!(!built.is_empty(), "{waited:?}");
    for u in built {
        assert!(u.phases.wait_boxes > std::time::Duration::ZERO, "{u:?}");
        assert!(u.phases.wait > std::time::Duration::ZERO, "{u:?}");
    }
}

/// A BLAS of several cells that hold boxes (the walking scene's regions are too few for two cells
/// of one BLAS): its boxes are its cells' in order, from its own corner; the hits are compute's;
/// and an Edit makes the whole BLAS again, from all its cells.
#[test]
fn a_blas_of_several_cells_has_all_their_boxes_in_order_and_follows_an_edit() {
    let mut store = generate_region(7, BrickCoord([-44, 178, -44]), BrickCoord([44, 193, 44]));
    store.build_lod(5);
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    let mut scene =
        Scene::rings(&store, [256.0, 1565.0, 256.0], Bands::with_d0(64.0), bound).unwrap();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    for grouping in [
        Grouping::uniform(8).unwrap(),
        Grouping::uniform(16).unwrap(),
        Grouping::uniform(32).unwrap(),
    ] {
        hw.set_hardware_grouping(&scene, grouping);
        hw.take_hardware_updates();
        let blases = blas_counts(&scene, &grouping);
        // Some BLAS has more than one cell with boxes in it.
        let mut cells: std::collections::BTreeMap<_, usize> = Default::default();
        for &(level, cell) in group_counts(&scene).keys() {
            *cells.entry(grouping.blas_of(level, cell)).or_default() += 1;
        }
        let many = cells.values().filter(|&&n| n > 1).count();
        assert!(
            many >= 1,
            "groups of {}: {many} BLASes of many cells",
            grouping.describe()
        );
        assert_not_stale(&format!("groups of {}", grouping.describe()), &hw, &scene);
        assert_eq!(hw.hardware_groups(), blases);
        let (hits, differing) =
            disagreement(&mut hw, &scene, &rays_down_from([256.0, 256.0], 65.0, 3));
        assert!(
            hits > 300 && differing <= 1024 / 100,
            "{hits} hits, {differing} differ"
        );
    }
    // An Edit in one cell of a BLAS of several makes the BLAS again, from all its cells.
    let grouping = Grouping::uniform(8).unwrap();
    hw.set_hardware_grouping(&scene, grouping);
    hw.take_hardware_updates();
    let top = (1400..1650)
        .rev()
        .find(|&y| store.get(VoxelCoord([250, y, 250])).is_some())
        .expect("ground");
    let mut changed = std::collections::BTreeSet::new();
    for x in 250..258 {
        for y in top + 1..top + 9 {
            for z in 250..258 {
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
    hw.settle_hardware();
    assert_not_stale("after the Edit", &hw, &scene);
    let mut rng = Lcg(9);
    let rays: Vec<_> = (0..512)
        .map(|_| {
            let x = 250.0 + f64::from(rng.next() * 8.0);
            let z = 250.0 + f64::from(rng.next() * 8.0);
            ray([x, top as f64 + 60.0, z], [x, top as f64 + 5.0, z])
        })
        .collect();
    let (hits, differing) = disagreement(&mut hw, &scene, &rays);
    assert!(
        hits > 450 && differing <= 512 / 100,
        "{hits} hits, {differing} differ"
    );
}

/// The frame's budget counts BLASes as well as boxes: with room for any number of boxes and one
/// BLAS, a frame builds one of the groups that may wait and no more; with two, two. They are not
/// counted as urgent, and what waits is built when the budget has room.
#[test]
fn the_blas_budget_bounds_the_groups_that_may_wait_to_so_many_a_frame() {
    let (store, mut scene) = walking_scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if hw.rt().active != RtBackend::Hardware {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    hw.set_hardware_grouping(&scene, Grouping::default());
    hw.set_hardware_budget(1 << 40);
    hw.set_hardware_blas_budget(1);
    hw.set_hardware_max_wait(100);
    let mut camera = [-60.0, 1565.0, -60.0];
    for step in 1..=10 {
        camera = [
            -60.0 + f64::from(step) * 14.0,
            1565.0,
            -60.0 + f64::from(step) * 9.0,
        ];
        if let Some(update) = scene.recentre(&store, camera).unwrap() {
            hw.update_scene(&scene, &update).unwrap();
        }
    }
    // The moves' updates built the groups that gained boxes whatever the budget of one.
    let moves = hw.take_hardware_updates();
    let (rebuilt, urgent): (u32, u32) = moves
        .iter()
        .fold((0, 0), |(r, u), l| (r + l.blas_rebuilt, u + l.blas_urgent));
    assert!(urgent > 1 && rebuilt >= urgent, "{moves:?}");
    // The frame they were in is closed by the first draw, which has the budget they left: none.
    hw.follow_hardware(Some(0), camera);
    hw.take_hardware_updates();
    let waiting = hw.hardware_pending();
    assert!(waiting >= 4, "{waiting} groups wait");
    for frame in 1..=2 {
        hw.follow_hardware(Some(frame), camera);
        let log = hw.take_hardware_updates();
        let rebuilt: u32 = log.iter().map(|u| u.blas_rebuilt).sum();
        let urgent: u32 = log.iter().map(|u| u.blas_urgent).sum();
        assert_eq!((rebuilt, urgent), (1, 0), "frame {frame}: {log:?}");
        assert_eq!(hw.hardware_pending(), waiting - frame as usize);
    }
    // With room for two, a frame builds two.
    hw.set_hardware_blas_budget(2);
    hw.follow_hardware(Some(3), camera);
    let rebuilt: u32 = hw
        .take_hardware_updates()
        .iter()
        .map(|u| u.blas_rebuilt)
        .sum();
    assert_eq!(rebuilt, 2);
    assert_eq!(hw.hardware_pending(), waiting - 4);
    // On a budget of one, what is left is built a frame at a time and nothing is stale after.
    hw.set_hardware_blas_budget(1);
    for frame in 4..12 {
        hw.follow_hardware(Some(frame), camera);
    }
    assert_eq!(hw.hardware_pending(), 0);
    assert_not_stale("after the budget's frames", &hw, &scene);
}

/// What the groups that must be built at once cost counts against the frame's box budget: an
/// update whose urgent builds took more than the budget leaves the groups that may wait waiting.
#[test]
fn the_groups_built_at_once_use_up_the_frames_box_budget() {
    // One move (the groups that gain boxes are built at once, those that only lose may wait) with
    // a box budget: the boxes it built and the groups left waiting.
    let moved = |budget: u64| -> Option<(u64, usize)> {
        let (store, mut scene) = walking_scene();
        let mut hw =
            Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
        if hw.rt().active != RtBackend::Hardware {
            eprintln!("skipped: {:?}", hw.rt().fallback);
            return None;
        }
        hw.set_hardware_grouping(&scene, Grouping::default());
        hw.set_hardware_budget(budget);
        hw.set_hardware_max_wait(100);
        // The first build of the scene is not the frame's: the move is in the first frame.
        hw.follow_hardware(Some(0), [-60.0, 1565.0, -60.0]);
        hw.take_hardware_updates();
        let update = scene
            .recentre(&store, [-60.0 + 14.0, 1565.0, -60.0 + 9.0])
            .unwrap()
            .expect("the move changes the rings");
        hw.update_scene(&scene, &update).unwrap();
        let boxes: u64 = hw
            .take_hardware_updates()
            .iter()
            .map(|u| u.prims_rebuilt)
            .sum();
        Some((boxes, hw.hardware_pending()))
    };
    let Some((all, none_wait)) = moved(1 << 40) else {
        return;
    };
    assert_eq!(none_wait, 0, "room for everything left groups waiting");
    // A budget of one box: the urgent groups are built, whatever it is, and that is all.
    let (urgent, waiting) = moved(1).unwrap();
    assert!(
        waiting >= 1 && urgent < all,
        "{urgent} of {all} boxes, {waiting} wait"
    );
    // Half of what the urgent builds took: it is spent, and the groups that may wait do not get
    // what the urgent ones were not charged.
    assert_eq!(moved(urgent / 2).unwrap(), (urgent, waiting));
}

/// An Edit of the ground by the camera, dug and placed in turn as the bench's Edit stress does,
/// fits in the scene's buffers: none is made again bigger and copied (a growth is a new buffer, a
/// copy and a new bind group, which the first Edit of a context that had not been where the others
/// were would pay for, #236), on either backend, and make no device allocation of a MiB or more (a
/// staging belt with no free chunk makes one of 4 MiB: 2 to 3 ms in the first Edit).
#[test]
fn edits_by_the_ground_fit_in_the_scenes_buffers_on_either_backend() {
    for backend in [RtBackend::Compute, RtBackend::Hardware] {
        let (mut store, mut scene) = walking_scene();
        let mut gpu = Offscreen::with_backend(Resolution::native(8, 8), &scene, backend).unwrap();
        if gpu.rt().active != backend {
            eprintln!("skipped: {:?}", gpu.rt().fallback);
            continue;
        }
        for edit in 0..6 {
            let mut changed = std::collections::BTreeSet::new();
            for x in 0..40 {
                for z in 0..40 {
                    for y in 1500..1540 {
                        let at = VoxelCoord([x, y, z]);
                        store.set(at, (edit % 2 == 0).then_some(STONE));
                        changed.insert(at.brick());
                    }
                }
            }
            let changed: Vec<BrickCoord> = changed.into_iter().collect();
            store.refresh_lod(changed.iter().copied());
            let update = scene.update(&store, &changed).unwrap();
            let before = gpu.device_allocation_sizes();
            gpu.update_scene(&scene, &update).unwrap();
            // The staging belt has its chunks before the first Edit: a chunk made for it is a
            // device allocation of 4 MiB, and 2 to 3 ms of the Edit's frame (a cold belt's first
            // Edit, #236). What the Edit allocates and is small is wgpu's own (a staging buffer
            // for a small write, the scratch of a build: some hundred bytes to some tens of KB,
            // sub-allocated, and which of them are live depends on when the GPU gave the last
            // ones back), so it is allocations of a MiB or more that are looked for. Where the
            // allocator does not count, there is nothing to compare.
            if let (Some(mut before), Some(after)) = (before, gpu.device_allocation_sizes()) {
                let mut made = Vec::new();
                for size in after {
                    match before.iter().position(|&b| b == size) {
                        Some(at) => {
                            before.remove(at);
                        }
                        None => made.push(size),
                    }
                }
                assert!(
                    made.iter().all(|&size| size < 1 << 20),
                    "{backend:?}: Edit {edit} made a device allocation of a MiB or more: {made:?}"
                );
            }
            assert_eq!(
                gpu.last_grown_bytes(),
                0,
                "{backend:?}: Edit {edit} made a scene buffer bigger"
            );
        }
    }
}
