//! Sun shadows in Primary (#52, ADR 0005): two shadow rays a pixel (`DEFAULT_SHADOW_RAYS`), each
//! towards its own sample of the sun's disc, from the geometric face, with no history. A pillar on a floor casts a shadow along -x;
//! the CPU twin (`render::reference::trace`) says which top faces see the sun, and the GPU is held
//! to it, a golden image holds the whole picture, and a sun that changes is right on the very next
//! frame. Needs a Vulkan device (lavapipe in CI).

mod common;

use std::time::Duration;

use brick_store::{BrickStore, Material, VoxelCoord};
use common::{PRIMARY, check_golden};
use materials::MaterialTable;
use render::reference::{Ray, trace};
use render::still::Plan;
use render::{Camera, Image, Offscreen, Scene, Sun, camera};

const SIZE: u32 = 64;
const FRAME: Duration = Duration::from_micros(16_667);

/// A floor of one material with no colour variation (its top face is at y = 0, x and z -64..=63)
/// and a pillar on it, Voxels x 0..=5, z 0..=5, y 0..=9.
fn scene() -> Scene {
    scene_of(Material(
        MaterialTable::built_in().by_name("clear glass").unwrap().id,
    ))
}

fn scene_of(glass: Material) -> Scene {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(glass));
            }
        }
    }
    for x in 0..6 {
        for z in 0..6 {
            for y in 0..10 {
                store.set(VoxelCoord([x, y, z]), Some(glass));
            }
        }
    }
    Scene::new(&store, None)
}

/// Looking straight down at the pillar and the ground west of it, where its shadow falls (a pixel
/// is about half a Voxel).
fn overhead() -> Camera {
    Camera::looking_at([-6.0, 25.0, 3.0], [-6.0, 0.0, 3.0])
}

/// The sun, towards +x and up: the pillar's shadow lies along -x, 12 Voxels long.
const TO_SUN: [f32; 3] = [0.6, 0.5, 0.1];

fn gpu(width: u32, height: u32, sun: Sun) -> Offscreen {
    let mut gpu = Offscreen::new(width, height, &scene()).unwrap();
    // The light of the old constants: no exposure to adapt, so a frame is a function of the sun.
    gpu.use_legacy_light();
    gpu.set_sun(sun);
    gpu
}

fn point_sun(to_sun: [f32; 3]) -> Sun {
    Sun::new(to_sun, 0.0).unwrap()
}

/// The ray that the shaders cast through the middle of `pixel`.
fn pixel_ray(camera: &Camera, pixel: (u32, u32), size: u32) -> Ray {
    let ndc = |p: u32| (f64::from(p) + 0.5) / f64::from(size) * 2.0 - 1.0;
    let tan_half = (camera.fov_y / 2.0).tan();
    let (forward, right, up) = (camera.forward(), camera.right(), camera.up());
    let d = [0, 1, 2]
        .map(|a| forward[a] + right[a] * ndc(pixel.0) * tan_half - up[a] * ndc(pixel.1) * tan_half);
    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let (cell, origin) = camera::split(camera.position);
    Ray {
        cell,
        origin,
        dir: d.map(|c| (c / l) as f32),
    }
}

/// Whether the sun reaches the top face (normal +y) that `pixel` sees, by the CPU twin of the
/// shadow ray: leave the face by a hair along its normal, and look for anything along the way to
/// the sun. `None` for a pixel that sees the sky or another face.
fn twin(scene: &Scene, camera: &Camera, pixel: (u32, u32), to_sun: [f32; 3]) -> Option<bool> {
    let ray = pixel_ray(camera, pixel, SIZE);
    let hit = trace(scene, &ray)?;
    if hit.normal != [0, 1, 0] {
        return None;
    }
    let length = to_sun.iter().map(|c| c * c).sum::<f32>().sqrt();
    let shadow = Ray {
        cell: ray.cell,
        origin: [hit.position[0], hit.position[1] + 1e-3, hit.position[2]],
        dir: to_sun.map(|c| c / length),
    };
    Some(trace(scene, &shadow).is_none())
}

/// The red of a pixel, as it was shaded: sRGB, 0..=255.
fn red(image: &Image, (x, y): (u32, u32)) -> u8 {
    image.rgba[((y * image.width + x) * 4) as usize]
}

type Pixels = Vec<(u32, u32)>;

/// The top faces the twin has lit, and in shadow, in a SIZE x SIZE view.
fn lit_and_shadowed(scene: &Scene, to_sun: [f32; 3]) -> (Pixels, Pixels) {
    let camera = overhead();
    let (mut lit, mut shadowed) = (vec![], vec![]);
    for y in 0..SIZE {
        for x in 0..SIZE {
            match twin(scene, &camera, (x, y), to_sun) {
                Some(true) => lit.push((x, y)),
                Some(false) => shadowed.push((x, y)),
                None => {}
            }
        }
    }
    (lit, shadowed)
}

/// The pixels of `set` all 8 of whose neighbours are in it too: away from a shadow's edge, where
/// the GPU and the twin cannot see different Voxels.
fn interior(set: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let has: std::collections::HashSet<_> = set.iter().copied().collect();
    set.iter()
        .copied()
        .filter(|&(x, y)| {
            (0..9).all(|n| {
                let (dx, dy) = (n % 3, n / 3);
                x + dx >= 1 && y + dy >= 1 && has.contains(&(x + dx - 1, y + dy - 1))
            })
        })
        .collect()
}

/// Every pixel of `image` whose top face the twin lit or shadowed, against the twin: a pixel is
/// lit when it is brighter than the middle of the brightest lit and darkest shadowed. A pixel
/// within a Voxel of a shadow's edge may see a neighbouring Voxel on the GPU: nearly all agree.
fn agreement(image: &Image, scene: &Scene, to_sun: [f32; 3]) -> (usize, usize) {
    let (lit, shadowed) = lit_and_shadowed(scene, to_sun);
    assert!(lit.len() > 1000, "{} lit pixels", lit.len());
    assert!(shadowed.len() > 60, "{} shadowed pixels", shadowed.len());
    let bright = lit.iter().map(|&p| red(image, p)).max().unwrap();
    let dark = shadowed.iter().map(|&p| red(image, p)).min().unwrap();
    assert!(
        bright > dark + 40,
        "the sun's light is {bright} and its shadow {dark}"
    );
    let middle = ((u16::from(bright) + u16::from(dark)) / 2) as u8;
    let right = lit.iter().filter(|&&p| red(image, p) > middle).count()
        + shadowed
            .iter()
            .filter(|&&p| red(image, p) <= middle)
            .count();
    (right, lit.len() + shadowed.len())
}

#[test]
fn a_lit_point_is_lit_and_a_shadowed_point_is_dark_as_the_cpu_twin_says() {
    let scene = scene();
    let camera = overhead();
    let mut gpu = gpu(SIZE, SIZE, point_sun(TO_SUN));
    let image = gpu.render(&camera);

    // The two points the twin is asked about: the ground in the pillar's shadow, 4 Voxels west of
    // it, and in the sun, 14 Voxels to its side. (The pillar is 10 high: the shadow of its top edge
    // is at x = -12, and the 4 Voxels are well inside it.)
    let pixel_of = |voxel: [i32; 3]| {
        (0..SIZE)
            .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
            .find(|&p| {
                trace(&scene, &pixel_ray(&camera, p, SIZE)).is_some_and(|h| h.voxel == voxel)
            })
            .expect("a pixel that sees the Voxel")
    };
    let in_shadow = pixel_of([-4, -1, 2]);
    let in_sun = pixel_of([-4, -1, 16]);
    assert_eq!(twin(&scene, &camera, in_shadow, TO_SUN), Some(false));
    assert_eq!(twin(&scene, &camera, in_sun, TO_SUN), Some(true));
    // Lit: the sun and the ambient (the floor of clear glass is the same everywhere in the sun).
    // Shadowed: the ambient alone.
    assert!(
        red(&image, in_sun) > red(&image, in_shadow) + 40,
        "lit {} and shadowed {}",
        red(&image, in_sun),
        red(&image, in_shadow)
    );

    // And every top face the twin looked at.
    let (right, total) = agreement(&image, &scene, TO_SUN);
    assert!(
        right * 100 >= total * 97,
        "{right} of {total} pixels agree with the CPU twin"
    );
}

/// The summed difference of the red of two images, over the whole picture.
fn distance(a: &Image, b: &Image) -> u64 {
    let (a, _) = a.rgba.as_chunks::<4>();
    let (b, _) = b.rgba.as_chunks::<4>();
    a.iter()
        .zip(b)
        .map(|(a, b)| u64::from(a[0].abs_diff(b[0])))
        .sum()
}

#[test]
fn more_shadow_rays_are_a_finer_penumbra_and_the_default_is_two() {
    assert_eq!(render::DEFAULT_SHADOW_RAYS, 2);
    // A wide sun, so that the pillar's shadow has a penumbra several pixels across.
    let wide = Sun::new(TO_SUN, 0.12).unwrap();
    let camera = overhead();
    let image = |rays: Option<u32>| {
        let mut gpu = gpu(SIZE, SIZE, wide);
        if let Some(rays) = rays {
            gpu.set_shadow_rays(rays);
        }
        gpu.render(&camera)
    };
    // Many rays are the penumbra as the disc has it, to within a few levels; one and two rays are
    // that with noise, and two have less of it than one (the same first frame, so the rays of a
    // pixel are samples 0, and 0 and 1, of its sequence).
    let nearly = image(Some(render::MAX_SHADOW_RAYS));
    let one = image(Some(1));
    let two = image(None);
    let (d1, d2) = (distance(&one, &nearly), distance(&two, &nearly));
    assert!(
        d1 > 0,
        "a wide sun has a penumbra, and one ray is noisy in it"
    );
    assert!(
        d2 * 10 < d1 * 9,
        "two rays are {d2} levels from the many-ray penumbra and one ray {d1}"
    );
    // The same count and frame is the same image: nothing but the frame decides the samples.
    assert_eq!(image(None).rgba, two.rgba);
    // A count of 0 or past the most is cut to the range, not an error.
    let mut gpu = gpu(SIZE, SIZE, wide);
    gpu.set_shadow_rays(0);
    assert_eq!(gpu.render(&camera).rgba, one.rgba);
}

#[test]
fn the_shadow_moves_with_the_sun_and_the_twin_follows_it() {
    let scene = scene();
    // The sun from the other side, and higher: the shadow lies along +z and is shorter.
    let other = [0.1, 0.8, -0.6];
    let image = gpu(SIZE, SIZE, point_sun(other)).render(&overhead());
    let (right, total) = agreement(&image, &scene, other);
    assert!(
        right * 100 >= total * 97,
        "{right} of {total} pixels agree with the CPU twin"
    );
}

#[test]
fn smooth_normals_shade_the_hit_and_leave_the_shadow_where_the_faces_put_it() {
    // Stone is a natural Material: with the smooth normals on, its hits are shaded by a normal
    // that leans from the face's (at the pillar's edges, where Voxels of other heights are
    // neighbours). The shadow ray leaves the face, so the shadow, which is where a pixel has the
    // sun's light at all in the sun and sky alone, is the same with them on or off.
    let stone = Material(1);
    let scene = scene_of(stone);
    let draw = |smooth_normals| {
        let mut gpu = Offscreen::new(SIZE, SIZE, &scene).unwrap();
        gpu.use_legacy_light();
        gpu.set_stages(render::Stages {
            smooth_normals,
            ..render::Stages::NONE
        });
        gpu.set_sun(point_sun(TO_SUN));
        gpu.set_sun_and_sky_only(true);
        gpu.render(&overhead())
    };
    let (smooth, flat) = (draw(true), draw(false));
    let (lit, shadowed) = lit_and_shadowed(&scene, TO_SUN);
    assert!(lit.len() > 1000 && shadowed.len() > 60);
    for (name, image) in [("smooth", &smooth), ("flat", &flat)] {
        let wrong = lit.iter().filter(|&&p| red(image, p) == 0).count()
            + shadowed.iter().filter(|&&p| red(image, p) != 0).count();
        assert!(
            wrong * 100 < (lit.len() + shadowed.len()) * 3,
            "{name}: {wrong} of {} top faces are not as the CPU twin says",
            lit.len() + shadowed.len()
        );
    }
}

#[test]
fn the_pillars_shadow_is_in_the_approved_golden_image() {
    // Grassy soil (a natural Material, so with its variation and smooth normals as the game draws it),
    // the sun low enough to throw a long shadow, and its disc 0.04 across so that the edge has a
    // penumbra. One frame of Primary: one shadow sample a pixel, as the window draws the first
    // frame of a view.
    let grass = Material(MaterialTable::built_in().by_name("grassy soil").unwrap().id);
    let mut gpu = Offscreen::new(192, 108, &scene_of(grass)).unwrap();
    gpu.set_sun(Sun::from_angles(1.0, 0.45, 0.04));
    let image = gpu.render(&Camera::looking_at([-10.0, 12.0, 22.0], [-4.0, 2.0, 3.0]));
    check_golden("shadow_pillar_192x108.png", &image, &PRIMARY);
}

#[test]
fn a_sun_that_changes_is_right_on_the_next_frame_and_restarts_the_reference() {
    let scene = scene();
    let camera = overhead();
    let (a, b) = (point_sun(TO_SUN), point_sun([0.1, 0.8, -0.6]));

    // Frames of sun A the upscaler has history of, then B: the next frame has none of A's
    // shadow, and is what a first frame under B is.
    let mut changed = gpu(SIZE, SIZE, a);
    for _ in 0..8 {
        changed.render_upscaled(&camera);
    }
    changed.set_sun(b);
    let next = changed.render_upscaled(&camera);
    let first = gpu(SIZE, SIZE, b).render_upscaled(&camera);
    assert_eq!(next, first, "the frame after the change has no history");
    let (right, total) = agreement(&next, &scene, [0.1, 0.8, -0.6]);
    assert!(
        right * 100 >= total * 95,
        "{right} of {total} pixels agree with the CPU twin"
    );

    // The window's frames, Reference mode: a long still view of sun A takes samples; the frame
    // after B is Primary, entirely, from no samples, with B's shadow.
    let mut window = gpu(SIZE, SIZE, a);
    let mut now = 0;
    let mut frames = |window: &mut Offscreen, count: u32| {
        (0..count)
            .map(|_| {
                now += 1;
                window.draw_frame(&camera, true, FRAME * now)
            })
            .last()
            .unwrap()
    };
    assert!(frames(&mut window, 14).samples >= 12);
    window.set_sun(b);
    let cut = frames(&mut window, 1);
    assert_eq!(cut.plan, Plan::Primary);
    assert_eq!(cut.samples, 0);
    assert_eq!(window.reference_samples(), 0, "started over");
    assert_eq!(window.shown(), window.primary_image(), "no blend");
    let (right, total) = agreement(&window.shown(), &scene, [0.1, 0.8, -0.6]);
    assert!(
        right * 100 >= total * 95,
        "{right} of {total} pixels agree with the CPU twin"
    );
    assert_eq!(window.shown(), first, "a first frame under B");
}

#[test]
fn the_sun_and_sky_alone_have_no_ambient_in_either_view() {
    let scene = scene();
    let camera = overhead();
    let (lit, shadowed) = lit_and_shadowed(&scene, TO_SUN);
    let (lit, shadowed) = (interior(&lit), interior(&shadowed));
    assert!(lit.len() > 500 && shadowed.len() > 30);
    let mut gpu = gpu(SIZE, SIZE, point_sun(TO_SUN));
    let with_gi = gpu.render(&camera);
    gpu.set_sun_and_sky_only(true);
    let primary = gpu.render(&camera);
    // The shadow is black: nothing of the sky's ambient is in Primary, and nothing but the sun's
    // light is left where the sun is.
    for &p in shadowed.iter().step_by(7) {
        assert!(red(&with_gi, p) > 0, "{p:?}: the ambient is in the shadow");
        assert_eq!(red(&primary, p), 0, "{p:?}");
    }
    for &p in lit.iter().step_by(97) {
        assert!(
            red(&primary, p) > 0 && red(&primary, p) < red(&with_gi, p),
            "{p:?}"
        );
    }
    // The Reference follows no path past the first surface: the shadow is black there too, and a
    // lit point is what Primary has, at the sun's own disc (a point here).
    let reference = gpu.render_reference(&camera, 4);
    for &p in shadowed.iter().step_by(7) {
        assert_eq!(red(&reference, p), 0, "{p:?}");
    }
    for &p in lit.iter().step_by(97) {
        let (r, q) = (i32::from(red(&reference, p)), i32::from(red(&primary, p)));
        assert!((r - q).abs() <= 2, "{p:?}: reference {r}, primary {q}");
    }
}
