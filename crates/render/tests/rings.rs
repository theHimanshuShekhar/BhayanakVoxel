//! The test Planet in rings (`Scene::rings`), at every distance from the surface to orbit: that
//! the GPU and the CPU walk them alike, that they show no cracks and match the approved golden
//! images, that moving the camera moves them without writing what stays, and that the GPU memory
//! they take does not grow. Needs a Vulkan device (lavapipe in CI).
//!
//! The whole Planet takes about half a minute to generate, so the tests share one.

mod common;

use std::sync::OnceLock;

use brick_store::BrickStore;
use common::{Lcg, PRIMARY, assert_agree, check_golden, ray};
use render::reference::{Hit, Ray, trace};
use render::scene::Core;
use render::{Camera, Image, Offscreen, Scene, SceneUpdate};

const SEED: u64 = 0xB4A7_A9AC;
/// Rings of the Planet: as many as the client draws.
const LEVELS: usize = 7;
const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;

/// The Planet with the levels of its pyramid that the rings are made of.
fn planet() -> &'static BrickStore {
    static PLANET: OnceLock<BrickStore> = OnceLock::new();
    PLANET.get_or_init(|| {
        let mut store = planet_gen::generate_planet(SEED);
        store.build_lod(LEVELS - 1);
        store
    })
}

fn rings_at(camera: [f64; 3]) -> Scene {
    Scene::rings(planet(), camera, LEVELS).unwrap()
}

/// Where the camera is and what it looks at, for the views that are at the surface, at the
/// horizon of a hill (where the ground crosses from one ring to the next), and in orbit.
const SURFACE: ([f64; 3], [f64; 3]) = ([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]);
const HORIZON: ([f64; 3], [f64; 3]) = ([0.0, 1700.0, 600.0], [0.0, 1450.0, -400.0]);
const ORBIT: ([f64; 3], [f64; 3]) = ([0.0, 400.0, 3400.0], [0.0; 3]);

fn render_view((from, to): ([f64; 3], [f64; 3])) -> Image {
    Offscreen::new(WIDTH, HEIGHT, &rings_at(from))
        .unwrap()
        .render(&Camera::looking_at(from, to))
}

#[test]
fn the_test_planet_from_the_surface_matches_the_approved_golden_image() {
    check_golden(
        "planet_rings_surface_256x144.png",
        &render_view(SURFACE),
        &PRIMARY,
    );
}

/// The ground runs on from the near ring to the next one at the horizon: the place where a
/// crack between rings would show as sky.
#[test]
fn the_test_planet_across_two_rings_matches_the_approved_golden_image() {
    check_golden(
        "planet_rings_horizon_256x144.png",
        &render_view(HORIZON),
        &PRIMARY,
    );
}

#[test]
fn the_test_planet_from_orbit_matches_the_approved_golden_image() {
    check_golden(
        "planet_rings_orbit_256x144.png",
        &render_view(ORBIT),
        &PRIMARY,
    );
}

/// The view from the surface is all in the first ring, which holds the same Voxels as the
/// single-level scene of a patch of the Planet that the older golden image is of: the two must
/// agree.
#[test]
fn the_rings_draw_the_surface_as_the_patch_of_the_planet_does() {
    check_golden("planet_256x144.png", &render_view(SURFACE), &PRIMARY);
}

/// Rays from `camera` at random points of the Planet, a little above or below its surface.
fn rays_at_the_planet(camera: [f64; 3], count: usize, rng: &mut Lcg) -> Vec<Ray> {
    (0..count)
        .map(|_| {
            let on = [rng.next() - 0.5, rng.next() - 0.5, rng.next() - 0.5];
            let len = on.iter().map(|c| c * c).sum::<f32>().sqrt();
            let radius = f64::from(1450.0 + 150.0 * rng.next());
            ray(camera, on.map(|c| f64::from(c / len) * radius))
        })
        .collect()
}

/// Cameras at a hand's breadth from the ground, over a hill, in the sky and in orbit.
const CAMERAS: [[f64; 3]; 5] = [
    [0.0, 1548.0, 250.0],
    [0.0, 1700.0, 600.0],
    [800.0, 2600.0, 1500.0],
    [0.0, 400.0, 3400.0],
    [-9000.0, 700.0, 6000.0],
];

#[test]
fn the_gpu_and_the_cpu_agree_on_rings_of_the_test_planet_from_the_surface_to_orbit() {
    let mut rng = Lcg(2025);
    for camera in CAMERAS {
        let scene = rings_at(camera);
        let rays = rays_at_the_planet(camera, 300, &mut rng);
        let hits = assert_agree(&format!("camera {camera:?}"), &rays, &scene);
        assert!(hits > 100, "camera {camera:?}: {hits} of 300 rays hit");
    }
}

/// The Planet at every Voxel of level 0, which is what the rings are of: the fine truth that no
/// ray may get through to the sky past.
fn whole_planet() -> Scene {
    let core = Core {
        radius: planet_gen::CORE_RADIUS,
        material: planet_gen::ROCK,
    };
    Scene::new(planet(), Some(core))
}

#[test]
fn no_ray_gets_through_to_the_sky_where_the_whole_planet_has_ground() {
    let truth = whole_planet();
    let mut rng = Lcg(77);
    for camera in CAMERAS {
        let scene = rings_at(camera);
        let (mut through, mut hits) = (0, 0);
        for ray in rays_at_the_planet(camera, 1500, &mut rng) {
            let (want, got): (Option<Hit>, Option<Hit>) =
                (trace(&truth, &ray), trace(&scene, &ray));
            hits += usize::from(want.is_some());
            through += usize::from(want.is_some() && got.is_none());
        }
        assert!(hits > 300, "camera {camera:?}: {hits} rays hit the Planet");
        assert_eq!(
            through, 0,
            "camera {camera:?}: {through} of {hits} rays get through the rings to the sky"
        );
    }
}

#[test]
fn every_ring_holds_about_as_many_bricks() {
    // From the surface, where the Planet's surface crosses every ring that is smaller than it.
    let scene = rings_at(SURFACE.0);
    let bricks: Vec<usize> = (0..3).map(|level| scene.level_bricks(level)).collect();
    let most = *bricks.iter().max().unwrap();
    assert!(
        bricks.iter().all(|&b| 2 * b > most),
        "bricks in the first three rings: {bricks:?}"
    );
    // The buffers are the same size wherever the camera is: what bounds the GPU memory.
    let size = |s: &Scene| s.table.len() + s.regions.len() + s.brick_refs.len() + s.bricks.len();
    assert_eq!(size(&scene), size(&rings_at(ORBIT.0)));
}

#[test]
fn moving_the_camera_moves_the_rings_without_writing_the_bricks_that_stay() {
    let (start, end) = ([0.0, 1548.0, 200.0], [0.0, 1548.0, 450.0]);
    let mut scene = rings_at(start);
    let mut gpu = Offscreen::new(WIDTH, HEIGHT, &scene).unwrap();
    let whole = gpu.uploaded_bytes();
    assert_eq!(whole, gpu.scene_bytes());

    // Still within the middle of the boxes: nothing is written.
    assert!(
        scene
            .recentre(planet(), [0.0, 1548.0, 210.0])
            .unwrap()
            .is_none()
    );
    assert_eq!(gpu.uploaded_bytes(), whole);

    // A few regions on: the regions that come into the boxes are packed and written, and the
    // bricks of those that stay are not.
    let Some(update) = scene.recentre(planet(), end).unwrap() else {
        panic!("the rings move");
    };
    let SceneUpdate::Rings(patch) = &update else {
        panic!("the rings are patched: {update:?}");
    };
    gpu.update_scene(&scene, &update).unwrap();
    let written = gpu.uploaded_bytes() - whole;
    let words = patch.words() as u64 * 4;
    assert!(
        (words..words + 2048).contains(&written),
        "{written} bytes written for a patch of {words}"
    );
    assert!(
        written * 20 < gpu.scene_bytes(),
        "{written} bytes written of {}",
        gpu.scene_bytes()
    );

    // And what is drawn is what the same rings built from scratch draw.
    let camera = Camera::looking_at(end, [0.0, 1535.0, -300.0]);
    let fresh = Offscreen::new(WIDTH, HEIGHT, &scene.rebuilt_rings(planet()).unwrap())
        .unwrap()
        .render(&camera);
    assert_eq!(gpu.render(&camera), fresh);
}

#[test]
fn a_flight_from_the_surface_to_orbit_and_back_keeps_the_gpu_memory_bounded() {
    let (low, high) = ([0.0, 1548.0, 250.0], [0.0, 3600.0, 3000.0]);
    let mut scene = rings_at(low);
    let mut gpu = Offscreen::new(64, 36, &scene).unwrap();
    let bytes = gpu.scene_bytes();
    let (mut written, mut moves, mut worst) = (0, 0, 0);
    let steps = 60;
    for step in 0..=2 * steps {
        // Up in a straight line, and down again.
        let t = f64::from(step.min(2 * steps - step)) / f64::from(steps);
        let at: [f64; 3] = std::array::from_fn(|a| low[a] + (high[a] - low[a]) * t);
        let before = gpu.uploaded_bytes();
        if let Some(update) = scene.recentre(planet(), at).unwrap() {
            gpu.update_scene(&scene, &update).unwrap();
            moves += 1;
        }
        let step_bytes = gpu.uploaded_bytes() - before;
        written += step_bytes;
        worst = worst.max(step_bytes);
        assert_eq!(gpu.scene_bytes(), bytes, "step {step}");
        // What the camera sees from here is what rings built here show, now and then.
        if step % 30 == 0 {
            let camera = Camera::looking_at(at, [0.0; 3]);
            let fresh = Offscreen::new(64, 36, &scene.rebuilt_rings(planet()).unwrap())
                .unwrap()
                .render(&camera);
            assert_eq!(gpu.render(&camera), fresh, "step {step} at {at:?}");
        }
    }
    assert!(moves > 40, "the rings moved {moves} times");
    // The GPU memory is the same all the way, and is a fraction of the budget. No step writes
    // more than a fraction of it, so nothing is packed again from scratch; the whole flight
    // writes it a few times over.
    assert!(bytes < 128 << 20, "{bytes} bytes of GPU memory");
    assert!(worst * 4 < bytes, "a step wrote {worst} bytes of {bytes}");
    assert!(written < 8 * bytes, "{written} bytes written, of {bytes}");
}

/// What the walk costs, in steps (see `reference::Steps`), averaged over a fan of primary rays
/// from cameras along the bench's flight over the Planet (`planet-flyover-v1`), by what the rays
/// do: miss, hit the ground face on, or graze it.
#[derive(Clone, Copy, Default)]
struct Cost {
    rays: u32,
    steps: render::reference::Steps,
}

impl Cost {
    fn add(&mut self, steps: render::reference::Steps) {
        let s = &mut self.steps;
        self.rays += 1;
        s.examined += steps.examined;
        s.levels += steps.levels;
        s.regions += steps.regions;
        s.probes += steps.probes;
        s.bricks += steps.bricks;
        s.voxels += steps.voxels;
    }

    /// Mean steps of each kind per ray: levels looked at, levels walked, regions, probes,
    /// bricks, Voxels.
    fn mean(&self) -> [f64; 6] {
        let s = &self.steps;
        [
            s.examined, s.levels, s.regions, s.probes, s.bricks, s.voxels,
        ]
        .map(|n| f64::from(n) / f64::from(self.rays.max(1)))
    }

    /// Region, brick and Voxel steps per ray.
    fn total(&self) -> f64 {
        let m = self.mean();
        m[2] + m[4] + m[5]
    }
}

/// The keyframes of the bench's `planet-flyover-v1`: where the camera is, and what it looks at.
const FLYOVER: [([f64; 3], [f64; 3]); 5] = [
    ([0.0, 400.0, 3400.0], [0.0; 3]),
    ([0.0, 1900.0, 1500.0], [0.0, 1500.0, 0.0]),
    ([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]),
    ([0.0, 1552.0, 0.0], [200.0, 1530.0, -300.0]),
    ([-150.0, 1556.0, -200.0], [-300.0, 1530.0, -360.0]),
];

/// The camera `frame` of `frames` along the flyover, as the bench's `CameraPath` has it.
fn flyover_camera(frame: u32, frames: u32) -> Camera {
    let segments = FLYOVER.len() - 1;
    let along = f64::from(frame) * segments as f64 / f64::from(frames - 1);
    let segment = (along.floor() as usize).min(segments - 1);
    let t = along - segment as f64;
    let lerp = |a: [f64; 3], b: [f64; 3]| [0, 1, 2].map(|i| a[i] * (1.0 - t) + b[i] * t);
    let (from, to) = (FLYOVER[segment], FLYOVER[segment + 1]);
    Camera::looking_at(lerp(from.0, to.0), lerp(from.1, to.1))
}

/// A fan of `width` by `width * 9 / 16` primary rays from `camera`.
fn primary_rays(camera: &Camera, width: u32) -> Vec<Ray> {
    let height = width * 9 / 16;
    let tan_half = (camera.fov_y / 2.0).tan();
    let (right, up, forward) = (camera.right(), camera.up(), camera.forward());
    let mut rays = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let ndc = [
                (f64::from(x) + 0.5) / f64::from(width) * 2.0 - 1.0,
                (f64::from(y) + 0.5) / f64::from(height) * 2.0 - 1.0,
            ];
            let aspect = f64::from(width) / f64::from(height);
            let d: [f64; 3] = std::array::from_fn(|a| {
                forward[a] + right[a] * ndc[0] * aspect * tan_half - up[a] * ndc[1] * tan_half
            });
            let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
            let (cell, origin) = render::camera::split(camera.position);
            rays.push(Ray {
                cell,
                origin,
                dir: d.map(|c| (c / len) as f32),
            });
        }
    }
    rays
}

/// Costs of `scene` along the flyover, by sky, ground and grazing rays, and in all.
fn flyover_cost(mut scene: Scene, rings: bool) -> [Cost; 4] {
    let mut costs = [Cost::default(); 4];
    for frame in 0..24 {
        let camera = flyover_camera(frame * 10, 240);
        if rings {
            scene.recentre(planet(), camera.position).unwrap();
        }
        for ray in primary_rays(&camera, 64) {
            let (hit, steps) = render::reference::trace_counting(&scene, &ray);
            let kind = match hit {
                None => 0,
                Some(hit) => {
                    let facing: f32 = (0..3).map(|a| ray.dir[a] * hit.normal[a] as f32).sum();
                    if facing.abs() > 0.3 { 1 } else { 2 }
                }
            };
            costs[kind].add(steps);
            costs[3].add(steps);
        }
    }
    costs
}

#[test]
fn the_rings_walk_about_as_many_steps_as_a_single_level_scene_along_the_bench_flight() {
    let single = flyover_cost(whole_planet(), false);
    let rings = flyover_cost(rings_at(flyover_camera(0, 240).position), true);
    for (name, (a, b)) in ["sky", "ground", "grazing", "all"]
        .iter()
        .zip(single.iter().zip(&rings))
    {
        eprintln!(
            "{name:8} rays {:5}  single: looked-at/walked levels, regions, probes, bricks, voxels {:5.1?} total {:6.1}   rings: {:5.1?} total {:6.1}   x{:.2}",
            a.rays,
            a.mean().map(|v| (v * 10.0).round() / 10.0),
            a.total(),
            b.mean().map(|v| (v * 10.0).round() / 10.0),
            b.total(),
            b.total() / a.total().max(1e-9)
        );
        // The rings cost what the one level does, rays that miss included: those cross every
        // ring, so without the walk's clipping to what each ring lists they cost about three
        // times as much.
        assert!(
            b.total() <= 1.1 * a.total(),
            "{name}: {} steps a ray, against {}",
            b.total(),
            a.total()
        );
    }
    // A ray that hits does not look at the rings beyond the one it hits in.
    assert!(rings[1].mean()[0] < 4.0, "{:?}", rings[1].mean());
}
