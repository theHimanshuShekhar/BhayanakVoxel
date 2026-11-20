//! The Reference view's estimator against answers that can be worked out (#188, ADR 0005): a
//! closed box that emits light, which has the exact answer L / (1 - albedo); the sun's light with
//! the cone's factor divided out; and the one colour function that both views take their colours
//! from. Each of the first has a twin that puts back the fault it is there to catch, and shows
//! that the test fails with it: a test that cannot fail is not a test.
//!
//! These run on lavapipe in CI. They run no FLIP and compare nothing against an image. Needs a
//! Vulkan device.

use brick_store::{BrickStore, Material, VoxelCoord};
use materials::MaterialTable;
use render::{
    Camera, EmitterEstimator, Fault, Offscreen, ReferenceOptions, Resolution, Scene, Sun,
};

const WALL: Material = Material(1);

/// A table of one Material: grey with this `albedo`, giving off `emission` (in every channel),
/// with the given colour variation. The shipped table has no Material of an albedo of 0.9 and
/// 0.98 that gives off light.
fn table(albedo: f32, emission: f32, noise_scale: f32, value_jitter: f32) -> MaterialTable {
    MaterialTable::parse(&format!(
        r#"
[[material]]
id = 1
name = "wall"
category = "natural"
colour = [{albedo}, {albedo}, {albedo}]
noise_scale = {noise_scale}
hue_jitter = 0.0
value_jitter = {value_jitter}
roughness = 1.0
metalness = 0.0
emissive = [1.0, 1.0, 1.0]
emissive_intensity = {emission}
transparency = 0.0
ior = 1.0
hardness = 1.0
density = 1000.0
fluid = "none"
"#
    ))
    .unwrap()
}

/// A hollow cube of `WALL`: the inside is `INSIDE` Voxels across and the walls are 2 thick, so a
/// ray inside never gets out.
const INSIDE: i32 = 10;

fn closed_box() -> Scene {
    let mut store = BrickStore::new();
    let outer = INSIDE + 2;
    for x in -2..outer + 2 {
        for y in -2..outer + 2 {
            for z in -2..outer + 2 {
                let inside = (0..outer - 2).contains(&x)
                    && (0..outer - 2).contains(&y)
                    && (0..outer - 2).contains(&z);
                if !inside {
                    store.set(VoxelCoord([x, y, z]), Some(WALL));
                }
            }
        }
    }
    Scene::new(&store, None)
}

const SIZE: u32 = 16;

fn inside_the_box() -> Camera {
    let middle = f64::from(INSIDE) / 2.0;
    Camera::looking_at(
        [middle, middle, middle],
        [middle + 3.0, middle - 1.0, middle + 2.0],
    )
}

/// The mean over the image's pixels and channels of the accumulation, and the standard error of
/// that mean, from how the pixels differ (each is an independent estimate: the scrambles are
/// independent).
fn mean_and_error(accumulation: &[f32]) -> (f64, f64) {
    let pixels: Vec<f64> = accumulation
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| f64::from(p[0] + p[1] + p[2]) / 3.0)
        .collect();
    let n = pixels.len() as f64;
    let mean = pixels.iter().sum::<f64>() / n;
    let variance = pixels.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, (variance / n).sqrt())
}

/// What the closed box's image comes to after `samples` samples, with `fault`: the mean and its
/// standard error, in units of the exact answer.
fn box_estimate(albedo: f32, fault: Fault, size: u32, samples: u32) -> (f64, f64) {
    let emission = 1.0 - albedo; // so that the exact answer is 1
    let mut offscreen = Offscreen::with_materials(
        Resolution::native(size, size),
        &closed_box(),
        &table(albedo, emission, 1.0, 0.0),
    )
    .unwrap();
    offscreen.use_legacy_light();
    offscreen.set_reference_options(ReferenceOptions {
        flat_colours: true,
        fault,
        ..Default::default()
    });
    // A sun that would light the inside if it could, and a sky: neither reaches in.
    offscreen.set_sun(Sun::from_angles(0.5, 0.9, 0.04));
    let camera = inside_the_box();
    for _ in 0..samples {
        offscreen.reference_sample(&camera);
    }
    mean_and_error(&offscreen.read_accumulation())
}

/// The light that the depth cap leaves out (ADR 0005's bias ledger): `albedo^256` of the answer.
fn ledger_bound(albedo: f32) -> f64 {
    f64::from(albedo).powi(256)
}

/// What the box comes to with the cap, in units of the exact answer L / (1 - albedo): a path
/// collects L at each of its (at most 256) surfaces, which is L (1 - albedo^256) / (1 - albedo).
fn answer_at_the_cap(albedo: f32) -> f64 {
    1.0 - ledger_bound(albedo)
}

/// The image of the precise test: 64 by 64 pixels and 256 samples. At 0.98 the pixels' standard
/// error is about 0.09 %, so five of them are 0.46 %: under the ledger's 0.57 % and well under
/// ADR 0005's 1 % rule, so a bias of 2 % (a depth cap of 190, say) cannot pass.
const PRECISE_SIZE: u32 = 64;
const SAMPLES: u32 = 256;

/// The precise test: the box comes to L / (1 - albedo), less the light the cap leaves out, within
/// five standard errors, which are themselves under the ledger's 0.57 %.
fn the_box_comes_to_its_answer(albedo: f32) {
    let started = std::time::Instant::now();
    let (mean, error) = box_estimate(albedo, Fault::None, PRECISE_SIZE, SAMPLES);
    // The exact answer is 1 in these units (L = 1 - albedo, so L / (1 - albedo) = 1); with the cap
    // it is 1 - albedo^256, which the estimate is off from by noise only, measured from the
    // pixels.
    let expected = answer_at_the_cap(albedo);
    eprintln!(
        "albedo {albedo}: {mean:.5} of the answer, expected {expected:.5} with the cap, error \
         {error:.5} ({:.1} s)",
        started.elapsed().as_secs_f64()
    );
    assert!(
        5.0 * error < 0.0057,
        "albedo {albedo}: the test's own precision, five errors = {}, is above the 0.57 % bound",
        5.0 * error
    );
    assert!(
        (mean - expected).abs() < 5.0 * error,
        "albedo {albedo}: the box comes to {mean} of its answer, expected {expected} (error {error})"
    );
}

#[test]
fn a_closed_box_that_emits_light_comes_to_l_over_one_minus_albedo_at_0_9() {
    the_box_comes_to_its_answer(0.9);
}

#[test]
fn a_closed_box_that_emits_light_comes_to_l_over_one_minus_albedo_at_0_98() {
    the_box_comes_to_its_answer(0.98);
}

#[test]
fn the_closed_box_fails_with_the_4_bounce_cut_put_back() {
    // The estimator that ADR 0005 replaced stops after five surfaces: at albedo 0.9 it leaves out
    // 0.9^5 = 59 % of the light, at 0.98 90 %. The test above cannot pass with it.
    for (albedo, loses) in [(0.9f32, 0.59f64), (0.98, 0.904)] {
        let (mean, error) = box_estimate(albedo, Fault::FourBounceCut, SIZE, 16);
        eprintln!("albedo {albedo}, 4-bounce cut: {mean:.4} of the answer (error {error:.4})");
        assert!(
            mean < answer_at_the_cap(albedo) - 5.0 * error,
            "the cut was not caught at albedo {albedo}: {mean}"
        );
        assert!(
            (mean - (1.0 - loses)).abs() < 0.03,
            "it is short by what the cut leaves out, {loses}: {mean}"
        );
    }
}

#[test]
fn the_closed_box_fails_with_a_depth_cap_of_190_instead_of_256() {
    // The cap at 190 leaves out 0.98^190 = 2.2 % of the light at albedo 0.98: more than ADR
    // 0005's 1 % rule, and what the precise test above must see.
    let albedo = 0.98f32;
    let (mean, error) = box_estimate(albedo, Fault::ShortCap, PRECISE_SIZE, SAMPLES);
    let expected = answer_at_the_cap(albedo);
    eprintln!("albedo {albedo}, cap of 190: {mean:.5} of the answer (error {error:.5})");
    assert!(
        (mean - expected).abs() > 5.0 * error,
        "a cap of 190 was not caught: {mean}, expected {expected} (error {error})"
    );
}

#[test]
fn the_closed_box_fails_with_the_roulettes_rescale_dropped() {
    // A path that goes on is not counted for more, so each roulette takes a share of the light
    // away: the box comes out dark at both albedos.
    for albedo in [0.9f32, 0.98] {
        let (mean, error) = box_estimate(albedo, Fault::NoRescale, SIZE, 64);
        eprintln!("albedo {albedo}, no rescale: {mean:.4} of the answer (error {error:.4})");
        assert!(
            mean < answer_at_the_cap(albedo) - 5.0 * error,
            "a dropped rescale was not caught at albedo {albedo}: {mean}"
        );
        assert!(mean < 0.9, "albedo {albedo}: {mean}");
    }
}

// ---------------------------------------------------------------------------------------------
// Emitters

const LAMP: Material = Material(2);

/// A table of the wall (grey, albedo 0.6, gives off nothing) and a lamp (gives off 6 in every
/// channel), flat.
fn lit_room_table() -> MaterialTable {
    let entry = |id: u32, name: &str, albedo: f32, emission: f32| {
        format!(
            r#"
[[material]]
id = {id}
name = "{name}"
category = "natural"
colour = [{albedo}, {albedo}, {albedo}]
noise_scale = 1.0
hue_jitter = 0.0
value_jitter = 0.0
roughness = 1.0
metalness = 0.0
emissive = [1.0, 1.0, 1.0]
emissive_intensity = {emission}
transparency = 0.0
ior = 1.0
hardness = 1.0
density = 1000.0
fluid = "none"
"#
        )
    };
    MaterialTable::parse(&format!(
        "{}{}",
        entry(1, "wall", 0.6, 0.0),
        entry(2, "lamp", 0.2, 6.0)
    ))
    .unwrap()
}

/// The closed box with two lamps in it: a block of 6 Voxels on the floor (across a brick boundary) and
/// a small one in the air, so that a Voxel's list holds two emitters, one of them hidden from part of the room by the
/// other.
fn a_room_with_two_lamps() -> Scene {
    let mut store = BrickStore::new();
    let outer = 24;
    for x in -2..outer + 2 {
        for y in -2..outer + 2 {
            for z in -2..outer + 2 {
                let inside =
                    (0..outer).contains(&x) && (0..outer).contains(&y) && (0..outer).contains(&z);
                if !inside {
                    store.set(VoxelCoord([x, y, z]), Some(WALL));
                }
            }
        }
    }
    // The big lamp straddles the boundary of two bricks (x = 8), so that it is two emitter boxes
    // that sampling must weigh as one light: the two halves' light is not counted twice, nor lost.
    for x in 5..11 {
        for y in 0..6 {
            for z in 9..15 {
                store.set(VoxelCoord([x, y, z]), Some(LAMP));
            }
        }
    }
    for x in 19..21 {
        for y in 12..14 {
            for z in 3..5 {
                store.set(VoxelCoord([x, y, z]), Some(LAMP));
            }
        }
    }
    Scene::new(&store, None)
}

/// The mean of the Reference's image of the room, in luminance, and its standard error, with the
/// emitters' light counted by `emitters` and with `fault`.
fn the_lamps_room(emitters: EmitterEstimator, fault: Fault, samples: u32) -> (f64, f64) {
    let size = 24;
    let mut offscreen = Offscreen::with_materials(
        Resolution::native(size, size),
        &a_room_with_two_lamps(),
        &lit_room_table(),
    )
    .unwrap();
    offscreen.use_legacy_light();
    offscreen.set_reference_options(ReferenceOptions {
        flat_colours: true,
        emitters,
        fault,
        ..Default::default()
    });
    offscreen.set_sun(Sun::from_angles(0.5, 0.9, 0.04));
    let camera = Camera::looking_at([3.0, 8.0, 21.0], [12.0, 3.0, 8.0]);
    for _ in 0..samples {
        offscreen.reference_sample(&camera);
    }
    mean_and_error(&offscreen.read_accumulation())
}

#[test]
fn the_emitters_samples_and_the_bounce_rays_weigh_to_the_same_light_as_either_alone() {
    // The light of the room is the same expected value whether the lamps' light is found by the
    // rays that bounce into them, by the rays aimed at them, or by both weighed against each
    // other (MIS): the estimators differ in noise, not in value.
    const SAMPLES: u32 = 192;
    let (both, e_both) = the_lamps_room(EmitterEstimator::Both, Fault::None, SAMPLES);
    let (bounce, e_bounce) = the_lamps_room(EmitterEstimator::BounceOnly, Fault::None, SAMPLES);
    let (samples, e_samples) = the_lamps_room(EmitterEstimator::SamplesOnly, Fault::None, SAMPLES);
    eprintln!(
        "both {both:.4} (+-{e_both:.4}), bounce {bounce:.4} (+-{e_bounce:.4}), samples \
         {samples:.4} (+-{e_samples:.4})"
    );
    assert!(both > 0.1, "there is light to count: {both}");
    for (name, other, error) in [
        ("bounce rays only", bounce, e_bounce),
        ("the emitters' samples only", samples, e_samples),
    ] {
        let d = (both - other).abs() / both;
        assert!(
            d < 0.03 && (both - other).abs() < 5.0 * (error + e_both),
            "{name}: {other} against {both} (both) differ by {d}"
        );
    }
}

#[test]
fn counting_the_emitters_in_full_twice_is_caught_by_that_test() {
    const SAMPLES: u32 = 192;
    let (honest, _) = the_lamps_room(EmitterEstimator::Both, Fault::None, SAMPLES);
    let (doubled, _) = the_lamps_room(EmitterEstimator::Both, Fault::EmitterDoubleCount, SAMPLES);
    let d = (doubled - honest) / honest;
    assert!(d > 0.1, "the weights add up to 2 and the light is {d} over");
}

// ---------------------------------------------------------------------------------------------
// The sun

#[test]
fn the_suns_light_is_its_radiance_with_the_cones_cosine_factor_divided_out() {
    // A floor under a sun straight above, as wide as a cone gets in a test (radius 0.4 rad, 400
    // times the engine's squared): the cone's mean cosine is (1 + cos 0.4) / 2 = 0.9605, so
    // without the divisor the floor would be 4 % short of the sun's light. The sun's light is the
    // image with the sun less the image with the sun below the horizon, which is the sky alone.
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(WALL));
            }
        }
    }
    let scene = Scene::new(&store, None);
    let table = table(0.5, 0.0, 1.0, 0.0);
    let overhead = Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5]);
    let image = |sun: Sun| {
        let mut offscreen =
            Offscreen::with_materials(Resolution::native(SIZE, SIZE), &scene, &table).unwrap();
        offscreen.use_legacy_light();
        offscreen.set_reference_options(ReferenceOptions {
            flat_colours: true,
            ..Default::default()
        });
        offscreen.set_sun(sun);
        for _ in 0..64 {
            offscreen.reference_sample(&overhead);
        }
        let accumulation = offscreen.read_accumulation();
        let n = f64::from(SIZE * SIZE);
        (0..3)
            .map(move |c| {
                accumulation
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|p| f64::from(p[c]))
                    .sum::<f64>()
                    / n
            })
            .collect::<Vec<f64>>()
    };
    let lit = image(Sun::new([0.0, 1.0, 0.0], 0.4).unwrap());
    let sky_only = image(Sun::new([0.0, -1.0, 0.0], 0.4).unwrap());
    let radiance = [1.6, 1.5, 1.35];
    for c in 0..3 {
        // albedo 0.5, facing the sun head on: 0.5 * SUN_RADIANCE exactly.
        let sun = lit[c] - sky_only[c];
        let want = 0.5 * radiance[c];
        assert!(
            (sun / want - 1.0).abs() < 0.003,
            "channel {c}: the sun adds {sun}, it should add {want} ({:+.2} %)",
            (sun / want - 1.0) * 100.0
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Colours

/// Primary's G-buffer colour of each pixel and the Reference view's image of the same floor,
/// where every pixel of the floor is lit by the same light (a flat floor, a sun straight above it,
/// a uniform sky): its light is albedo times a constant. The G-buffer holds the colour of the
/// Voxel at the centre of the pixel, so a pixel that straddles two Voxels may differ.
/// Returns the share of pixels whose Reference value is that colour times the same constant, to
/// within 5 %.
fn share_that_follow_primarys_colours(flat_colours: bool) -> f64 {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(WALL));
            }
        }
    }
    let scene = Scene::new(&store, None);
    // Colours that vary a lot between Voxels, so that following them is not an accident.
    let table = table(0.5, 0.0, 1.0, 0.5);
    let size = 64;
    let mut offscreen =
        Offscreen::with_materials(Resolution::native(size, size), &scene, &table).unwrap();
    offscreen.use_legacy_light();
    offscreen.set_reference_options(ReferenceOptions {
        flat_colours,
        ..Default::default()
    });
    offscreen.set_sun(Sun::new([0.0, 1.0, 0.0], 0.02).unwrap());
    // Close to the floor, so that a pixel is a small part of a Voxel.
    let camera = Camera::looking_at([0.5, 3.0, 0.5], [0.5, 0.0, 0.5]);
    offscreen.render(&camera);
    let primary = offscreen.gbuffer().albedo;
    offscreen.render_reference(&camera, 64);
    let reference = offscreen.read_accumulation();
    let ratios: Vec<f64> = (0..(size * size) as usize)
        .map(|i| {
            let colour = f64::from(render::srgb_decode(f32::from(primary[i * 4 + 1]) / 255.0));
            f64::from(reference[i * 3 + 1]) / colour
        })
        .collect();
    let mut sorted = ratios.clone();
    sorted.sort_by(f64::total_cmp);
    let typical = sorted[sorted.len() / 2];
    ratios
        .iter()
        .filter(|r| (*r / typical - 1.0).abs() < 0.05)
        .count() as f64
        / ratios.len() as f64
}

#[test]
fn primary_and_the_reference_take_their_colours_from_one_function() {
    // The Reference's pixel is the colour of the Voxel there, times the light that every pixel of
    // this floor has, and Primary's G-buffer holds that colour: the two follow each other.
    let varied = share_that_follow_primarys_colours(false);
    eprintln!(
        "the Reference follows Primary's colours at {:.1} % of the pixels",
        varied * 100.0
    );
    assert!(varied > 0.85, "{varied}");
    // And the test can fail: in the flat mode the Reference has the Material's one colour, which
    // Primary's varied colours do not follow.
    let flat = share_that_follow_primarys_colours(true);
    eprintln!("the flat mode follows them at {:.1} %", flat * 100.0);
    assert!(flat < 0.5, "{flat}");
}
